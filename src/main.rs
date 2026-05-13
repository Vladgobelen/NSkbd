use anyhow::{anyhow, Context, Result};
use evdev::{uinput::VirtualDeviceBuilder, AttributeSet, EventType, InputEvent, Key};
use log::{error, info};
use rdev::{listen, Event as KbdEvent, EventType as RdevEventType};
use rdev::Key as RdevKey;
use serde::{Deserialize, Serialize};
use simplelog::{Config as LogConfig, LevelFilter, WriteLogger};
use std::io::Write as IoWrite;
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    io::Read,
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
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

const MAX_CHARS: usize = 500;
const SPELL_SERVER_HOST: &str = "127.0.0.1";
const SPELL_SERVER_PORT: u16 = 9876;
const SPELL_SERVER_SCRIPT: &str = "spell_server.py";

fn default_python_interpreter() -> String {
    "python3".to_string()
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
struct AppConfig {
    window_layout_map: HashMap<String, u8>,
    hotkeys: HashMap<String, String>,
    #[serde(default)]
    enable_spell_check: bool,
    #[serde(default)]
    python_interpreter: Option<String>,
}

#[derive(Debug, Default)]
struct ModifierState {
    shift: bool,
    ctrl: bool,
    alt: bool,
    meta: bool,
}

impl ModifierState {
    fn update(&mut self, key: &RdevKey, is_press: bool) {
        match key {
            RdevKey::ShiftLeft | RdevKey::ShiftRight => self.shift = is_press,
            RdevKey::ControlLeft | RdevKey::ControlRight => self.ctrl = is_press,
            RdevKey::Alt | RdevKey::AltGr => self.alt = is_press,
            RdevKey::MetaLeft | RdevKey::MetaRight => self.meta = is_press,
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

struct SpellChecker {
    enabled: Arc<Mutex<bool>>,
}

impl SpellChecker {
    fn new() -> Self {
        Self {
            enabled: Arc::new(Mutex::new(false)),
        }
    }

    fn set_enabled(&self, enabled: bool) {
        if let Ok(mut e) = self.enabled.lock() {
            *e = enabled;
            if enabled {
                info!("Spell check: ON");
            } else {
                info!("Spell check: OFF");
            }
        }
    }

    fn is_enabled(&self) -> bool {
        self.enabled.lock().map(|e| *e).unwrap_or(false)
    }

    fn check_word(&self, word: &str) -> Option<String> {
        if !self.is_enabled() || word.len() < 3 {
            return None;
        }

        match TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], SPELL_SERVER_PORT)),
            Duration::from_millis(100),
        ) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
                stream.write_all(word.as_bytes()).ok()?;
                stream.shutdown(std::net::Shutdown::Write).ok()?;

                let mut response = String::new();
                stream.read_to_string(&mut response).ok()?;
                let corrected = response.trim().to_string();

                if corrected.is_empty() || corrected == word || corrected.len() > word.len() * 3 {
                    None
                } else {
                    info!("Corrected: '{}' -> '{}'", word, corrected);
                    Some(corrected)
                }
            }
            Err(_) => {
                error!("Spell server not available");
                None
            }
        }
    }
}

impl Clone for SpellChecker {
    fn clone(&self) -> Self {
        Self {
            enabled: Arc::clone(&self.enabled),
        }
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
    caps_lock_count: Arc<Mutex<u32>>,
    last_caps_lock_time: Arc<Mutex<Instant>>,
    shift_count: Arc<Mutex<u32>>,
    last_shift_time: Arc<Mutex<Instant>>,
    char_buffer: Arc<Mutex<String>>,
    buffer_window_id: Arc<Mutex<Option<u32>>>,
    uinput: Arc<Mutex<evdev::uinput::VirtualDevice>>,
    spell_checker: SpellChecker,
    replacing_text: Arc<Mutex<bool>>,
    server_process: Arc<Mutex<Option<Child>>>,
    handling_window_change: Arc<Mutex<bool>>,
    last_change_time: Arc<Mutex<Instant>>,
    replacing_start_time: Arc<Mutex<Option<Instant>>>,
}

impl KeyboardLayoutSwitcher {
    fn new(config_file: &str, log_file: &str) -> Result<Self> {
        info!("Initializing KeyboardLayoutSwitcher");

        let current_dir = env::current_dir().context("Failed to get current directory")?;
        let config_path = current_dir.join(config_file);
        let log_path = current_dir.join(log_file);

        info!("Config path: {}", config_path.display());
        info!("Log path: {}", log_path.display());

        if log_path.exists() {
            fs::remove_file(&log_path).ok();
        }

        let log_file = File::create(&log_path)
            .context(format!("Failed to create log file: {}", log_path.display()))?;

        WriteLogger::init(LevelFilter::Info, LogConfig::default(), log_file)
            .context("Failed to initialize logger")?;

        info!("Logger initialized");

        let config = AppConfig::load_from_file(&config_path)?;
        info!("Config loaded: {} windows configured", config.window_layout_map.len());

        info!("Connecting to X11 server...");
        let (conn, screen_num) = x11rb::connect(None).context("Failed to connect to X11 server")?;
        let conn = Arc::new(conn);
        info!("Connected to X11, screen: {}", screen_num);

        info!("Setting up XKB extension...");
        let xkb = XKeyboard::new(Arc::clone(&conn))?;
        info!("XKB extension ready");

        info!("Creating virtual keyboard device...");
        let uinput = create_virtual_keyboard()?;
        info!("Virtual keyboard created");

        let spell_checker = SpellChecker::new();
        let server_process = Arc::new(Mutex::new(None));

        let switcher = Self {
            config_path,
            log_path,
            config: Arc::new(Mutex::new(config)),
            last_window_id: None,
            conn,
            screen_num,
            xkb,
            caps_lock_count: Arc::new(Mutex::new(0)),
            last_caps_lock_time: Arc::new(Mutex::new(Instant::now())),
            shift_count: Arc::new(Mutex::new(0)),
            last_shift_time: Arc::new(Mutex::new(Instant::now())),
            char_buffer: Arc::new(Mutex::new(String::with_capacity(MAX_CHARS + 100))),
            buffer_window_id: Arc::new(Mutex::new(None)),
            uinput: Arc::new(Mutex::new(uinput)),
            spell_checker,
            replacing_text: Arc::new(Mutex::new(false)),
            server_process,
            handling_window_change: Arc::new(Mutex::new(false)),
            last_change_time: Arc::new(Mutex::new(Instant::now())),
            replacing_start_time: Arc::new(Mutex::new(None)),
        };

        let enable_spell = {
            let config = match switcher.config.lock() {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to lock config: {}", e);
                    return Err(anyhow!("Config lock failed: {}", e));
                }
            };
            config.enable_spell_check
        };

        if enable_spell {
            switcher.spell_checker.set_enabled(true);
            match switcher.start_spell_server() {
                Ok(()) => info!("Spell server auto-started"),
                Err(e) => error!("Auto-start spell server failed: {}", e),
            }
        }

        info!("KeyboardLayoutSwitcher initialization complete");
        Ok(switcher)
    }

