#!/usr/bin/env python3
"""
Whisper Push - Menu Bar App for macOS
Shows status icon in menu bar, handles global hotkey, and manages recording.
Model stays loaded in RAM for instant transcription.
"""

import subprocess
import sys
import os
import shutil
import signal
import fcntl
import tomllib
import threading
import time
import logging
from logging.handlers import RotatingFileHandler
from pathlib import Path
from enum import Enum

# Application logger. Owns the rotating daemon.log file (the launcher redirects
# native stdout/stderr to a separate crash sink). Per-keystroke traces are at
# DEBUG level and off by default.
logger = logging.getLogger("whisper_push")


def setup_logging(debug: bool = False):
    """Configure rotating file logging. Idempotent."""
    logger.setLevel(logging.DEBUG if debug else logging.INFO)
    logger.propagate = False
    if logger.handlers:
        return
    SUPPORT_DIR.mkdir(parents=True, exist_ok=True)
    handler = RotatingFileHandler(LOG_FILE, maxBytes=2_000_000, backupCount=3)
    handler.setFormatter(logging.Formatter(
        "%(asctime)s [%(levelname)s] %(message)s", datefmt="%Y-%m-%d %H:%M:%S"
    ))
    logger.addHandler(handler)

# Audio imports
try:
    import numpy as np
    import sounddevice as sd
    import soundfile as sf
except ImportError:
    print("Error: Audio libraries not installed. Run: pip3 install sounddevice soundfile numpy", file=sys.stderr)
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
        NSOnState,
        NSOffState,
        NSAlert,
        NSAlertFirstButtonReturn,
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
        NSMakeSize,
    )
    from PyObjCTools import AppHelper
except ImportError:
    print("Error: PyObjC not installed. Run: pip3 install pyobjc-framework-Cocoa pyobjc-framework-Quartz", file=sys.stderr)
    sys.exit(1)


class State(Enum):
    IDLE = "idle"
    LOADING = "loading"
    RECORDING = "recording"
    PROCESSING = "processing"


# Paths
# User data (writable) always lives in Application Support.
SUPPORT_DIR = Path.home() / "Library" / "Application Support" / "whisper-push"
CONFIG_FILE = SUPPORT_DIR / "config.toml"
LOG_FILE = SUPPORT_DIR / "daemon.log"
LOCK_FILE = SUPPORT_DIR / "daemon.lock"

# Keep the downloaded model inside our own folder (under SUPPORT_DIR) instead of
# the shared ~/.cache/huggingface, so "Uninstall" only has to remove SUPPORT_DIR.
MODELS_DIR = SUPPORT_DIR / "models"
os.environ.setdefault("HF_HOME", str(MODELS_DIR))

# Read-only resources: bundled inside the .app when frozen (PyInstaller), else
# installed into SUPPORT_DIR by install.sh.
if getattr(sys, "frozen", False):
    RESOURCE_DIR = Path(getattr(sys, "_MEIPASS", Path(sys.executable).parent))
else:
    RESOURCE_DIR = SUPPORT_DIR
ICONS_DIR = RESOURCE_DIR / "icons"
SOUNDS_DIR = RESOURCE_DIR / "sounds"


def _app_bundle_path():
    """Path to the enclosing .app bundle when frozen (PyInstaller), else None."""
    if not getattr(sys, "frozen", False):
        return None
    for parent in Path(sys.executable).parents:
        if parent.suffix == ".app":
            return str(parent)
    return None

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

# Global model reference (stays in RAM). Holds a parakeet-mlx model instance.
_asr_model = None
_model_name = None  # repo short name, kept so we can reload after an idle unload
_model_loading = False
# MLX/Metal is not safe for concurrent GPU work across threads. Serialize every
# model inference (transcribe / warmup / unload) through this lock.
_model_lock = threading.Lock()

# Bound the MLX GPU buffer cache so idle footprint stays ~weights-only (~1.3GB).
# Clearing/limiting this cache has no measured latency cost on the next call.
GPU_CACHE_LIMIT_BYTES = 256 * 1024 * 1024  # 256 MB

# Settings exposed in the menu bar (label, value). All write the portable
# config.toml so the Linux/Windows daemons read the same settings.
IDLE_PRESETS = [("Never", 0), ("After 5 min", 5), ("After 15 min", 15), ("After 30 min", 30)]
# (label, hotkey, mode) -- each preset pairs a key with a compatible mode.
HOTKEY_PRESETS = [
    ("Hold — Control", "ctrl", "hold"),
    ("Hold — Right Control", "rctrl", "hold"),
    ("Hold — Right Command", "rcmd", "hold"),
    ("Hold — Right Option", "ralt", "hold"),
    ("Toggle — ⌘⇧Space", "cmd+shift+space", "toggle"),
    ("Toggle — ⌃⇧Space", "ctrl+shift+space", "toggle"),
]
BOOL_SETTINGS = [
    ("Notifications", "notifications"),
    ("Sound feedback", "sound_feedback"),
    ("Debug logging", "debug"),
]


def load_config():
    """Load configuration from file."""
    config = {
        "hotkey": "ctrl",
        "hotkey_mode": "hold",  # toggle | hold
        "hold_delay": 0.15,  # confirm window before committing (audio is pre-rolled, not lost)
        "language": "auto",  # Parakeet v3 auto-detects among 25 European languages
        "model": "parakeet-tdt-0.6b-v3",
        "idle_unload_minutes": 0,  # free the model after N min idle (0 = always resident)
        "debug": False,  # verbose per-keystroke logging
        "notifications": True,
        "sound_feedback": True,
        "input_device": "auto",
        "output_device": "auto",
    }
    if CONFIG_FILE.exists():
        try:
            with open(CONFIG_FILE, "rb") as f:
                user_config = tomllib.load(f)
                config.update(user_config)
        except Exception as e:
            logger.warning(f"Warning: Could not load config: {e}")
    return config


