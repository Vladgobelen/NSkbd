use anyhow::{anyhow, Context, Result};
use log::{error, info};
use rdev::{listen, Event, EventType, Key};
use regex::Regex;
use serde::{Deserialize, Serialize};
use simplelog::{Config as LogConfig, LevelFilter, WriteLogger};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
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
    last_config_check: SystemTime,
}

impl NSKeyboardLayoutSwitcher {
    fn new(config_file: &str, log_file: &str) -> Result<Self> {
        let current_dir = env::current_dir()?;
        let config_path = current_dir.join(config_file);
        let log_path = current_dir.join(log_file);

        WriteLogger::init(
            LevelFilter::Info,
            LogConfig::default(),
            File::create(&log_path).context("Failed to create log file")?,
        )?;

        let config = AppConfig::load_from_file(&config_path)?;

        Ok(Self {
            config_path,
            log_path,
            config: Arc::new(Mutex::new(config)),
            last_window_class: None,
            last_config_check: SystemTime::now(),
        })
    }

    fn get_xkblayout_state_path(&self) -> PathBuf {
        env::current_exe()
            .expect("Failed to get executable path")
            .parent()
            .expect("No parent directory")
            .join("xkblayout-state")
    }

    fn str_to_key(key_str: &str) -> Option<Key> {
        match key_str.to_lowercase().as_str() {
            "a" => Some(Key::KeyA),
            "b" => Some(Key::KeyB),
            // ... остальные клавиши
            "q" => Some(Key::KeyQ),
            // ... остальные клавиши
            "space" => Some(Key::Space),
            "enter" => Some(Key::Return),
            _ => None,
        }
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
                "meta" | "super" | "win" => required_mods.insert("meta"),
                key_str => {
                    required_key = Self::str_to_key(key_str);
                    false
                }
            };
        }

        modifiers.matches(&required_mods)
            && required_key.map_or(false, |k| pressed_keys.contains(&k))
    }

    fn get_active_window_class(&self) -> Option<String> {
        let output = Command::new("xdotool")
            .arg("getactivewindow")
            .output()
            .ok()?;

        let window_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

        let output = Command::new("xprop")
            .arg("-id")
            .arg(&window_id)
            .arg("WM_CLASS")
            .output()
            .ok()?;

        let wm_class = String::from_utf8_lossy(&output.stdout);
        let re = Regex::new(r#"WM_CLASS.*?"(?:[^"]*?",\s*)?"([^"]+)"#).unwrap();

        re.captures(&wm_class)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_lowercase())
    }

    fn get_current_layout(&self) -> Option<u8> {
        let output = Command::new(self.get_xkblayout_state_path())
            .arg("print")
            .arg("%c")
            .output()
            .ok()?;

        if !output.status.success() {
            error!(
                "xkblayout-state failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }

        String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .parse::<u8>()
            .map_err(|e| {
                error!("Failed to parse layout: {}", e);
                e
            })
            .ok()
    }

    fn add_current_window(&self) -> Result<()> {
        let window_class = self
            .get_active_window_class()
            .context("Failed to detect window class")?;

        let layout = self
            .get_current_layout()
            .context("Failed to detect current layout")?;

        let mut config = self
            .config
            .lock()
            .map_err(|e| anyhow!("Config lock error: {}", e))?;

        config
            .window_layout_map
            .insert(window_class.clone(), layout);

        config.save_to_file(&self.config_path)?;

        info!("Added mapping: {} => {}", window_class, layout);
        Ok(())
    }

    fn switch_layout(&self, layout: u8) -> Result<()> {
        Command::new(self.get_xkblayout_state_path())
            .arg("set")
            .arg(layout.to_string())
            .status()
            .context("Failed to switch layout")?;
        Ok(())
    }

    fn start_keyboard_listener(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let switcher = self.clone();

        thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_hotkey = SystemTime::now();

            let callback = move |event: Event| match event.event_type {
                EventType::KeyPress(key) => {
                    pressed_keys.insert(key.clone());
                    modifiers.update(&key, true);

                    let config = config.lock().unwrap();
                    if let Some(hotkey) = config.hotkeys.get("add_window") {
                        if Self::check_hotkey(&pressed_keys, &modifiers, hotkey) {
                            let now = SystemTime::now();
                            if now.duration_since(last_hotkey).unwrap() > Duration::from_secs(1) {
                                last_hotkey = now;
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
            };

            if let Err(e) = listen(callback) {
                error!("Keyboard listener error: {:?}", e);
            }
        });

        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        info!("Starting keyboard layout switcher");
        self.start_keyboard_listener()?;

        loop {
            if let Some(current_class) = self.get_active_window_class() {
                if self.last_window_class.as_ref() != Some(&current_class) {
                    self.last_window_class = Some(current_class.clone());

                    let config = self.config.lock().unwrap();
                    if let Some(target_layout) = config.window_layout_map.get(&current_class) {
                        if let Some(current_layout) = self.get_current_layout() {
                            if current_layout != *target_layout {
                                self.switch_layout(*target_layout)?;
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
            let content = fs::read_to_string(path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            let config = AppConfig {
                window_layout_map: HashMap::new(),
                hotkeys: HashMap::from([("add_window".into(), "ctrl shift q".into())]),
            };
            config.save_to_file(path)?;
            Ok(config)
        }
    }

    fn save_to_file(&self, path: &PathBuf) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }
}

fn main() -> Result<()> {
    let mut switcher = NSKeyboardLayoutSwitcher::new("config.json", "kbd_switcher.log")?;

    if env::args().any(|arg| arg == "--add") {
        switcher.add_current_window()?;
        println!("Current window added to config");
    } else {
        switcher.run()?;
    }

    Ok(())
}
