#!/usr/bin/env python3
"""
whisper-push - Push-to-talk dictation using faster-whisper (macOS version)

Toggle mode: press hotkey to start recording, press again to transcribe and type.
Compatible with Apple Silicon (M1/M2/M3/M4).
"""

import argparse
import atexit
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
import tomllib
from pathlib import Path
from typing import Iterator, Optional

# Paths - macOS conventions
SCRIPT_DIR = Path(__file__).resolve().parent
if getattr(sys, 'frozen', False):
    # Running as app bundle
    SCRIPT_DIR = Path(sys._MEIPASS) if hasattr(sys, '_MEIPASS') else SCRIPT_DIR
    RESOURCES_DIR = SCRIPT_DIR
else:
    RESOURCES_DIR = SCRIPT_DIR

CONFIG_DIR = Path.home() / "Library" / "Application Support" / "whisper-push"
CONFIG_FILE = CONFIG_DIR / "config.toml"
RUNTIME_DIR = Path(os.environ.get("TMPDIR", "/tmp"))
LOCK_FILE = RUNTIME_DIR / "whisper-push.lock"
AUDIO_FILE = RUNTIME_DIR / "whisper-push.wav"
SOUNDS_DIR = RESOURCES_DIR / "sounds"

# Default configuration (macOS optimized)
DEFAULT_CONFIG = {
    "language": "auto",
    "model": "large-v3-turbo",
    "compute_type": "int8",
    "device": "cpu",  # Use CPU for Apple Silicon (efficient with ANE)
    "notifications": True,
    "sound_feedback": True,
    "beam_size": 5,
    "debug": False,
}

# Lazy-loaded model
_model: Optional[object] = None


def _cleanup_model() -> None:
    """Cleanup model on exit to free memory."""
    global _model
    if _model is not None:
        del _model
        _model = None


atexit.register(_cleanup_model)


def load_config() -> dict:
    """Load configuration from file, with defaults."""
    config = DEFAULT_CONFIG.copy()
    if CONFIG_FILE.exists():
        try:
            with open(CONFIG_FILE, "rb") as f:
                user_config = tomllib.load(f)
                config.update(user_config)
        except Exception as e:
            notify(f"Config error: {e}", urgency="critical")
    return config


def notify(message: str, urgency: str = "normal") -> None:
    """Send macOS notification via osascript."""
    try:
        # Map urgency to sound
        sound = "Basso" if urgency == "critical" else "default"
        script = f'''
        display notification "{message}" with title "Whisper Push" sound name "{sound}"
        '''
        subprocess.run(
            ["osascript", "-e", script],
            capture_output=True,
            timeout=5,
        )
    except Exception:
        pass


