use anyhow::{anyhow, Context, Result};
use log::error;
use rdev::{listen, Event as KbdEvent, EventType, Key};
use serde::{Deserialize, Serialize};
use simplelog::{Config as LogConfig, LevelFilter, WriteLogger};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
};
use x11rb::{
    connection::Connection,
    protocol::{
        xkb::{ConnectionExt as XkbConnectionExt, Group, ID},
        xproto::*,
        Event as X11Event,
    },
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
    conn: Arc<RustConnection>,
    screen_num: usize,
    xkb: XKeyboard,
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

        WriteLogger::init(LevelFilter::Error, LogConfig::default(), log_file)
            .context("Failed to initialize logger")?;

        let config = AppConfig::load_from_file(&config_path)?;

        let (conn, screen_num) = x11rb::connect(None).context("Failed to connect to X11 server")?;
        let conn = Arc::new(conn);
        let xkb = XKeyboard::new(Arc::clone(&conn))?;

        Ok(Self {
            config_path,
            log_path,
            config: Arc::new(Mutex::new(config)),
            last_window_id: None,
            conn,
            screen_num,
            xkb,
        })
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
        let wm_class_atom = self
            .conn
            .intern_atom(false, b"WM_CLASS")
            .ok()?
            .reply()
            .ok()?
            .atom;

        let reply = self
            .conn
            .get_property::<u32, u32>(
                false,
                window_id,
                wm_class_atom,
                AtomEnum::STRING.into(),
                0,
                1024,
            )
            .ok()?
            .reply()
            .ok()?;

        if reply.format != 8 || reply.value.is_empty() {
            error!("WM_CLASS property error for window {}", window_id);
            return None;
        }

        let value = String::from_utf8_lossy(&reply.value);
        let parts: Vec<&str> = value.split('\0').collect();

        if parts.len() < 2 {
            error!("WM_CLASS format error for window {}", window_id);
            return None;
        }

        let class = if !parts[1].is_empty() {
            parts[1]
        } else {
            parts[0]
        };

        if class.is_empty() {
            error!("Empty window class for window {}", window_id);
            return None;
        }

        Some(class.to_lowercase())
    }

    fn get_current_layout(&self) -> Option<u8> {
        self.xkb
            .current_layout()
            .map_err(|e| {
                error!("Failed to get current layout: {}", e);
                e
            })
            .ok()
    }

    fn add_current_window(&self) -> Result<()> {
        let window_id = self
            .get_active_window()
            .context("Failed to get window ID")?;

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

        Ok(())
    }

    fn switch_layout(&self, layout: u8) -> Result<()> {
        self.xkb
            .set_layout(layout)
            .context("Failed to switch layout")
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

                    let hotkey = {
                        let config = match config.lock() {
                            Ok(c) => c,
                            Err(e) => {
                                error!("Config lock error: {}", e);
                                return;
                            }
                        };
                        config.hotkeys.get("add_window").cloned()
                    };

                    if let Some(hotkey) = hotkey {
                        if Self::check_hotkey(&pressed_keys, &modifiers, &hotkey) {
                            let now = SystemTime::now();
                            if let Ok(duration) = now.duration_since(last_hotkey) {
                                if duration > Duration::from_secs(1) {
                                    last_hotkey = now;
                                    let switcher_clone = switcher.clone();
                                    thread::spawn(move || {
                                        if let Err(e) = switcher_clone.add_current_window() {
                                            error!("Failed to add window: {}", e);
                                        }
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
        let net_active_window = self
            .conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;

        let reply = self
            .conn
            .get_property::<u32, u32>(
                false,
                self.conn.setup().roots[self.screen_num].root,
                net_active_window,
                AtomEnum::WINDOW.into(),
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?;

        if reply.format == 32 && !reply.value.is_empty() {
            Some(u32::from_ne_bytes([
                reply.value[0],
                reply.value[1],
                reply.value[2],
                reply.value[3],
            ]))
        } else {
            None
        }
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

            if let Some(&target_layout) = config.window_layout_map.get(&window_class) {
                if let Err(e) = self.switch_layout(target_layout) {
                    error!("Failed to switch layout: {}", e);
                }
            }
        }

        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        self.start_keyboard_listener()?;

        let screen = &self.conn.setup().roots[self.screen_num];
        let net_active_window = self
            .conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom;

        self.conn.change_window_attributes(
            screen.root,
            &ChangeWindowAttributesAux::default().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        self.conn.flush()?;

        if let Some(win) = self.get_active_window() {
            self.handle_window_change(win)?;
        }

        loop {
            match self.conn.wait_for_event() {
                Ok(event) => {
                    if let X11Event::PropertyNotify(ev) = event {
                        if ev.atom == net_active_window {
                            if let Some(win) = self.get_active_window() {
                                self.handle_window_change(win)?;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("X11 event error: {}", e);
                    thread::sleep(Duration::from_millis(100));
                }
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
            screen_num: self.screen_num,
            xkb: self.xkb.clone(),
        }
    }
}

#[derive(Clone)]
struct XKeyboard {
    conn: Arc<RustConnection>,
    device_id: u16,
}

impl XKeyboard {
    fn new(conn: Arc<RustConnection>) -> Result<Self> {
        conn.xkb_use_extension(1, 0)
            .context("Failed to initialize XKB extension")?
            .reply()
            .context("Failed to get XKB extension reply")?;

        Ok(Self {
            conn,
            device_id: ID::USE_CORE_KBD.into(),
        })
    }

    fn current_layout(&self) -> Result<u8> {
        let state = self
            .conn
            .xkb_get_state(self.device_id)
            .context("Failed to get XKB state")?
            .reply()
            .context("Failed to get XKB state reply")?;
        Ok(state.group.into())
    }

    fn set_layout(&self, group_num: u8) -> Result<()> {
        for _ in 1..=3 {
            self.conn
                .xkb_latch_lock_state(
                    self.device_id,
                    ModMask::from(0u8),
                    ModMask::from(0u8),
                    true,
                    Group::from(group_num),
                    ModMask::from(0u8),
                    false,
                    0,
                )
                .context("Failed to set XKB layout")?;

            self.conn
                .flush()
                .context("Failed to flush X11 connection")?;
            thread::sleep(Duration::from_millis(50));

            match self.current_layout() {
                Ok(new_layout) if new_layout == group_num => return Ok(()),
                _ => continue,
            }
        }

        Err(anyhow!("Layout switch failed after multiple attempts"))
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
    } else {
        switcher.run()?;
    }

    Ok(())
}