def render_config(config: dict) -> str:
    """Render a fully self-documenting config.toml from the current values.

    Every option is written with its valid values as a comment, so the file is
    a living reference. Internal keys (leading underscore) are never written.
    """
    def s(key):  # quoted string value
        return f'"{config.get(key, "")}"'

    def n(key):  # numeric value
        return config.get(key, 0)

    def b(key):  # bool value
        return "true" if config.get(key) else "false"

    return f'''# Whisper Push Configuration
# Use the menu bar icon (changes apply live and rewrite this file, keeping these
# comments), or edit here directly -- direct edits apply on the next restart.

# --- Hotkey ---
# Keys: a-z, 0-9, space, return, tab, escape, f1-f12
# Modifiers: ctrl, shift, alt (option), cmd  |  left/right: lctrl, rctrl, lcmd, rcmd, lalt, ralt
# Examples: "ctrl"  "rctrl"  "cmd+shift+space"
hotkey = {s("hotkey")}

# "hold"   = push-to-talk: hold the hotkey while speaking (modifier-only keys OK)
# "toggle" = press once to start, again to stop (needs a real key, e.g. cmd+shift+space)
hotkey_mode = {s("hotkey_mode")}

# Confirm window (seconds). Audio is captured the instant you press (pre-roll),
# so nothing is lost -- this only filters quick taps / shortcuts before showing
# the recording state. 0 = instant feedback (ideal with a dedicated key like rctrl).
hold_delay = {n("hold_delay")}

# --- Transcription ---
# Language: Parakeet v3 auto-detects 25 European languages. Leave as "auto".
language = {s("language")}

# Model (mlx-community Parakeet). Stays warm in RAM for instant transcription.
model = {s("model")}

# Free the model from RAM (~1.3GB) after N minutes idle (0 = always resident).
# It reloads while you record, so the reload time stays hidden.
idle_unload_minutes = {n("idle_unload_minutes")}

# --- Audio devices ---
# "auto" or an exact device name. Easiest: pick from the menu bar submenus.
input_device = {s("input_device")}
output_device = {s("output_device")}

# --- Feedback ---
notifications = {b("notifications")}     # macOS notification after each transcription
sound_feedback = {b("sound_feedback")}    # start/stop sounds

# --- Debugging ---
debug = {b("debug")}             # verbose per-keystroke logging in daemon.log
'''


def save_config(config: dict):
    """Persist the config as a self-documenting TOML (internal keys excluded)."""
    try:
        CONFIG_FILE.write_text(render_config(config))
        logger.info(f"Config saved to {CONFIG_FILE}")
    except Exception as e:
        logger.error(f"Error saving config: {e}")


def load_asr_model(model_name: str, callback=None):
    """Load the Parakeet ASR model into RAM and warm it up (background thread).

    We never touch ffmpeg or the filesystem: weights are loaded from the HF
    cache and warmed up with an in-memory silent buffer. This is what keeps
    transcription instant and avoids the GUI-PATH ffmpeg crash.
    """
    global _asr_model, _model_name, _model_loading

    if _asr_model is not None or _model_loading:
        return

    _model_loading = True
    _model_name = model_name
    repo = f"mlx-community/{model_name}"
    logger.info(f"Loading ASR model '{repo}'...")

    try:
        import mlx.core as mx
        from parakeet_mlx import from_pretrained

        mx.set_cache_limit(GPU_CACHE_LIMIT_BYTES)
        model = from_pretrained(repo)

        # Warm up (compile Metal kernels + page weights in) BEFORE publishing the
        # model, so wait_for_model() only returns once it's fully ready -- this
        # prevents a transcribe thread from racing the warmup on the GPU.
        _warm(model)

        _asr_model = model
        _model_loading = False
        logger.info(f"Model '{repo}' loaded and warm in GPU memory!")

        if callback:
            callback()

    except Exception as e:
        logger.error(f"Error loading model: {e}")
        import traceback
        traceback.print_exc()
        _model_loading = False
        _asr_model = None


def _warm(model):
    """Run one inference on in-memory silence. Serialized via _model_lock."""
    import mlx.core as mx
    from parakeet_mlx.audio import get_logmel
    with _model_lock:
        silent = mx.zeros((1, 16000), dtype=mx.float32)
        mel = get_logmel(silent, model.preprocessor_config)
        mx.eval(model.generate(mel))


def warm_model():
    """Re-warm the resident model (used after wake-from-sleep)."""
    model = _asr_model
    if model is None:
        return
    try:
        _warm(model)
    except Exception as e:
        logger.warning(f"Warmup error: {e}")


def unload_asr_model():
    """Free the model and reclaim GPU memory (called after an idle period)."""
    global _asr_model
    if _asr_model is None:
        return
    with _model_lock:
        _asr_model = None
        try:
            import mlx.core as mx
            mx.clear_cache()
        except Exception:
            pass
    logger.info("Model unloaded to free memory (idle)")


def ensure_model_loading():
    """Kick off a background load if the model isn't resident. Non-blocking.

    Safe to call from the hotkey path: recording starts immediately while the
    model loads in parallel, so an idle unload costs nothing perceptible.
    """
    if _asr_model is None and not _model_loading and _model_name:
        threading.Thread(
            target=load_asr_model, args=(_model_name,), daemon=True
        ).start()


def wait_for_model(timeout=15.0):
    """Block until the model is resident (or timeout). Used right before transcribe."""
    deadline = time.time() + timeout
    while _asr_model is None and time.time() < deadline:
        time.sleep(0.05)
    return _asr_model is not None


