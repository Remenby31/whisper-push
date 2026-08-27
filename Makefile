# Whisper Push — Rust build helpers
.PHONY: dmg-artwork build release onboarding onboarding-preview bundle sign \
	notarize-app dmg dmg-package zip zip-package notarize notarize-dmg \
	notarize-ci release-macos clean check deploy install uninstall

# The release chain is strictly ordered — sign, notarize, staple, package —
# and a stapled ticket does not survive a re-sign. `make -j` would happily run
# packaging next to notarization, so opt the whole file out of parallelism;
# nothing here is worth parallelising anyway.
.NOTPARALLEL:

APP_NAME = Whisper Push
APP_DIR = build/$(APP_NAME).app
BINARY = target/release/whisper-push
SIGN_ID = Developer ID Application: Baptiste Cruvellier (3SNT64YKAS)
BUNDLE_ID = com.whisper-push.app

# Ad-hoc signing has no timestamp service to talk to, so asking for one is a
# hard codesign error. Real Developer ID signing needs the secure timestamp:
# notarization rejects anything without it.
TIMESTAMP = $(if $(filter -,$(SIGN_ID)),--timestamp=none,--timestamp)

# Notarization credentials. CI passes an App Store Connect API key
# (APPLE_API_KEY_PATH / _ID / _ISSUER_ID); a local machine falls back to the
# "whisper-push" keychain profile written by `notarytool store-credentials`.
NOTARY_AUTH = $(if $(APPLE_API_KEY_PATH),--key "$(APPLE_API_KEY_PATH)" --key-id "$(APPLE_API_KEY_ID)" --issuer "$(APPLE_API_ISSUER_ID)",--keychain-profile "whisper-push")

# The two macOS release artefacts: the DMG humans install from, and the ZIP
# the in-app updater downloads.
DMG = build/dist/Whisper-Push-macOS-arm64.dmg
ZIP = build/dist/Whisper-Push-macOS-arm64.zip

# Onboarding wizard sub-bundle — own Info.plist + own bundle ID so the
# wizard isn't targeted by "Quit and reopen" popups when the user grants
# a TCC permission for the daemon.
WIZARD_BUNDLE = $(APP_DIR)/Contents/Library/Helpers/Onboarding.app
WIZARD_BUNDLE_ID = com.whisper-push.onboarding

# Installer window artwork. These numbers ARE the background image: the icon
# and drop-link centres are drawn into resources/dmg-background.svg (the arrow
# points from one to the other, and the label band under them is left empty).
# Change one and you must redraw the background (resources/dmg-background.svg
# carries the same coordinates in its own comments).
DMG_BACKGROUND = resources/dmg-background.tiff
DMG_WINDOW_W = 640
DMG_WINDOW_H = 480
DMG_ICON_SIZE = 120
DMG_ICON_Y = 250
DMG_APP_X = 170
DMG_DROP_X = 470

# Install target: a stable /Applications location + login autostart agent
INSTALL_DIR = /Applications
INSTALLED_APP = $(INSTALL_DIR)/$(APP_NAME).app
LAUNCH_AGENT = $(HOME)/Library/LaunchAgents/$(BUNDLE_ID).plist


# Debug build
build:
	cargo build
	@echo "✓ Debug build complete"

# Release build. Default features = parakeet + voxtral; on macOS, Metal is
# auto-enabled via a target-specific whisper-rs dependency, so this is GPU-built.
release:
	cargo build --release
	@echo "✓ Release build complete"

# Check (no build, just type-check)
check:
	cargo check
	@echo "✓ Check passed"

# Build the SwiftUI onboarding wizard.
# No `| tail -1`: piping into tail swallowed swift build's exit code (the
# pipeline's status is tail's, always 0), so a failed build still printed
# "✓ built" and then died on the cp in `bundle` with a useless one-line
# "error: fatalError". Let swift build fail loudly and stop make.
onboarding:
	@cd macos/Onboarding && swift build -c release
	@echo "✓ Onboarding wizard built"

