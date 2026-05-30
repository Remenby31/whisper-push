# Whisper Push — Rust build helpers
.PHONY: build release onboarding bundle sign dmg pkg clean check deploy install uninstall

APP_NAME = Whisper Push
APP_DIR = build/$(APP_NAME).app
BINARY = target/release/whisper-push
SIGN_ID = Developer ID Application: Baptiste Cruvellier (3SNT64YKAS)
BUNDLE_ID = com.whisper-push.app

# Onboarding wizard sub-bundle. Lives inside the parent .app but has its
# OWN bundle ID via resources/Onboarding-Info.plist. This is what stops
# macOS from killing the wizard with the "Quit and reopen" popup when the
# user toggles a TCC permission for the daemon.
WIZARD_BUNDLE = $(APP_DIR)/Contents/Library/Helpers/Onboarding.app
WIZARD_BUNDLE_ID = com.whisper-push.onboarding

# Install target: a stable /Applications location + login autostart agent
INSTALL_DIR = /Applications
INSTALLED_APP = $(INSTALL_DIR)/$(APP_NAME).app
LAUNCH_AGENT = $(HOME)/Library/LaunchAgents/$(BUNDLE_ID).plist


# Debug build
build:
	cargo build
	@echo "✓ Debug build complete"

# Release build (default features include metal + parakeet + voxtral)
release:
	cargo build --release
	@echo "✓ Release build complete"

# Check (no build, just type-check)
check:
	cargo check
	@echo "✓ Check passed"

# Build the SwiftUI onboarding wizard
onboarding:
	@cd macos/Onboarding && swift build -c release 2>&1 | tail -1
	@echo "✓ Onboarding wizard built"

# Create macOS .app bundle
bundle: release onboarding
	@rm -rf "$(APP_DIR)"
	@mkdir -p "$(APP_DIR)/Contents/MacOS"
	@mkdir -p "$(APP_DIR)/Contents/Resources"
	@cp $(BINARY) "$(APP_DIR)/Contents/MacOS/whisper-push"
	@cp resources/Info.plist "$(APP_DIR)/Contents/"
	@echo "APPL????" > "$(APP_DIR)/Contents/PkgInfo"
	@# Brand app icon
	@test -f resources/AppIcon.icns && cp resources/AppIcon.icns "$(APP_DIR)/Contents/Resources/AppIcon.icns" || echo "  (warning: resources/AppIcon.icns missing — bundle will have no icon)"
	@# Onboarding wizard — full sub-bundle (own Info.plist + Bundle ID) so
	@# TCC and the "Quit and reopen" popup treat it as a separate app from
	@# the daemon. See resources/Onboarding-Info.plist.
	@mkdir -p "$(WIZARD_BUNDLE)/Contents/MacOS"
	@mkdir -p "$(WIZARD_BUNDLE)/Contents/Resources"
	@cp macos/Onboarding/.build/arm64-apple-macosx/release/Onboarding "$(WIZARD_BUNDLE)/Contents/MacOS/Onboarding"
	@cp resources/Onboarding-Info.plist "$(WIZARD_BUNDLE)/Contents/Info.plist"
	@echo "APPL????" > "$(WIZARD_BUNDLE)/Contents/PkgInfo"
	@# Brand app icon for the wizard's Dock entry + window-title icon.
	@# Same icns as the main bundle (referenced by CFBundleIconFile=AppIcon
	@# in Onboarding-Info.plist).
	@test -f resources/AppIcon.icns && cp resources/AppIcon.icns "$(WIZARD_BUNDLE)/Contents/Resources/AppIcon.icns" || echo "  (warning: resources/AppIcon.icns missing — wizard will have no icon)"
	@echo "✓ App bundle created at $(APP_DIR)"
	@echo "  └── wizard sub-bundle at Contents/Library/Helpers/Onboarding.app"

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

