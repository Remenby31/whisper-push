# -*- mode: python ; coding: utf-8 -*-
"""
PyInstaller spec for the Whisper Push menu-bar app (Parakeet / MLX).
Apple Silicon only (MLX requires an M-series chip).
"""

from pathlib import Path
from PyInstaller.utils.hooks import collect_all, collect_submodules

PROJECT_ROOT = Path(SPECPATH).parent
SCRIPT_PATH = PROJECT_ROOT / "macos" / "menubar-daemon.py"
ICON_PATH = PROJECT_ROOT / "macos" / "whisper-push.icns"

# Bundled read-only resources (the daemon reads these from sys._MEIPASS when frozen)
datas = [
    (str(PROJECT_ROOT / "macos" / "icons"), "icons"),
    (str(PROJECT_ROOT / "sounds"), "sounds"),
]
binaries = []
hiddenimports = []

# Pull in everything for the native/runtime-heavy packages.
for pkg in ("mlx", "parakeet_mlx", "sounddevice", "soundfile", "huggingface_hub"):
    d, b, h = collect_all(pkg)
    datas += d
    binaries += b
    hiddenimports += h

# scipy (used for resampling) + PyObjC frameworks we import.
hiddenimports += collect_submodules("scipy.signal")
hiddenimports += [
    "scipy", "numpy", "tomllib",
    "objc", "Foundation", "AppKit", "Cocoa", "Quartz",
    "PyObjCTools", "PyObjCTools.AppHelper",
]

a = Analysis(
    [str(SCRIPT_PATH)],
    pathex=[str(PROJECT_ROOT)],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=["torch", "tensorflow", "nvidia", "matplotlib", "tkinter", "PIL"],
    noarchive=False,
    optimize=0,
)

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="Whisper Push",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,  # use the host arch as-is (arm64); avoids lipo thinning
    codesign_identity=None,
    entitlements_file=None,
)

coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    upx_exclude=[],
    name="Whisper Push",
)

app = BUNDLE(
    coll,
    name="Whisper Push.app",
    icon=str(ICON_PATH) if ICON_PATH.exists() else None,
    bundle_identifier="com.whisper-push.app",
    version="1.0.0",
    info_plist={
        "CFBundleName": "Whisper Push",
        "CFBundleDisplayName": "Whisper Push",
        "CFBundleShortVersionString": "1.0.0",
        "CFBundleVersion": "1.0.0",
        "LSMinimumSystemVersion": "14.0",
        "NSHighResolutionCapable": True,
        "LSUIElement": True,  # menu-bar app, no Dock icon
        "NSMicrophoneUsageDescription":
            "Whisper Push records your voice to transcribe it into text.",
        "NSAppleEventsUsageDescription":
            "Whisper Push uses accessibility to paste transcribed text.",
    },
)