# Launch the wizard in DESIGN PREVIEW mode — no real downloader, no daemon
# probe, no JSON-to-stdout exit. Use STEP=... to jump to a screen:
#   make onboarding-preview                → starts at welcome
#   make onboarding-preview STEP=download  → starts at download
# Inside the window, Cmd+→ / Cmd+← sweep through the screens. The
# `onboarding-design` Claude skill drives this target for fast iteration.
STEP ?= welcome
onboarding-preview: onboarding
	@./macos/Onboarding/.build/release/Onboarding \
		--design-preview \
		--start-at $(STEP) \
		--hardware "Apple M4 Max" \
		--recommended "parakeet"

# Create macOS .app bundle
bundle: release onboarding
	@rm -rf "$(APP_DIR)"
	@mkdir -p "$(APP_DIR)/Contents/MacOS"
	@mkdir -p "$(APP_DIR)/Contents/Resources"
	@cp $(BINARY) "$(APP_DIR)/Contents/MacOS/whisper-push"
	@cp resources/Info.plist "$(APP_DIR)/Contents/"
	@echo "APPL????" > "$(APP_DIR)/Contents/PkgInfo"
	@# Brand app icon
	@test -f resources/AppIcon.icns && cp resources/AppIcon.icns "$(APP_DIR)/Contents/Resources/AppIcon.icns" || echo "  (warning: resources/AppIcon.icns missing - bundle will have no icon)"
	@# Onboarding wizard as a separate sub-bundle (own Info.plist + bundle ID).
	@mkdir -p "$(WIZARD_BUNDLE)/Contents/MacOS"
	@mkdir -p "$(WIZARD_BUNDLE)/Contents/Resources"
	@cp macos/Onboarding/.build/arm64-apple-macosx/release/Onboarding "$(WIZARD_BUNDLE)/Contents/MacOS/Onboarding"
	@# Swift Package Manager emits resources (logo PNG) into a sibling
	@# .bundle. Inside an .app, it must live in Contents/Resources/ so
	@# Bundle.module finds it via Bundle.main.resourceURL, and so codesign
	@# doesn't choke on a directory inside Contents/MacOS/.
	@if [ -d macos/Onboarding/.build/arm64-apple-macosx/release/Onboarding_Onboarding.bundle ]; then \
		cp -R macos/Onboarding/.build/arm64-apple-macosx/release/Onboarding_Onboarding.bundle "$(WIZARD_BUNDLE)/Contents/Resources/"; \
	fi
	@cp resources/Onboarding-Info.plist "$(WIZARD_BUNDLE)/Contents/Info.plist"
	@echo "APPL????" > "$(WIZARD_BUNDLE)/Contents/PkgInfo"
	@test -f resources/AppIcon.icns && cp resources/AppIcon.icns "$(WIZARD_BUNDLE)/Contents/Resources/AppIcon.icns" || true
	@echo "✓ App bundle created at $(APP_DIR)"
	@echo "  L wizard sub-bundle at Contents/Library/Helpers/Onboarding.app"

# Sign the bundle, bottom-up: the wizard sub-bundle first, then the daemon
# binary, then the outer .app wrap. Each signature seals everything below it,
# so signing top-down would record the outer seal before its contents settled
# and every nested signature after it would break that seal.
#
# This is the ONE place the bundle gets signed. `dmg` and `zip` used to carry
# their own copies of these four codesign calls, which drifted (the DMG path
# never signed the wizard with a timestamp) and — worse — re-signing after
# `notarize-app` would strip the stapled ticket back off.
sign: bundle
	@codesign --force --options runtime $(TIMESTAMP) \
		-s "$(SIGN_ID)" \
		"$(WIZARD_BUNDLE)/Contents/MacOS/Onboarding"
	@codesign --force --options runtime $(TIMESTAMP) \
		-s "$(SIGN_ID)" \
		"$(WIZARD_BUNDLE)"
	@codesign --force --options runtime $(TIMESTAMP) \
		-s "$(SIGN_ID)" \
		-i "$(BUNDLE_ID)" \
		--entitlements resources/entitlements.plist \
		"$(APP_DIR)/Contents/MacOS/whisper-push"
	@codesign --force --options runtime $(TIMESTAMP) \
		-s "$(SIGN_ID)" \
		--entitlements resources/entitlements.plist \
		"$(APP_DIR)"
	@if [ "$(SIGN_ID)" = "-" ]; then \
		echo "✓ App ad-hoc signed - right-click then Open to bypass Gatekeeper"; \
	else \
		echo "✓ App signed with Developer ID (hardened runtime, timestamped)"; \
	fi

