use anyhow::{anyhow, Context, Result};
use log::{debug, error, info, warn};
use rdev::{listen, Event, EventType, Key};
use regex::Regex;
use serde::{Deserialize, Serialize};
use simplelog::{Config as LogConfig, LevelFilter, WriteLogger};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    os::unix::prelude::FileExt,
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
        let current_dir = env::current_dir().context("Failed to get current directory")?;
        let config_path = current_dir.join(config_file);
        let log_path = current_dir.join(log_file);

        if log_path.exists() {
            fs::remove_file(&log_path).ok();
        }

        let log_file = File::create(&log_path).context(format!(
            "Failed to create log file at {}",
            log_path.display()
        ))?;

        WriteLogger::init(LevelFilter::Debug, LogConfig::default(), log_file)
            .context("Failed to initialize logger")?;

        info!("Initializing keyboard switcher...");
        debug!("Config path: {:?}", config_path);

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
            "c" => Some(Key::KeyC),
            "d" => Some(Key::KeyD),
            "e" => Some(Key::KeyE),
            "f" => Some(Key::KeyF),
            "g" => Some(Key::KeyG),
            "h" => Some(Key::KeyH),
            "i" => Some(Key::KeyI),
            "j" => Some(Key::KeyJ),
            "k" => Some(Key::KeyK),
            "l" => Some(Key::KeyL),
            "m" => Some(Key::KeyM),
            "n" => Some(Key::KeyN),
            "o" => Some(Key::KeyO),
            "p" => Some(Key::KeyP),
            "q" => Some(Key::KeyQ),
            "r" => Some(Key::KeyR),
            "s" => Some(Key::KeyS),
            "t" => Some(Key::KeyT),
            "u" => Some(Key::KeyU),
            "v" => Some(Key::KeyV),
            "w" => Some(Key::KeyW),
            "x" => Some(Key::KeyX),
            "y" => Some(Key::KeyY),
            "z" => Some(Key::KeyZ),
            "0" => Some(Key::Num0),
            "1" => Some(Key::Num1),
            "2" => Some(Key::Num2),
            "3" => Some(Key::Num3),
            "4" => Some(Key::Num4),
            "5" => Some(Key::Num5),
            "6" => Some(Key::Num6),
            "7" => Some(Key::Num7),
            "8" => Some(Key::Num8),
            "9" => Some(Key::Num9),
            "f1" => Some(Key::F1),
            "f2" => Some(Key::F2),
            "f3" => Some(Key::F3),
            "f4" => Some(Key::F4),
            "f5" => Some(Key::F5),
            "f6" => Some(Key::F6),
            "f7" => Some(Key::F7),
            "f8" => Some(Key::F8),
            "f9" => Some(Key::F9),
            "f10" => Some(Key::F10),
            "f11" => Some(Key::F11),
            "f12" => Some(Key::F12),
            "space" => Some(Key::Space),
            "enter" => Some(Key::Return),
            "tab" => Some(Key::Tab),
            "backspace" => Some(Key::Backspace),
            "escape" => Some(Key::Escape),
            "insert" => Some(Key::Insert),
            "delete" => Some(Key::Delete),
            "home" => Some(Key::Home),
            "end" => Some(Key::End),
            "pageup" => Some(Key::PageUp),
            "pagedown" => Some(Key::PageDown),
            "up" => Some(Key::UpArrow),
            "down" => Some(Key::DownArrow),
            "left" => Some(Key::LeftArrow),
            "right" => Some(Key::RightArrow),
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
        debug!("Getting active window class...");

        let output = match Command::new("xdotool").arg("getactivewindow").output() {
            Ok(o) => o,
            Err(e) => {
                error!("xdotool failed: {}", e);
                return None;
            }
        };

        let window_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!("Window ID: {}", window_id);

        let output = match Command::new("xprop")
            .arg("-id")
            .arg(&window_id)
            .arg("WM_CLASS")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                error!("xprop failed: {}", e);
                return None;
            }
        };

        let wm_class = String::from_utf8_lossy(&output.stdout);
        debug!("Raw WM_CLASS output:\n{}", wm_class);

        let re = Regex::new(r#"WM_CLASS.*?"(?:[^"]*?",\s*)?"([^"]+)"#).unwrap();

        re.captures(&wm_class)
            .and_then(|caps| caps.get(1))
            .map(|m| {
                let class = m.as_str().to_lowercase();
                debug!("Parsed window class: {}", class);
                class
            })
    }

    fn get_current_layout(&self) -> Option<u8> {
        debug!("Getting current layout...");

        let output = match Command::new(self.get_xkblayout_state_path())
            .arg("print")
            .arg("%c")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                error!("xkblayout-state execution failed: {}", e);
                return None;
            }
        };

        if !output.status.success() {
            error!(
                "xkblayout-state error: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        debug!("xkblayout-state output: {}", output_str);

        output_str
            .trim()
            .parse::<u8>()
            .map_err(|e| {
                error!("Failed to parse layout: {}", e);
                e
            })
            .ok()
    }

    fn add_current_window(&self) -> Result<()> {
        info!("Attempting to add current window...");

        let window_class = self
            .get_active_window_class()
            .context("Window class detection failed")?;

        let layout = self
            .get_current_layout()
            .context("Layout detection failed")?;

        info!("Detected: window '{}' -> layout {}", window_class, layout);

        let mut config = match self.config.lock() {
            Ok(c) => c,
            Err(e) => {
                error!("Config lock poisoned: {}", e);
                return Err(anyhow!("Config lock error"));
            }
        };

        config
            .window_layout_map
            .insert(window_class.clone(), layout);

        config
            .save_to_file(&self.config_path)
            .context("Failed to save config")?;

        // Явная синхронизация файла
        let file = File::open(&self.config_path)?;
        file.sync_all()?;

        info!("Successfully added mapping: {} => {}", window_class, layout);
        debug!("Current config state: {:?}", *config);

        Ok(())
    }

    fn switch_layout(&self, layout: u8) -> Result<()> {
        info!("Switching to layout {}", layout);

        Command::new(self.get_xkblayout_state_path())
            .arg("set")
            .arg(layout.to_string())
            .status()
            .context("Failed to execute layout switch")?;

        Ok(())
    }

    fn start_keyboard_listener(&self) -> Result<()> {
        info!("Starting keyboard listener...");

        let config = Arc::clone(&self.config);
        let switcher = self.clone();

        thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_hotkey = SystemTime::now();

            info!("Keyboard listener thread started");

            let callback = move |event: Event| {
                match event.event_type {
                    EventType::KeyPress(key) => {
                        debug!("Key pressed: {:?}", key);
                        pressed_keys.insert(key.clone());
                        modifiers.update(&key, true);

                        let config = match config.lock() {
                            Ok(c) => c,
                            Err(e) => {
                                error!("Config lock error: {}", e);
                                return;
                            }
                        };

                        if let Some(hotkey) = config.hotkeys.get("add_window") {
                            debug!("Checking hotkey: {}", hotkey);

                            if Self::check_hotkey(&pressed_keys, &modifiers, hotkey) {
                                let now = SystemTime::now();
                                let since_last =
                                    now.duration_since(last_hotkey).unwrap_or_default();

                                if since_last > Duration::from_secs(1) {
                                    info!("Hotkey detected!");
                                    last_hotkey = now;

                                    // Клонируем и запускаем в отдельном потоке
                                    let switcher_clone = switcher.clone();
                                    thread::spawn(move || {
                                        if let Err(e) = switcher_clone.add_current_window() {
                                            error!("Failed to process hotkey: {}", e);
                                        }
                                    });
                                }
                            }
                        }
                    }
                    EventType::KeyRelease(key) => {
                        debug!("Key released: {:?}", key);
                        pressed_keys.remove(&key);
                        modifiers.update(&key, false);
                    }
                    _ => {}
                }
            };

            if let Err(e) = listen(callback) {
                error!("Keyboard listener error: {}", e);
            }
        });

        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        info!("Starting main loop...");
        self.start_keyboard_listener()?;

        loop {
            if let Some(current_class) = self.get_active_window_class() {
                debug!("Current window class: {}", current_class);

                if self.last_window_class.as_ref() != Some(&current_class) {
                    info!("Window focus changed to: {}", current_class);
                    self.last_window_class = Some(current_class.clone());

                    let config = match self.config.lock() {
                        Ok(c) => c,
                        Err(e) => {
                            error!("Config lock error: {}", e);
                            continue;
                        }
                    };

                    if let Some(target_layout) = config.window_layout_map.get(&current_class) {
                        debug!("Found target layout: {}", target_layout);

                        if let Some(current_layout) = self.get_current_layout() {
                            if current_layout != *target_layout {
                                info!(
                                    "Switching layout from {} to {}",
                                    current_layout, target_layout
                                );
                                if let Err(e) = self.switch_layout(*target_layout) {
                                    error!("Layout switch failed: {}", e);
                                }
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
        info!("Loading config from: {:?}", path);

        if path.exists() {
            let content = fs::read_to_string(path)
                .context(format!("Failed to read config file: {}", path.display()))?;

            serde_json::from_str(&content)
                .context(format!("Failed to parse config file: {}", path.display()))
        } else {
            warn!("Config file not found, creating default");
            let config = AppConfig {
                window_layout_map: HashMap::new(),
                hotkeys: HashMap::from([("add_window".into(), "ctrl shift q".into())]),
            };
            config.save_to_file(path)?;
            Ok(config)
        }
    }

    fn save_to_file(&self, path: &PathBuf) -> Result<()> {
        info!("Saving config to: {:?}", path);

        let content = serde_json::to_string_pretty(self).context("Failed to serialize config")?;

        let mut file = File::create(path)
            .context(format!("Failed to create config file: {}", path.display()))?;

        file.write_all(content.as_bytes())
            .context(format!("Failed to write config to: {}", path.display()))?;

        file.sync_all()?; // Форсированная запись на диск

        Ok(())
    }
}

fn main() -> Result<()> {
    let mut switcher = match NSKeyboardLayoutSwitcher::new("config.json", "kbd_switcher.log") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FATAL INIT ERROR: {}", e);
            std::process::exit(1);
        }
    };

    info!("Application started");

    if env::args().any(|arg| arg == "--add") {
        info!("Running in add mode");
        switcher.add_current_window()?;
        println!("Current window added to config");
    } else {
        info!("Running in daemon mode");
        switcher.run()?;
    }

    Ok(())
}
