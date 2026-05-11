use anyhow::{Context, Result};
use std::process::Command;

pub struct SpellChecker;

impl SpellChecker {
    pub fn check_word(word: &str) -> Result<String> {
        let output = Command::new("python3")
            .arg("check_word.py")
            .arg(word)
            .output()
            .context("Failed to run spell checker")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Spell check failed: {}", stderr));
        }
        
        let corrected = String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_string();
        
        Ok(corrected)
    }
}