def transcribe_audio(audio: "np.ndarray") -> str:
    """Transcribe an in-memory 16kHz mono float32 array with Parakeet.

    No file IO, no ffmpeg: the recorded samples go straight to the model.
    Language is auto-detected (Parakeet v3 covers 25 European languages).
    """
    model = _asr_model  # snapshot: another thread may unload it
    if model is None:
        logger.info("Model not loaded yet!")
        return ""
    if audio is None or len(audio) == 0:
        return ""

    try:
        import mlx.core as mx
        from parakeet_mlx.audio import get_logmel

        with _model_lock:
            mel = get_logmel(mx.array(audio)[None], model.preprocessor_config)
            results = model.generate(mel)
            text = (results[0].text if results else "").strip()
            # Release the GPU buffer cache so idle footprint drops back to
            # weights only. Verified to have no latency cost on the next call.
            mx.clear_cache()
        logger.info(f"Transcription result: '{text}'")
        return text

    except Exception as e:
        logger.error(f"Transcription error: {e}")
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
            logger.warning(f"Warning: Unknown key '{part}' in hotkey")

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


# Pre-loaded sound buffers (filled at startup)
_sound_cache = {}

def preload_sounds():
    """Load sound files into RAM for instant playback."""
    for name in ("start", "stop"):
        sound_file = SOUNDS_DIR / f"{name}.wav"
        if sound_file.exists():
            try:
                data, sr = sf.read(str(sound_file), dtype='float32')
                _sound_cache[name] = (data, sr)
            except Exception as e:
                logger.warning(f"Failed to preload {name}: {e}")


def play_sound(name: str, config=None):
    """Play a pre-loaded sound non-blocking via sounddevice."""
    cached = _sound_cache.get(name)
    if cached is None:
        return
    try:
        data, sample_rate = cached
        output_device = None
        if config:
            configured = config.get("output_device", "auto")
            if configured != "auto":
                output_device = config.get("_output_device_idx")
        sd.play(data, samplerate=sample_rate, device=output_device)
    except Exception as e:
        logger.warning(f"Sound error: {e}")


def paste_text(text: str):
    """Copy text to clipboard and paste with Cmd+V into the focused input."""
    if not text:
        return

    # Copy to clipboard, snapshotting the user's current clipboard so we can
    # restore it afterwards (a dictation tool shouldn't clobber the clipboard).
    from AppKit import NSPasteboard, NSPasteboardTypeString
    pasteboard = NSPasteboard.generalPasteboard()
    saved_items = _snapshot_pasteboard(pasteboard)
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
            kCGEventSourceStateHIDSystemState,
            CGEventSourceCreate,
        )

        # Wait for clipboard to be ready and any modifier keys to be fully released
        time.sleep(0.15)

        # Create a dedicated event source for clean modifier state
        source = CGEventSourceCreate(kCGEventSourceStateHIDSystemState)

        # Key code for 'v' is 9
        v_keycode = 9

        # Press Cmd+V
        event_down = CGEventCreateKeyboardEvent(source, v_keycode, True)
        if event_down:
            # Set ONLY Command flag (clear any lingering ctrl/shift/alt)
            CGEventSetFlags(event_down, kCGEventFlagMaskCommand)
            CGEventPost(kCGHIDEventTap, event_down)

            # Small delay between press and release for apps to register
            time.sleep(0.05)

            # Release
            event_up = CGEventCreateKeyboardEvent(source, v_keycode, False)
            CGEventSetFlags(event_up, 0)
            CGEventPost(kCGHIDEventTap, event_up)
            logger.info("Pasted via CGEvent")
        else:
            logger.warning("CGEvent failed - text copied to clipboard, press Cmd+V")
    except Exception as e:
        logger.warning(f"Paste error: {e} - text copied to clipboard")
    finally:
        _restore_pasteboard_later(saved_items)


def _snapshot_pasteboard(pasteboard):
    """Copy every item/type currently on the pasteboard so it can be restored."""
    try:
        from AppKit import NSPasteboardItem
        items = []
        for item in pasteboard.pasteboardItems() or []:
            copy = NSPasteboardItem.alloc().init()
            for t in item.types():
                data = item.dataForType_(t)
                if data is not None:
                    copy.setData_forType_(data, t)
            items.append(copy)
        return items
    except Exception as e:
        logger.warning(f"Could not snapshot clipboard: {e}")
        return None


def _restore_pasteboard_later(saved_items, delay=0.3):
    """Restore the snapshotted clipboard after the paste has been consumed."""
    if not saved_items:
        return

    def restore():
        time.sleep(delay)
        try:
            from AppKit import NSPasteboard
            pb = NSPasteboard.generalPasteboard()
            pb.clearContents()
            pb.writeObjects_(saved_items)
        except Exception as e:
            logger.warning(f"Could not restore clipboard: {e}")

    threading.Thread(target=restore, daemon=True).start()


def notify(title: str, message: str = ""):
    """Show macOS notification."""
    script = f'display notification "{message}" with title "{title}"'
    subprocess.run(["osascript", "-e", script], capture_output=True)


# Global reference for hotkey handler
_app_instance = None

# Set by the SIGTERM/SIGINT handler; polled on the main thread by an NSTimer.
_shutdown_requested = False


def _request_shutdown(signum, frame):
    """Signal handler: just flag it. A plain handler can't safely touch Cocoa,
    and can't act while the run loop is blocked -- the NSTimer polls this flag."""
    global _shutdown_requested
    _shutdown_requested = True