# Notarize the .app itself and staple the ticket INTO the bundle.
#
# Stapling the DMG alone leaves the app inside it ticketless: Gatekeeper then
# has to reach Apple over the network to clear it, and the copy the in-app
# updater unpacks from the ZIP carries no ticket at all. A stapled bundle
# validates offline wherever it travels — DMG install and updater alike.
#
# Nothing may re-sign the bundle after this point; a fresh signature drops the
# ticket. Hence `dmg-package` and `zip-package` only package.
notarize-app: sign
	@if [ "$(SIGN_ID)" = "-" ]; then \
		echo "⚠ ad-hoc signed - skipping notarization (nothing to staple)"; \
	else \
		mkdir -p build/dist; \
		rm -f build/notarize-app.zip; \
		echo "Notarizing the .app..."; \
		(cd build && ditto -c -k --sequesterRsrc --keepParent "$(APP_NAME).app" notarize-app.zip); \
		xcrun notarytool submit build/notarize-app.zip $(NOTARY_AUTH) --wait; \
		xcrun stapler staple "$(APP_DIR)"; \
		rm -f build/notarize-app.zip; \
		echo "✓ .app notarized, ticket stapled into the bundle"; \
	fi

# Build the distributable DMG from the already-signed (and, in a release,
# already-stapled) bundle. Signing lives in `sign`; this target only packages.
dmg: notarize-app dmg-package

dmg-package:
	@# Package the DMG with the pixel-perfect drag-to-Applications layout via
	@# create-dmg (install it with `brew install create-dmg`). If it's missing or
	@# fails, fall back to a plain image that STILL carries an Applications
	@# drop-link, so users can always drag-to-install.
	@mkdir -p build/dist
	@rm -f "$(DMG)"
	@if command -v create-dmg > /dev/null; then \
		rm -rf build/dmg-stage && mkdir -p build/dmg-stage; \
		ditto "$(APP_DIR)" "build/dmg-stage/$(APP_NAME).app"; \
		BG=""; \
		if [ -f "$(DMG_BACKGROUND)" ]; then \
			BG="--background $(DMG_BACKGROUND)"; \
		else \
			echo "⚠ $(DMG_BACKGROUND) missing — DMG will have no artwork"; \
		fi; \
		create-dmg \
			--volname "$(APP_NAME)" \
			$$BG \
			--window-size $(DMG_WINDOW_W) $(DMG_WINDOW_H) \
			--icon-size $(DMG_ICON_SIZE) \
			--icon "$(APP_NAME).app" $(DMG_APP_X) $(DMG_ICON_Y) \
			--app-drop-link $(DMG_DROP_X) $(DMG_ICON_Y) \
			--hide-extension "$(APP_NAME).app" \
			"$(DMG)" build/dmg-stage || true; \
		rm -rf build/dmg-stage; \
	else \
		echo "⚠ create-dmg not found — run 'brew install create-dmg' for the styled DMG"; \
	fi
	@if [ ! -f "$(DMG)" ]; then \
		echo "→ building drag-to-Applications fallback DMG (no styled background)"; \
		rm -rf build/dmg-stage && mkdir -p build/dmg-stage; \
		ditto "$(APP_DIR)" "build/dmg-stage/$(APP_NAME).app"; \
		ln -s /Applications build/dmg-stage/Applications; \
		hdiutil create -volname "$(APP_NAME)" -srcfolder build/dmg-stage -ov -format UDZO \
			"$(DMG)"; \
		rm -rf build/dmg-stage; \
	fi
	@# Sign the disk image itself. Without this the .dmg carries "no usable
	@# signature" of its own: only the .app inside it was ever signed, so
	@# Gatekeeper has nothing to check before the image is mounted.
	@codesign --force $(TIMESTAMP) -s "$(SIGN_ID)" "$(DMG)"
	@du -h "$(DMG)" | sed 's|^|  |'
	@echo "✓ DMG created and signed at $(DMG)"

