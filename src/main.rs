use anyhow::{Context, Result};
use log::{error, info};
use rdev::{listen, Event, EventType, Key};
use regex::Regex;
use serde::{Deserialize, Serialize};
use simplelog::{Config, LevelFilter, WriteLogger};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Serialize, Deserialize, Default, PartialEq, Clone)]
struct AppConfig {
    window_layout_map: HashMap<String, u8>,
    hotkeys: HashMap<String, String>,
}

#[derive(Debug, Default)]
struct ModifierState {
    shift: bool,
    ctrl: bool,
    alt: bool,
    meta: bool,
}

impl ModifierState {
    fn update(&mut self, key: &Key, is_press: bool) {
        match key {
            Key::ShiftLeft | Key::ShiftRight => self.shift = is_press,
            Key::ControlLeft | Key::ControlRight => self.ctrl = is_press,
            Key::Alt | Key::AltGr => self.alt = is_press,
            Key::MetaLeft | Key::MetaRight => self.meta = is_press,
            _ => {}
        }
    }

    fn matches(&self, required_mods: &HashSet<&str>) -> bool {
        (required_mods.contains("shift") == self.shift)
            && (required_mods.contains("ctrl") == self.ctrl)
            && (required_mods.contains("alt") == self.alt)
            && (required_mods.contains("meta") == self.meta)
    }
}

struct NSKeyboardLayoutSwitcher {
    config_path: PathBuf,
    log_path: PathBuf,
    config: Arc<Mutex<AppConfig>>,
    last_window_class: Option<String>,
    last_config_check: u64,
}

impl NSKeyboardLayoutSwitcher {
    fn new(config_file: &str, log_file: &str) -> Result<Self> {
        let current_dir = env::current_dir()?;
        let config_path = current_dir.join(config_file);
        let log_path = current_dir.join(log_file);

        WriteLogger::init(
            LevelFilter::Info,
            Config::default(),
            File::create(&log_path)
                .with_context(|| format!("Failed to create log file: {:?}", log_path))?,
        )?;

        let config = AppConfig::load_from_file(&config_path)?;

        Ok(Self {
            config_path,
            log_path,
            config: Arc::new(Mutex::new(config)),
            last_window_class: None,
            last_config_check: 0,
        })
    }

    fn get_xkblayout_state_path(&self) -> PathBuf {
        let mut path = env::current_exe()
            .expect("Failed to get current executable path")
            .parent()
            .expect("Failed to get parent directory")
            .to_path_buf();
        path.push("xkblayout-state-bin");
        path
    }