    fn str_to_key(key_str: &str) -> Option<RdevKey> {
        match key_str.to_lowercase().as_str() {
            "a" => Some(RdevKey::KeyA), "b" => Some(RdevKey::KeyB), "c" => Some(RdevKey::KeyC),
            "d" => Some(RdevKey::KeyD), "e" => Some(RdevKey::KeyE), "f" => Some(RdevKey::KeyF),
            "g" => Some(RdevKey::KeyG), "h" => Some(RdevKey::KeyH), "i" => Some(RdevKey::KeyI),
            "j" => Some(RdevKey::KeyJ), "k" => Some(RdevKey::KeyK), "l" => Some(RdevKey::KeyL),
            "m" => Some(RdevKey::KeyM), "n" => Some(RdevKey::KeyN), "o" => Some(RdevKey::KeyO),
            "p" => Some(RdevKey::KeyP), "q" => Some(RdevKey::KeyQ), "r" => Some(RdevKey::KeyR),
            "s" => Some(RdevKey::KeyS), "t" => Some(RdevKey::KeyT), "u" => Some(RdevKey::KeyU),
            "v" => Some(RdevKey::KeyV), "w" => Some(RdevKey::KeyW), "x" => Some(RdevKey::KeyX),
            "y" => Some(RdevKey::KeyY), "z" => Some(RdevKey::KeyZ),
            "0" => Some(RdevKey::Num0), "1" => Some(RdevKey::Num1), "2" => Some(RdevKey::Num2),
            "3" => Some(RdevKey::Num3), "4" => Some(RdevKey::Num4), "5" => Some(RdevKey::Num5),
            "6" => Some(RdevKey::Num6), "7" => Some(RdevKey::Num7), "8" => Some(RdevKey::Num8),
            "9" => Some(RdevKey::Num9),
            "f1" => Some(RdevKey::F1), "f2" => Some(RdevKey::F2), "f3" => Some(RdevKey::F3),
            "f4" => Some(RdevKey::F4), "f5" => Some(RdevKey::F5), "f6" => Some(RdevKey::F6),
            "f7" => Some(RdevKey::F7), "f8" => Some(RdevKey::F8), "f9" => Some(RdevKey::F9),
            "f10" => Some(RdevKey::F10), "f11" => Some(RdevKey::F11), "f12" => Some(RdevKey::F12),
            "space" => Some(RdevKey::Space), "enter" => Some(RdevKey::Return),
            "tab" => Some(RdevKey::Tab), "backspace" => Some(RdevKey::Backspace),
            "escape" => Some(RdevKey::Escape), "insert" => Some(RdevKey::Insert),
            "delete" => Some(RdevKey::Delete), "home" => Some(RdevKey::Home),
            "end" => Some(RdevKey::End), "pageup" => Some(RdevKey::PageUp),
            "pagedown" => Some(RdevKey::PageDown), "up" => Some(RdevKey::UpArrow),
            "down" => Some(RdevKey::DownArrow), "left" => Some(RdevKey::LeftArrow),
            "right" => Some(RdevKey::RightArrow),
            _ => None,
        }
    }

    fn is_typing_key(key: &RdevKey) -> bool {
        matches!(key,
            RdevKey::KeyA | RdevKey::KeyB | RdevKey::KeyC | RdevKey::KeyD | RdevKey::KeyE |
            RdevKey::KeyF | RdevKey::KeyG | RdevKey::KeyH | RdevKey::KeyI | RdevKey::KeyJ |
            RdevKey::KeyK | RdevKey::KeyL | RdevKey::KeyM | RdevKey::KeyN | RdevKey::KeyO |
            RdevKey::KeyP | RdevKey::KeyQ | RdevKey::KeyR | RdevKey::KeyS | RdevKey::KeyT |
            RdevKey::KeyU | RdevKey::KeyV | RdevKey::KeyW | RdevKey::KeyX | RdevKey::KeyY |
            RdevKey::KeyZ |
            RdevKey::Num0 | RdevKey::Num1 | RdevKey::Num2 | RdevKey::Num3 | RdevKey::Num4 |
            RdevKey::Num5 | RdevKey::Num6 | RdevKey::Num7 | RdevKey::Num8 | RdevKey::Num9 |
            RdevKey::Minus | RdevKey::Equal |
            RdevKey::LeftBracket | RdevKey::RightBracket |
            RdevKey::SemiColon | RdevKey::Quote |
            RdevKey::Comma | RdevKey::Dot | RdevKey::Slash |
            RdevKey::BackSlash
        )
    }

    fn is_boundary(key: &RdevKey) -> bool {
        matches!(key,
            RdevKey::Return | RdevKey::Tab | RdevKey::Escape |
            RdevKey::UpArrow | RdevKey::DownArrow | RdevKey::LeftArrow | RdevKey::RightArrow |
            RdevKey::Home | RdevKey::End | RdevKey::PageUp | RdevKey::PageDown |
            RdevKey::Delete
        )
    }

