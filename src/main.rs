use anyhow::{Context, Result};
use log::{error, info};
use rdev::{listen, Event, EventType, Key, ListenError};
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
            LevelFilter::Debug,
            Config::default(),
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
                "shift" => {
                    required_mods.insert("shift");
                }
                "ctrl" => {
                    required_mods.insert("ctrl");
                }
                "alt" => {
                    required_mods.insert("alt");
                }
                "meta" => {
                    required_mods.insert("meta");
                }
                "super" => {
                    required_mods.insert("meta");
                }
                "win" => {
                    required_mods.insert("meta");
                }
                key_str => {
                    required_key = Self::str_to_key(key_str);
                }
            }
        }

        modifiers.matches(&required_mods)
            && required_key.map_or(false, |k| pressed_keys.contains(&k))
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
        Regex::new(r#"WM_CLASS.*?"\w+",\s*"(\w+)"#)
            .ok()?
            .captures(&wm_class)?
            .get(1)
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
            .map_err(|e| error!("Failed to parse layout: {}", e))
            .ok()
    }

    fn add_current_window(&self) -> Result<()> {
        info!("Adding current window to config");

        let window_class = self
            .get_active_window_class()
            .context("Failed to detect window class")?;

        let layout = self
            .get_current_layout()
            .context("Failed to detect current layout")?;

        info!("Detected window: {}, layout: {}", window_class, layout);

        let mut config = self
            .config
            .lock()
            .map_err(|e| anyhow::anyhow!("Mutex poison error: {}", e))?;

        config
            .window_layout_map
            .insert(window_class.clone(), layout);
        info!("Inserted new mapping: {} => {}", window_class, layout);

        config
            .save_to_file(&self.config_path)
            .context("Failed to save config")?;

        info!("Config saved to {:?}", self.config_path);
        Ok(())
    }

    fn switch_layout(&self, layout: u8) -> Result<()> {
        info!("Switching to layout {}", layout);

        Command::new(self.get_xkblayout_state_path())
            .arg("set")
            .arg(layout.to_string())
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to execute xkblayout-state: {}", e))
            .and_then(|status| {
                if status.success() {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "xkblayout-state failed with exit code: {}",
                        status
                    ))
                }
            })
    }

    fn start_keyboard_listener(&self) -> Result<()> {
        info!("Starting keyboard listener");

        let config = Arc::clone(&self.config);
        let config_path = self.config_path.clone();
        let log_path = self.log_path.clone();
        let xkblayout_path = self.get_xkblayout_state_path();

        thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_hotkey = SystemTime::now();

            let callback = move |event: Event| {
                match event.event_type {
                    EventType::KeyPress(key) => {
                        pressed_keys.insert(key.clone());
                        modifiers.update(&key, true);

                        let config_guard = match config.lock() {
                            Ok(guard) => guard,
                            Err(e) => {
                                error!("Mutex poison error in callback: {}", e);
                                return;
                            }
                        };

                        if let Some(hotkey) = config_guard.hotkeys.get("add_window") {
                            if Self::check_hotkey(&pressed_keys, &modifiers, hotkey) {
                                let now = SystemTime::now();
                                if now.duration_since(last_hotkey).unwrap() > Duration::from_secs(1)
                                {
                                    last_hotkey = now;
                                    info!("Hotkey detected: {}", hotkey);

                                    // Создаем полноценный экземпляр для работы
                                    let temp_switcher = NSKeyboardLayoutSwitcher {
                                        config_path: config_path.clone(),
                                        log_path: log_path.clone(),
                                        config: Arc::clone(&config),
                                        last_window_class: None,
                                        last_config_check: SystemTime::now(),
                                    };

                                    // ВЫЗЫВАЕМ ТОЧНО ТУ ЖЕ ФУНКЦИЮ, ЧТО И ПРИ --add
                                    if let Err(e) = temp_switcher.add_current_window() {
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

    fn run(&mut self) -> Result<()> {
        info!("Starting main loop");

        self.start_keyboard_listener()?;

        loop {
            if let Some(current_class) = self.get_active_window_class() {
                if self.last_window_class.as_ref() != Some(&current_class) {
                    info!("Active window changed to: {}", current_class);
                    self.last_window_class = Some(current_class.clone());

                    let config = self
                        .config
                        .lock()
                        .map_err(|e| anyhow::anyhow!("Mutex poison error: {}", e))?;

                    if let Some(target_layout) = config.window_layout_map.get(&current_class) {
                        info!(
                            "Switching layout for {} to {}",
                            current_class, target_layout
                        );
                        self.switch_layout(*target_layout)?;
                    }
                }
            }
            thread::sleep(Duration::from_millis(300));
        }
    }
}

impl AppConfig {
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            serde_json::from_str(&content).context("Failed to parse config file")
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
