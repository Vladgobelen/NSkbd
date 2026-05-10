use anyhow::{anyhow, Context, Result};
use log::{error, info};
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
    time::{Duration, Instant, SystemTime},
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
    caps_lock_pressed: Arc<Mutex<bool>>,
    space_pressed: Arc<Mutex<bool>>,
    caps_space_triggered: Arc<Mutex<bool>>,
    shift_count: Arc<Mutex<u32>>,
    last_shift_time: Arc<Mutex<Instant>>,
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

        info!("Keyboard Layout Switcher started");

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
            caps_lock_pressed: Arc::new(Mutex::new(false)),
            space_pressed: Arc::new(Mutex::new(false)),
            caps_space_triggered: Arc::new(Mutex::new(false)),
            shift_count: Arc::new(Mutex::new(0)),
            last_shift_time: Arc::new(Mutex::new(Instant::now())),
        })
    }

    fn str_to_key(key_str: &str) -> Option<Key> {
        match key_str.to_lowercase().as_str() {
            "a" => Some(Key::KeyA), "b" => Some(Key::KeyB), "c" => Some(Key::KeyC),
            "d" => Some(Key::KeyD), "e" => Some(Key::KeyE), "f" => Some(Key::KeyF),
            "g" => Some(Key::KeyG), "h" => Some(Key::KeyH), "i" => Some(Key::KeyI),
            "j" => Some(Key::KeyJ), "k" => Some(Key::KeyK), "l" => Some(Key::KeyL),
            "m" => Some(Key::KeyM), "n" => Some(Key::KeyN), "o" => Some(Key::KeyO),
            "p" => Some(Key::KeyP), "q" => Some(Key::KeyQ), "r" => Some(Key::KeyR),
            "s" => Some(Key::KeyS), "t" => Some(Key::KeyT), "u" => Some(Key::KeyU),
            "v" => Some(Key::KeyV), "w" => Some(Key::KeyW), "x" => Some(Key::KeyX),
            "y" => Some(Key::KeyY), "z" => Some(Key::KeyZ),
            "0" => Some(Key::Num0), "1" => Some(Key::Num1), "2" => Some(Key::Num2),
            "3" => Some(Key::Num3), "4" => Some(Key::Num4), "5" => Some(Key::Num5),
            "6" => Some(Key::Num6), "7" => Some(Key::Num7), "8" => Some(Key::Num8),
            "9" => Some(Key::Num9),
            "f1" => Some(Key::F1), "f2" => Some(Key::F2), "f3" => Some(Key::F3),
            "f4" => Some(Key::F4), "f5" => Some(Key::F5), "f6" => Some(Key::F6),
            "f7" => Some(Key::F7), "f8" => Some(Key::F8), "f9" => Some(Key::F9),
            "f10" => Some(Key::F10), "f11" => Some(Key::F11), "f12" => Some(Key::F12),
            "space" => Some(Key::Space), "enter" => Some(Key::Return),
            "tab" => Some(Key::Tab), "backspace" => Some(Key::Backspace),
            "escape" => Some(Key::Escape), "insert" => Some(Key::Insert),
            "delete" => Some(Key::Delete), "home" => Some(Key::Home),
            "end" => Some(Key::End), "pageup" => Some(Key::PageUp),
            "pagedown" => Some(Key::PageDown), "up" => Some(Key::UpArrow),
            "down" => Some(Key::DownArrow), "left" => Some(Key::LeftArrow),
            "right" => Some(Key::RightArrow),
            _ => None,
        }
    }

    fn check_hotkey(pressed_keys: &HashSet<Key>, modifiers: &ModifierState, hotkey_str: &str) -> bool {
        let parts: Vec<&str> = hotkey_str.split_whitespace().collect();
        let mut required_mods = HashSet::new();
        let mut required_key = None;

        for part in parts {
            match part.to_lowercase().as_str() {
                "shift" => { required_mods.insert("shift"); }
                "ctrl" => { required_mods.insert("ctrl"); }
                "alt" => { required_mods.insert("alt"); }
                "meta" | "super" | "win" => { required_mods.insert("meta"); }
                key_str => { required_key = Self::str_to_key(key_str); }
            };
        }

        modifiers.matches(&required_mods) && required_key.map_or(false, |k| pressed_keys.contains(&k))
    }

    fn get_window_class(&self, window_id: u32) -> Option<String> {
        let wm_class_atom = self.conn.intern_atom(false, b"WM_CLASS").ok()?.reply().ok()?.atom;
        let reply = self.conn
            .get_property::<u32, u32>(false, window_id, wm_class_atom, AtomEnum::STRING.into(), 0, 1024)
            .ok()?.reply().ok()?;

        if reply.format != 8 || reply.value.is_empty() { return None; }

        let value = String::from_utf8_lossy(&reply.value);
        let parts: Vec<&str> = value.split('\0').collect();
        if parts.len() < 2 { return None; }

        let class = if !parts[1].is_empty() { parts[1] } else { parts[0] };
        if class.is_empty() { return None; }

        Some(class.to_lowercase())
    }

    fn get_current_layout(&self) -> Option<u8> {
        self.xkb.current_layout().ok()
    }

    fn add_current_window(&self) -> Result<()> {
        let window_id = self.get_active_window().context("Failed to get window ID")?;
        let window_class = self.get_window_class(window_id).context("Failed to detect window class")?;
        let layout = self.get_current_layout().context("Failed to detect current layout")?;

        let mut config = self.config.lock().map_err(|e| anyhow!("Config lock error: {}", e))?;
        config.window_layout_map.insert(window_class.clone(), layout);
        config.save_to_file(&self.config_path)?;
        info!("Added window '{}' with layout {}", window_class, layout);
        Ok(())
    }

    fn switch_layout(&self, layout: u8) -> Result<()> {
        self.xkb.set_layout(layout)
    }

    fn get_primary_selection(&self) -> String {
        Command::new("xclip")
            .args(["-selection", "primary", "-o"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    }

    fn type_text(&self, text: &str) -> Result<()> {
        let output = Command::new("xdotool")
            .args(["type", "--delay", "5", text])
            .output()
            .context("Failed to type text")?;
        if !output.status.success() { return Err(anyhow!("xdotool type failed")); }
        Ok(())
    }

    fn simulate_key(&self, keys: &[&str]) -> Result<()> {
        let key_sequence = keys.join("+");
        let output = Command::new("xdotool")
            .args(["key", &key_sequence])
            .output()
            .context("Failed to simulate key")?;
        if !output.status.success() { return Err(anyhow!("xdotool failed")); }
        Ok(())
    }

    fn convert_layout(&self, text: &str) -> String {
        let eng_to_rus: HashMap<char, char> = [
            ('a', 'ф'), ('b', 'и'), ('c', 'с'), ('d', 'в'), ('e', 'у'),
            ('f', 'а'), ('g', 'п'), ('h', 'р'), ('i', 'ш'), ('j', 'о'),
            ('k', 'л'), ('l', 'д'), ('m', 'ь'), ('n', 'т'), ('o', 'щ'),
            ('p', 'з'), ('q', 'й'), ('r', 'к'), ('s', 'ы'), ('t', 'е'),
            ('u', 'г'), ('v', 'м'), ('w', 'ц'), ('x', 'ч'), ('y', 'н'),
            ('z', 'я'),
            ('A', 'Ф'), ('B', 'И'), ('C', 'С'), ('D', 'В'), ('E', 'У'),
            ('F', 'А'), ('G', 'П'), ('H', 'Р'), ('I', 'Ш'), ('J', 'О'),
            ('K', 'Л'), ('L', 'Д'), ('M', 'Ь'), ('N', 'Т'), ('O', 'Щ'),
            ('P', 'З'), ('Q', 'Й'), ('R', 'К'), ('S', 'Ы'), ('T', 'Е'),
            ('U', 'Г'), ('V', 'М'), ('W', 'Ц'), ('X', 'Ч'), ('Y', 'Н'),
            ('Z', 'Я'),
            ('[', 'х'), (']', 'ъ'), ('{', 'Х'), ('}', 'Ъ'),
            (';', 'ж'), (':', 'Ж'),
            ('\'', 'э'), ('"', 'Э'),
            (',', 'б'), ('<', 'Б'),
            ('.', 'ю'), ('>', 'Ю'),
            ('/', '.'), ('?', ','),
            ('`', 'ё'), ('~', 'Ё'),
            ('&', '?'), ('|', '/'),
            ('@', '"'), ('#', '№'),
            ('$', ';'), ('^', ':'),
            ('(', '9'), (')', '0'),
            ('-', '-'), ('_', '_'),
            ('=', '='), ('+', '+'),
            ('*', '*'), ('%', '%'),
        ].iter().cloned().collect();

        let rus_to_eng: HashMap<char, char> = eng_to_rus.iter().map(|(&k, &v)| (v, k)).collect();
        let current_layout = self.get_current_layout().unwrap_or(0);

        text.chars().map(|c| {
            if current_layout == 0 {
                *eng_to_rus.get(&c).unwrap_or(&c)
            } else {
                *rus_to_eng.get(&c).unwrap_or(&c)
            }
        }).collect()
    }

    fn handle_shift_count(&self, count: usize) -> Result<()> {
        let words = count - 1;
        info!("Shift pressed {} times, converting last {} words...", count, words);
        
        for _ in 0..words {
            self.simulate_key(&["ctrl", "shift", "Left"])?;
            thread::sleep(Duration::from_millis(30));
        }
        
        thread::sleep(Duration::from_millis(200));
        let selected_text = self.get_primary_selection();
        
        if selected_text.is_empty() {
            info!("Nothing selected, aborting");
            return Ok(());
        }
        
        let converted_text = self.convert_layout(&selected_text);
        
        self.simulate_key(&["BackSpace"])?;
        thread::sleep(Duration::from_millis(50));
        
        let current_layout = self.get_current_layout().unwrap_or(0);
        let target_layout = if current_layout == 0 { 1 } else { 0 };
        self.switch_layout(target_layout)?;
        
        thread::sleep(Duration::from_millis(50));
        self.type_text(&converted_text)?;
        
        Ok(())
    }

    fn start_keyboard_listener(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let switcher = self.clone();
        let caps_lock = Arc::clone(&self.caps_lock_pressed);
        let space_pressed = Arc::clone(&self.space_pressed);
        let caps_space_triggered = Arc::clone(&self.caps_space_triggered);
        let shift_count = Arc::clone(&self.shift_count);
        let last_shift_time = Arc::clone(&self.last_shift_time);

        thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_hotkey = SystemTime::now();

            let callback = move |event: KbdEvent| {
                match event.event_type {
                    EventType::KeyPress(key) => {
                        pressed_keys.insert(key.clone());
                        modifiers.update(&key, true);

                        if key == Key::CapsLock {
                            *caps_lock.lock().unwrap() = true;
                        }
                        
                        if key == Key::Space {
                            let caps_is_pressed;
                            {
                                *space_pressed.lock().unwrap() = true;
                                caps_is_pressed = *caps_lock.lock().unwrap();
                            }
                            
                            if caps_is_pressed {
                                *caps_space_triggered.lock().unwrap() = true;
                                let switcher_clone = switcher.clone();
                                thread::spawn(move || {
                                    let _ = switcher_clone.simulate_key(&["BackSpace"]);
                                    thread::sleep(Duration::from_millis(30));
                                    info!("Switching to Russian layout (CapsLock+Space)");
                                    let _ = switcher_clone.switch_layout(1);
                                });
                            }
                        }

                        if key == Key::ShiftLeft || key == Key::ShiftRight {
                            let now = Instant::now();
                            let mut last = last_shift_time.lock().unwrap();
                            let mut count = shift_count.lock().unwrap();
                            let elapsed = now.duration_since(*last);
                            
                            if *count > 0 && elapsed > Duration::from_millis(300) {
                                *count = 1;
                            } else {
                                *count += 1;
                            }
                            *last = now;
                        } else {
                            *shift_count.lock().unwrap() = 0;
                        }

                        let hotkey = {
                            let config = match config.lock() {
                                Ok(c) => c,
                                Err(_) => return,
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
                                        thread::spawn(move || { let _ = switcher_clone.add_current_window(); });
                                    }
                                }
                            }
                        }
                    }
                    EventType::KeyRelease(key) => {
                        pressed_keys.remove(&key);
                        modifiers.update(&key, false);

                        if key == Key::CapsLock {
                            *caps_lock.lock().unwrap() = false;
                            let triggered;
                            let space_was_pressed;
                            {
                                let mut trig = caps_space_triggered.lock().unwrap();
                                triggered = *trig;
                                *trig = false;
                            }
                            {
                                let mut sp = space_pressed.lock().unwrap();
                                space_was_pressed = *sp;
                                *sp = false;
                            }
                            
                            if !triggered && !space_was_pressed {
                                let switcher_clone = switcher.clone();
                                thread::spawn(move || {
                                    info!("CapsLock alone - switching to English");
                                    let _ = switcher_clone.switch_layout(0);
                                });
                            }
                        }

                        if key == Key::Space {
                            *space_pressed.lock().unwrap() = false;
                        }

                        if key == Key::ShiftLeft || key == Key::ShiftRight {
                            let count = *shift_count.lock().unwrap();
                            
                            if count >= 2 {
                                let switcher_clone = switcher.clone();
                                let shift_check = Arc::clone(&shift_count);
                                let captured_count = count.min(10) as usize;
                                
                                thread::spawn(move || {
                                    thread::sleep(Duration::from_millis(300));
                                    let final_count = *shift_check.lock().unwrap();
                                    if final_count == captured_count as u32 {
                                        *shift_check.lock().unwrap() = 0;
                                        info!("Shift series detected: {} presses", captured_count);
                                        if let Err(e) = switcher_clone.handle_shift_count(captured_count) {
                                            error!("Shift count error: {}", e);
                                        }
                                    }
                                });
                            }
                        }
                    }
                    _ => {}
                }
            };

            info!("Keyboard listener started");
            if let Err(e) = listen(callback) {
                error!("Keyboard listener error: {:?}", e);
            }
        });

        Ok(())
    }

    fn get_active_window(&self) -> Option<u32> {
        let net_active_window = self.conn.intern_atom(false, b"_NET_ACTIVE_WINDOW").ok()?.reply().ok()?.atom;
        let reply = self.conn
            .get_property::<u32, u32>(false, self.conn.setup().roots[self.screen_num].root, net_active_window, AtomEnum::WINDOW.into(), 0, 1)
            .ok()?.reply().ok()?;

        if reply.format == 32 && !reply.value.is_empty() {
            Some(u32::from_ne_bytes([reply.value[0], reply.value[1], reply.value[2], reply.value[3]]))
        } else {
            None
        }
    }

    fn handle_window_change(&mut self, window_id: u32) -> Result<()> {
        if self.last_window_id == Some(window_id) { return Ok(()); }

        info!("Window changed to {}", window_id);
        self.last_window_id = Some(window_id);

        if let Some(window_class) = self.get_window_class(window_id) {
            let config = self.config.lock().map_err(|e| anyhow!("Config lock error: {}", e))?;
            if let Some(&target_layout) = config.window_layout_map.get(&window_class) {
                if let Some(current_layout) = self.get_current_layout() {
                    if current_layout != target_layout {
                        self.switch_layout(target_layout)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        self.start_keyboard_listener()?;

        let screen = &self.conn.setup().roots[self.screen_num];
        let net_active_window = self.conn.intern_atom(false, b"_NET_ACTIVE_WINDOW")?.reply()?.atom;

        let mask = ChangeWindowAttributesAux::default().event_mask(EventMask::PROPERTY_CHANGE);
        self.conn.change_window_attributes(screen.root, &mask)?;
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
                                if let Err(e) = self.handle_window_change(win) {
                                    error!("Window change error: {}", e);
                                }
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
            caps_lock_pressed: Arc::clone(&self.caps_lock_pressed),
            space_pressed: Arc::clone(&self.space_pressed),
            caps_space_triggered: Arc::clone(&self.caps_space_triggered),
            shift_count: Arc::clone(&self.shift_count),
            last_shift_time: Arc::clone(&self.last_shift_time),
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
        conn.xkb_use_extension(1, 0)?.reply()?;
        Ok(Self { conn, device_id: ID::USE_CORE_KBD.into() })
    }

    fn current_layout(&self) -> Result<u8> {
        let state = self.conn.xkb_get_state(self.device_id)?.reply()?;
        Ok(state.group.into())
    }

    fn set_layout(&self, group_num: u8) -> Result<()> {
        for _ in 1..=3 {
            self.conn.xkb_latch_lock_state(
                self.device_id,
                ModMask::from(0u8), ModMask::from(0u8), true,
                Group::from(group_num), ModMask::from(0u8), false, 0,
            )?;
            self.conn.flush()?;
            thread::sleep(Duration::from_millis(50));
            if self.current_layout()? == group_num { return Ok(()); }
        }
        Err(anyhow!("Layout switch failed"))
    }
}

impl AppConfig {
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        if path.exists() {
            Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
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