# Re-render the installer artwork from its SVG source. Needs librsvg
# (`brew install librsvg`); the rendered files are committed, so a plain
# `make dmg` — and CI — never needs it.
# The .tiff pairs 1x and 2x in one file, which is how a DMG background goes
# Retina: Finder picks the 144 dpi directory on a HiDPI display.
.PHONY: dmg-artwork
dmg-artwork:
	@rsvg-convert -w $(DMG_WINDOW_W) -h $(DMG_WINDOW_H) resources/dmg-background.svg \
		-o resources/dmg-background.png
	@rsvg-convert -w $$(( $(DMG_WINDOW_W) * 2 )) -h $$(( $(DMG_WINDOW_H) * 2 )) \
		resources/dmg-background.svg -o resources/dmg-background@2x.png
	@tiffutil -cathidpicheck resources/dmg-background.png resources/dmg-background@2x.png \
		-out resources/dmg-background.tiff > /dev/null
	@echo "✓ DMG artwork re-rendered (1x + 2x → resources/dmg-background.tiff)"

# ZIP the signed+stapled .app bundle (this is what the in-app updater
# downloads). ditto preserves the code signature, the xattrs, and the
# notarization ticket `notarize-app` stapled in.
#
# A ZIP can't be stapled itself — `stapler` refuses archives — which is
# exactly why the ticket has to be inside the bundle before we zip it.
zip: notarize-app zip-package

zip-package:
	@mkdir -p build/dist
	@cd build && ditto -c -k --sequesterRsrc --keepParent "$(APP_NAME).app" "$(CURDIR)/$(ZIP)"
	@echo "✓ ZIP created at $(ZIP)"

# Notarize the DMG and staple its ticket, so Gatekeeper clears the image
# offline. One target for both callers: NOTARY_AUTH picks the App Store
# Connect API key when CI passes one, and the local keychain profile
# otherwise — the old notarize / notarize-ci split only differed in that.
notarize-dmg:
	@if [ "$(SIGN_ID)" = "-" ]; then \
		echo "⚠ ad-hoc signed - skipping DMG notarization"; \
	else \
		echo "Notarizing the DMG..."; \
		xcrun notarytool submit "$(DMG)" $(NOTARY_AUTH) --wait; \
		xcrun stapler staple "$(DMG)"; \
		echo "✓ DMG notarized and stapled"; \
	fi

notarize: dmg notarize-dmg

# Kept so an older invocation still works; NOTARY_AUTH makes it the same path.
notarize-ci: notarize

# Everything a macOS release ships, in one pass: sign once, notarize the .app
# once, then package the DMG and the updater ZIP from that same stapled
# bundle, and notarize the image itself. Running `make dmg` and `make zip`
# back to back would sign and notarize the app twice over.
release-macos: notarize-app dmg-package notarize-dmg zip-package
	@echo "✓ Release ready: $(DMG) + $(ZIP)"

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
	@mkdir -p "$(HOME)/Library/Application Support/whisper-push/logs"
	@printf '<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0">\n<dict>\n\t<key>Label</key>\n\t<string>$(BUNDLE_ID)</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>$(INSTALLED_APP)/Contents/MacOS/whisper-push</string>\n\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n\t<key>ProcessType</key>\n\t<string>Interactive</string>\n\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n\t<key>ThrottleInterval</key>\n\t<integer>10</integer>\n\t<key>StandardOutPath</key>\n\t<string>$(HOME)/Library/Application Support/whisper-push/logs/launchd-stdout.log</string>\n\t<key>StandardErrorPath</key>\n\t<string>$(HOME)/Library/Application Support/whisper-push/logs/launchd-stderr.log</string>\n</dict>\n</plist>\n' > "$(LAUNCH_AGENT)"
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
