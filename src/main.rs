use anyhow::{anyhow, Context, Result};
use log::{debug, error, info};
use rdev::{listen, Event as KbdEvent, EventType, Key};
use serde::{Deserialize, Serialize};
use simplelog::{Config as LogConfig, LevelFilter, WriteLogger};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime},
};
use x11rb::{
    connection::Connection,
    protocol::{xproto::*, Event as X11Event},
    rust_connection::RustConnection,
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
        required_mods.iter().all(|&modifier| match modifier {
            "shift" => self.shift,
            "ctrl" => self.ctrl,
            "alt" => self.alt,
            "meta" => self.meta,
            _ => false,
        })
    }
}

struct KeyboardLayoutSwitcher {
    config_path: PathBuf,
    log_path: PathBuf,
    config: Arc<AppConfig>,
    last_window_id: Option<u32>,
    conn: Arc<RustConnection>,
    root_window: Window,
    net_active_window: Atom,
    wm_class: Atom,
}

impl KeyboardLayoutSwitcher {
    fn new(config_file: &str, log_file: &str) -> Result<Self> {
        let current_dir = env::current_dir().context("Failed to get current directory")?;
        let config_path = current_dir.join(config_file);
        let log_path = current_dir.join(log_file);

        if log_path.exists() {
            fs::remove_file(&log_path).ok();
        }

        let log_file = File::create(&log_path)
            .context(format!("Failed to create log file: {}", log_path.display()))?;
        WriteLogger::init(LevelFilter::Info, LogConfig::default(), log_file)
            .context("Failed to initialize logger")?;

        info!("Initializing keyboard layout switcher");

        let (conn, screen_num) = x11rb::connect(None)?;
        let screen = conn.setup().roots[screen_num].clone();

        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom;

        let wm_class = conn.intern_atom(false, b"WM_CLASS")?.reply()?.atom;

        Ok(Self {
            config_path: config_path.clone(),
            log_path,
            config: Arc::new(AppConfig::load_from_file(&config_path)?),
            last_window_id: None,
            conn: Arc::new(conn),
            root_window: screen.root,
            net_active_window,
            wm_class,
        })
    }

    fn get_active_window(&self) -> Result<u32> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root_window,
                self.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?;

