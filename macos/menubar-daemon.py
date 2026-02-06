#!/usr/bin/env python3
"""
Whisper Push - Menu Bar App for macOS
Shows status icon in menu bar, handles global hotkey, and manages recording.
Model stays loaded in RAM for instant transcription.
"""

import os
import subprocess
import sys
import signal
import tomllib
import threading
import time
import tempfile
from pathlib import Path
from enum import Enum
from queue import Queue

# Audio imports
try:
    import numpy as np
    import sounddevice as sd
    import soundfile as sf
except ImportError:
    print("Error: Audio libraries not installed. Run: pip3 install sounddevice soundfile numpy")
    sys.exit(1)

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
        NSSound,
    )
    from Cocoa import (
        NSEvent,
        NSKeyDownMask,
        NSFlagsChangedMask,
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
    LOADING = "loading"
    RECORDING = "recording"
    PROCESSING = "processing"


# Paths
SUPPORT_DIR = Path.home() / "Library" / "Application Support" / "whisper-push"
CONFIG_FILE = SUPPORT_DIR / "config.toml"
ICONS_DIR = SUPPORT_DIR / "icons"
SOUNDS_DIR = SUPPORT_DIR / "sounds"
AUDIO_FILE = Path(tempfile.gettempdir()) / "whisper-push-recording.wav"

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

# Modifier-only key codes (left/right)
MODIFIER_KEYCODES = {
    'lctrl': 59,
    'rctrl': 62,
    'lshift': 56,
    'rshift': 60,
    'lalt': 58,
    'ralt': 61,
    'lcmd': 55,
    'rcmd': 54,
}

# Global model reference (stays in RAM)
_whisper_model = None
_model_loading = False


def load_config():
    """Load configuration from file."""
    config = {
        "hotkey": "ctrl",
        "hotkey_mode": "hold",  # toggle | hold
        "hold_delay": 0.3,  # seconds to wait before activating (avoids triggering on shortcuts)
        "language": "auto",
        "model": "large-v3-turbo",
        "beam_size": 5,
        "notifications": True,
        "sound_feedback": True,
    }
    if CONFIG_FILE.exists():
        try:
            with open(CONFIG_FILE, "rb") as f:
                user_config = tomllib.load(f)
                config.update(user_config)
        except Exception as e:
            print(f"Warning: Could not load config: {e}")
    return config


def load_whisper_model(model_name: str, callback=None):
    """Load Whisper model into RAM (background thread)."""
    global _whisper_model, _model_loading

    if _whisper_model is not None or _model_loading:
        return

    _model_loading = True
    print(f"Loading Whisper model '{model_name}'...")

    try:
        # Just import mlx_whisper - actual model loading happens on first transcription
        import mlx_whisper

        # Store model name for later use
        _whisper_model = model_name
        _model_loading = False

        print(f"Model '{model_name}' ready! (will load weights on first use)")

        if callback:
            callback()

    except Exception as e:
        print(f"Error loading model: {e}")
        _model_loading = False
        _whisper_model = None


def transcribe_audio(audio_path: str, config: dict) -> str:
    """Transcribe audio file using mlx-whisper."""
    global _whisper_model

    if _whisper_model is None:
        print("Model not loaded yet!")
        return ""

    try:
        import mlx_whisper

        print(f"Transcribing: {audio_path}")

        language = None if config["language"] == "auto" else config["language"]

        result = mlx_whisper.transcribe(
            audio_path,
            path_or_hf_repo=f"mlx-community/whisper-{_whisper_model}",
            language=language,
        )

        text = result.get("text", "").strip()
        print(f"Transcription result: '{text}'")
        return text

    except Exception as e:
        print(f"Transcription error: {e}")
        import traceback
        traceback.print_exc()
        return ""


def parse_hotkey(hotkey_str):
    """Parse hotkey string into modifiers and key code.

    Returns (modifiers, key_code, modifier_keycode)
    - key_code is for non-modifier keys (toggle mode)
    - modifier_keycode is for modifier-only hold mode (lctrl/rctrl/etc)
    """
    parts = hotkey_str.lower().split('+')
    modifiers = 0
    key_code = None
    modifier_keycode = None

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
        elif part in MODIFIER_KEYCODES:
            modifier_keycode = MODIFIER_KEYCODES[part]
            if 'ctrl' in part:
                modifiers |= NSControlKeyMask
            elif 'shift' in part:
                modifiers |= NSShiftKeyMask
            elif 'alt' in part:
                modifiers |= NSAlternateKeyMask
            elif 'cmd' in part:
                modifiers |= NSCommandKeyMask
        elif part in KEY_CODES:
            key_code = KEY_CODES[part]
        else:
            print(f"Warning: Unknown key '{part}' in hotkey")

    return modifiers, key_code, modifier_keycode


def format_hotkey_display(hotkey_str):
    """Format hotkey for display (e.g., 'cmd+shift+space' -> '⌘⇧Space')."""
    symbols = {
        'cmd': '⌘', 'command': '⌘',
        'shift': '⇧',
        'alt': '⌥', 'option': '⌥',
        'ctrl': '⌃', 'control': '⌃',
        'lctrl': '⌃(L)', 'rctrl': '⌃(R)',
        'lshift': '⇧(L)', 'rshift': '⇧(R)',
        'lalt': '⌥(L)', 'ralt': '⌥(R)',
        'lcmd': '⌘(L)', 'rcmd': '⌘(R)',
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


def load_icon(state):
    """Load icon for the given state."""
    icon_files = {
        State.IDLE: ICONS_DIR / "icon-idle.png",
        State.LOADING: ICONS_DIR / "icon-processing.png",
        State.RECORDING: ICONS_DIR / "icon-recording.png",
        State.PROCESSING: ICONS_DIR / "icon-processing.png",
    }

    icon_path = icon_files.get(state)
    if icon_path and icon_path.exists():
        image = NSImage.alloc().initWithContentsOfFile_(str(icon_path))
        if image:
            image.setSize_(NSMakeSize(18, 18))
            return image

    return None


def play_sound(name: str):
    """Play a sound file."""
    sound_file = SOUNDS_DIR / f"{name}.wav"
    if sound_file.exists():
        sound = NSSound.alloc().initWithContentsOfFile_byReference_(str(sound_file), True)
        if sound:
            sound.play()


def paste_text(text: str):
    """Copy text to clipboard and try to paste with Cmd+V."""
    if not text:
        return

    # Copy to clipboard using pasteboard
    from AppKit import NSPasteboard, NSPasteboardTypeString
    pasteboard = NSPasteboard.generalPasteboard()
    pasteboard.clearContents()
    pasteboard.setString_forType_(text, NSPasteboardTypeString)

    # Try to paste with Cmd+V using Quartz CGEvent
    try:
        from Quartz import (
            CGEventCreateKeyboardEvent,
            CGEventSetFlags,
            CGEventPost,
            kCGHIDEventTap,
            kCGEventFlagMaskCommand,
        )

        time.sleep(0.05)

        # Key code for 'v' is 9
        v_keycode = 9

        # Press Cmd+V
        event_down = CGEventCreateKeyboardEvent(None, v_keycode, True)
        if event_down:
            CGEventSetFlags(event_down, kCGEventFlagMaskCommand)
            CGEventPost(kCGHIDEventTap, event_down)

            # Release
            event_up = CGEventCreateKeyboardEvent(None, v_keycode, False)
            CGEventSetFlags(event_up, kCGEventFlagMaskCommand)
            CGEventPost(kCGHIDEventTap, event_up)
            print("Pasted via CGEvent")
        else:
            print("CGEvent failed - text copied to clipboard, press Cmd+V")
    except Exception as e:
        print(f"Paste error: {e} - text copied to clipboard")


def notify(title: str, message: str = ""):
    """Show macOS notification."""
    script = f'display notification "{message}" with title "{title}"'
    subprocess.run(["osascript", "-e", script], capture_output=True)


# Global reference for hotkey handler
_app_instance = None


def _handle_global_hotkey(event):
    """Handle global key events - must be outside NSObject class."""
    global _app_instance
    if _app_instance is None:
        return

    # Cancel any pending hold-to-talk activation (user is pressing a shortcut like Ctrl+C)
    if _app_instance.hold_pending:
        print("Key pressed during hold delay - cancelling hold activation")
        _app_instance.hold_pending = False
        _app_instance.hold_active = False
        if _app_instance._hold_timer is not None:
            _app_instance._hold_timer.cancel()
            _app_instance._hold_timer = None
        return

    # Debug: log all key events
    key_code = event.keyCode()
    modifiers = event.modifierFlags()

    # Only log if modifiers are pressed (to avoid spam)
    if modifiers & (NSCommandKeyMask | NSShiftKeyMask | NSControlKeyMask | NSAlternateKeyMask):
        print(f"Key event: code={key_code}, modifiers={modifiers:#x}, expected_code={_app_instance.key_code}, expected_mods={_app_instance.modifiers:#x}")

    if key_code == _app_instance.key_code:
        if (modifiers & _app_instance.modifiers) == _app_instance.modifiers:
            print("Hotkey matched! Triggering recording...")
            _app_instance.toggle_recording()


def _handle_flags_changed(event):
    """Handle modifier-only hold-to-talk hotkey with activation delay."""
    global _app_instance
    if _app_instance is None or _app_instance.hotkey_mode != "hold":
        return

    key_code = event.keyCode()
    modifiers = event.modifierFlags()

    if _app_instance.hold_keycode is not None and key_code != _app_instance.hold_keycode:
        return

    is_pressed = (modifiers & _app_instance.hold_modifiers) == _app_instance.hold_modifiers

    if is_pressed and not _app_instance.hold_active:
        _app_instance.hold_active = True
        if _app_instance.state == State.IDLE:
            # Start a delayed activation to avoid triggering on keyboard shortcuts
            _app_instance.hold_pending = True
            delay = _app_instance.config.get("hold_delay", 0.3)
            _app_instance._hold_timer = threading.Timer(delay, _app_instance._delayed_start_recording)
            _app_instance._hold_timer.daemon = True
            _app_instance._hold_timer.start()
    elif not is_pressed and _app_instance.hold_active:
        _app_instance.hold_active = False
        # Cancel pending delayed start if still waiting
        if _app_instance.hold_pending:
            _app_instance.hold_pending = False
            if _app_instance._hold_timer is not None:
                _app_instance._hold_timer.cancel()
                _app_instance._hold_timer = None
        elif _app_instance.state == State.RECORDING:
            _app_instance.stop_and_transcribe()


class AudioRecorder:
    """Records audio using sounddevice."""

    def __init__(self, sample_rate=16000):
        self.sample_rate = sample_rate
        self.recording = False
        self.audio_data = []
        self._lock = threading.Lock()

    def start(self):
        """Start recording."""
        with self._lock:
            self.audio_data = []
            self.recording = True

        def callback(indata, frames, time_info, status):
            if self.recording:
                self.audio_data.append(indata.copy())

        self.stream = sd.InputStream(
            samplerate=self.sample_rate,
            channels=1,
            dtype=np.float32,
            callback=callback,
        )
        self.stream.start()

    def stop(self) -> str:
        """Stop recording and save to file."""
        with self._lock:
            self.recording = False

        self.stream.stop()
        self.stream.close()

        if not self.audio_data:
            return None

        audio = np.concatenate(self.audio_data, axis=0)
        sf.write(str(AUDIO_FILE), audio, self.sample_rate)

        return str(AUDIO_FILE)

    def cancel(self):
        """Cancel recording without saving."""
        with self._lock:
            self.recording = False
            self.audio_data = []

        if hasattr(self, 'stream'):
            self.stream.stop()
            self.stream.close()


class MenuBarApp(NSObject):
    """Menu bar application with status icon and hotkey handling."""

    def init(self):
        self = objc.super(MenuBarApp, self).init()
        if self is None:
            return None

        self.config = load_config()
        self.hotkey_mode = self.config.get("hotkey_mode", "toggle").lower()
        self.modifiers, self.key_code, self.hold_keycode = parse_hotkey(
            self.config.get("hotkey", "ctrl+shift+space")
        )
        self.hold_modifiers = self.modifiers
        self.hold_active = False
        self.hold_pending = False
        self._hold_timer = None
        self.state = State.LOADING
        self.recorder = AudioRecorder()

        # Load icons
        self.icons = {
            State.IDLE: load_icon(State.IDLE),
            State.LOADING: load_icon(State.LOADING),
            State.RECORDING: load_icon(State.RECORDING),
            State.PROCESSING: load_icon(State.PROCESSING),
        }

        if self.hotkey_mode == "hold":
            if self.key_code is not None:
                print("Hold mode supports modifier-only hotkeys. Falling back to toggle.")
                self.hotkey_mode = "toggle"
            if self.hotkey_mode == "hold" and self.hold_modifiers == 0:
                print("Error: Invalid hotkey for hold mode")
                sys.exit(1)
            if self.hotkey_mode == "hold" and self.hold_keycode is None:
                print("Warning: Hold mode with generic modifier may conflict with other shortcuts.")
                print("Tip: use 'rctrl' or another right-side modifier for fewer conflicts.")
        if self.hotkey_mode == "toggle" and self.key_code is None:
            print("Error: Invalid hotkey configuration")
            sys.exit(1)

        # Create status bar item
        self.status_bar = NSStatusBar.systemStatusBar()
        self.status_item = self.status_bar.statusItemWithLength_(NSVariableStatusItemLength)

        # Set initial icon (loading state)
        self.update_icon()

        # Create menu
        self.menu = NSMenu.alloc().init()
        self.setup_menu()
        self.status_item.setMenu_(self.menu)

        # Set up global hotkey monitor
        global _app_instance
        _app_instance = self
        NSEvent.addGlobalMonitorForEventsMatchingMask_handler_(
            NSKeyDownMask,
            _handle_global_hotkey
        )
        NSEvent.addGlobalMonitorForEventsMatchingMask_handler_(
            NSFlagsChangedMask,
            _handle_flags_changed
        )

        hotkey_display = format_hotkey_display(self.config.get("hotkey", "ctrl+shift+space"))
        if self.hotkey_mode == "hold":
            hotkey_display = f"Hold {hotkey_display}"
        print(f"Menu bar app starting. Hotkey: {hotkey_display}")

        # Load model in background thread
        def on_model_loaded():
            self.set_state(State.IDLE)
            if self.config.get("notifications"):
                notify("Whisper Push", "Model loaded and ready!")

        threading.Thread(
            target=load_whisper_model,
            args=(self.config.get("model", "large-v3-turbo"), on_model_loaded),
            daemon=True
        ).start()

        return self

    def setup_menu(self):
        """Set up the dropdown menu."""
        self.menu.removeAllItems()

        hotkey_display = format_hotkey_display(self.config.get("hotkey", "ctrl+shift+space"))
        if self.hotkey_mode == "hold":
            hotkey_display = f"Hold {hotkey_display}"
        status_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            f"Whisper Push ({hotkey_display})",
            None,
            ""
        )
        status_item.setEnabled_(False)
        self.menu.addItem_(status_item)

        self.menu.addItem_(NSMenuItem.separatorItem())

        self.toggle_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            "Loading model...",
            "toggleRecordingMenu:",
            ""
        )
        self.toggle_item.setTarget_(self)
        self.toggle_item.setEnabled_(False)
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
            fallback = {
                State.IDLE: "◆",
                State.LOADING: "◐",
                State.RECORDING: "●",
                State.PROCESSING: "◐",
            }
            self.status_item.setImage_(None)
            self.status_item.setTitle_(fallback.get(self.state, "◆"))

        if hasattr(self, 'toggle_item'):
            if self.state == State.LOADING:
                self.toggle_item.setTitle_("Loading model...")
                self.toggle_item.setEnabled_(False)
                self.cancel_item.setHidden_(True)
            elif self.state == State.IDLE:
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

    def toggle_recording(self):
        """Toggle recording on/off."""
        if self.state == State.LOADING:
            print("Model still loading, please wait...")
            return

        if self.state == State.PROCESSING:
            return

        if self.state == State.IDLE:
            self.start_recording()
        elif self.state == State.RECORDING:
            self.stop_and_transcribe()

    def _delayed_start_recording(self):
        """Called after hold delay expires - start recording if still holding."""
        if self.hold_pending and self.hold_active and self.state == State.IDLE:
            self.hold_pending = False
            self._hold_timer = None
            print("Hold delay passed - starting recording")
            self.start_recording()
        else:
            self.hold_pending = False
            self._hold_timer = None

    def start_recording(self):
        """Start audio recording."""
        self.set_state(State.RECORDING)

        if self.config.get("sound_feedback"):
            play_sound("start")

        self.recorder.start()
        print("Recording started...")

    def stop_and_transcribe(self):
        """Stop recording and transcribe."""
        self.set_state(State.PROCESSING)

        if self.config.get("sound_feedback"):
            play_sound("stop")

        # Stop recording
        audio_path = self.recorder.stop()

        if not audio_path:
            print("No audio recorded")
            self.set_state(State.IDLE)
            return

        print("Recording stopped, transcribing...")

        # Transcribe in background thread
        def do_transcribe():
            text = transcribe_audio(audio_path, self.config)

            if text:
                print(f"Transcribed: {text}")
                paste_text(text)
                if self.config.get("notifications"):
                    notify("Whisper Push", f"Transcribed {len(text)} characters")
            else:
                print("No transcription result")
                if self.config.get("notifications"):
                    notify("Whisper Push", "No speech detected")

            self.set_state(State.IDLE)

        threading.Thread(target=do_transcribe, daemon=True).start()

    def toggleRecordingMenu_(self, sender):
        self.toggle_recording()

    def cancelRecording_(self, sender):
        if self.state == State.RECORDING:
            self.recorder.cancel()
            self.set_state(State.IDLE)
            print("Recording cancelled")

    def openConfig_(self, sender):
        config_path = str(CONFIG_FILE)
        if not CONFIG_FILE.exists():
            CONFIG_FILE.parent.mkdir(parents=True, exist_ok=True)
            CONFIG_FILE.write_text('''# Whisper Push Configuration
hotkey_mode = "hold"
hotkey = "ctrl"
language = "auto"
model = "large-v3-turbo"
''')
        subprocess.run(["open", config_path])

    def viewLogs_(self, sender):
        log_path = SUPPORT_DIR / "daemon.log"
        if log_path.exists():
            subprocess.run(["open", "-a", "Console", str(log_path)])
        else:
            subprocess.run(["open", str(SUPPORT_DIR)])

    def quitApp_(self, sender):
        NSApp.terminate_(None)


