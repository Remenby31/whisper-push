#!/usr/bin/env python3
"""
Whisper Push - Menu Bar App for macOS
Shows status icon in menu bar, handles global hotkey, and manages recording.
"""

import os
import subprocess
import sys
import signal
import tomllib
import threading
import time
from pathlib import Path
from enum import Enum

# PyObjC imports
try:
    import objc
    from AppKit import (
        NSApplication,
        NSApp,
        NSApplicationActivationPolicyAccessory,
        NSStatusBar,
        NSVariableStatusItemLength,
        NSMenu,
        NSMenuItem,
        NSImage,
    )
    from Cocoa import (
        NSEvent,
        NSKeyDownMask,
        NSCommandKeyMask,
        NSShiftKeyMask,
        NSAlternateKeyMask,
        NSControlKeyMask,
        NSObject,
        NSTimer,
        NSRunLoop,
        NSDefaultRunLoopMode,
        NSMakeSize,
    )
    from PyObjCTools import AppHelper
except ImportError:
    print("Error: PyObjC not installed. Run: pip3 install pyobjc-framework-Cocoa pyobjc-framework-Quartz")
    sys.exit(1)


class State(Enum):
    IDLE = "idle"
    RECORDING = "recording"
    PROCESSING = "processing"


# Paths
SUPPORT_DIR = Path.home() / "Library" / "Application Support" / "whisper-push"
CONFIG_FILE = SUPPORT_DIR / "config.toml"
ICONS_DIR = SUPPORT_DIR / "icons"
LOCK_FILE = Path(os.environ.get("TMPDIR", "/tmp")) / "whisper-push.lock"
APP_PATH = Path("/Applications/Whisper Push.app/Contents/MacOS/whisper-push")
SOURCE_PATH = SUPPORT_DIR / "whisper-push"

# Key code mapping (US keyboard layout)
KEY_CODES = {
    'a': 0, 'b': 11, 'c': 8, 'd': 2, 'e': 14, 'f': 3, 'g': 5, 'h': 4, 'i': 34,
    'j': 38, 'k': 40, 'l': 37, 'm': 46, 'n': 45, 'o': 31, 'p': 35, 'q': 12,
    'r': 15, 's': 1, 't': 17, 'u': 32, 'v': 9, 'w': 13, 'x': 7, 'y': 16, 'z': 6,
    '1': 18, '2': 19, '3': 20, '4': 21, '5': 23, '6': 22, '7': 26, '8': 28,
    '9': 25, '0': 29, 'space': 49, 'return': 36, 'tab': 48, 'escape': 53,
    'delete': 51, 'f1': 122, 'f2': 120, 'f3': 99, 'f4': 118, 'f5': 96,
    'f6': 97, 'f7': 98, 'f8': 100, 'f9': 101, 'f10': 109, 'f11': 103, 'f12': 111,
}


def load_config():
    """Load configuration from file."""
    config = {
        "hotkey": "cmd+shift+space",
        "language": "auto",
    }
    if CONFIG_FILE.exists():
        try:
            with open(CONFIG_FILE, "rb") as f:
                user_config = tomllib.load(f)
                config.update(user_config)
        except Exception as e:
            print(f"Warning: Could not load config: {e}")
    return config


def parse_hotkey(hotkey_str):
    """Parse hotkey string like 'cmd+shift+space' into modifiers and key code."""
    parts = hotkey_str.lower().split('+')
    modifiers = 0
    key_code = None

    for part in parts:
        part = part.strip()
        if part in ('cmd', 'command'):
            modifiers |= NSCommandKeyMask
        elif part in ('shift',):
            modifiers |= NSShiftKeyMask
        elif part in ('alt', 'option'):
            modifiers |= NSAlternateKeyMask
        elif part in ('ctrl', 'control'):
            modifiers |= NSControlKeyMask
        elif part in KEY_CODES:
            key_code = KEY_CODES[part]
        else:
            print(f"Warning: Unknown key '{part}' in hotkey")

    return modifiers, key_code


def format_hotkey_display(hotkey_str):
    """Format hotkey for display (e.g., 'cmd+shift+space' -> '⌘⇧Space')."""
    symbols = {
        'cmd': '⌘', 'command': '⌘',
        'shift': '⇧',
        'alt': '⌥', 'option': '⌥',
        'ctrl': '⌃', 'control': '⌃',
        'space': 'Space',
        'return': '↩',
        'tab': '⇥',
        'escape': '⎋',
        'delete': '⌫',
    }
    parts = hotkey_str.lower().split('+')
    result = []
    for part in parts:
        part = part.strip()
        if part in symbols:
            result.append(symbols[part])
        elif part.startswith('f') and part[1:].isdigit():
            result.append(part.upper())
        else:
            result.append(part.upper())
    return ''.join(result)


