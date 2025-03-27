use anyhow::{anyhow, Context, Result};
use log::{error, info, warn};
use rdev::{listen, Event as KbdEvent, EventType, Key};
use serde::{Deserialize, Serialize};
use simplelog::{Config as LogConfig, LevelFilter, WriteLogger};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    io::Write,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
};
use x11rb::{
    connection::Connection,
    protocol::{xproto::*, Event as X11Event},
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

struct KeyboardLayoutSwitcher {
    config_path: PathBuf,
    log_path: PathBuf,
    config: Arc<Mutex<AppConfig>>,
    last_window_id: Option<u32>,
    conn: Option<x11rb::rust_connection::RustConnection>,
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

        info!("Initializing keyboard switcher");
        let config = AppConfig::load_from_file(&config_path)?;

        // Initialize X11 connection and required atoms
        let (conn, _screen_num) =
            x11rb::connect(None).context("Failed to connect to X11 server")?;
        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .context("Failed to intern _NET_ACTIVE_WINDOW atom")?
            .reply()
            .context("Failed to get _NET_ACTIVE_WINDOW atom reply")?
            .atom;
        let wm_class = conn
            .intern_atom(false, b"WM_CLASS")
            .context("Failed to intern WM_CLASS atom")?
            .reply()
            .context("Failed to get WM_CLASS atom reply")?
            .atom;

        Ok(Self {
            config_path,
            log_path,
            config: Arc::new(Mutex::new(config)),
            last_window_id: None,
            conn: Some(conn),
            net_active_window,
            wm_class,
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

    fn get_window_class(&self, window_id: u32) -> Option<String> {
        let conn = self.conn.as_ref()?;

        let reply = conn
            .get_property(false, window_id, self.wm_class, AtomEnum::ANY, 0, 1024)
            .ok()?
            .reply()
            .ok()?;

        if reply.format == 8 {
            // 8 means STRING format
            let value = reply.value;
            let string = String::from_utf8_lossy(&value);

            // WM_CLASS format is usually "instance\0class\0"
            let parts: Vec<&str> = string.split('\0').collect();
            if parts.len() >= 2 {
                return Some(parts[1].to_lowercase());
            }
        }

        None
    }

    fn get_current_layout(&self) -> Option<u8> {
        let output = match Command::new(self.get_xkblayout_state_path())
            .arg("print")
            .arg("%c")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                error!("xkblayout-state error: {}", e);
                return None;
            }
        };

        String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .parse::<u8>()
            .ok()
    }

    fn add_current_window(&self) -> Result<()> {
        let conn = self.conn.as_ref().context("X11 connection not available")?;
        let screen = &conn.setup().roots[0]; // Using first screen

        let reply = conn
            .get_property(
                false,
                screen.root,
                self.net_active_window,
                AtomEnum::ANY,
                0,
                1,
            )
            .context("Failed to get active window")?
            .reply()
            .context("Failed to get active window reply")?;

        if reply.value.is_empty() {
            return Err(anyhow!("No active window found"));
        }

        let window_id = u32::from_ne_bytes([
            reply.value[0],
            reply.value[1],
            reply.value[2],
            reply.value[3],
        ]);

        let window_class = self
            .get_window_class(window_id)
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
        info!("Switched layout to {}", layout);
        Ok(())
    }

    fn start_keyboard_listener(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let switcher = self.clone();

        thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_hotkey = SystemTime::now();

            let callback = move |event: KbdEvent| match event.event_type {
                EventType::KeyPress(key) => {
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
                        if Self::check_hotkey(&pressed_keys, &modifiers, hotkey) {
                            let now = SystemTime::now();
                            if let Ok(duration) = now.duration_since(last_hotkey) {
                                if duration > Duration::from_secs(1) {
                                    last_hotkey = now;
                                    let switcher_clone = switcher.clone();
                                    thread::spawn(move || {
                                        switcher_clone.add_current_window().ok();
                                    });
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

    fn get_active_window(&self) -> Option<u32> {
        let conn = self.conn.as_ref()?;
        let screen = &conn.setup().roots[0]; // Using first screen

        let reply = conn
            .get_property(
                false,
                screen.root,
                self.net_active_window,
                AtomEnum::ANY,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?;

        if reply.value.is_empty() {
            return None;
        }

        Some(u32::from_ne_bytes([
            reply.value[0],
            reply.value[1],
            reply.value[2],
            reply.value[3],
        ]))
    }

    fn handle_window_change(&mut self, window_id: u32) -> Result<()> {
        if self.last_window_id == Some(window_id) {
            return Ok(());
        }

        self.last_window_id = Some(window_id);

        if let Some(window_class) = self.get_window_class(window_id) {
            let config = self
                .config
                .lock()
                .map_err(|e| anyhow!("Config lock error: {}", e))?;

            if let Some(target_layout) = config.window_layout_map.get(&window_class) {
                if let Some(current_layout) = self.get_current_layout() {
                    if current_layout != *target_layout {
                        self.switch_layout(*target_layout)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        info!("Starting keyboard layout switcher (X11 event-based)");
        self.start_keyboard_listener()?;

        let conn = self.conn.take().context("X11 connection not available")?;
        let screen = &conn.setup().roots[0];

        conn.change_window_attributes(
            screen.root,
            &ChangeWindowAttributesAux::default().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        conn.flush()?;

        // Initial window check
        if let Some(win) = self.get_active_window() {
            self.handle_window_change(win)?;
        }

        loop {
            match conn.wait_for_event() {
                Ok(event) => {
                    if let X11Event::PropertyNotify(ev) = event {
                        if ev.atom == self.net_active_window {
                            if let Some(win) = self.get_active_window() {
                                self.handle_window_change(win)?;
                            }
                        }
                    }
                }
                Err(e) => error!("X11 event error: {}", e),
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
            conn: None, // Not cloned as it can't be safely shared
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
            warn!("Creating new config file");
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
        let mut file = File::create(path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }
}

fn main() -> Result<()> {
    let mut switcher = KeyboardLayoutSwitcher::new("config.json", "kbd_switcher.log")?;

    if env::args().any(|arg| arg == "--add") {
        switcher.add_current_window()?;
        println!("Current window added to config");
    } else {
        switcher.run()?;
    }

    Ok(())
}