def _handle_global_hotkey(event):
    """Handle global key events - must be outside NSObject class."""
    if _app_instance is None:
        return

    # Another key during the delay -> it's a shortcut (e.g. Ctrl+C). Discard.
    if _app_instance.hold_pending:
        logger.debug("Key pressed during hold delay - discarding pre-roll")
        _app_instance.hold_pending = False
        _app_instance.hold_active = False
        if _app_instance._hold_timer is not None:
            _app_instance._hold_timer.cancel()
            _app_instance._hold_timer = None
        _app_instance._discard_capture()
        return

    key_code = event.keyCode()
    modifiers = event.modifierFlags()

    if modifiers & (NSCommandKeyMask | NSShiftKeyMask | NSControlKeyMask | NSAlternateKeyMask):
        logger.debug(f"Key event: code={key_code}, modifiers={modifiers:#x}, expected_code={_app_instance.key_code}, expected_mods={_app_instance.modifiers:#x}")

    if key_code == _app_instance.key_code:
        if (modifiers & _app_instance.modifiers) == _app_instance.modifiers:
            logger.info("Hotkey matched! Triggering recording...")
            _app_instance.toggle_recording()


def _handle_flags_changed(event):
    """Handle modifier-only hold-to-talk hotkey with activation delay."""
    if _app_instance is None or _app_instance.hotkey_mode != "hold":
        return

    key_code = event.keyCode()
    modifiers = event.modifierFlags()

    if _app_instance.hold_keycode is not None and key_code != _app_instance.hold_keycode:
        return

    is_pressed = (modifiers & _app_instance.hold_modifiers) == _app_instance.hold_modifiers
    logger.debug(f"Flags changed: keycode={key_code}, mods={modifiers:#x}, is_pressed={is_pressed}, hold_active={_app_instance.hold_active}, state={_app_instance.state}")

    if is_pressed and not _app_instance.hold_active:
        _app_instance.hold_active = True
        if _app_instance.state == State.IDLE:
            delay = _app_instance.config.get("hold_delay", 0.15)
            if delay <= 0:
                # Instant: commit immediately (best with a dedicated key).
                _app_instance.start_recording()
            else:
                # Pre-roll: start capturing NOW so no speech is lost. The delay
                # only gates commit (real hold) vs discard (shortcut/quick tap).
                _app_instance.hold_pending = True
                _app_instance._begin_capture()
                _app_instance._hold_timer = threading.Timer(delay, _app_instance._delayed_start_recording)
                _app_instance._hold_timer.daemon = True
                _app_instance._hold_timer.start()
    elif not is_pressed and _app_instance.hold_active:
        _app_instance.hold_active = False
        if _app_instance.hold_pending:
            logger.debug("Released during hold delay - discarding pre-roll")
            _app_instance.hold_pending = False
            if _app_instance._hold_timer is not None:
                _app_instance._hold_timer.cancel()
                _app_instance._hold_timer = None
            _app_instance._discard_capture()
        elif _app_instance.state == State.RECORDING:
            # Kill audio stream + grab the buffer IMMEDIATELY in this callback
            _app_instance.recorder.recording = False
            _app_instance.recorder.stream.abort()
            _app_instance.recorder.stream.close()
            audio = _app_instance.recorder.get_audio()
            logger.info("Released - stream killed and audio captured immediately")
            _app_instance.stop_and_transcribe(audio)
        else:
            logger.debug(f"Released but state is {_app_instance.state}, not RECORDING")


def find_input_device():
    """Find the best input device (prefer built-in mic, avoid virtual/external display devices)."""
    devices = sd.query_devices()
    best = None
    for i, d in enumerate(devices):
        if d['max_input_channels'] < 1:
            continue
        name = d['name'].lower()
        # Skip virtual audio devices
        if 'teams' in name or 'zoom' in name or 'virtual' in name:
            continue
        # Prefer built-in MacBook mic
        if 'macbook' in name or 'built-in' in name:
            return i
        if best is None:
            best = i
    return best


def find_device_by_name(name, kind='input'):
    """Find a device index by name. kind is 'input' or 'output'."""
    devices = sd.query_devices()
    channel_key = 'max_input_channels' if kind == 'input' else 'max_output_channels'
    for i, d in enumerate(devices):
        if d[channel_key] < 1:
            continue
        if d['name'] == name:
            return i
    logger.warning(f"Warning: device '{name}' not found, falling back to default")
    return None