def get_whisper_push_command():
    """Get the command to run whisper-push."""
    if APP_PATH.exists():
        return [str(APP_PATH)]
    elif SOURCE_PATH.exists():
        return [str(SOURCE_PATH)]
    else:
        print("Error: whisper-push not found!")
        return None


def is_recording():
    """Check if recording is in progress."""
    if not LOCK_FILE.exists():
        return False
    try:
        pid = int(LOCK_FILE.read_text().strip())
        os.kill(pid, 0)
        return True
    except (ValueError, ProcessLookupError, PermissionError, FileNotFoundError):
        return False


def load_icon(state):
    """Load icon for the given state."""
    icon_files = {
        State.IDLE: ICONS_DIR / "icon-idle.png",
        State.RECORDING: ICONS_DIR / "icon-recording.png",
        State.PROCESSING: ICONS_DIR / "icon-processing.png",
    }

    icon_path = icon_files.get(state)
    if icon_path and icon_path.exists():
        image = NSImage.alloc().initWithContentsOfFile_(str(icon_path))
        if image:
            # Resize for menu bar (18x18 is standard)
            image.setSize_(NSMakeSize(18, 18))
            return image

    # Fallback to None (will use text)
    return None


# Global reference for hotkey handler (outside class to avoid PyObjC selector issues)
_app_instance = None


def _handle_global_hotkey(event):
    """Handle global key events - must be outside NSObject class."""
    global _app_instance
    if _app_instance is None:
        return
    if event.keyCode() == _app_instance.key_code:
        current_modifiers = event.modifierFlags()
        if (current_modifiers & _app_instance.modifiers) == _app_instance.modifiers:
            _app_instance.trigger_whisper_push()


