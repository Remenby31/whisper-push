# -*- mode: python ; coding: utf-8 -*-
"""
PyInstaller spec file for whisper-push macOS app bundle.
Supports Apple Silicon (M1/M2/M3/M4) and Intel Macs.
"""

import sys
from pathlib import Path

# Paths
PROJECT_ROOT = Path(SPECPATH).parent
SCRIPT_PATH = PROJECT_ROOT / "whisper-push-macos.py"
SOUNDS_DIR = PROJECT_ROOT / "sounds"
ICON_PATH = PROJECT_ROOT / "macos" / "whisper-push.icns"

# Data files to include
datas = [
    (str(SOUNDS_DIR), "sounds"),
    (str(PROJECT_ROOT / "config.toml"), "."),
]

# Hidden imports for faster-whisper and its dependencies
hiddenimports = [
    "faster_whisper",
    "ctranslate2",
    "tokenizers",
    "huggingface_hub",
    "numpy",
    "av",
    "tqdm",
]

a = Analysis(
    [str(SCRIPT_PATH)],
    pathex=[str(PROJECT_ROOT)],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        # Exclude CUDA libraries (not needed on macOS)
        "nvidia",
        "nvidia_cublas_cu12",
        "nvidia_cudnn_cu12",
        "torch",
        "tensorflow",
    ],
    noarchive=False,
    optimize=0,
)

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="whisper-push",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,  # Don't use UPX on macOS
    console=False,  # No terminal window
    disable_windowed_traceback=False,
    argv_emulation=True,  # Enable argv emulation for macOS
    target_arch=None,  # Build for current architecture (arm64 on M1+)
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
    name="whisper-push",
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
        "CFBundleIdentifier": "com.whisper-push.app",
        "CFBundlePackageType": "APPL",
        "CFBundleSignature": "????",
        "LSMinimumSystemVersion": "11.0",  # macOS Big Sur minimum
        "NSHighResolutionCapable": True,
        "NSMicrophoneUsageDescription": "Whisper Push needs microphone access to record your voice for transcription.",
        "NSAppleEventsUsageDescription": "Whisper Push needs accessibility permissions to type transcribed text.",
        "LSUIElement": True,  # Run as background app (no dock icon)
    },
)
