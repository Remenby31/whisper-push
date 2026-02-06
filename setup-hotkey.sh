#!/bin/bash
set -e

HOTKEY_INPUT="${1:-}"

if [ -z "$HOTKEY_INPUT" ]; then
    echo "Usage: $0 \"Super+V\""
    exit 1
fi

if ! command -v gsettings >/dev/null 2>&1; then
    echo "GNOME gsettings not available. Configure the shortcut manually."
    exit 0
fi

DESKTOP="${XDG_CURRENT_DESKTOP:-}"
if [[ "$DESKTOP" != *GNOME* && "$DESKTOP" != *Unity* && "$DESKTOP" != *ubuntu* ]]; then
    echo "Hotkey auto-config supports GNOME only. Configure the shortcut manually."
    exit 0
fi

PYTHON_BIN="$(command -v python3 || command -v python || true)"
if [ -z "$PYTHON_BIN" ]; then
    echo "Python is required to configure GNOME hotkeys. Configure manually."
    exit 1
fi

BIND_PATH="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/whisper-push/"
CMD="$HOME/.local/bin/whisper-push"

BINDING="$("$PYTHON_BIN" - <<'PY'
import re
import sys

raw = sys.argv[1].strip()
if not raw:
    sys.exit(1)

parts = re.split(r"\s*\+\s*", raw)
mods_map = {
    "super": "<Super>",
    "meta": "<Super>",
    "win": "<Super>",
    "command": "<Super>",
    "cmd": "<Super>",
    "ctrl": "<Control>",
    "control": "<Control>",
    "shift": "<Shift>",
    "alt": "<Alt>",
    "option": "<Alt>",
}
mods = []
key = None
for part in parts:
    name = part.strip().lower()
    if name in mods_map:
        mods.append(mods_map[name])
    else:
        key = name

if not key:
    sys.exit(1)

key_map = {
    "space": "space",
    "return": "Return",
    "enter": "Return",
    "tab": "Tab",
    "esc": "Escape",
    "escape": "Escape",
    "backspace": "BackSpace",
}

key = key_map.get(key, key)
print("".join(mods) + key)
PY
"$HOTKEY_INPUT")"

if [ -z "$BINDING" ]; then
    echo "Invalid hotkey format. Use something like Super+V or Ctrl+Shift+Space."
    exit 1
fi

CURRENT="$(gsettings get org.gnome.settings-daemon.plugins.media-keys custom-keybindings)"
UPDATED="$("$PYTHON_BIN" - <<'PY'
import ast
import sys

raw = sys.argv[1]
path = sys.argv[2]
if raw.startswith("@as "):
    raw = raw[4:]

lst = ast.literal_eval(raw) if raw else []
if path not in lst:
    lst.append(path)
print(lst)
PY
"$CURRENT" "$BIND_PATH")"

gsettings set org.gnome.settings-daemon.plugins.media-keys custom-keybindings "$UPDATED"
gsettings set org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:"$BIND_PATH" name "Whisper Push"
gsettings set org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:"$BIND_PATH" command "$CMD"
gsettings set org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:"$BIND_PATH" binding "$BINDING"

echo "Configured GNOME hotkey: $HOTKEY_INPUT"
