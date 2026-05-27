# Whisper Push — Rust build helpers
.PHONY: build release bundle sign dmg clean check deploy install uninstall

APP_NAME = Whisper Push
APP_DIR = build/$(APP_NAME).app
BINARY = target/release/whisper-push
SIGN_ID = Developer ID Application: Baptiste Cruvellier (3SNT64YKAS)
BUNDLE_ID = com.whisper-push.app

# Install target: a stable /Applications location + login autostart agent
INSTALL_DIR = /Applications
INSTALLED_APP = $(INSTALL_DIR)/$(APP_NAME).app
LAUNCH_AGENT = $(HOME)/Library/LaunchAgents/$(BUNDLE_ID).plist

# Debug build
build:
	cargo build
	@echo "✓ Debug build complete"

# Release build (all backends on macOS)
release:
	cargo build --release --features "metal,parakeet,voxtral"
	@echo "✓ Release build complete (Metal + Parakeet + Voxtral)"

# Check (no build, just type-check)
check:
	cargo check
	@echo "✓ Check passed"

# Create macOS .app bundle
bundle: release
	@rm -rf "$(APP_DIR)"
	@mkdir -p "$(APP_DIR)/Contents/MacOS"
	@mkdir -p "$(APP_DIR)/Contents/Resources"
	@cp $(BINARY) "$(APP_DIR)/Contents/MacOS/whisper-push"
	@cp resources/Info.plist "$(APP_DIR)/Contents/"
	@echo "APPL????" > "$(APP_DIR)/Contents/PkgInfo"
	@# Brand app icon (PADDOCK squircle, generated from the brand kit)
	@test -f resources/AppIcon.icns && cp resources/AppIcon.icns "$(APP_DIR)/Contents/Resources/AppIcon.icns" || echo "  (warning: resources/AppIcon.icns missing — bundle will have no icon)"
	@echo "✓ App bundle created at $(APP_DIR)"

# Sign the app with Developer ID
sign: bundle
	@codesign --force --options runtime \
		-s "$(SIGN_ID)" \
		-i "$(BUNDLE_ID)" \
		--entitlements resources/entitlements.plist \
		--timestamp \
		"$(APP_DIR)/Contents/MacOS/whisper-push"
	@codesign --force --options runtime --deep \
		-s "$(SIGN_ID)" \
		--entitlements resources/entitlements.plist \
		--timestamp \
		"$(APP_DIR)"
	@echo "✓ App signed with Developer ID"

# Create DMG
dmg: sign
	@mkdir -p build/dist
	@rm -f "build/dist/Whisper-Push-macOS-arm64.dmg"
	@if command -v create-dmg > /dev/null; then \
		rm -rf build/dmg-stage && mkdir -p build/dmg-stage; \
		cp -R "$(APP_DIR)" build/dmg-stage/; \
		create-dmg \
			--volname "$(APP_NAME)" \
			--window-size 600 380 \
			--icon-size 120 \
			--icon "$(APP_NAME).app" 150 190 \
			--app-drop-link 450 190 \
			--hide-extension "$(APP_NAME).app" \
			"build/dist/Whisper-Push-macOS-arm64.dmg" build/dmg-stage || true; \
		rm -rf build/dmg-stage; \
	else \
		hdiutil create -volname "$(APP_NAME)" -srcfolder "$(APP_DIR)" -ov -format UDZO \
			"build/dist/Whisper-Push-macOS-arm64.dmg"; \
	fi
	@echo "✓ DMG created at build/dist/Whisper-Push-macOS-arm64.dmg"

# Notarize the DMG (requires Apple Developer account + App Store Connect API key)
notarize: dmg
	@echo "Notarizing..."
	@xcrun notarytool submit "build/dist/Whisper-Push-macOS-arm64.dmg" \
		--keychain-profile "whisper-push" \
		--wait
	@xcrun stapler staple "build/dist/Whisper-Push-macOS-arm64.dmg"
	@echo "✓ DMG notarized and stapled"

# Full release: build + sign + DMG + notarize
release-macos: notarize
	@echo "✓ Release ready at build/dist/Whisper-Push-macOS-arm64.dmg"

# Build + sign + relaunch (dev workflow)
deploy: sign
	@if pgrep -f "whisper-push" > /dev/null 2>&1; then \
		echo "Stopping whisper-push..."; \
		pkill -f "whisper-push" || true; \
		sleep 1; \
	fi
	@open "$(APP_DIR)"
	@echo "✓ Whisper Push relaunched"

# Install into /Applications (shows in Launchpad/Finder) + register the
# login autostart agent pointing at the installed copy.
install: sign
	@echo "Installing to $(INSTALLED_APP)..."
	@pkill -f "Whisper Push.app/Contents/MacOS/whisper-push" 2>/dev/null || true
	@sleep 1
	@rm -rf "$(INSTALLED_APP)"
	@cp -R "$(APP_DIR)" "$(INSTALL_DIR)/"
	@printf '<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0">\n<dict>\n\t<key>Label</key>\n\t<string>$(BUNDLE_ID)</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>$(INSTALLED_APP)/Contents/MacOS/whisper-push</string>\n\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n\t<key>ProcessType</key>\n\t<string>Interactive</string>\n\t<key>StandardOutPath</key>\n\t<string>/tmp/whisper-push.out.log</string>\n\t<key>StandardErrorPath</key>\n\t<string>/tmp/whisper-push.err.log</string>\n</dict>\n</plist>\n' > "$(LAUNCH_AGENT)"
	@launchctl bootout gui/$$(id -u)/$(BUNDLE_ID) 2>/dev/null || true
	@launchctl bootstrap gui/$$(id -u) "$(LAUNCH_AGENT)" 2>/dev/null || true
	@echo "✓ Installed to /Applications + registered login autostart"
	@echo "  (ad-hoc signed: you may need to re-grant Accessibility/Mic on first launch)"

# Remove the installed app + autostart agent
uninstall:
	@launchctl bootout gui/$$(id -u)/$(BUNDLE_ID) 2>/dev/null || true
	@rm -f "$(LAUNCH_AGENT)"
	@rm -rf "$(INSTALLED_APP)"
	@echo "✓ Uninstalled from /Applications + removed autostart agent"

# Clean
clean:
	cargo clean
	rm -rf build/
	@echo "✓ Clean"