class AudioRecorder:
    """Records audio using sounddevice."""

    def __init__(self, config=None, target_sample_rate=16000):
        self.config = config or {}
        self.target_sample_rate = target_sample_rate
        self.recording = False
        self.audio_data = []
        self._lock = threading.Lock()

    def start(self):
        """Start recording."""
        with self._lock:
            self.audio_data = []
            self.recording = True

        # Resolve input device from config
        configured = self.config.get("input_device", "auto")
        if configured == "auto":
            input_device = find_input_device()
        else:
            input_device = find_device_by_name(configured, kind='input')
            if input_device is None:
                input_device = find_input_device()  # fallback

        if input_device is not None:
            dev_info = sd.query_devices(input_device)
            self.device_sample_rate = int(dev_info['default_samplerate'])
            logger.info(f"Using input device: {dev_info['name']} @ {self.device_sample_rate}Hz")
        else:
            input_device = None  # use system default
            self.device_sample_rate = self.target_sample_rate
            logger.info("Using default input device")

        def callback(indata, frames, time_info, status):
            if self.recording:
                self.audio_data.append(indata.copy())

        self.stream = sd.InputStream(
            device=input_device,
            samplerate=self.device_sample_rate,
            channels=1,
            dtype=np.float32,
            callback=callback,
        )
        self.stream.start()

    def stop_array(self) -> "np.ndarray":
        """Stop recording and return the recorded audio as a 16kHz mono array."""
        self.recording = False
        if hasattr(self, 'stream'):
            self.stream.abort()
            self.stream.close()
        return self.get_audio()

    def get_audio(self) -> "np.ndarray":
        """Return the recorded buffer as a 16kHz mono float32 array (no file IO)."""
        if not self.audio_data:
            return None

        audio = np.concatenate(self.audio_data, axis=0).reshape(-1).astype(np.float32)
        sample_rate = getattr(self, 'device_sample_rate', self.target_sample_rate)

        # Resample to 16kHz if recorded at a different rate (the model expects 16kHz)
        if sample_rate != self.target_sample_rate:
            import scipy.signal
            num_samples = int(len(audio) * self.target_sample_rate / sample_rate)
            audio = scipy.signal.resample(audio, num_samples).astype(np.float32)
            logger.info(f"Resampled {sample_rate}Hz -> {self.target_sample_rate}Hz")

        logger.info(f"Captured audio ({len(audio)/self.target_sample_rate:.1f}s)")
        return audio

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
        preload_sounds()
        self.hotkey_mode = self.config.get("hotkey_mode", "toggle").lower()
        self.modifiers, self.key_code, self.hold_keycode = parse_hotkey(
            self.config.get("hotkey", "ctrl+shift+space")
        )
        self.hold_modifiers = self.modifiers
        self.hold_active = False
        self.hold_pending = False
        self._hold_timer = None
        self.state = State.LOADING
        self.recorder = AudioRecorder(config=self.config)
        self._resolve_output_device_idx()

        # Load icons
        self.icons = {
            State.IDLE: load_icon(State.IDLE),
            State.LOADING: load_icon(State.LOADING),
            State.RECORDING: load_icon(State.RECORDING),
            State.PROCESSING: load_icon(State.PROCESSING),
        }

        if self.hotkey_mode == "hold":
            if self.key_code is not None:
                logger.info("Hold mode supports modifier-only hotkeys. Falling back to toggle.")
                self.hotkey_mode = "toggle"
            if self.hotkey_mode == "hold" and self.hold_modifiers == 0:
                logger.info("Error: Invalid hotkey for hold mode")
                sys.exit(1)
            if self.hotkey_mode == "hold" and self.hold_keycode is None:
                logger.warning("Warning: Hold mode with generic modifier may conflict with other shortcuts.")
                logger.info("Tip: use 'rctrl' or another right-side modifier for fewer conflicts.")
        if self.hotkey_mode == "toggle" and self.key_code is None:
            logger.info("Error: Invalid hotkey configuration")
            sys.exit(1)

        # Create status bar item
        self.status_bar = NSStatusBar.systemStatusBar()
        self.status_item = self.status_bar.statusItemWithLength_(NSVariableStatusItemLength)

        # Set initial icon (loading state)
        self.update_icon()

        # Create menu (refs populated by setup_menu, used by the open-refresh)
        self._title_item = None
        self._hotkey_submenu = None
        self._choice_submenus = []   # [(submenu, config_key)]
        self._bool_items = []        # [item]
        self.menu = NSMenu.alloc().init()
        self.setup_menu()
        self.status_item.setMenu_(self.menu)
        self.menu.setDelegate_(self)

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
        logger.info(f"Menu bar app starting. Hotkey: {hotkey_display}")

        # Re-warm the model when the Mac wakes from sleep (its pages may have
        # been compressed/swapped while asleep -> first call would be slow).
        self._idle_timer = None
        self._register_wake_observer()

        # Poll for shutdown requests: ticking the run loop lets the Python
        # signal handler run and lets us terminate cleanly on the main thread.
        NSTimer.scheduledTimerWithTimeInterval_target_selector_userInfo_repeats_(
            0.5, self, "checkShutdown:", None, True
        )

        # Load model in background thread
        def on_model_loaded():
            self.set_state(State.IDLE)
            if self.config.get("notifications"):
                notify("Whisper Push", "Model loaded and ready!")

        threading.Thread(
            target=load_asr_model,
            args=(self.config.get("model", "parakeet-tdt-0.6b-v3"), on_model_loaded),
            daemon=True
        ).start()

        return self

    # --- Model lifecycle / resource management ---

    def _register_wake_observer(self):
        """Observe system wake so we can re-warm the model in the background."""
        try:
            from AppKit import NSWorkspace, NSWorkspaceDidWakeNotification
            nc = NSWorkspace.sharedWorkspace().notificationCenter()
            nc.addObserver_selector_name_object_(
                self, "systemDidWake:", NSWorkspaceDidWakeNotification, None
            )
        except Exception as e:
            logger.warning(f"Could not register wake observer: {e}")

    def systemDidWake_(self, notification):
        logger.info("System woke from sleep - re-warming model")
        def rewarm():
            if _asr_model is not None:
                warm_model()
                logger.info("Re-warm after wake done")
        threading.Thread(target=rewarm, daemon=True).start()
        self._schedule_idle_unload()

    def _schedule_idle_unload(self):
        """(Re)arm the idle-unload timer based on config. Debounced on activity."""
        if self._idle_timer is not None:
            self._idle_timer.cancel()
            self._idle_timer = None
        minutes = self.config.get("idle_unload_minutes", 0) or 0
        if minutes <= 0:
            return  # 0 = always resident
        self._idle_timer = threading.Timer(minutes * 60, self._idle_unload_fired)
        self._idle_timer.daemon = True
        self._idle_timer.start()

    def _idle_unload_fired(self):
        # Only unload if truly idle (not mid-recording/processing).
        if self.state == State.IDLE:
            unload_asr_model()

    def checkShutdown_(self, timer):
        """Terminate cleanly (removes the menu-bar icon) when a signal arrived."""
        if _shutdown_requested:
            logger.info("Shutdown requested, terminating")
            NSApp.terminate_(None)

    # --- Settings menu (all write the portable config.toml) ---

    def _status_title(self):
        disp = format_hotkey_display(self.config.get("hotkey", "ctrl"))
        if self.hotkey_mode == "hold":
            disp = f"Hold {disp}"
        return f"Whisper Push ({disp})"

    def _add_bool_item(self, menu, title, key):
        item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(title, "toggleSetting:", "")
        item.setTarget_(self)
        item.setRepresentedObject_(key)
        item.setState_(NSOnState if self.config.get(key) else NSOffState)
        menu.addItem_(item)
        self._bool_items.append(item)

    def toggleSetting_(self, sender):
        key = sender.representedObject()
        self.config[key] = not bool(self.config.get(key))
        save_config(self.config)
        sender.setState_(NSOnState if self.config[key] else NSOffState)
        if key == "debug":
            logger.setLevel(logging.DEBUG if self.config["debug"] else logging.INFO)
        logger.info(f"Setting '{key}' = {self.config[key]}")

    def _add_choice_submenu(self, title, key, choices):
        """Add a radio-style submenu (label, value) bound to a config key."""
        parent = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(title, None, "")
        submenu = NSMenu.alloc().init()
        current = self.config.get(key)
        for label, value in choices:
            item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(label, "selectChoice:", "")
            item.setTarget_(self)
            item.setRepresentedObject_([key, value])
            item.setState_(NSOnState if current == value else NSOffState)
            submenu.addItem_(item)
        parent.setSubmenu_(submenu)
        self.menu.addItem_(parent)
        self._choice_submenus.append((submenu, key))

    def selectChoice_(self, sender):
        ro = sender.representedObject()
        key, raw = str(ro[0]), ro[1]
        value = int(raw) if str(raw).lstrip("-").isdigit() else str(raw)
        if self.config.get(key) != value:
            self.config[key] = value
            save_config(self.config)
            if key == "idle_unload_minutes":
                self._schedule_idle_unload()
            logger.info(f"Setting '{key}' = {value}")
        for it in sender.menu().itemArray():  # move the radio checkmark
            it.setState_(NSOffState)
        sender.setState_(NSOnState)

    def _add_hotkey_submenu(self):
        """Submenu of hotkey presets (each implies a mode). Changing it re-parses
        and re-binds live -- no restart needed."""
        parent = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_("Hotkey", None, "")
        self._hotkey_submenu = NSMenu.alloc().init()
        self._fill_hotkey_submenu()
        parent.setSubmenu_(self._hotkey_submenu)
        self.menu.addItem_(parent)

    def _fill_hotkey_submenu(self):
        sub = self._hotkey_submenu
        sub.removeAllItems()
        h, m = self.config.get("hotkey"), self.config.get("hotkey_mode")
        for label, hotkey, mode in HOTKEY_PRESETS:
            item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(label, "selectHotkey:", "")
            item.setTarget_(self)
            item.setRepresentedObject_([hotkey, mode])
            item.setState_(NSOnState if (hotkey == h and mode == m) else NSOffState)
            sub.addItem_(item)

    def selectHotkey_(self, sender):
        ro = sender.representedObject()
        hotkey, mode = str(ro[0]), str(ro[1])
        self.config["hotkey"], self.config["hotkey_mode"] = hotkey, mode
        self.modifiers, self.key_code, self.hold_keycode = parse_hotkey(hotkey)
        self.hold_modifiers = self.modifiers
        self.hotkey_mode = mode
        save_config(self.config)
        self._fill_hotkey_submenu()
        if self._title_item is not None:
            self._title_item.setTitle_(self._status_title())
        logger.info(f"Hotkey set to '{hotkey}' ({mode})")

    def setup_menu(self):
        """Set up the dropdown menu."""
        self.menu.removeAllItems()
        self._choice_submenus = []
        self._bool_items = []

        self._title_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            self._status_title(), None, ""
        )
        self._title_item.setEnabled_(False)
        self.menu.addItem_(self._title_item)

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

        # Input device submenu
        self.input_menu_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            self._input_menu_title(), None, ""
        )
        self.input_submenu = NSMenu.alloc().init()
        self.input_menu_item.setSubmenu_(self.input_submenu)
        self.menu.addItem_(self.input_menu_item)

        # Output device submenu
        self.output_menu_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            self._output_menu_title(), None, ""
        )
        self.output_submenu = NSMenu.alloc().init()
        self.output_menu_item.setSubmenu_(self.output_submenu)
        self.menu.addItem_(self.output_menu_item)

        self._rebuild_device_submenus()

        # Settings submenus (write config.toml; applied live)
        self._add_hotkey_submenu()
        self._add_choice_submenu("Idle unload", "idle_unload_minutes", IDLE_PRESETS)

        self.menu.addItem_(NSMenuItem.separatorItem())

        # Boolean toggles
        for title, key in BOOL_SETTINGS:
            self._add_bool_item(self.menu, title, key)

        self.menu.addItem_(NSMenuItem.separatorItem())

        config_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            "Open Config (TOML)...",
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

        uninstall_item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(
            "Uninstall Whisper Push...",
            "uninstallApp:",
            ""
        )
        uninstall_item.setTarget_(self)
        self.menu.addItem_(uninstall_item)

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
            # Idle-unload timer: arm when idle, cancel while busy.
            if getattr(self, '_idle_timer', None) is not None or new_state == State.IDLE:
                if new_state == State.IDLE:
                    self._schedule_idle_unload()
                elif self._idle_timer is not None:
                    self._idle_timer.cancel()
                    self._idle_timer = None
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
            logger.info("Model still loading, please wait...")
            return

        if self.state == State.PROCESSING:
            return

        if self.state == State.IDLE:
            self.start_recording()
        elif self.state == State.RECORDING:
            self.stop_and_transcribe()

    def _delayed_start_recording(self):
        """Hold delay expired: confirm the pre-rolled capture (sound + UI)."""
        if self.hold_pending and self.hold_active and self.state == State.IDLE:
            self.hold_pending = False
            self._hold_timer = None
            self._commit_recording()
        else:
            self.hold_pending = False
            self._hold_timer = None

    def _begin_capture(self):
        """Open the mic and start buffering IMMEDIATELY (zero-latency pre-roll).

        No sound/UI yet -- in hold mode this runs on key-down so we never lose
        the start of speech; the capture is committed or discarded after the
        hold delay confirms it's a real hold (not a shortcut)."""
        ensure_model_loading()
        self.recorder.start()
        self._recording_ready = True

    def _discard_capture(self):
        """Throw away a pre-rolled capture (it was a shortcut / quick tap)."""
        self.recorder.cancel()
        self._recording_ready = False

    def _commit_recording(self):
        """Promote a pre-rolled capture to an active recording (sound + UI)."""
        if self.config.get("sound_feedback"):
            play_sound("start", self.config)
        self.set_state(State.RECORDING)
        logger.info("Recording started...")

    def start_recording(self):
        """Immediate start (toggle mode): begin capturing and commit at once."""
        self._begin_capture()
        self._commit_recording()

    def stop_and_transcribe(self, audio=None):
        """Stop recording and transcribe an in-memory audio array.

        In hold mode the caller passes the already-captured array; in toggle
        mode we stop the recorder here.
        """
        if not getattr(self, '_recording_ready', False):
            logger.info("Recording not ready yet, ignoring stop")
            return
        self._recording_ready = False
        self.set_state(State.PROCESSING)

        if self.config.get("sound_feedback"):
            play_sound("stop", self.config)

        if audio is None:
            audio = self.recorder.stop_array()

        if audio is None or len(audio) == 0:
            logger.info("No audio recorded")
            self.set_state(State.IDLE)
            return

        # Fast-path: skip transcription for too-short or silent recordings
        duration = len(audio) / self.recorder.target_sample_rate
        rms = float(np.sqrt(np.mean(audio ** 2)))
        if duration < 0.3:
            logger.info(f"Audio too short ({duration:.2f}s), skipping transcription")
            self.set_state(State.IDLE)
            return
        if rms < 0.005:
            logger.info(f"Audio is silence (RMS={rms:.5f}), skipping transcription")
            self.set_state(State.IDLE)
            return

        logger.info("Recording stopped, transcribing...")

        # Only transcription in background thread
        def do_transcribe():
            # If the model was idle-unloaded, it's reloading in parallel (kicked
            # off in start_recording); wait for it before transcribing.
            if _asr_model is None:
                logger.info("Waiting for model to finish loading...")
                if not wait_for_model():
                    logger.error("Model load timed out")
                    self.set_state(State.IDLE)
                    return
            text = transcribe_audio(audio)

            if text:
                logger.info(f"Transcribed: {text}")
                # Paste on main thread for reliable CGEvent delivery
                self._pending_text = text
                self.performSelectorOnMainThread_withObject_waitUntilDone_(
                    "pasteOnMainThread:",
                    None,
                    False
                )
                if self.config.get("notifications"):
                    notify("Whisper Push", f"Transcribed {len(text)} characters")
            else:
                logger.info("No transcription result")
                if self.config.get("notifications"):
                    notify("Whisper Push", "No speech detected")

            self.set_state(State.IDLE)

        threading.Thread(target=do_transcribe, daemon=True).start()

    def pasteOnMainThread_(self, _):
        text = getattr(self, '_pending_text', None)
        if text:
            paste_text(text)
            self._pending_text = None

    def toggleRecordingMenu_(self, sender):
        self.toggle_recording()

    def cancelRecording_(self, sender):
        if self.state == State.RECORDING:
            self.recorder.cancel()
            self.set_state(State.IDLE)
            logger.info("Recording cancelled")

    def openConfig_(self, sender):
        if not CONFIG_FILE.exists():
            CONFIG_FILE.parent.mkdir(parents=True, exist_ok=True)
            save_config(load_config())  # write the fully-documented template
        subprocess.run(["open", str(CONFIG_FILE)])

    def viewLogs_(self, sender):
        log_path = SUPPORT_DIR / "daemon.log"
        if log_path.exists():
            subprocess.run(["open", "-a", "Console", str(log_path)])
        else:
            subprocess.run(["open", str(SUPPORT_DIR)])

    def quitApp_(self, sender):
        NSApp.terminate_(None)

    def uninstallApp_(self, sender):
        """Remove all app data + the model, move the app to the Trash, then quit.

        macOS runs no code when an app is dragged to the Trash, so this menu item
        is the only way to clean up everything Whisper Push wrote outside its
        bundle (settings, logs, and the ~600 MB model)."""
        alert = NSAlert.alloc().init()
        alert.setMessageText_("Uninstall Whisper Push?")
        alert.setInformativeText_(
            "This deletes your settings and the downloaded model (~600 MB), "
            "moves the app to the Trash, and quits.\n\nThis cannot be undone."
        )
        alert.addButtonWithTitle_("Uninstall")
        alert.addButtonWithTitle_("Cancel")
        NSApp.activateIgnoringOtherApps_(True)
        if alert.runModal() != NSAlertFirstButtonReturn:
            return

        logger.info("Uninstalling: removing app data and model...")

        # 1. Settings, logs, and the model (now stored under SUPPORT_DIR).
        shutil.rmtree(SUPPORT_DIR, ignore_errors=True)

        # 2. Model left in the shared HuggingFace cache by older builds.
        model_name = self.config.get("model", "parakeet-tdt-0.6b-v3")
        legacy_model = (Path.home() / ".cache" / "huggingface" / "hub"
                        / f"models--mlx-community--{model_name}")
        shutil.rmtree(legacy_model, ignore_errors=True)

        # 3. Move the .app bundle itself to the Trash (frozen builds only).
        bundle = _app_bundle_path()
        if bundle:
            subprocess.run(
                ["osascript", "-e",
                 f'tell application "Finder" to delete (POSIX file "{bundle}" as alias)'],
                check=False,
            )

        NSApp.terminate_(None)

    # --- Device selection ---

    def _input_menu_title(self):
        name = self.config.get("input_device", "auto")
        if name == "auto":
            # Show which device auto resolves to
            idx = find_input_device()
            if idx is not None:
                resolved = sd.query_devices(idx)['name']
                return f"Input: {resolved} (Auto)"
            return "Input: Auto"
        return f"Input: {name}"

    def _output_menu_title(self):
        name = self.config.get("output_device", "auto")
        if name == "auto":
            try:
                default_out = sd.query_devices(kind='output')
                return f"Output: {default_out['name']} (Auto)"
            except Exception:
                return "Output: Auto"
        return f"Output: {name}"

    def _add_device_menu_item(self, submenu, title, represented, action, checked):
        item = NSMenuItem.alloc().initWithTitle_action_keyEquivalent_(title, action, "")
        item.setTarget_(self)
        item.setRepresentedObject_(represented)
        item.setState_(NSOnState if checked else NSOffState)
        submenu.addItem_(item)

    def _build_device_submenu(self, submenu, kind, current, auto_label, action):
        """Populate one device submenu (kind is 'input' or 'output')."""
        channel_key = 'max_input_channels' if kind == 'input' else 'max_output_channels'
        submenu.removeAllItems()
        self._add_device_menu_item(submenu, auto_label, "auto", action, current == "auto")
        submenu.addItem_(NSMenuItem.separatorItem())

        seen = set()
        for d in sd.query_devices():
            name = d['name']
            if d[channel_key] < 1 or name in seen:
                continue
            seen.add(name)
            self._add_device_menu_item(submenu, name, name, action, current == name)

    def _rebuild_device_submenus(self):
        """Rebuild input/output device submenus from current system devices."""
        self._build_device_submenu(
            self.input_submenu, 'input', self.config.get("input_device", "auto"),
            "Auto (built-in mic heuristic)", "selectInputDevice:")
        self._build_device_submenu(
            self.output_submenu, 'output', self.config.get("output_device", "auto"),
            "Auto (system default)", "selectOutputDevice:")
        self.input_menu_item.setTitle_(self._input_menu_title())
        self.output_menu_item.setTitle_(self._output_menu_title())

    def selectInputDevice_(self, sender):
        name = sender.representedObject()
        self.config["input_device"] = name
        save_config(self.config)
        self._rebuild_device_submenus()
        logger.info(f"Input device set to: {name}")

    def selectOutputDevice_(self, sender):
        name = sender.representedObject()
        self.config["output_device"] = name
        self._resolve_output_device_idx()
        save_config(self.config)
        self._rebuild_device_submenus()
        logger.info(f"Output device set to: {name}")

    def _resolve_output_device_idx(self):
        """Cache the output device index for instant playback."""
        configured = self.config.get("output_device", "auto")
        if configured != "auto":
            self.config["_output_device_idx"] = find_device_by_name(configured, kind='output')
        else:
            self.config.pop("_output_device_idx", None)

    def menuNeedsUpdate_(self, menu):
        """NSMenuDelegate: refresh dynamic state when the menu opens, so it
        reflects external config.toml edits and current devices.

        Only submenu contents and item states are touched -- rebuilding the
        whole top menu here corrupts AppKit's in-progress presentation.
        """
        if menu != self.menu or not hasattr(self, 'input_submenu'):
            return
        self._rebuild_device_submenus()
        self._fill_hotkey_submenu()
        for submenu, key in self._choice_submenus:
            current = self.config.get(key)
            for it in submenu.itemArray():
                ro = it.representedObject()
                it.setState_(NSOnState if (ro is not None and ro[1] == current) else NSOffState)
        for it in self._bool_items:
            it.setState_(NSOnState if self.config.get(it.representedObject()) else NSOffState)
        self._title_item.setTitle_(self._status_title())


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
            logger.info("Accessibility permission: GRANTED")
        else:
            logger.info("Accessibility permission: NOT GRANTED - dialog shown to user")
            notify("Whisper Push", "Please grant Accessibility permission, then restart the app")

        return result

    except Exception as e:
        logger.error(f"Accessibility check error: {e}")
        return False