# Create a distributable DMG (model downloads on first launch). Ad-hoc
# signed — no Developer ID in keychain. Bottom-up so the wizard sub-bundle
# keeps its own identity (com.whisper-push.onboarding) distinct from the
# daemon's (com.whisper-push.app); that's what keeps the wizard alive
# during onboarding when the user grants Accessibility / Input Monitoring
# (macOS would otherwise pop "Quit and reopen" for whatever process holds
# the matching bundle ID).
dmg: bundle
	@# Sign the wizard sub-bundle first (standalone, no entitlements).
	@codesign --force --options runtime -s - \
		"$(WIZARD_BUNDLE)/Contents/MacOS/Onboarding"
	@codesign --force --options runtime -s - \
		"$(WIZARD_BUNDLE)"
	@# Then the daemon binary with its entitlements.
	@codesign --force --options runtime -s - \
		-i "$(BUNDLE_ID)" \
		--entitlements resources/entitlements.plist \
		"$(APP_DIR)/Contents/MacOS/whisper-push"
	@# Then the outer .app wrap (no --deep — children are already signed).
	@codesign --force --options runtime -s - \
		--entitlements resources/entitlements.plist \
		"$(APP_DIR)"
	@echo "✓ App ad-hoc signed (DMG path) — right-click → Open the .app to bypass Gatekeeper"
	@# Package the DMG (drag-to-Applications layout via create-dmg).
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
	@du -h "build/dist/Whisper-Push-macOS-arm64.dmg" | sed 's|^|  |'
	@echo "✓ DMG created at build/dist/Whisper-Push-macOS-arm64.dmg"

# Self-contained ad-hoc PKG installer with auto-launch postinstall.
# Independent of the `dmg`/`sign` targets so it works without a Developer ID
# in the keychain. The postinstall script (resources/pkg-scripts/postinstall)
# launches Whisper Push as the logged-in user right after install so the
# onboarding wizard appears with zero extra clicks.
PKG_VERSION = $(shell awk -F'"' '/^version = / { print $$2; exit }' Cargo.toml)
pkg: bundle
	@echo "Ad-hoc signing the bundle for PKG ..."
	@# Bottom-up: wizard sub-bundle first (own Info.plist with
	@# com.whisper-push.onboarding — sign as standalone, no entitlements,
	@# so TCC treats it as a separate identity), then the daemon with its
	@# entitlements, then the outer .app wrap.
	@codesign --force --options runtime -s - \
		"$(WIZARD_BUNDLE)/Contents/MacOS/Onboarding"
	@codesign --force --options runtime -s - \
		"$(WIZARD_BUNDLE)"
	@codesign --force --options runtime -s - \
		-i "$(BUNDLE_ID)" \
		--entitlements resources/entitlements.plist \
		"$(APP_DIR)/Contents/MacOS/whisper-push"
	@codesign --force --options runtime -s - \
		--entitlements resources/entitlements.plist \
		"$(APP_DIR)"
	@echo "Packaging PKG (auto-launch on install) ..."
	@mkdir -p build/dist
	@# Stage in mktemp dirs so stale (potentially root-owned) state from
	@# previous pkgbuild runs never blocks the next build.
	@STAGE_ROOT=$$(mktemp -d "/tmp/wp-pkg-root.XXXXXX"); \
	 STAGE_SCRIPTS=$$(mktemp -d "/tmp/wp-pkg-scripts.XXXXXX"); \
	 cp -R "$(APP_DIR)" "$$STAGE_ROOT/$(APP_NAME).app"; \
	 cp resources/pkg-scripts/postinstall "$$STAGE_SCRIPTS/postinstall"; \
	 chmod +x "$$STAGE_SCRIPTS/postinstall"; \
	 rm -f "build/dist/Whisper-Push-macOS-arm64.pkg"; \
	 pkgbuild \
		--root "$$STAGE_ROOT" \
		--identifier "$(BUNDLE_ID)" \
		--version "$(PKG_VERSION)" \
		--install-location /Applications \
		--scripts "$$STAGE_SCRIPTS" \
		"build/dist/Whisper-Push-macOS-arm64.pkg" >/dev/null; \
	 rm -rf "$$STAGE_ROOT" "$$STAGE_SCRIPTS" 2>/dev/null || true
	@du -h "build/dist/Whisper-Push-macOS-arm64.pkg" | sed 's|^|  |'
	@echo "✓ PKG created at build/dist/Whisper-Push-macOS-arm64.pkg"
	@echo "  (ad-hoc: right-click → Open the .pkg the first time to bypass Gatekeeper)"

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

# Build + sign + launch (dev workflow)
deploy: sign
	@open "$(APP_DIR)"
	@echo "✓ Whisper Push launched"

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