    fn reload_config_if_needed(&mut self) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        if now - self.last_config_check > 5 {
            self.last_config_check = now;
            let new_config = AppConfig::load_from_file(&self.config_path)?;
            let mut config = self.config.lock().unwrap();
            if new_config != *config {
                info!("Config reloaded from disk");
                *config = new_config;
            }
        }
        Ok(())
    }

    fn save_config(&self) -> Result<()> {
        let config = self.config.lock().unwrap();
        config.save_to_file(&self.config_path)
    }

    fn get_active_window_class(&self) -> Option<String> {
        let window_id = Command::new("xdotool")
            .arg("getactivewindow")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())?;

        let output = Command::new("xprop")
            .arg("-id")
            .arg(window_id.trim())
            .arg("WM_CLASS")
            .output()
            .ok()?;

        let wm_class = String::from_utf8(output.stdout).ok()?;
        Regex::new(r#"WM_CLASS.*?"[^"]*",\s*"([^"]*)"#)
            .ok()?
            .captures(&wm_class)?
            .get(1)
            .map(|m| m.as_str().to_lowercase())
    }

    fn get_current_layout(&self) -> Option<u8> {
        let xkblayout_path = self.get_xkblayout_state_path();

        Command::new(xkblayout_path)
            .arg("print")
            .arg("%s")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|l| {
                if l.to_lowercase().contains("ru") {
                    1
                } else {
                    0
                }
            })
    }

    fn add_current_window(&self) -> Result<()> {
        match (self.get_active_window_class(), self.get_current_layout()) {
            (Some(window_class), Some(layout)) => {
                let mut config = self.config.lock().unwrap();
                config
                    .window_layout_map
                    .insert(window_class.clone(), layout);
                self.save_config()?;
                println!("Added: {} -> {}", window_class, layout);
                info!("Added mapping: {} -> {}", window_class, layout);
                Ok(())
            }
            _ => {
                let msg = "Failed to detect window or layout";
                error!("{}", msg);
                Err(anyhow::anyhow!(msg))
            }
        }
    }

    fn switch_layout(&self, layout_code: u8) -> Result<()> {
        let xkblayout_path = self.get_xkblayout_state_path();

        Command::new(xkblayout_path)
            .arg("set")
            .arg(layout_code.to_string())
            .status()
            .with_context(|| "Failed to switch layout")?;
        info!("Layout switched to: {}", layout_code);
        Ok(())
    }

    fn start_keyboard_listener(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let switcher = Arc::new(self.clone());

        thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_action_time = SystemTime::UNIX_EPOCH;

            let callback = move |event: Event| {
                let now = SystemTime::now();

                match event.event_type {
                    EventType::KeyPress(key) => {
                        pressed_keys.insert(key.clone());
                        modifiers.update(&key, true);

                        let config = config.lock().unwrap();
                        if let Some(hotkey) = config.hotkeys.get("add_window") {
                            if Self::check_hotkey(&pressed_keys, &modifiers, hotkey) {
                                if now.duration_since(last_action_time).unwrap_or_default()
                                    > Duration::from_millis(500)
                                {
                                    last_action_time = now;
                                    if let Err(e) = switcher.add_current_window() {
                                        error!("Failed to add window: {}", e);
                                    }
                                }
                            }
                        }
                    }
                    EventType::KeyRelease(key) => {
                        pressed_keys.remove(&key);
                        modifiers.update(&key, false);
                    }
                    _ => {}
                }
            };

            if let Err(e) = listen(callback) {
                error!("Keyboard listener error: {:?}", e);
            }
        });

        Ok(())
    }

    fn check_hotkey(
        pressed_keys: &HashSet<Key>,
        modifiers: &ModifierState,
        hotkey_str: &str,
    ) -> bool {
        let parts: Vec<&str> = hotkey_str.split_whitespace().collect();
        let mut required_mods = HashSet::new();
        let mut required_key = None;

        for part in parts {
            match part.to_lowercase().as_str() {
                "shift" => required_mods.insert("shift"),
                "ctrl" => required_mods.insert("ctrl"),
                "alt" => required_mods.insert("alt"),
                "meta" => required_mods.insert("meta"),
                key => {
                    required_key = match key {
                        "q" => Some(Key::KeyQ),
                        "w" => Some(Key::KeyW),
                        "e" => Some(Key::KeyE),
                        "r" => Some(Key::KeyR),
                        "t" => Some(Key::KeyT),
                        "a" => Some(Key::KeyA),
                        "s" => Some(Key::KeyS),
                        "d" => Some(Key::KeyD),
                        "f" => Some(Key::KeyF),
                        "g" => Some(Key::KeyG),
                        "z" => Some(Key::KeyZ),
                        "x" => Some(Key::KeyX),
                        "c" => Some(Key::KeyC),
                        "v" => Some(Key::KeyV),
                        "b" => Some(Key::KeyB),
                        "m" => Some(Key::KeyM),
                        _ => None,
                    };
                    false
                }
            };
        }

        modifiers.matches(&required_mods)
            && required_key.map_or(false, |k| pressed_keys.contains(&k))
    }

    fn run(&mut self) -> Result<()> {
        info!("Service started");
        println!("Keyboard layout switcher started (Ctrl+C to stop)");
        println!("Logging to: {:?}", self.log_path);

        self.start_keyboard_listener()?;

        loop {
            self.reload_config_if_needed()?;
            if let Some(current_class) = self.get_active_window_class() {
                if self.last_window_class.as_ref() != Some(&current_class) {
                    self.last_window_class = Some(current_class.clone());
                    let config = self.config.lock().unwrap();
                    if let Some(target_layout) = config.window_layout_map.get(&current_class) {
                        if let Some(current_layout) = self.get_current_layout() {
                            if current_layout != *target_layout {
                                self.switch_layout(*target_layout)
                                    .unwrap_or_else(|e| error!("Failed to switch layout: {}", e));
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(300));
        }
    }
}

impl Clone for NSKeyboardLayoutSwitcher {
    fn clone(&self) -> Self {
        Self {
            config_path: self.config_path.clone(),
            log_path: self.log_path.clone(),
            config: Arc::clone(&self.config),
            last_window_class: self.last_window_class.clone(),
            last_config_check: self.last_config_check,
        }
    }
}

impl AppConfig {
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        if path.exists() {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {:?}", path))?;
            let mut config: AppConfig =
                serde_json::from_str(&content).with_context(|| "Failed to parse config")?;

            if !config.hotkeys.contains_key("add_window") {
                config
                    .hotkeys
                    .insert("add_window".to_string(), "ctrl shift q".to_string());
                config.save_to_file(path)?;
            }

            Ok(config)
        } else {
            let default_config = AppConfig {
                window_layout_map: HashMap::new(),
                hotkeys: [("add_window".to_string(), "ctrl shift q".to_string())]
                    .iter()
                    .cloned()
                    .collect(),
            };
            default_config.save_to_file(path)?;
            Ok(default_config)
        }
    }

    fn save_to_file(&self, path: &PathBuf) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content).with_context(|| format!("Failed to write config to {:?}", path))
    }
}

fn main() -> Result<()> {
    let mut switcher = NSKeyboardLayoutSwitcher::new("config.json", "kbd.log")?;
    if std::env::args().any(|arg| arg == "--add") {
        println!("Adding current window...");
        match switcher.add_current_window() {
            Ok(_) => println!("Success! Window added to config."),
            Err(e) => println!("Failed! Error: {}", e),
        }
    } else {
        switcher.run()?;
    }
    Ok(())
}
