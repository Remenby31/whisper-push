# Whisper Push — Rust build helpers
.PHONY: build release bundle sign dmg clean check

APP_NAME = Whisper Push
APP_DIR = build/$(APP_NAME).app
BINARY = target/release/whisper-push
SIGN_ID = Developer ID Application: Baptiste Cruvellier (3SNT64YKAS)
BUNDLE_ID = com.whisper-push.app

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

# Create macOS .app bundle
bundle: release
	@rm -rf "$(APP_DIR)"
	@mkdir -p "$(APP_DIR)/Contents/MacOS"
	@mkdir -p "$(APP_DIR)/Contents/Resources"
	@cp $(BINARY) "$(APP_DIR)/Contents/MacOS/whisper-push"
	@cp resources/Info.plist "$(APP_DIR)/Contents/"
	@echo "APPL????" > "$(APP_DIR)/Contents/PkgInfo"
	@# Copy icon if available
	@test -f macos/whisper-push.icns && cp macos/whisper-push.icns "$(APP_DIR)/Contents/Resources/AppIcon.icns" || true
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

# Build + sign + launch (dev workflow)
deploy: sign
	@open "$(APP_DIR)"
	@echo "✓ Whisper Push launched"

# Clean
clean:
	cargo clean
	rm -rf build/
	@echo "✓ Clean"
