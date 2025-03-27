#!/usr/bin/env python3
import subprocess
import time
import re
import logging
import json
import sys
from pathlib import Path


class NSKeyboardLayoutSwitcher:
    def __init__(self, config_file="config.json", log_file="kbd.log"):
        self.script_dir = Path(__file__).parent.absolute()
        self.config_path = self.script_dir / config_file
        self.log_path = self.script_dir / log_file

        logging.basicConfig(
            filename=str(self.log_path),
            level=logging.INFO,
            format="%(asctime)s - %(levelname)s - %(message)s",
        )
        self.logger = logging.getLogger(__name__)
        self.config = self._load_config()
        self.last_window_class = None
        self.last_config_check = 0

    def _load_config(self):
        default_config = {"window_layout_map": {}}
        try:
            if self.config_path.exists():
                with open(self.config_path, "r") as f:
                    return json.load(f)
            return default_config
        except Exception as e:
            self.logger.error(f"Config error: {e}")
            return default_config

    def get_current_config(self):
        now = time.time()
        if now - self.last_config_check > 5:
            self.last_config_check = now
            new_config = self._load_config()
            if new_config != self.config:
                self.logger.info("Config reloaded from disk")
                self.config = new_config
        return self.config

    def save_config(self):
        try:
            with open(self.config_path, "w") as f:
                json.dump(self.config, f, indent=4)
            self.config = self._load_config()
        except Exception as e:
            self.logger.error(f"Failed to save config: {e}")

    def _get_active_window_class(self):
        try:
            window_id = subprocess.run(
                ["xdotool", "getactivewindow"],
                capture_output=True,
                text=True,
                timeout=1,
            ).stdout.strip()

            wm_class = subprocess.run(
                ["xprop", "-id", window_id, "WM_CLASS"],
                capture_output=True,
                text=True,
                timeout=1,
            ).stdout

            match = re.search(r'WM_CLASS.*?"[^"]*",\s*"([^"]*)"', wm_class)
            return match.group(1).lower() if match else None
        except Exception as e:
            self.logger.error(f"Window detection failed: {e}")
            return None

    def _get_current_layout(self):
        try:
            result = subprocess.run(
                ["xkblayout-state", "print", "%s"],
                capture_output=True,
                text=True,
                timeout=1,
            )
            layout = result.stdout.strip().lower()
            return 1 if layout in ("ru", "rus", "russian") else 0
        except Exception as e:
            self.logger.error(f"Layout detection failed: {e}")
            return None

    def add_current_window(self):
        window_class = self._get_active_window_class()
        current_layout = self._get_current_layout()

        if window_class and current_layout is not None:
            self.config["window_layout_map"][window_class] = current_layout
            self.save_config()
            print(f"Added: {window_class} -> {current_layout}")
            self.logger.info(f"Added mapping: {window_class} -> {current_layout}")
            return True
        self.logger.error("Failed to add window")
        return False

    def _switch_layout(self, layout_code):
        try:
            subprocess.run(
                ["xkblayout-state", "set", str(layout_code)], check=True, timeout=1
            )
            self.logger.info(f"Layout switched to: {layout_code}")
            return True
        except subprocess.CalledProcessError as e:
            self.logger.error(f"Failed to switch layout: {e}")
            return False

    def run(self):
        try:
            self.logger.info("Service started")
            while True:
                config = self.get_current_config()
                current_class = self._get_active_window_class()

                if current_class and current_class != self.last_window_class:
                    self.last_window_class = current_class
                    target_layout = config["window_layout_map"].get(current_class)

                    if target_layout is not None:
                        current_layout = self._get_current_layout()
                        if current_layout != target_layout:
                            self._switch_layout(target_layout)

                time.sleep(0.3)

        except KeyboardInterrupt:
            self.logger.info("Service stopped")
            print("\nStopped by user")


if __name__ == "__main__":
    switcher = NSKeyboardLayoutSwitcher()

    if "--add" in sys.argv:
        print("Adding current window...")
        if switcher.add_current_window():
            print("Success! Window added to config.")
        else:
            print("Failed! Check logs for details.")
    else:
        print("Keyboard layout switcher started (Ctrl+C to stop)")
        print(f"Logging to: {switcher.log_path}")
        switcher.run()