class MenuBarApp(NSObject):
    """Menu bar application with status icon and hotkey handling."""

    def init(self):
        self = objc.super(MenuBarApp, self).init()
        if self is None:
            return None

        self.config = load_config()
        self.modifiers, self.key_code = parse_hotkey(self.config.get("hotkey", "cmd+shift+space"))
        self.command = get_whisper_push_command()
        self.state = State.IDLE
        self._processing = False

        # Load icons
        self.icons = {
            State.IDLE: load_icon(State.IDLE),
            State.RECORDING: load_icon(State.RECORDING),
            State.PROCESSING: load_icon(State.PROCESSING),
        }

        if self.key_code is None:
            print("Error: Invalid hotkey configuration")
            sys.exit(1)

        if self.command is None:
            sys.exit(1)

        # Create status bar item
        self.status_bar = NSStatusBar.systemStatusBar()
        self.status_item = self.status_bar.statusItemWithLength_(NSVariableStatusItemLength)

        # Set initial icon
        self.update_icon()

        # Create menu
        self.menu = NSMenu.alloc().init()
        self.setup_menu()
        self.status_item.setMenu_(self.menu)

        # Set up global hotkey monitor
        # Store reference for global handler function
        global _app_instance
        _app_instance = self
        NSEvent.addGlobalMonitorForEventsMatchingMask_handler_(
            NSKeyDownMask,
            _handle_global_hotkey
        )

        # Start state polling timer
        self.timer = NSTimer.scheduledTimerWithTimeInterval_target_selector_userInfo_repeats_(
            0.5,  # Check every 500ms for responsive UI
            self,
            "checkState:",
            None,
            True
        )
        NSRunLoop.currentRunLoop().addTimer_forMode_(self.timer, NSDefaultRunLoopMode)

        hotkey_display = format_hotkey_display(self.config.get("hotkey", "cmd+shift+space"))
        print(f"Menu bar app started. Hotkey: {hotkey_display}")

        return self

    def setup_menu(self):
        """Set up the dropdown menu."""
        self.menu.removeAllItems()

        hotkey_display = format_hotkey_display(self.config.get("hotkey", "cmd+shift+space"))
        status_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            f"Whisper Push ({hotkey_display})",
            None,
            ""
        )
        status_item.setEnabled_(False)
        self.menu.addItem_(status_item)

        self.menu.addItem_(NSMenuItem.separatorItem())

        self.toggle_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            "Start Recording",
            "toggleRecording:",
            ""
        )
        self.toggle_item.setTarget_(self)
        self.menu.addItem_(self.toggle_item)

        self.cancel_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            "Cancel Recording",
            "cancelRecording:",
            ""
        )
        self.cancel_item.setTarget_(self)
        self.cancel_item.setHidden_(True)
        self.menu.addItem_(self.cancel_item)

        self.menu.addItem_(NSMenuItem.separatorItem())

        config_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            "Open Config...",
            "openConfig:",
            ","
        )
        config_item.setTarget_(self)
        self.menu.addItem_(config_item)

        logs_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            "View Logs...",
            "viewLogs:",
            ""
        )
        logs_item.setTarget_(self)
        self.menu.addItem_(logs_item)

        self.menu.addItem_(NSMenuItem.separatorItem())

        quit_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            "Quit Whisper Push",
            "quitApp:",
            "q"
        )
        quit_item.setTarget_(self)
        self.menu.addItem_(quit_item)

    def update_icon(self):
        """Update the menu bar icon based on current state."""
        icon = self.icons.get(self.state)
        if icon:
            self.status_item.setImage_(icon)
            self.status_item.setTitle_("")
        else:
            # Fallback to text if no icon
            fallback = {
                State.IDLE: "◆",
                State.RECORDING: "●",
                State.PROCESSING: "◐",
            }
            self.status_item.setImage_(None)
            self.status_item.setTitle_(fallback.get(self.state, "◆"))

        if hasattr(self, 'toggle_item'):
            if self.state == State.IDLE:
                self.toggle_item.setTitle_("Start Recording")
                self.toggle_item.setEnabled_(True)
                self.cancel_item.setHidden_(True)
            elif self.state == State.RECORDING:
                self.toggle_item.setTitle_("Stop & Transcribe")
                self.toggle_item.setEnabled_(True)
                self.cancel_item.setHidden_(False)
            elif self.state == State.PROCESSING:
                self.toggle_item.setTitle_("Processing...")
                self.toggle_item.setEnabled_(False)
                self.cancel_item.setHidden_(True)

    def set_state(self, new_state):
        """Set the current state and update UI."""
        if self.state != new_state:
            self.state = new_state
            self.performSelectorOnMainThread_withObject_waitUntilDone_(
                "updateIconOnMainThread:",
                None,
                False
            )

    def updateIconOnMainThread_(self, _):
        self.update_icon()

    def checkState_(self, timer):
        """Periodically check the recording state."""
        if self._processing:
            return

        if is_recording():
            if self.state != State.RECORDING:
                self.set_state(State.RECORDING)
        else:
            if self.state == State.RECORDING:
                self.set_state(State.PROCESSING)
                threading.Timer(0.5, lambda: self.set_state(State.IDLE)).start()
            elif self.state != State.IDLE:
                self.set_state(State.IDLE)

    def trigger_whisper_push(self):
        """Trigger whisper-push toggle."""
        if self._processing:
            return

        try:
            was_recording = is_recording()

            subprocess.Popen(
                self.command,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )

            if was_recording:
                self.set_state(State.PROCESSING)
                self._processing = True
                def reset_processing():
                    time.sleep(30)
                    self._processing = False
                    if self.state == State.PROCESSING:
                        self.set_state(State.IDLE)
                threading.Thread(target=reset_processing, daemon=True).start()
            else:
                self.set_state(State.RECORDING)

        except Exception as e:
            print(f"Error triggering whisper-push: {e}")

    def toggleRecording_(self, sender):
        self.trigger_whisper_push()

    def cancelRecording_(self, sender):
        try:
            subprocess.run(
                self.command + ["--stop"],
                capture_output=True,
                timeout=5,
            )
            self.set_state(State.IDLE)
        except Exception as e:
            print(f"Error canceling: {e}")

    def openConfig_(self, sender):
        config_path = str(CONFIG_FILE)
        if not CONFIG_FILE.exists():
            CONFIG_FILE.parent.mkdir(parents=True, exist_ok=True)
            CONFIG_FILE.write_text('''# Whisper Push Configuration
hotkey = "cmd+shift+space"
language = "auto"
model = "large-v3-turbo"
''')
        subprocess.run(["open", config_path])

    def viewLogs_(self, sender):
        log_path = SUPPORT_DIR / "hotkey.log"
        if log_path.exists():
            subprocess.run(["open", "-a", "Console", str(log_path)])
        else:
            subprocess.run(["open", str(SUPPORT_DIR)])

    def quitApp_(self, sender):
        NSApp.terminate_(None)


def main():
    # Create application
    app = NSApplication.sharedApplication()
    app.setActivationPolicy_(NSApplicationActivationPolicyAccessory)

    # Create and set delegate
    delegate = MenuBarApp.alloc().init()
    app.setDelegate_(delegate)

    # Handle signals
    def signal_handler(signum, frame):
        print("\nShutting down...")
        AppHelper.stopEventLoop()

    signal.signal(signal.SIGTERM, signal_handler)
    signal.signal(signal.SIGINT, signal_handler)

    # Run
    AppHelper.runEventLoop()


if __name__ == "__main__":
    main()
