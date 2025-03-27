use anyhow::{anyhow, Context, Result};
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
        info!("[DEBUG] Checking hotkey: {}", hotkey_str);

        let parts: Vec<&str> = hotkey_str.split_whitespace().collect();
        let mut required_mods = HashSet::new();
        let mut required_key = None;

        for part in parts {
            let lower_part = part.to_lowercase();
            match lower_part.as_str() {
                "shift" => required_mods.insert("shift"),
                "ctrl" => required_mods.insert("ctrl"),
                "alt" => required_mods.insert("alt"),
                "meta" | "super" | "win" => required_mods.insert("meta"),
                _ => {
                    required_key = Self::str_to_key(&lower_part);
                    false
                }
            };
        }

        info!("[DEBUG] Parsed hotkey components:");
        info!("  - Required mods: {:?}", required_mods);
        info!("  - Required key: {:?}", required_key);
        info!("[DEBUG] Current state:");
        info!("  - Pressed keys: {:?}", pressed_keys);
        info!("  - Modifiers: {:?}", modifiers);

        let mods_ok = modifiers.matches(&required_mods);
        let key_ok = required_key.map_or(false, |k| pressed_keys.contains(&k));

        info!(
            "[DEBUG] Match result: mods_ok={}, key_ok={}",
            mods_ok, key_ok
        );

        mods_ok && key_ok
    }

    fn get_active_window_class(&self) -> Option<String> {
        let output = Command::new("xdotool")
            .arg("getactivewindow")
            .output()
            .map_err(|e| {
                error!("[ERROR] xdotool failed: {}", e);
                e
            })
            .ok()?;

        let window_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        info!("[DEBUG] Active window ID: {}", window_id);

        let output = Command::new("xprop")
            .arg("-id")
            .arg(&window_id)
            .arg("WM_CLASS")
            .output()
            .map_err(|e| {
                error!("[ERROR] xprop failed: {}", e);
                e
            })
            .ok()?;

        let wm_class = String::from_utf8_lossy(&output.stdout);
        info!("[DEBUG] Raw WM_CLASS output:\n{}", wm_class);

        let re = Regex::new(r#"WM_CLASS.*?"(?:.*?",\s*)?"([^"]+)"#).unwrap();
        let captures = re.captures(&wm_class);

        match captures {
            Some(caps) => {
                let class = caps.get(1).unwrap().as_str().to_lowercase();
                info!("[DEBUG] Parsed window class: {}", class);
                Some(class)
            }
            None => {
                error!("[ERROR] Failed to parse WM_CLASS");
                None
            }
        }
    }

    fn get_current_layout(&self) -> Option<u8> {
        let output = Command::new(self.get_xkblayout_state_path())
            .arg("print")
            .arg("%c")
            .output()
            .map_err(|e| {
                error!("Ошибка выполнения xkblayout-state: {}", e); // Добавлено
                e
            })
            .ok()?;

        // Логируем вывод команды
        let output_str = String::from_utf8_lossy(&output.stdout);
        info!("[DEBUG] Вывод xkblayout-state: {:?}", output_str); // Добавлено

        output_str
            .trim() // Убираем пробелы и переносы строк
            .parse::<u8>()
            .map_err(|e| {
                error!("Ошибка парсинга раскладки: {}", e); // Добавлено
                e
            })
            .ok()
    }

    fn add_current_window(&self) -> Result<()> {
        let window_class = self
            .get_active_window_class()
            .context("Не удалось определить класс окна")?;

        let layout = self
            .get_current_layout()
            .context("Не удалось определить текущую раскладку")?;

        info!(
            "Добавляем правило: окно '{}' → раскладка {}",
            window_class, layout
        );

        // Исправленная строка:
        let mut config = self
            .config
            .lock()
            .map_err(|e| anyhow!("Ошибка блокировки конфига: {}", e))?;

        config
            .window_layout_map
            .insert(window_class.clone(), layout);

        config
            .save_to_file(&self.config_path)
            .context("Ошибка сохранения конфига")?;

        info!("Успешно добавлено!");

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
                info!("[DEBUG] Current window class: {}", current_class);

                if self.last_window_class.as_ref() != Some(&current_class) {
                    info!("[DEBUG] Window class changed");

                    let config = self.config.lock().unwrap();
                    info!("[DEBUG] Current config: {:?}", *config);

                    if let Some(target_layout) = config.window_layout_map.get(&current_class) {
                        info!("[DEBUG] Found layout mapping: {}", target_layout);

                        if let Some(current_layout) = self.get_current_layout() {
                            info!("[DEBUG] Current layout: {}", current_layout);

                            if current_layout != *target_layout {
                                info!("[DEBUG] Switching layout to {}", target_layout);
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
        let content = serde_json::to_string_pretty(self).map_err(|e| {
            error!("[ERROR] JSON serialization failed: {}", e);
            e
        })?;

        info!("[DEBUG] Saving config content:\n{}", content);

        fs::write(path, content).map_err(|e| {
            error!("[ERROR] File write failed: {}", e);
            e
        })?;

        info!("[SUCCESS] Config saved successfully");
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