def check_accessibility_permission():
    """Check and request accessibility permission - will show system prompt if needed."""
    import ctypes
    from Foundation import NSMutableDictionary, NSNumber
    import objc

    try:
        # Load HIServices framework
        hi_services = ctypes.cdll.LoadLibrary(
            '/System/Library/Frameworks/ApplicationServices.framework/Versions/A/Frameworks/HIServices.framework/Versions/A/HIServices'
        )

        # Setup function
        AXIsProcessTrustedWithOptions = hi_services.AXIsProcessTrustedWithOptions
        AXIsProcessTrustedWithOptions.restype = ctypes.c_bool
        AXIsProcessTrustedWithOptions.argtypes = [ctypes.c_void_p]

        # Create options with prompt=True (this triggers the system dialog!)
        options = NSMutableDictionary.alloc().init()
        options.setObject_forKey_(NSNumber.numberWithBool_(True), "AXTrustedCheckOptionPrompt")

        # Call - this will show permission dialog if not trusted
        result = AXIsProcessTrustedWithOptions(objc.pyobjc_id(options))

        if result:
            print("Accessibility permission: GRANTED")
        else:
            print("Accessibility permission: NOT GRANTED - dialog shown to user")
            notify("Whisper Push", "Please grant Accessibility permission, then restart the app")

        return result

    except Exception as e:
        print(f"Accessibility check error: {e}")
        return False


def main():
    # Check accessibility permission first (will prompt user if needed)
    check_accessibility_permission()

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