        reply
            .value32()
            .and_then(|mut iter| iter.next())
            .map(|id| id as u32)
            .context("No active window found")
    }

    fn get_window_class(&self, window: u32) -> Result<String> {
        let reply = self
            .conn
            .get_property(false, window, self.wm_class, AtomEnum::STRING, 0, 1024)?
            .reply()?;

        let value = reply.value;
        let first_null = value
            .iter()
            .position(|&c| c == 0)
            .context("Invalid WM_CLASS format (missing first null)")?;

        if value.len() <= first_null + 1 {
            return Err(anyhow!("WM_CLASS too short"));
        }

        let class_part = &value[first_null + 1..];
        let end = class_part
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(class_part.len());

        Ok(String::from_utf8(class_part[..end].to_vec())
            .context("Invalid UTF-8 in WM_CLASS")?
            .to_lowercase())
    }

    fn add_current_window(&self) -> Result<()> {
        let window_id = self.get_active_window()?;
        let window_class = self.get_window_class(window_id)?;
        let layout = self.get_current_layout()?;

        let mut config = AppConfig::load_from_file(&self.config_path)?;
        config
            .window_layout_map
            .insert(window_class.clone(), layout);
        config.save_to_file(&self.config_path)?;

        info!("Added window mapping: {} => {}", window_class, layout);
        Ok(())
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
        let mut required_mods = HashSet::new();
        let mut required_key = None;

        for part in hotkey_str.split_whitespace() {
            match part.to_lowercase().as_str() {
                "shift" => required_mods.insert("shift"),
                "ctrl" => required_mods.insert("ctrl"),
                "alt" => required_mods.insert("alt"),
                "meta" | "super" | "win" => required_mods.insert("meta"),
                key_str => {
                    required_key = Self::str_to_key(key_str);
                    break;
                }
            };
        }

        modifiers.matches(&required_mods)
            && required_key.map_or(false, |k| pressed_keys.contains(&k))
    }

    fn switch_layout(&self, layout: u8) -> Result<()> {
        Command::new("xkblayout-state")
            .arg("set")
            .arg(layout.to_string())
            .status()
            .context("Failed to switch layout")?;
        info!("Switched keyboard layout to {}", layout);
        Ok(())
    }

    fn get_current_layout(&self) -> Result<u8> {
        let output = Command::new("xkblayout-state")
            .arg("print")
            .arg("%c")
            .output()
            .context("Failed to execute xkblayout-state")?;

        String::from_utf8(output.stdout)?
            .trim()
            .parse()
            .context("Failed to parse layout number")
    }

    fn start_keyboard_listener(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let switcher = self.clone();

        std::thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_hotkey = SystemTime::now();

            let callback = move |event: KbdEvent| match event.event_type {
                EventType::KeyPress(key) => {
                    pressed_keys.insert(key.clone());
                    modifiers.update(&key, true);

                    if let Some(hotkey) = config.hotkeys.get("add_window") {
                        if Self::check_hotkey(&pressed_keys, &modifiers, hotkey) {
                            let now = SystemTime::now();
                            if now.duration_since(last_hotkey).unwrap() > Duration::from_secs(1) {
                                last_hotkey = now;
                                let switcher = switcher.clone();
                                std::thread::spawn(move || {
                                    if let Err(e) = switcher.add_current_window() {
                                        error!("Failed to add window: {}", e);
                                    }
                                });
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

    fn handle_window_change(&mut self, window_id: u32) -> Result<()> {
        if self.last_window_id == Some(window_id) {
            return Ok(());
        }

        self.last_window_id = Some(window_id);
        let window_class = match self.get_window_class(window_id) {
            Ok(c) => {
                debug!("Active window class: {}", c);
                c
            }
            Err(e) => {
                error!("Failed to get window class: {}", e);
                return Ok(());
            }
        };

        if let Some(target_layout) = self.config.window_layout_map.get(&window_class) {
            if let Ok(current_layout) = self.get_current_layout() {
                if current_layout != *target_layout {
                    self.switch_layout(*target_layout)?;
                }
            }
        }

        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        self.start_keyboard_listener()?;

        self.conn.change_window_attributes(
            self.root_window,
            &ChangeWindowAttributesAux::default().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        self.conn.flush()?;

        if let Ok(window_id) = self.get_active_window() {
            self.handle_window_change(window_id)?;
        }

        loop {
            match self.conn.wait_for_event()? {
                X11Event::PropertyNotify(event) if event.atom == self.net_active_window => {
                    match self.get_active_window() {
                        Ok(window_id) => self.handle_window_change(window_id)?,
                        Err(e) => error!("Failed to handle window change: {}", e),
                    }
                }
                _ => {}
            }
        }
    }
}

impl Clone for KeyboardLayoutSwitcher {
    fn clone(&self) -> Self {
        Self {
            config_path: self.config_path.clone(),
            log_path: self.log_path.clone(),
            config: Arc::clone(&self.config),
            last_window_id: self.last_window_id,
            conn: Arc::clone(&self.conn),
            root_window: self.root_window,
            net_active_window: self.net_active_window,
            wm_class: self.wm_class,
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
                hotkeys: HashMap::from([("add_window".to_string(), "ctrl shift q".to_string())]),
            };
            config.save_to_file(path)?;
            Ok(config)
        }
    }

    fn save_to_file(&self, path: &PathBuf) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}

fn main() -> Result<()> {
    let mut switcher =
        KeyboardLayoutSwitcher::new("keyboard-switcher.json", "keyboard-switcher.log")?;

    if env::args().any(|arg| arg == "--add") {
        switcher.add_current_window()?;
        println!("Current window added to configuration");
    } else {
        switcher.run()?;
    }

    Ok(())
}
