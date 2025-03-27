use anyhow::{anyhow, Context, Result};
use log::{debug, error, info, warn};
use rdev::{listen, Event as KbdEvent, EventType, Key};
use serde::{Deserialize, Serialize};
use serde_json;
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
    wm_class_atom: Atom,
    utf8_string_atom: Atom,
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

        WriteLogger::init(LevelFilter::Debug, LogConfig::default(), log_file)
            .context("Failed to initialize logger")?;

        info!("Initializing keyboard switcher");
        let config = AppConfig::load_from_file(&config_path)?;

        Ok(Self {
            config_path,
            log_path,
            config: Arc::new(Mutex::new(config)),
            last_window_id: None,
            wm_class_atom: 0,
            utf8_string_atom: 0,
        })
    }

    fn setup_x11_connection(&mut self) -> Result<(RustConnection, usize)> {
        let (conn, screen_num) = x11rb::connect(None)?;

        self.wm_class_atom = conn.intern_atom(false, b"WM_CLASS")?.reply()?.atom;
        self.utf8_string_atom = conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;

        Ok((conn, screen_num))
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

    fn get_window_class(&self, conn: &impl Connection, window: u32) -> Option<String> {
        let reply = match conn.get_property(
            false,
            window,
            self.wm_class_atom,
            self.utf8_string_atom,
            0,
            1024,
        ) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply,
                Err(e) => {
                    error!("Failed to get property reply: {}", e);
                    return None;
                }
            },
            Err(e) => {
                error!("Failed to get window property: {}", e);
                return None;
            }
        };

        match String::from_utf8(reply.value) {
            Ok(s) => s
                .split('\0')
                .next()
                .map(|class| class.trim().to_lowercase()),
            Err(e) => {
                error!("Invalid UTF-8 in window class: {}", e);
                None
            }
        }
    }

    fn get_current_layout(&self) -> Option<u8> {
        let output = match Command::new(self.get_xkblayout_state_path())
            .arg("print")
            .arg("%c")
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                error!("Failed to execute xkblayout-state: {}", e);
                return None;
            }
        };

        if !output.status.success() {
            error!(
                "xkblayout-state failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }

        match String::from_utf8(output.stdout) {
            Ok(s) => match s.trim().parse::<u8>() {
                Ok(layout) => Some(layout),
                Err(e) => {
                    error!("Failed to parse layout: {}", e);
                    None
                }
            },
            Err(e) => {
                error!("Invalid UTF-8 from xkblayout-state: {}", e);
                None
            }
        }
    }

    fn add_current_window(&mut self, conn: &impl Connection) -> Result<()> {
        info!("Attempting to add current window to config");

        let active_win = match self.get_active_window(conn) {
            Ok(win) => win,
            Err(e) => {
                error!("Failed to get active window: {}", e);
                return Err(e);
            }
        };

        let window_class = match self.get_window_class(conn, active_win) {
            Some(class) => class,
            None => return Err(anyhow!("Failed to detect window class")),
        };

        let layout = match self.get_current_layout() {
            Some(layout) => layout,
            None => return Err(anyhow!("Failed to detect current layout")),
        };

        let mut config = match self.config.lock() {
            Ok(config) => config,
            Err(e) => {
                error!("Config lock error: {}", e);
                return Err(anyhow!("Config lock error"));
            }
        };

        info!("Adding window mapping: {} => {}", window_class, layout);
        config
            .window_layout_map
            .insert(window_class.clone(), layout);

        if let Err(e) = config.save_to_file(&self.config_path) {
            error!("Failed to save config: {}", e);
            return Err(e);
        }

        info!("Successfully added mapping: {} => {}", window_class, layout);
        Ok(())
    }

    fn switch_layout(&self, layout: u8) -> Result<()> {
        info!("Switching layout to {}", layout);
        match Command::new(self.get_xkblayout_state_path())
            .arg("set")
            .arg(layout.to_string())
            .status()
        {
            Ok(status) if status.success() => {
                info!("Successfully switched layout to {}", layout);
                Ok(())
            }
            Ok(status) => {
                error!("Failed to switch layout, exit code: {:?}", status.code());
                Err(anyhow!("Failed to switch layout"))
            }
            Err(e) => {
                error!("Failed to execute layout switch: {}", e);
                Err(e.into())
            }
        }
    }

    fn start_keyboard_listener(&mut self) -> Result<()> {
        info!("Starting keyboard listener");
        let config = Arc::clone(&self.config);
        let config_path = self.config_path.clone();

        thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_hotkey = SystemTime::now();

            let callback = move |event: KbdEvent| match event.event_type {
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
                        if Self::check_hotkey(&pressed_keys, &modifiers, hotkey) {
                            let now = SystemTime::now();
                            if let Ok(duration) = now.duration_since(last_hotkey) {
                                if duration > Duration::from_secs(1) {
                                    last_hotkey = now;
                                    info!("Detected hotkey combination");

                                    match x11rb::connect(None) {
                                        Ok((conn, _)) => {
                                            let wm_class_atom = match conn
                                                .intern_atom(false, b"WM_CLASS")
                                            {
                                                Ok(cookie) => {
                                                    match cookie.reply() {
                                                        Ok(reply) => reply.atom,
                                                        Err(e) => {
                                                            error!("Failed to get WM_CLASS atom reply: {}", e);
                                                            return;
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Failed to intern WM_CLASS atom: {}", e);
                                                    return;
                                                }
                                            };

                                            let net_active_window = match conn
                                                .intern_atom(false, b"_NET_ACTIVE_WINDOW")
                                            {
                                                Ok(cookie) => match cookie.reply() {
                                                    Ok(reply) => reply.atom,
                                                    Err(e) => {
                                                        error!("Failed to get _NET_ACTIVE_WINDOW atom reply: {}", e);
                                                        return;
                                                    }
                                                },
                                                Err(e) => {
                                                    error!("Failed to intern _NET_ACTIVE_WINDOW atom: {}", e);
                                                    return;
                                                }
                                            };

                                            let active_win = match conn.get_property(
                                                false,
                                                conn.setup().roots[0].root,
                                                net_active_window,
                                                AtomEnum::WINDOW,
                                                0,
                                                1,
                                            ) {
                                                Ok(cookie) => {
                                                    match cookie.reply() {
                                                        Ok(reply) => reply
                                                            .value32()
                                                            .and_then(|mut i| i.next())
                                                            .unwrap_or(0),
                                                        Err(e) => {
                                                            error!("Failed to get active window reply: {}", e);
                                                            return;
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Failed to get active window: {}", e);
                                                    return;
                                                }
                                            };

                                            let window_class = match conn.get_property(
                                                false,
                                                active_win,
                                                wm_class_atom,
                                                AtomEnum::STRING,
                                                0,
                                                1024,
                                            ) {
                                                Ok(cookie) => match cookie.reply() {
                                                    Ok(reply) => {
                                                        match String::from_utf8(reply.value) {
                                                            Ok(s) => s
                                                                .split('\0')
                                                                .next()
                                                                .map(|c| c.trim().to_lowercase())
                                                                .unwrap_or_default(),
                                                            Err(e) => {
                                                                error!("Invalid UTF-8 in window class: {}", e);
                                                                return;
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        error!(
                                                            "Failed to get window class reply: {}",
                                                            e
                                                        );
                                                        return;
                                                    }
                                                },
                                                Err(e) => {
                                                    error!("Failed to get window class: {}", e);
                                                    return;
                                                }
                                            };

                                            let layout = match Command::new("xkblayout-state")
                                                .arg("print")
                                                .arg("%c")
                                                .output()
                                            {
                                                Ok(output) if output.status.success() => {
                                                    match String::from_utf8(output.stdout) {
                                                        Ok(s) => {
                                                            match s.trim().parse::<u8>() {
                                                                Ok(layout) => layout,
                                                                Err(e) => {
                                                                    error!("Failed to parse layout: {}", e);
                                                                    return;
                                                                }
                                                            }
                                                        }
                                                        Err(e) => {
                                                            error!("Invalid UTF-8 from xkblayout-state: {}", e);
                                                            return;
                                                        }
                                                    }
                                                }
                                                _ => {
                                                    error!("Failed to get current layout");
                                                    return;
                                                }
                                            };

                                            info!(
                                                "Adding window mapping: {} => {}",
                                                window_class, layout
                                            );
                                            let mut local_config =
                                                match AppConfig::load_from_file(&config_path) {
                                                    Ok(c) => c,
                                                    Err(e) => {
                                                        error!("Failed to load config: {}", e);
                                                        return;
                                                    }
                                                };

                                            local_config
                                                .window_layout_map
                                                .insert(window_class, layout);

                                            if let Err(e) = local_config.save_to_file(&config_path)
                                            {
                                                error!("Failed to save config: {}", e);
                                            } else {
                                                info!("Config saved successfully");
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to connect to X11 server: {}", e);
                                        }
                                    }
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

    fn get_active_window(&self, conn: &impl Connection) -> Result<u32> {
        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom;
        let reply = conn
            .get_property(
                false,
                conn.setup().roots[0].root,
                net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?;

        reply
            .value32()
            .and_then(|mut iter| iter.next())
            .context("No active window found")
    }

    fn handle_window_change(&mut self, conn: &impl Connection, window_id: u32) -> Result<()> {
        if self.last_window_id == Some(window_id) {
            return Ok(());
        }

        self.last_window_id = Some(window_id);

        if let Some(window_class) = self.get_window_class(conn, window_id) {
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

        {
            let config = self.config.lock().unwrap();
            info!("Current config: {:?}", *config);
            info!("Hotkeys: {:?}", config.hotkeys);
        }

        self.start_keyboard_listener()?;

        let (conn, screen_num) = self.setup_x11_connection()?;
        let screen = &conn.setup().roots[screen_num];
        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom;

        conn.change_window_attributes(
            screen.root,
            &ChangeWindowAttributesAux::default().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        conn.flush()?;

        if let Ok(win) = self.get_active_window(&conn) {
            self.handle_window_change(&conn, win)?;
        }

        loop {
            match conn.wait_for_event() {
                Ok(event) => {
                    if let X11Event::PropertyNotify(ev) = event {
                        if ev.atom == net_active_window {
                            if let Ok(win) = self.get_active_window(&conn) {
                                self.handle_window_change(&conn, win)?;
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
            wm_class_atom: self.wm_class_atom,
            utf8_string_atom: self.utf8_string_atom,
        }
    }
}

impl AppConfig {
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            serde_json::from_str(&content).context("Failed to parse config file")
        } else {
            warn!("Creating new config file");
            let config = AppConfig {
                window_layout_map: HashMap::new(),
                hotkeys: [("add_window".to_string(), "ctrl shift q".to_string())]
                    .iter()
                    .cloned()
                    .collect(),
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
        let (conn, _) = switcher.setup_x11_connection()?;
        switcher.add_current_window(&conn)?;
        println!("Current window added to config");
    } else {
        switcher.run()?;
    }

    Ok(())
}