    fn check_hotkey(pressed_keys: &HashSet<RdevKey>, modifiers: &ModifierState, hotkey_str: &str) -> bool {
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

    fn toggle_spell_check(&self) -> Result<()> {
        let enabled = self.spell_checker.is_enabled();
        let new_state = !enabled;
        self.spell_checker.set_enabled(new_state);

        let python_cmd = {
            let mut config = self.config.lock().map_err(|e| anyhow!("Config lock error: {}", e))?;
            config.enable_spell_check = new_state;
            config.save_to_file(&self.config_path)?;
            config.python_interpreter.clone().unwrap_or_else(default_python_interpreter)
        };

        if new_state {
            match self.start_spell_server_internal(&python_cmd) {
                Ok(()) => {}
                Err(e) => error!("Failed to start spell server: {}", e),
            }
        } else {
            match self.stop_spell_server() {
                Ok(()) => {}
                Err(e) => error!("Failed to stop spell server: {}", e),
            }
        }

        Ok(())
    }

    fn start_spell_server(&self) -> Result<()> {
        let python_cmd = {
            let config = self.config.lock().unwrap();
            config.python_interpreter.clone().unwrap_or_else(default_python_interpreter)
        };
        self.start_spell_server_internal(&python_cmd)
    }

    fn start_spell_server_internal(&self, python_cmd: &str) -> Result<()> {
        let mut proc = match self.server_process.try_lock() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };

        if let Some(ref mut child) = *proc {
            match child.try_wait() {
                Ok(None) => return Ok(()),
                Ok(Some(_)) => *proc = None,
                Err(_) => *proc = None,
            }
        }

        if TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], SPELL_SERVER_PORT)),
            Duration::from_millis(100),
        ).is_ok() {
            let _ = Command::new("fuser")
                .args(["-k", "9876/tcp"])
                .output();

            for _ in 0..20 {
                thread::sleep(Duration::from_millis(500));
                if TcpStream::connect_timeout(
                    &std::net::SocketAddr::from(([127, 0, 0, 1], SPELL_SERVER_PORT)),
                    Duration::from_millis(100),
                ).is_err() {
                    break;
                }
            }
        }

        let script_path = match Self::find_spell_server_script() {
            Some(p) => p,
            None => return Ok(()),
        };

        let stderr_file = match File::create("spell_server.log") {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };

        match Command::new(python_cmd)
            .arg(&script_path)
            .stdout(Stdio::null())
            .stderr(stderr_file)
            .spawn()
        {
            Ok(child) => {
                *proc = Some(child);
                info!("Spell server started");
            }
            Err(e) => {
                error!("Failed to spawn spell server: {}", e);
            }
        }

        Ok(())
    }

    fn find_spell_server_script() -> Option<PathBuf> {
        if let Ok(exe) = env::current_exe() {
            if let Some(parent) = exe.parent() {
                let candidate = parent.join(SPELL_SERVER_SCRIPT);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        if let Ok(cwd) = env::current_dir() {
            let candidate = cwd.join(SPELL_SERVER_SCRIPT);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }

    fn stop_spell_server(&self) -> Result<()> {
        let mut proc = match self.server_process.try_lock() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };

        if let Some(ref mut child) = *proc {
            let _ = child.kill();
            let _ = child.wait();
            *proc = None;
            info!("Spell server stopped");
        }

        Ok(())
    }

    fn switch_layout(&self, layout: u8) -> Result<()> {
        self.xkb.set_layout(layout)
    }

    fn type_text_uinput(&self, text: &str, layout: u8) -> Result<()> {
        let layout_is_ru = layout == 1;
        let mut dev = self.uinput.lock().unwrap();
        for ch in text.chars() {
            if let Some((code, shift)) = char_to_keycode(ch, layout_is_ru) {
                if shift {
                    dev.emit(&[InputEvent::new(EventType::KEY, Key::KEY_LEFTSHIFT.code(), 1)])?;
                }
                dev.emit(&[InputEvent::new(EventType::KEY, code, 1)])?;
                dev.emit(&[InputEvent::new(EventType::KEY, code, 0)])?;
                if shift {
                    dev.emit(&[InputEvent::new(EventType::KEY, Key::KEY_LEFTSHIFT.code(), 0)])?;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }

    fn take_last_n_words(buf: &str, n: usize) -> String {
        let words: Vec<&str> = buf.split_whitespace().collect();
        if words.is_empty() || n == 0 { return String::new(); }
        let start = words.len().saturating_sub(n);
        words[start..].join(" ")
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
            ('`', 'ё'), ('~', 'Ё'),
            ('[', 'х'), (']', 'ъ'), ('{', 'Х'), ('}', 'Ъ'),
            (';', 'ж'), (':', 'Ж'), ('\'', 'э'), ('"', 'Э'),
            (',', 'б'), ('<', 'Б'), ('.', 'ю'), ('>', 'Ю'),
            ('/', '.'), ('?', ','), ('\\', '\\'), ('|', '/'),
        ].iter().cloned().collect();

        let rus_to_eng: HashMap<char, char> = [
            ('ф', 'a'), ('и', 'b'), ('с', 'c'), ('в', 'd'), ('у', 'e'),
            ('а', 'f'), ('п', 'g'), ('р', 'h'), ('ш', 'i'), ('о', 'j'),
            ('л', 'k'), ('д', 'l'), ('ь', 'm'), ('т', 'n'), ('щ', 'o'),
            ('з', 'p'), ('й', 'q'), ('к', 'r'), ('ы', 's'), ('е', 't'),
            ('г', 'u'), ('м', 'v'), ('ц', 'w'), ('ч', 'x'), ('н', 'y'),
            ('я', 'z'),
            ('Ф', 'A'), ('И', 'B'), ('С', 'C'), ('В', 'D'), ('У', 'E'),
            ('А', 'F'), ('П', 'G'), ('Р', 'H'), ('Ш', 'I'), ('О', 'J'),
            ('Л', 'K'), ('Д', 'L'), ('Ь', 'M'), ('Т', 'N'), ('Щ', 'O'),
            ('З', 'P'), ('Й', 'Q'), ('К', 'R'), ('Ы', 'S'), ('Е', 'T'),
            ('Г', 'U'), ('М', 'V'), ('Ц', 'W'), ('Ч', 'X'), ('Н', 'Y'),
            ('Я', 'Z'),
            ('ё', '`'), ('Ё', '~'),
            ('х', '['), ('ъ', ']'), ('Х', '{'), ('Ъ', '}'),
            ('ж', ';'), ('Ж', ':'), ('э', '\''), ('Э', '"'),
            ('б', ','), ('Б', '<'), ('ю', '.'), ('Ю', '>'),
            ('.', '/'), (',', '?'), ('\\', '\\'), ('/', '|'),
        ].iter().cloned().collect();

        let current_layout = self.get_current_layout().unwrap_or(0);

        text.chars().map(|c| {
            if current_layout == 0 {
                *eng_to_rus.get(&c).unwrap_or(&c)
            } else {
                *rus_to_eng.get(&c).unwrap_or(&c)
            }
        }).collect()
    }

    fn is_russian_word(word: &str) -> bool {
        word.chars().any(|c| matches!(c, 'А'..='я' | 'ё' | 'Ё'))
    }

    fn try_correct_last_word(&self, text: &str) -> (String, bool) {
        if self.spell_checker.is_enabled() {
            let words: Vec<&str> = text.split_whitespace().collect();
            let mut corrected_words = Vec::new();

            for word in &words {
                let layout_converted = if Self::is_russian_word(word) {
                    word.to_string()
                } else {
                    self.convert_layout(word)
                };

                if let Some(llm_corrected) = self.spell_checker.check_word(&layout_converted) {
                    corrected_words.push(llm_corrected);
                } else {
                    corrected_words.push(layout_converted);
                }
            }

            let result = corrected_words.join(" ");
            let is_ru = Self::is_russian_word(&result);
            return (result, is_ru);
        }

        let converted = self.convert_layout(text);
        let is_ru = Self::is_russian_word(&converted);
        (converted, is_ru)
    }

    fn handle_shift_count(&self, count: usize) -> Result<()> {
        info!("handle_shift_count: count={}", count);

        {
            let mut replacing = match self.replacing_text.lock() {
                Ok(r) => r,
                Err(e) => {
                    error!("replacing_text mutex POISONED: {}", e);
                    return Ok(());
                }
            };

            if *replacing {
                if let Ok(mut start_time) = self.replacing_start_time.lock() {
                    let force_reset = match *start_time {
                        Some(t) => {
                            let elapsed = t.elapsed();
                            if elapsed > Duration::from_secs(5) {
                                error!("Replacing flag stuck for {:.1}s, force resetting", elapsed.as_secs_f32());
                                true
                            } else {
                                info!("Already replacing (elapsed {:.1}s), skipping", elapsed.as_secs_f32());
                                false
                            }
                        }
                        None => false,
                    };

                    if force_reset {
                        *replacing = false;
                        *start_time = None;
                    } else {
                        return Ok(());
                    }
                } else {
                    return Ok(());
                }
            }

            *replacing = true;

            if let Ok(mut start_time) = self.replacing_start_time.lock() {
                *start_time = Some(Instant::now());
            }
        }

        struct ReplacingGuard {
            flag: Arc<Mutex<bool>>,
            start_time: Arc<Mutex<Option<Instant>>>,
        }

        impl Drop for ReplacingGuard {
            fn drop(&mut self) {
                if let Ok(mut f) = self.flag.lock() {
                    *f = false;
                }
                if let Ok(mut t) = self.start_time.lock() {
                    *t = None;
                }
                info!("Replacing guard: cleaned up");
            }
        }

        let _guard = ReplacingGuard {
            flag: Arc::clone(&self.replacing_text),
            start_time: Arc::clone(&self.replacing_start_time),
        };

        match self.shift_count.lock() {
            Ok(mut c) => *c = 0,
            Err(e) => {
                error!("shift_count mutex POISONED: {}", e);
                return Ok(());
            }
        }

        let words = count.saturating_sub(1);
        info!("Converting last {} word(s)", words);

        let text_to_convert = {
            let buf = match self.char_buffer.lock() {
                Ok(b) => b,
                Err(e) => {
                    error!("char_buffer mutex POISONED: {}", e);
                    return Ok(());
                }
            };

            if buf.is_empty() {
                info!("Buffer empty, nothing to convert");
                return Ok(());
            }

            let result = Self::take_last_n_words(&buf, words);
            drop(buf);

            if let Ok(mut b) = self.char_buffer.lock() {
                b.clear();
            }

            if result.is_empty() {
                info!("No words extracted from buffer");
                return Ok(());
            }

            info!("Extracted from buffer: '{}'", result);
            result
        };

        let actual_words = text_to_convert.split_whitespace().count();
        if actual_words == 0 {
            info!("No actual words to process");
            return Ok(());
        }

        let original_layout = self.get_current_layout().unwrap_or(0);
        info!("Current layout: {}", original_layout);

        let (converted_text, converted_is_ru) = self.try_correct_last_word(&text_to_convert);
        info!("Converting: '{}' -> '{}' (is_ru={})", text_to_convert, converted_text, converted_is_ru);

        {
            let mut dev = match self.uinput.lock() {
                Ok(d) => d,
                Err(e) => {
                    error!("uinput mutex POISONED: {}", e);
                    return Err(anyhow!("uinput lock failed: {}", e));
                }
            };

            let ctrl = Key::KEY_LEFTCTRL.code();
            let bs = Key::KEY_BACKSPACE.code();

            info!("Deleting {} word(s)", actual_words);

            for i in 0..actual_words {
                info!("Deleting word {}/{}", i + 1, actual_words);

                if let Err(e) = dev.emit(&[InputEvent::new(EventType::KEY, ctrl, 1)]) {
                    error!("Failed to emit Ctrl press: {}", e);
                    return Err(anyhow!("uinput emit failed: {}", e));
                }
                thread::sleep(Duration::from_millis(5));

                if let Err(e) = dev.emit(&[InputEvent::new(EventType::KEY, bs, 1)]) {
                    error!("Failed to emit Backspace press: {}", e);
                    let _ = dev.emit(&[InputEvent::new(EventType::KEY, ctrl, 0)]);
                    return Err(anyhow!("uinput emit failed: {}", e));
                }
                thread::sleep(Duration::from_millis(5));

                if let Err(e) = dev.emit(&[InputEvent::new(EventType::KEY, bs, 0)]) {
                    error!("Failed to emit Backspace release: {}", e);
                    let _ = dev.emit(&[InputEvent::new(EventType::KEY, ctrl, 0)]);
                    return Err(anyhow!("uinput emit failed: {}", e));
                }
                thread::sleep(Duration::from_millis(5));

                if let Err(e) = dev.emit(&[InputEvent::new(EventType::KEY, ctrl, 0)]) {
                    error!("Failed to emit Ctrl release: {}", e);
                    return Err(anyhow!("uinput emit failed: {}", e));
                }
                thread::sleep(Duration::from_millis(30));
            }

            info!("Deletion complete");
        }

        thread::sleep(Duration::from_millis(200));

        let target_layout = if converted_is_ru { 1 } else { 0 };
        if target_layout != original_layout {
            info!("Switching layout from {} to {}", original_layout, target_layout);
            if let Err(e) = self.switch_layout(target_layout) {
                error!("Failed to switch layout: {}", e);
                return Err(anyhow!("Layout switch failed: {}", e));
            }
            thread::sleep(Duration::from_millis(100));
        } else {
            info!("Layout already correct ({})", target_layout);
        }

        if let Ok(mut buf) = self.char_buffer.lock() {
            buf.clear();
        }

        info!("Typing: '{}'", converted_text);
        if let Err(e) = self.type_text_uinput(&converted_text, target_layout) {
            error!("Failed to type text: {}", e);
            return Err(anyhow!("Type text failed: {}", e));
        }

        thread::sleep(Duration::from_millis(150));

        if let Ok(mut buf) = self.char_buffer.lock() {
            buf.clear();
        }

        info!("Replacement completed");
        Ok(())
    }

    fn start_keyboard_listener(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let switcher = self.clone();
        let caps_lock_count = Arc::clone(&self.caps_lock_count);
        let last_caps_lock_time = Arc::clone(&self.last_caps_lock_time);
        let shift_count = Arc::clone(&self.shift_count);
        let last_shift_time = Arc::clone(&self.last_shift_time);
        let char_buffer = Arc::clone(&self.char_buffer);
        let buffer_window_id = Arc::clone(&self.buffer_window_id);
        let replacing_text = Arc::clone(&self.replacing_text);

        thread::spawn(move || {
            let mut pressed_keys = HashSet::new();
            let mut modifiers = ModifierState::default();
            let mut last_hotkey = SystemTime::now();
            let mut last_spell_toggle = SystemTime::now();
            let mut current_layout_is_ru = false;
            let mut last_layout_poll = Instant::now();

            let callback = move |event: KbdEvent| {
                match event.event_type {
                    RdevEventType::KeyPress(key) => {
                        pressed_keys.insert(key.clone());
                        modifiers.update(&key, true);

                        if key == RdevKey::CapsLock {
                            let mut count = caps_lock_count.lock().unwrap();
                            *count += 1;
                            *last_caps_lock_time.lock().unwrap() = Instant::now();
                        }

                        if key == RdevKey::Space {
                            let is_replacing = *replacing_text.lock().unwrap();
                            if !is_replacing {
                                let mut buf = char_buffer.lock().unwrap();
                                buf.push(' ');
                                if buf.chars().count() > MAX_CHARS {
                                    let skip = buf.chars().count() - MAX_CHARS;
                                    *buf = buf.chars().skip(skip).collect();
                                }
                            }
                        }

                        if key == RdevKey::ShiftLeft || key == RdevKey::ShiftRight {
                            let is_replacing = *replacing_text.lock().unwrap();
                            if !is_replacing {
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
                            }
                        }

                        if key != RdevKey::ShiftLeft && key != RdevKey::ShiftRight {
                            let is_replacing = *replacing_text.lock().unwrap();
                            if !is_replacing {
                                *shift_count.lock().unwrap() = 0;
                            }
                        }

                        if Self::is_typing_key(&key) && last_layout_poll.elapsed() > Duration::from_millis(250) {
                            if let Some(layout) = switcher.get_current_layout() {
                                current_layout_is_ru = layout == 1;
                            }
                            last_layout_poll = Instant::now();
                        }

                        if Self::is_typing_key(&key) {
                            let is_replacing = *replacing_text.lock().unwrap();
                            if !is_replacing {
                                let current_window = switcher.get_active_window();
                                let mut buf_window = buffer_window_id.lock().unwrap();

                                if *buf_window != current_window {
                                    char_buffer.lock().unwrap().clear();
                                    *buf_window = current_window;
                                }

                                if let Some(c) = char_from_key(&key, modifiers.shift, current_layout_is_ru) {
                                    let mut buf = char_buffer.lock().unwrap();
                                    buf.push(c);
                                    if buf.chars().count() > MAX_CHARS {
                                        let skip = buf.chars().count() - MAX_CHARS;
                                        *buf = buf.chars().skip(skip).collect();
                                    }
                                }
                            }
                        }

                        if Self::is_boundary(&key) {
                            let is_replacing = *replacing_text.lock().unwrap();
                            if !is_replacing {
                                char_buffer.lock().unwrap().clear();
                            }
                        }

                        if key == RdevKey::Backspace {
                            let is_replacing = *replacing_text.lock().unwrap();
                            if !is_replacing {
                                let mut buf = char_buffer.lock().unwrap();
                                buf.pop();
                            }
                        }

                        let (add_window_hotkey, spell_toggle_hotkey) = {
                            let config = match config.lock() {
                                Ok(c) => c,
                                Err(_) => return,
                            };
                            (
                                config.hotkeys.get("add_window").cloned(),
                                config.hotkeys.get("toggle_spell").cloned(),
                            )
                        };

                        if let Some(hotkey) = add_window_hotkey {
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

                        if let Some(hotkey) = spell_toggle_hotkey {
                            if Self::check_hotkey(&pressed_keys, &modifiers, &hotkey) {
                                let now = SystemTime::now();
                                if let Ok(duration) = now.duration_since(last_spell_toggle) {
                                    if duration > Duration::from_secs(1) {
                                        last_spell_toggle = now;
                                        let switcher_clone = switcher.clone();
                                        thread::spawn(move || { let _ = switcher_clone.toggle_spell_check(); });
                                    }
                                }
                            }
                        }
                    }
                    RdevEventType::KeyRelease(key) => {
                        pressed_keys.remove(&key);
                        modifiers.update(&key, false);

                        if key == RdevKey::CapsLock {
                            let captured_count = {
                                *caps_lock_count.lock().unwrap()
                            };
                            if captured_count > 0 {
                                let switcher_clone = switcher.clone();
                                let caps_count = Arc::clone(&caps_lock_count);
                                let char_buf = Arc::clone(&char_buffer);
                                let buf_win = Arc::clone(&buffer_window_id);
                                thread::spawn(move || {
                                    thread::sleep(Duration::from_millis(300));
                                    let final_count = *caps_count.lock().unwrap();
                                    if final_count == captured_count {
                                        if captured_count == 1 {
                                            let _ = switcher_clone.switch_layout(0);
                                        } else {
                                            let _ = switcher_clone.switch_layout(1);
                                        }
                                        char_buf.lock().unwrap().clear();
                                        *buf_win.lock().unwrap() = None;
                                        *caps_count.lock().unwrap() = 0;
                                    }
                                });
                            }
                        }

                        if key == RdevKey::ShiftLeft || key == RdevKey::ShiftRight {
                            let is_replacing = *replacing_text.lock().unwrap();
                            if !is_replacing {
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
                                            if let Err(e) = switcher_clone.handle_shift_count(captured_count) {
                                                error!("Shift count error: {}", e);
                                            }
                                        }
                                    });
                                }
                            }
                        }
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

    fn apply_layout_to_window(&self, window_id: u32) -> Result<()> {
        info!("apply_layout_to_window: window {}", window_id);

        let active_window = self.get_active_window();
        if active_window != Some(window_id) {
            info!("Window {} no longer active (active: {:?}), skipping", window_id, active_window);
            return Ok(());
        }

        if window_id == 0 {
            return Ok(());
        }

        let window_class = match self.get_window_class(window_id) {
            Some(wc) => wc,
            None => {
                info!("Window {} has no class, skipping", window_id);
                return Ok(());
            }
        };

        info!("Window {} class: '{}'", window_id, window_class);

        let config = match self.config.lock() {
            Ok(c) => c,
            Err(e) => {
                error!("Config lock error in apply_layout: {}", e);
                return Err(anyhow!("Config lock failed: {}", e));
            }
        };

        if let Some(&target_layout) = config.window_layout_map.get(&window_class) {
            let current = match self.get_current_layout() {
                Some(l) => l,
                None => {
                    error!("Failed to get current layout");
                    return Err(anyhow!("Get layout failed"));
                }
            };

            if current != target_layout {
                info!("Delayed: switching to layout {} for '{}' (current={})", target_layout, window_class, current);
                self.switch_layout(target_layout)?;
                info!("Delayed layout switch successful");
            } else {
                info!("Delayed: layout already {}", current);
            }
        } else {
            info!("Delayed: class '{}' not in config", window_class);
        }

        Ok(())
    }

    fn handle_window_change(&mut self, window_id: u32) -> Result<()> {
        info!("handle_window_change: window_id={}", window_id);

        if self.last_window_id == Some(window_id) {
            info!("Same window as before, skipping");
            return Ok(());
        }

        if window_id == 0 {
            info!("Ignoring window 0 (root/desktop)");
            return Ok(());
        }

        {
            let mut handling = match self.handling_window_change.lock() {
                Ok(h) => h,
                Err(e) => {
                    error!("handling_window_change mutex POISONED: {}", e);
                    return Ok(());
                }
            };

            if *handling {
                info!("Already handling window change, queueing {}", window_id);
                self.last_window_id = Some(window_id);
                return Ok(());
            }

            let mut last_time = match self.last_change_time.lock() {
                Ok(t) => t,
                Err(e) => {
                    error!("last_change_time mutex POISONED: {}", e);
                    return Ok(());
                }
            };

            let elapsed = last_time.elapsed();
            if elapsed < Duration::from_millis(100) {
                info!("Window change too fast ({:.0}ms), delaying for window {}", elapsed.as_millis(), window_id);
                self.last_window_id = Some(window_id);

                let switcher_clone = self.clone();
                let target_window = window_id;
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(200));
                    if let Err(e) = switcher_clone.apply_layout_to_window(target_window) {
                        error!("Delayed layout switch failed: {}", e);
                    }
                });

                return Ok(());
            }

            *last_time = Instant::now();
            *handling = true;
        }

        struct WindowChangeGuard {
            flag: Arc<Mutex<bool>>,
        }

        impl Drop for WindowChangeGuard {
            fn drop(&mut self) {
                if let Ok(mut h) = self.flag.lock() {
                    *h = false;
                    info!("Window change guard: flag reset");
                } else {
                    error!("Window change guard: failed to reset flag");
                }
            }
        }

        let _guard = WindowChangeGuard {
            flag: Arc::clone(&self.handling_window_change),
        };

        info!("Window changed from {:?} to {}", self.last_window_id, window_id);
        self.last_window_id = Some(window_id);

        match self.char_buffer.lock() {
            Ok(mut buf) => {
                buf.clear();
                info!("Char buffer cleared");
            },
            Err(e) => error!("Failed to clear char buffer: {}", e),
        }

        match self.buffer_window_id.lock() {
            Ok(mut bw) => {
                *bw = Some(window_id);
                info!("Buffer window ID updated to {}", window_id);
            },
            Err(e) => error!("Failed to set buffer window ID: {}", e),
        }

        let window_class = match self.get_window_class(window_id) {
            Some(wc) => {
                info!("Window {} class: '{}'", window_id, wc);
                wc
            }
            None => {
                info!("Window {} has no class, skipping layout switch", window_id);
                return Ok(());
            }
        };

        let config = match self.config.lock() {
            Ok(c) => c,
            Err(e) => {
                error!("Config lock error: {}", e);
                return Ok(());
            }
        };

        if let Some(&target_layout) = config.window_layout_map.get(&window_class) {
            let current = match self.get_current_layout() {
                Some(l) => l,
                None => {
                    error!("Failed to get current layout");
                    255
                }
            };

            if current != target_layout {
                info!("Switching to layout {} for '{}' (current={})", target_layout, window_class, current);
                if let Err(e) = self.switch_layout(target_layout) {
                    error!("Failed to switch layout: {}", e);
                } else {
                    info!("Layout switched successfully");
                }
            } else {
                info!("Layout already {}", current);
            }
        } else {
            info!("Window class '{}' not in config", window_class);
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
            caps_lock_count: Arc::clone(&self.caps_lock_count),
            last_caps_lock_time: Arc::clone(&self.last_caps_lock_time),
            shift_count: Arc::clone(&self.shift_count),
            last_shift_time: Arc::clone(&self.last_shift_time),
            char_buffer: Arc::clone(&self.char_buffer),
            buffer_window_id: Arc::clone(&self.buffer_window_id),
            uinput: Arc::clone(&self.uinput),
            spell_checker: self.spell_checker.clone(),
            replacing_text: Arc::clone(&self.replacing_text),
            server_process: Arc::clone(&self.server_process),
            handling_window_change: Arc::clone(&self.handling_window_change),
            last_change_time: Arc::clone(&self.last_change_time),
            replacing_start_time: Arc::clone(&self.replacing_start_time),
        }
    }
}

fn char_from_key(key: &RdevKey, shift: bool, layout_is_ru: bool) -> Option<char> {
    if layout_is_ru {
        match key {
            RdevKey::KeyA => if shift { Some('Ф') } else { Some('ф') },
            RdevKey::KeyB => if shift { Some('И') } else { Some('и') },
            RdevKey::KeyC => if shift { Some('С') } else { Some('с') },
            RdevKey::KeyD => if shift { Some('В') } else { Some('в') },
            RdevKey::KeyE => if shift { Some('У') } else { Some('у') },
            RdevKey::KeyF => if shift { Some('А') } else { Some('а') },
            RdevKey::KeyG => if shift { Some('П') } else { Some('п') },
            RdevKey::KeyH => if shift { Some('Р') } else { Some('р') },
            RdevKey::KeyI => if shift { Some('Ш') } else { Some('ш') },
            RdevKey::KeyJ => if shift { Some('О') } else { Some('о') },
            RdevKey::KeyK => if shift { Some('Л') } else { Some('л') },
            RdevKey::KeyL => if shift { Some('Д') } else { Some('д') },
            RdevKey::KeyM => if shift { Some('Ь') } else { Some('ь') },
            RdevKey::KeyN => if shift { Some('Т') } else { Some('т') },
            RdevKey::KeyO => if shift { Some('Щ') } else { Some('щ') },
            RdevKey::KeyP => if shift { Some('З') } else { Some('з') },
            RdevKey::KeyQ => if shift { Some('Й') } else { Some('й') },
            RdevKey::KeyR => if shift { Some('К') } else { Some('к') },
            RdevKey::KeyS => if shift { Some('Ы') } else { Some('ы') },
            RdevKey::KeyT => if shift { Some('Е') } else { Some('е') },
            RdevKey::KeyU => if shift { Some('Г') } else { Some('г') },
            RdevKey::KeyV => if shift { Some('М') } else { Some('м') },
            RdevKey::KeyW => if shift { Some('Ц') } else { Some('ц') },
            RdevKey::KeyX => if shift { Some('Ч') } else { Some('ч') },
            RdevKey::KeyY => if shift { Some('Н') } else { Some('н') },
            RdevKey::KeyZ => if shift { Some('Я') } else { Some('я') },
            RdevKey::Num0 => if shift { Some(')') } else { Some('0') },
            RdevKey::Num1 => if shift { Some('!') } else { Some('1') },
            RdevKey::Num2 => if shift { Some('"') } else { Some('2') },
            RdevKey::Num3 => if shift { Some('№') } else { Some('3') },
            RdevKey::Num4 => if shift { Some(';') } else { Some('4') },
            RdevKey::Num5 => if shift { Some('%') } else { Some('5') },
            RdevKey::Num6 => if shift { Some(':') } else { Some('6') },
            RdevKey::Num7 => if shift { Some('?') } else { Some('7') },
            RdevKey::Num8 => if shift { Some('*') } else { Some('8') },
            RdevKey::Num9 => if shift { Some('(') } else { Some('9') },
            RdevKey::Minus => if shift { Some('_') } else { Some('-') },
            RdevKey::Equal => if shift { Some('+') } else { Some('=') },
            RdevKey::LeftBracket => if shift { Some('Х') } else { Some('х') },
            RdevKey::RightBracket => if shift { Some('Ъ') } else { Some('ъ') },
            RdevKey::SemiColon => if shift { Some('Ж') } else { Some('ж') },
            RdevKey::Quote => if shift { Some('Э') } else { Some('э') },
            RdevKey::Comma => if shift { Some('Б') } else { Some('б') },
            RdevKey::Dot => if shift { Some('Ю') } else { Some('ю') },
            RdevKey::Slash => if shift { Some(',') } else { Some('.') },
            RdevKey::BackSlash => if shift { Some('/') } else { Some('\\') },
            RdevKey::Space => Some(' '),
            _ => None,
        }
    } else {
        match key {
            RdevKey::KeyA => if shift { Some('A') } else { Some('a') },
            RdevKey::KeyB => if shift { Some('B') } else { Some('b') },
            RdevKey::KeyC => if shift { Some('C') } else { Some('c') },
            RdevKey::KeyD => if shift { Some('D') } else { Some('d') },
            RdevKey::KeyE => if shift { Some('E') } else { Some('e') },
            RdevKey::KeyF => if shift { Some('F') } else { Some('f') },
            RdevKey::KeyG => if shift { Some('G') } else { Some('g') },
            RdevKey::KeyH => if shift { Some('H') } else { Some('h') },
            RdevKey::KeyI => if shift { Some('I') } else { Some('i') },
            RdevKey::KeyJ => if shift { Some('J') } else { Some('j') },
            RdevKey::KeyK => if shift { Some('K') } else { Some('k') },
            RdevKey::KeyL => if shift { Some('L') } else { Some('l') },
            RdevKey::KeyM => if shift { Some('M') } else { Some('m') },
            RdevKey::KeyN => if shift { Some('N') } else { Some('n') },
            RdevKey::KeyO => if shift { Some('O') } else { Some('o') },
            RdevKey::KeyP => if shift { Some('P') } else { Some('p') },
            RdevKey::KeyQ => if shift { Some('Q') } else { Some('q') },
            RdevKey::KeyR => if shift { Some('R') } else { Some('r') },
            RdevKey::KeyS => if shift { Some('S') } else { Some('s') },
            RdevKey::KeyT => if shift { Some('T') } else { Some('t') },
            RdevKey::KeyU => if shift { Some('U') } else { Some('u') },
            RdevKey::KeyV => if shift { Some('V') } else { Some('v') },
            RdevKey::KeyW => if shift { Some('W') } else { Some('w') },
            RdevKey::KeyX => if shift { Some('X') } else { Some('x') },
            RdevKey::KeyY => if shift { Some('Y') } else { Some('y') },
            RdevKey::KeyZ => if shift { Some('Z') } else { Some('z') },
            RdevKey::Num0 => if shift { Some(')') } else { Some('0') },
            RdevKey::Num1 => if shift { Some('!') } else { Some('1') },
            RdevKey::Num2 => if shift { Some('@') } else { Some('2') },
            RdevKey::Num3 => if shift { Some('#') } else { Some('3') },
            RdevKey::Num4 => if shift { Some('$') } else { Some('4') },
            RdevKey::Num5 => if shift { Some('%') } else { Some('5') },
            RdevKey::Num6 => if shift { Some('^') } else { Some('6') },
            RdevKey::Num7 => if shift { Some('&') } else { Some('7') },
            RdevKey::Num8 => if shift { Some('*') } else { Some('8') },
            RdevKey::Num9 => if shift { Some('(') } else { Some('9') },
            RdevKey::Minus => if shift { Some('_') } else { Some('-') },
            RdevKey::Equal => if shift { Some('+') } else { Some('=') },
            RdevKey::LeftBracket => if shift { Some('{') } else { Some('[') },
            RdevKey::RightBracket => if shift { Some('}') } else { Some(']') },
            RdevKey::SemiColon => if shift { Some(':') } else { Some(';') },
            RdevKey::Quote => if shift { Some('"') } else { Some('\'') },
            RdevKey::Comma => if shift { Some('<') } else { Some(',') },
            RdevKey::Dot => if shift { Some('>') } else { Some('.') },
            RdevKey::Slash => if shift { Some('?') } else { Some('/') },
            RdevKey::BackSlash => if shift { Some('|') } else { Some('\\') },
            RdevKey::Space => Some(' '),
            _ => None,
        }
    }
}

fn char_to_keycode(ch: char, layout_is_ru: bool) -> Option<(u16, bool)> {
    if layout_is_ru {
        match ch {
            'ф' | 'Ф' => Some((Key::KEY_A.code(), ch == 'Ф')),
            'и' | 'И' => Some((Key::KEY_B.code(), ch == 'И')),
            'с' | 'С' => Some((Key::KEY_C.code(), ch == 'С')),
            'в' | 'В' => Some((Key::KEY_D.code(), ch == 'В')),
            'у' | 'У' => Some((Key::KEY_E.code(), ch == 'У')),
            'а' | 'А' => Some((Key::KEY_F.code(), ch == 'А')),
            'п' | 'П' => Some((Key::KEY_G.code(), ch == 'П')),
            'р' | 'Р' => Some((Key::KEY_H.code(), ch == 'Р')),
            'ш' | 'Ш' => Some((Key::KEY_I.code(), ch == 'Ш')),
            'о' | 'О' => Some((Key::KEY_J.code(), ch == 'О')),
            'л' | 'Л' => Some((Key::KEY_K.code(), ch == 'Л')),
            'д' | 'Д' => Some((Key::KEY_L.code(), ch == 'Д')),
            'ь' | 'Ь' => Some((Key::KEY_M.code(), ch == 'Ь')),
            'т' | 'Т' => Some((Key::KEY_N.code(), ch == 'Т')),
            'щ' | 'Щ' => Some((Key::KEY_O.code(), ch == 'Щ')),
            'з' | 'З' => Some((Key::KEY_P.code(), ch == 'З')),
            'й' | 'Й' => Some((Key::KEY_Q.code(), ch == 'Й')),
            'к' | 'К' => Some((Key::KEY_R.code(), ch == 'К')),
            'ы' | 'Ы' => Some((Key::KEY_S.code(), ch == 'Ы')),
            'е' | 'Е' => Some((Key::KEY_T.code(), ch == 'Е')),
            'г' | 'Г' => Some((Key::KEY_U.code(), ch == 'Г')),
            'м' | 'М' => Some((Key::KEY_V.code(), ch == 'М')),
            'ц' | 'Ц' => Some((Key::KEY_W.code(), ch == 'Ц')),
            'ч' | 'Ч' => Some((Key::KEY_X.code(), ch == 'Ч')),
            'н' | 'Н' => Some((Key::KEY_Y.code(), ch == 'Н')),
            'я' | 'Я' => Some((Key::KEY_Z.code(), ch == 'Я')),
            ' ' => Some((Key::KEY_SPACE.code(), false)),
            '0' | ')' => Some((Key::KEY_0.code(), ch == ')')),
            '1' | '!' => Some((Key::KEY_1.code(), ch == '!')),
            '2' | '"' => Some((Key::KEY_2.code(), ch == '"')),
            '3' | '№' => Some((Key::KEY_3.code(), ch == '№')),
            '4' | ';' => Some((Key::KEY_4.code(), ch == ';')),
            '5' | '%' => Some((Key::KEY_5.code(), ch == '%')),
            '6' | ':' => Some((Key::KEY_6.code(), ch == ':')),
            '7' | '?' => Some((Key::KEY_7.code(), ch == '?')),
            '8' | '*' => Some((Key::KEY_8.code(), ch == '*')),
            '9' | '(' => Some((Key::KEY_9.code(), ch == '(')),
            '-' | '_' => Some((Key::KEY_MINUS.code(), ch == '_')),
            '=' | '+' => Some((Key::KEY_EQUAL.code(), ch == '+')),
            'х' | 'Х' => Some((Key::KEY_LEFTBRACE.code(), ch == 'Х')),
            'ъ' | 'Ъ' => Some((Key::KEY_RIGHTBRACE.code(), ch == 'Ъ')),
            'ж' | 'Ж' => Some((Key::KEY_SEMICOLON.code(), ch == 'Ж')),
            'э' | 'Э' => Some((Key::KEY_APOSTROPHE.code(), ch == 'Э')),
            'б' | 'Б' => Some((Key::KEY_COMMA.code(), ch == 'Б')),
            'ю' | 'Ю' => Some((Key::KEY_DOT.code(), ch == 'Ю')),
            '.' | ',' => Some((Key::KEY_SLASH.code(), ch == ',')),
            '/' | '\\' => Some((Key::KEY_BACKSLASH.code(), ch == '\\')),
            'ё' | 'Ё' => Some((Key::KEY_GRAVE.code(), ch == 'Ё')),
            _ => None,
        }
    } else {
        match ch {
            'a' | 'A' => Some((Key::KEY_A.code(), ch == 'A')),
            'b' | 'B' => Some((Key::KEY_B.code(), ch == 'B')),
            'c' | 'C' => Some((Key::KEY_C.code(), ch == 'C')),
            'd' | 'D' => Some((Key::KEY_D.code(), ch == 'D')),
            'e' | 'E' => Some((Key::KEY_E.code(), ch == 'E')),
            'f' | 'F' => Some((Key::KEY_F.code(), ch == 'F')),
            'g' | 'G' => Some((Key::KEY_G.code(), ch == 'G')),
            'h' | 'H' => Some((Key::KEY_H.code(), ch == 'H')),
            'i' | 'I' => Some((Key::KEY_I.code(), ch == 'I')),
            'j' | 'J' => Some((Key::KEY_J.code(), ch == 'J')),
            'k' | 'K' => Some((Key::KEY_K.code(), ch == 'K')),
            'l' | 'L' => Some((Key::KEY_L.code(), ch == 'L')),
            'm' | 'M' => Some((Key::KEY_M.code(), ch == 'M')),
            'n' | 'N' => Some((Key::KEY_N.code(), ch == 'N')),
            'o' | 'O' => Some((Key::KEY_O.code(), ch == 'O')),
            'p' | 'P' => Some((Key::KEY_P.code(), ch == 'P')),
            'q' | 'Q' => Some((Key::KEY_Q.code(), ch == 'Q')),
            'r' | 'R' => Some((Key::KEY_R.code(), ch == 'R')),
            's' | 'S' => Some((Key::KEY_S.code(), ch == 'S')),
            't' | 'T' => Some((Key::KEY_T.code(), ch == 'T')),
            'u' | 'U' => Some((Key::KEY_U.code(), ch == 'U')),
            'v' | 'V' => Some((Key::KEY_V.code(), ch == 'V')),
            'w' | 'W' => Some((Key::KEY_W.code(), ch == 'W')),
            'x' | 'X' => Some((Key::KEY_X.code(), ch == 'X')),
            'y' | 'Y' => Some((Key::KEY_Y.code(), ch == 'Y')),
            'z' | 'Z' => Some((Key::KEY_Z.code(), ch == 'Z')),
            ' ' => Some((Key::KEY_SPACE.code(), false)),
            '0' | ')' => Some((Key::KEY_0.code(), ch == ')')),
            '1' | '!' => Some((Key::KEY_1.code(), ch == '!')),
            '2' | '@' => Some((Key::KEY_2.code(), ch == '@')),
            '3' | '#' => Some((Key::KEY_3.code(), ch == '#')),
            '4' | '$' => Some((Key::KEY_4.code(), ch == '$')),
            '5' | '%' => Some((Key::KEY_5.code(), ch == '%')),
            '6' | '^' => Some((Key::KEY_6.code(), ch == '^')),
            '7' | '&' => Some((Key::KEY_7.code(), ch == '&')),
            '8' | '*' => Some((Key::KEY_8.code(), ch == '*')),
            '9' | '(' => Some((Key::KEY_9.code(), ch == '(')),
            '-' | '_' => Some((Key::KEY_MINUS.code(), ch == '_')),
            '=' | '+' => Some((Key::KEY_EQUAL.code(), ch == '+')),
            '[' | '{' => Some((Key::KEY_LEFTBRACE.code(), ch == '{')),
            ']' | '}' => Some((Key::KEY_RIGHTBRACE.code(), ch == '}')),
            ';' | ':' => Some((Key::KEY_SEMICOLON.code(), ch == ':')),
            '\'' | '"' => Some((Key::KEY_APOSTROPHE.code(), ch == '"')),
            ',' | '<' => Some((Key::KEY_COMMA.code(), ch == '<')),
            '.' | '>' => Some((Key::KEY_DOT.code(), ch == '>')),
            '/' | '?' => Some((Key::KEY_SLASH.code(), ch == '?')),
            '\\' | '|' => Some((Key::KEY_BACKSLASH.code(), ch == '|')),
            '`' | '~' => Some((Key::KEY_GRAVE.code(), ch == '~')),
            _ => None,
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
        let mut config = if path.exists() {
            serde_json::from_str(&fs::read_to_string(path)?)?
        } else {
            AppConfig {
                window_layout_map: HashMap::new(),
                hotkeys: HashMap::new(),
                enable_spell_check: false,
                python_interpreter: None,
            }
        };

        let mut changed = false;

        if !config.hotkeys.contains_key("add_window") {
            config.hotkeys.insert("add_window".into(), "ctrl shift q".into());
            changed = true;
        }
        if !config.hotkeys.contains_key("toggle_spell") {
            config.hotkeys.insert("toggle_spell".into(), "ctrl shift l".into());
            changed = true;
        }

        let python_ok = match &config.python_interpreter {
            Some(s) if !s.is_empty() => true,
            _ => false,
        };
        if !python_ok {
            config.python_interpreter = Some(default_python_interpreter());
            changed = true;
        }

        if changed {
            config.save_to_file(path)?;
        }

        Ok(config)
    }

    fn save_to_file(&self, path: &PathBuf) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        let mut file = File::create(path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }
}

fn create_virtual_keyboard() -> Result<evdev::uinput::VirtualDevice> {
    let mut keys = AttributeSet::new();
    let all_keys = [
        Key::KEY_A, Key::KEY_B, Key::KEY_C, Key::KEY_D, Key::KEY_E,
        Key::KEY_F, Key::KEY_G, Key::KEY_H, Key::KEY_I, Key::KEY_J,
        Key::KEY_K, Key::KEY_L, Key::KEY_M, Key::KEY_N, Key::KEY_O,
        Key::KEY_P, Key::KEY_Q, Key::KEY_R, Key::KEY_S, Key::KEY_T,
        Key::KEY_U, Key::KEY_V, Key::KEY_W, Key::KEY_X, Key::KEY_Y,
        Key::KEY_Z,
        Key::KEY_0, Key::KEY_1, Key::KEY_2, Key::KEY_3, Key::KEY_4,
        Key::KEY_5, Key::KEY_6, Key::KEY_7, Key::KEY_8, Key::KEY_9,
        Key::KEY_SPACE, Key::KEY_ENTER, Key::KEY_BACKSPACE,
        Key::KEY_LEFTSHIFT, Key::KEY_RIGHTSHIFT,
        Key::KEY_LEFTCTRL, Key::KEY_RIGHTCTRL,
        Key::KEY_LEFTALT, Key::KEY_RIGHTALT,
        Key::KEY_MINUS, Key::KEY_EQUAL,
        Key::KEY_LEFTBRACE, Key::KEY_RIGHTBRACE,
        Key::KEY_SEMICOLON, Key::KEY_APOSTROPHE,
        Key::KEY_COMMA, Key::KEY_DOT, Key::KEY_SLASH,
        Key::KEY_BACKSLASH, Key::KEY_GRAVE,
        Key::KEY_TAB, Key::KEY_ESC,
        Key::KEY_LEFT, Key::KEY_RIGHT, Key::KEY_UP, Key::KEY_DOWN,
        Key::KEY_HOME, Key::KEY_END, Key::KEY_PAGEUP, Key::KEY_PAGEDOWN,
        Key::KEY_DELETE, Key::KEY_INSERT,
    ];
    for k in all_keys {
        keys.insert(k);
    }

    VirtualDeviceBuilder::new()
        .context("Failed to create VirtualDeviceBuilder")?
        .name("nskbd-virtual-keyboard")
        .with_keys(&keys)
        .context("Failed to set keys")?
        .build()
        .context("Failed to create virtual device")
}

fn main() -> Result<()> {
    std::panic::set_hook(Box::new(|panic_info| {
        use std::io::Write;

        let location = panic_info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic".to_string()
        };

        let panic_msg = format!("!!! PANIC at {:?}: {}", location, message);
        eprintln!("{}", panic_msg);

        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("kbd_switcher.log")
        {
            let _ = writeln!(file, "{}", panic_msg);
            let _ = writeln!(file, "Stack backtrace:");
            let backtrace = std::backtrace::Backtrace::force_capture();
            let _ = writeln!(file, "{}", backtrace);
            let _ = file.flush();
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }));

    match run_app() {
        Ok(()) => Ok(()),
        Err(e) => {
            error!("Application error: {}", e);
            eprintln!("Error: {}", e);
            for cause in e.chain() {
                eprintln!("Caused by: {}", cause);
            }
            std::process::exit(1);
        }
    }
}

fn run_app() -> Result<()> {
    info!("Starting NSKeyboardLayoutSwitcher");

    let mut switcher = KeyboardLayoutSwitcher::new("config.json", "kbd_switcher.log")?;

    if env::args().any(|arg| arg == "--add") {
        switcher.add_current_window()?;
    } else {
        switcher.run()?;
    }

    Ok(())
}