_lock_fd = None  # kept alive for the process lifetime to hold the flock


def acquire_single_instance_lock():
    """Hold an exclusive lock so only one daemon runs (no duplicate icons)."""
    global _lock_fd
    try:
        SUPPORT_DIR.mkdir(parents=True, exist_ok=True)
        _lock_fd = open(LOCK_FILE, "w")
        fcntl.flock(_lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except OSError:
        return False


def main():
    setup_logging(load_config().get("debug", False))

    if not acquire_single_instance_lock():
        logger.info("Another instance is already running - exiting")
        sys.exit(0)

    logger.info("Whisper Push daemon starting")

    # Flag-and-poll shutdown: the handler sets a flag (it can't safely act while
    # the Cocoa run loop is blocked); the app's NSTimer polls it and terminates.
    signal.signal(signal.SIGTERM, _request_shutdown)
    signal.signal(signal.SIGINT, _request_shutdown)

    # Check accessibility permission first (will prompt user if needed)
    check_accessibility_permission()

    # Create application
    app = NSApplication.sharedApplication()
    app.setActivationPolicy_(NSApplicationActivationPolicyAccessory)

    # Create and set delegate
    delegate = MenuBarApp.alloc().init()
    app.setDelegate_(delegate)

    # Run
    AppHelper.runEventLoop()


if __name__ == "__main__":
    main()
