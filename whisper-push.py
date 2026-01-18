#!/usr/bin/env python3
"""
whisper-push - Push-to-talk dictation using faster-whisper

Toggle mode: press hotkey to start recording, press again to transcribe and type.
"""

import argparse
import os
import shutil
import signal
import subprocess
import sys
import time
import tomllib
from pathlib import Path
from typing import Iterator, Optional

# Paths
SCRIPT_DIR = Path(__file__).resolve().parent
CONFIG_DIR = Path.home() / ".config" / "whisper-push"
CONFIG_FILE = CONFIG_DIR / "config.toml"
RUNTIME_DIR = Path(os.environ.get("XDG_RUNTIME_DIR", "/tmp"))
LOCK_FILE = RUNTIME_DIR / "whisper-push.lock"
AUDIO_FILE = RUNTIME_DIR / "whisper-push.wav"
SOUNDS_DIR = SCRIPT_DIR / "sounds"

# Default configuration
DEFAULT_CONFIG = {
    "language": "auto",
    "model": "large-v3-turbo",
    "compute_type": "int8",
    "device": "cuda",
    "notifications": True,
    "sound_feedback": True,
    "beam_size": 5,
    "debug": False,
}

# Lazy-loaded model
_model: Optional[object] = None


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
    """Send desktop notification."""
    try:
        subprocess.run(
            ["notify-send", "-u", urgency, "-a", "whisper-push", "Whisper Push", message],
            capture_output=True,
            timeout=5,
        )
    except Exception:
        pass


def play_sound(sound_type: str) -> None:
    """Play feedback sound (start/stop)."""
    sound_file = SOUNDS_DIR / f"{sound_type}.wav"
    if sound_file.exists():
        try:
            subprocess.Popen(
                ["paplay", str(sound_file)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        except Exception:
            pass


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


def start_recording(config: dict) -> None:
    """Start audio recording with pw-record."""
    AUDIO_FILE.unlink(missing_ok=True)

    process = subprocess.Popen(
        ["pw-record", "--format=s16", "--rate=16000", "--channels=1", str(AUDIO_FILE)],
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
        _model = WhisperModel(
            config["model"],
            device=config["device"],
            compute_type=config["compute_type"],
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
    """Copy text to clipboard and paste with Ctrl+Shift+V."""
    subprocess.Popen(
        ["wl-copy", "--", text],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.1)

    # Paste with Ctrl+Shift+V via ydotool
    # Keycodes: 29=Ctrl, 42=Shift, 47=V
    subprocess.run(
        ["ydotool", "key", "-d", "20", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"],
        capture_output=True,
        timeout=2,
    )
    time.sleep(0.1)


def transcribe_and_type(config: dict) -> str:
    """Transcribe audio and type text segment by segment (streaming).

    Returns the full transcribed text for notification purposes.
    """
    # Save clipboard before we start
    old_clipboard = subprocess.run(
        ["wl-paste", "-n"], capture_output=True, timeout=2
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
            subprocess.Popen(
                ["wl-copy", "--"],
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            ).communicate(input=old_clipboard, timeout=2)

    return " ".join(typed_segments)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Push-to-talk dictation with Whisper"
    )
    parser.add_argument("--config", "-c", type=Path, help="Config file path")
    parser.add_argument("--language", "-l", help="Override language (auto, fr, en, ...)")
    parser.add_argument("--status", "-s", action="store_true", help="Show status")
    parser.add_argument("--stop", action="store_true", help="Force stop recording")
    args = parser.parse_args()

    # Load config
    global CONFIG_FILE
    if args.config:
        CONFIG_FILE = args.config

    config = load_config()

    if args.language:
        config["language"] = args.language

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
                    debug_file = Path.home() / ".cache" / "whisper-push-last.wav"
                    debug_file.parent.mkdir(exist_ok=True)
                    shutil.copy2(AUDIO_FILE, debug_file)
                AUDIO_FILE.unlink(missing_ok=True)
    else:
        start_recording(config)


if __name__ == "__main__":
    main()