def play_sound(sound_type: str) -> None:
    """Play feedback sound using afplay (macOS native)."""
    sound_file = SOUNDS_DIR / f"{sound_type}.wav"
    if sound_file.exists():
        def _play():
            try:
                subprocess.run(
                    ["afplay", str(sound_file)],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
            except Exception:
                pass
        # Run in daemon thread to avoid blocking and prevent zombie processes
        thread = threading.Thread(target=_play, daemon=True)
        thread.start()


def is_recording() -> bool:
    """Check if recording is currently in progress."""
    if not LOCK_FILE.exists():
        return False
    try:
        pid = int(LOCK_FILE.read_text().strip())
        os.kill(pid, 0)
        return True
    except (ValueError, ProcessLookupError, PermissionError):
        LOCK_FILE.unlink(missing_ok=True)
        return False


def find_sox() -> str:
    """Find sox/rec binary (Homebrew or bundled)."""
    # Check common Homebrew locations for Apple Silicon and Intel
    candidates = [
        "/opt/homebrew/bin/rec",  # Apple Silicon Homebrew
        "/usr/local/bin/rec",      # Intel Homebrew
        shutil.which("rec"),
    ]
    for path in candidates:
        if path and Path(path).exists():
            return path
    raise RuntimeError(
        "sox not found. Install with: brew install sox"
    )


def start_recording(config: dict) -> None:
    """Start audio recording with sox/rec."""
    AUDIO_FILE.unlink(missing_ok=True)

    rec_path = find_sox()

    # Record with sox: 16kHz mono WAV
    process = subprocess.Popen(
        [
            rec_path,
            "-q",  # Quiet
            "-r", "16000",  # Sample rate
            "-c", "1",  # Mono
            "-b", "16",  # 16-bit
            str(AUDIO_FILE),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )

    # Wait for recorder to initialize
    time.sleep(0.1)

    LOCK_FILE.write_text(str(process.pid))

    if config["sound_feedback"]:
        play_sound("start")

    if config["notifications"]:
        notify("Recording...")


def stop_recording(config: dict) -> bool:
    """Stop recording gracefully. Returns True if audio was captured."""
    if not LOCK_FILE.exists():
        return False

    try:
        pid = int(LOCK_FILE.read_text().strip())
        os.kill(pid, signal.SIGINT)

        # Wait for process to flush and exit
        for _ in range(20):
            time.sleep(0.1)
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
    except (ValueError, ProcessLookupError, PermissionError):
        pass

    LOCK_FILE.unlink(missing_ok=True)

    if config["notifications"]:
        notify("Processing...")

    return AUDIO_FILE.exists() and AUDIO_FILE.stat().st_size > 44


def load_model(config: dict):
    """Load Whisper model (lazy loading, cached)."""
    global _model
    if _model is None:
        from faster_whisper import WhisperModel

        device = config["device"]
        compute_type = config["compute_type"]

        # On Apple Silicon, use CPU with int8 for best performance
        # MPS support in CTranslate2 is experimental
        if device == "auto":
            device = "cpu"
        if device == "cpu" and compute_type not in ("int8", "float32"):
            compute_type = "int8"

        _model = WhisperModel(
            config["model"],
            device=device,
            compute_type=compute_type,
        )
    return _model


def transcribe_segments(config: dict) -> Iterator[str]:
    """Transcribe audio file and yield text segments as they are processed."""
    model = load_model(config)
    language = None if config["language"] == "auto" else config["language"]

    segments, _ = model.transcribe(
        str(AUDIO_FILE),
        beam_size=config["beam_size"],
        language=language,
        vad_filter=True,
    )

    for segment in segments:
        text = segment.text.strip()
        if text:
            yield text


def _paste_text(text: str) -> None:
    """Copy text to clipboard and paste with Cmd+V via AppleScript."""
    # Copy to clipboard using pbcopy
    process = subprocess.Popen(
        ["pbcopy"],
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    process.communicate(input=text.encode("utf-8"))
    time.sleep(0.05)

    # Paste with Cmd+V using AppleScript
    script = '''
    tell application "System Events"
        keystroke "v" using command down
    end tell
    '''
    subprocess.run(
        ["osascript", "-e", script],
        capture_output=True,
        timeout=2,
    )
    time.sleep(0.05)


def transcribe_and_type(config: dict) -> str:
    """Transcribe audio and type text segment by segment (streaming).

    Returns the full transcribed text for notification purposes.
    """
    # Save clipboard before we start
    old_clipboard = subprocess.run(
        ["pbpaste"], capture_output=True, timeout=2
    ).stdout

    typed_segments: list[str] = []

    try:
        for segment_text in transcribe_segments(config):
            # Add space before segment if not the first one
            if typed_segments:
                _paste_text(" " + segment_text)
            else:
                _paste_text(segment_text)
            typed_segments.append(segment_text)
    finally:
        # Restore previous clipboard
        if old_clipboard:
            time.sleep(0.1)
            process = subprocess.Popen(
                ["pbcopy"],
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            process.communicate(input=old_clipboard, timeout=2)

    return " ".join(typed_segments)


def check_accessibility_permissions() -> bool:
    """Check if app has accessibility permissions (needed for keystroke simulation)."""
    # This is a basic check - the actual permission prompt is handled by macOS
    script = '''
    tell application "System Events"
        return true
    end tell
    '''
    try:
        result = subprocess.run(
            ["osascript", "-e", script],
            capture_output=True,
            timeout=5,
        )
        return result.returncode == 0
    except Exception:
        return False


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Push-to-talk dictation with Whisper (macOS)"
    )
    parser.add_argument("--config", "-c", type=Path, help="Config file path")
    parser.add_argument("--language", "-l", help="Override language (auto, fr, en, ...)")
    parser.add_argument("--status", "-s", action="store_true", help="Show status")
    parser.add_argument("--stop", action="store_true", help="Force stop recording")
    parser.add_argument("--check-permissions", action="store_true",
                       help="Check accessibility permissions")
    args = parser.parse_args()

    # Permission check
    if args.check_permissions:
        if check_accessibility_permissions():
            print("Accessibility permissions: OK")
            sys.exit(0)
        else:
            print("Accessibility permissions: DENIED")
            print("Go to System Settings > Privacy & Security > Accessibility")
            print("and enable whisper-push or Terminal.")
            sys.exit(1)

    # Load config
    global CONFIG_FILE
    if args.config:
        CONFIG_FILE = args.config

    config = load_config()

    if args.language:
        config["language"] = args.language

    # Ensure config directory exists
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)

    # Status check
    if args.status:
        print("Recording in progress" if is_recording() else "Idle")
        sys.exit(0)

    # Force stop
    if args.stop:
        if is_recording():
            stop_recording(config)
            notify("Recording cancelled")
        sys.exit(0)

    # Toggle logic
    if is_recording():
        if stop_recording(config):
            try:
                text = transcribe_and_type(config)
                if text:
                    if config["sound_feedback"]:
                        play_sound("stop")
                    if config["notifications"]:
                        preview = (text[:50] + "...") if len(text) > 50 else text
                        notify(f"Typed: {preview}")
                else:
                    if config["notifications"]:
                        notify("No speech detected", urgency="low")
            except Exception as e:
                notify(f"Error: {e}", urgency="critical")
                sys.exit(1)
            finally:
                if config.get("debug"):
                    debug_file = Path.home() / "Library" / "Caches" / "whisper-push-last.wav"
                    debug_file.parent.mkdir(exist_ok=True)
                    shutil.copy2(AUDIO_FILE, debug_file)
                AUDIO_FILE.unlink(missing_ok=True)
    else:
        start_recording(config)


if __name__ == "__main__":
    main()
