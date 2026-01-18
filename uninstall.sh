#!/bin/bash

echo "Uninstalling whisper-push..."

rm -f ~/.local/bin/whisper-push
rm -f ~/.local/share/applications/whisper-push.desktop
rm -f ~/.local/share/icons/hicolor/scalable/apps/whisper-push.svg

echo "Uninstalled. Config kept at ~/.config/whisper-push/"
echo "To remove config: rm -rf ~/.config/whisper-push"
