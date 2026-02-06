#!/usr/bin/env python3
"""
whisper-push - Push-to-talk dictation using faster-whisper

Toggle mode: press hotkey to start recording, press again to transcribe and type.
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
from pathlib import Path
from typing import Iterator, Optional

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # Python 3.10 fallback
    import tomli as tomllib

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
    if not sound_file.exists():
        return

    def _play() -> None:
        players = ["paplay", "pw-play", "aplay"]
        for player in players:
            if shutil.which(player):
                try:
                    subprocess.run(
                        [player, str(sound_file)],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                    )
                    return
                except Exception:
                    return

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
    subprocess.run(
        ["wl-copy", "--", text],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=2,
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
            subprocess.run(
                ["wl-copy", "--"],
                input=old_clipboard,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=2,
            )

    return " ".join(typed_segments)


def _format_check(label: str, ok: bool, detail: str = "") -> None:
    status = "OK" if ok else "MISSING"
    tail = f" - {detail}" if detail else ""
    print(f"[{status}] {label}{tail}")


def doctor() -> int:
    """Run environment checks and print a quick diagnostic report."""
    print("whisper-push doctor")
    print(f"Python: {sys.version.split()[0]}")
    print(f"Config: {CONFIG_FILE}")
    print(f"Runtime dir: {RUNTIME_DIR}")

    required_cmds = ["pw-record", "wl-copy", "wl-paste", "ydotool"]
    optional_cmds = ["notify-send", "paplay", "pw-play", "aplay"]
    missing_required = []

    for cmd in required_cmds:
        ok = shutil.which(cmd) is not None
        _format_check(cmd, ok)
        if not ok:
            missing_required.append(cmd)

    for cmd in optional_cmds:
        _format_check(cmd, shutil.which(cmd) is not None)

    # ydotool socket
    ydotool_socket = os.environ.get("YDOTOOL_SOCKET", "/tmp/.ydotool_socket")
    _format_check("ydotool socket", os.path.exists(ydotool_socket), ydotool_socket)

    # input group membership (needed by ydotool)
    try:
        import grp

        groups = {grp.getgrgid(g).gr_name for g in os.getgroups()}
        _format_check("input group", "input" in groups, "logout/login required")
    except Exception:
        _format_check("input group", False, "could not determine")

    # faster-whisper import
    try:
        import faster_whisper  # noqa: F401

        _format_check("faster-whisper", True)
    except Exception as e:
        _format_check("faster-whisper", False, str(e))
        missing_required.append("faster-whisper (python package)")

    # config parse
    if CONFIG_FILE.exists():
        try:
            load_config()
            _format_check("config parse", True)
        except Exception as e:
            _format_check("config parse", False, str(e))
    else:
        _format_check("config file", False, "missing")

    # runtime dir write access
    _format_check("runtime writable", os.access(RUNTIME_DIR, os.W_OK), str(RUNTIME_DIR))

    if missing_required:
        print("")
        print("Missing required dependencies:")
        for item in missing_required:
            print(f"- {item}")
        return 1

    print("")
    print("All required dependencies look present.")
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Push-to-talk dictation with Whisper"
    )
    parser.add_argument("--config", "-c", type=Path, help="Config file path")
    parser.add_argument("--language", "-l", help="Override language (auto, fr, en, ...)")
    parser.add_argument("--status", "-s", action="store_true", help="Show status")
    parser.add_argument("--stop", action="store_true", help="Force stop recording")
    parser.add_argument("--doctor", action="store_true", help="Run dependency checks")
    args = parser.parse_args()

    # Load config
    global CONFIG_FILE
    if args.config:
        CONFIG_FILE = args.config

    if args.doctor:
        sys.exit(doctor())

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
