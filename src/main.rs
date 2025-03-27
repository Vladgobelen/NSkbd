use anyhow::{Context, Result};
use log::{error, info};
use regex::Regex;
use serde::{Deserialize, Serialize};
use simplelog::{Config, LevelFilter, WriteLogger};
use std::{
    collections::HashMap,
    fs::{self, File},
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Serialize, Deserialize, Default, PartialEq)]
struct AppConfig {
    window_layout_map: HashMap<String, u8>,
}

impl AppConfig {
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        if path.exists() {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {:?}", path))?;
            serde_json::from_str(&content).with_context(|| "Failed to parse config")
        } else {
            Ok(Self::default())
        }
    }

    fn save_to_file(&self, path: &PathBuf) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content).with_context(|| format!("Failed to write config to {:?}", path))
    }
}

struct NSKeyboardLayoutSwitcher {
    config_path: PathBuf,
    log_path: PathBuf,
    config: AppConfig,
    last_window_class: Option<String>,
    last_config_check: u64,
}

impl NSKeyboardLayoutSwitcher {
    fn new(config_file: &str, log_file: &str) -> Result<Self> {
        let current_dir = std::env::current_dir()?;
        let config_path = current_dir.join(config_file);
        let log_path = current_dir.join(log_file);

        WriteLogger::init(
            LevelFilter::Info,
            Config::default(),
            File::create(&log_path)
                .with_context(|| format!("Failed to create log file: {:?}", log_path))?,
        )?;

        let config = AppConfig::load_from_file(&config_path)?;

        Ok(Self {
            config_path,
            log_path,
            config,
            last_window_class: None,
            last_config_check: 0,
        })
    }

    fn reload_config_if_needed(&mut self) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        if now - self.last_config_check > 5 {
            self.last_config_check = now;
            let new_config = AppConfig::load_from_file(&self.config_path)?;
            if new_config != self.config {
                info!("Config reloaded from disk");
                self.config = new_config;
            }
        }
        Ok(())
    }

    fn save_config(&mut self) -> Result<()> {
        self.config.save_to_file(&self.config_path)
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
        Regex::new(r#"WM_CLASS.*?"[^"]*",\s*"([^"]*)"#)
            .ok()?
            .captures(&wm_class)?
            .get(1)
            .map(|m| m.as_str().to_lowercase())
    }

    fn get_current_layout(&self) -> Option<u8> {
        Command::new("xkblayout-state")
            .arg("print")
            .arg("%s")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|l| {
                if l.to_lowercase().contains("ru") {
                    1
                } else {
                    0
                }
            })
    }

    fn add_current_window(&mut self) -> Result<()> {
        match (self.get_active_window_class(), self.get_current_layout()) {
            (Some(window_class), Some(layout)) => {
                self.config
                    .window_layout_map
                    .insert(window_class.clone(), layout);
                self.save_config()?;
                println!("Added: {} -> {}", window_class, layout);
                info!("Added mapping: {} -> {}", window_class, layout);
                Ok(())
            }
            _ => {
                let msg = "Failed to detect window or layout";
                error!("{}", msg);
                Err(anyhow::anyhow!(msg))
            }
        }
    }

    fn switch_layout(&self, layout_code: u8) -> Result<()> {
        Command::new("xkblayout-state")
            .arg("set")
            .arg(layout_code.to_string())
            .status()
            .with_context(|| "Failed to switch layout")?;
        info!("Layout switched to: {}", layout_code);
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        info!("Service started");
        println!("Keyboard layout switcher started (Ctrl+C to stop)");
        println!("Logging to: {:?}", self.log_path);

        loop {
            self.reload_config_if_needed()?;
            if let Some(current_class) = self.get_active_window_class() {
                if self.last_window_class.as_ref() != Some(&current_class) {
                    self.last_window_class = Some(current_class.clone());
                    if let Some(target_layout) = self.config.window_layout_map.get(&current_class) {
                        if let Some(current_layout) = self.get_current_layout() {
                            if current_layout != *target_layout {
                                self.switch_layout(*target_layout)
                                    .unwrap_or_else(|e| error!("Failed to switch layout: {}", e));
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(300));
        }
    }
}

fn main() -> Result<()> {
    let mut switcher = NSKeyboardLayoutSwitcher::new("config.json", "kbd.log")?;
    if std::env::args().any(|arg| arg == "--add") {
        println!("Adding current window...");
        match switcher.add_current_window() {
            Ok(_) => println!("Success! Window added to config."),
            Err(e) => println!("Failed! Error: {}", e),
        }
    } else {
        switcher.run()?;
    }
    Ok(())
}
