import SwiftUI
import AppKit

/// Owns the one wizard window. AppKit creates and presents it directly instead
/// of a SwiftUI `WindowGroup`, for two reasons learned the hard way:
///
/// 1. Built against the macOS 26 SDK, a `WindowGroup` scene does not present
///    its window until the app becomes *active* — and a process spawned by our
///    menu-bar daemon is routinely denied activation (cooperative activation,
///    macOS 14+), so the wizard ran with **no window at all**. An `NSWindow` we
///    order front ourselves shows regardless of activation.
/// 2. Closing a SwiftUI window (red button / ⌘W) left the process alive with no
///    window. The daemon's `open -W` then found a running instance, poked it,
///    and nothing appeared — "Unlock…" / "I already have a license key" looked
///    dead until the orphan was killed. Last window closed ⇒ we quit.
@MainActor
final class OnboardingAppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let state = OnboardingState()
    private var window: NSWindow?

    /// The standalone payment/activation modal (menu bar → License, or the
    /// "Subscribe"/"Renew" notification button) is launched with `--license-only`.
    /// That popup must stay *above every other app* until the user acts on it —
    /// a normal window drops behind as soon as another app takes focus, so the
    /// user thinks nothing happened. The full first-launch wizard stays a normal
    /// window (it owns the whole session, so pinning it on top would be rude).
    private var isPaymentPopup: Bool { state.licenseOnly }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        let window = makeWindow()
        self.window = window
        bringToFront()
        // The daemon that launches us is a menu-bar accessory, and since macOS 14
        // activation is cooperative: our own `activate()` is ignored whenever the
        // user is busy elsewhere (typing in another app at that instant is
        // enough). The window is already on screen (orderFrontRegardless), but
        // keyboard focus needs the app active: the daemon yields activation to us
        // before launching; on our side keep re-asserting for a few seconds until
        // we actually are the active app (each attempt is idempotent and cheap).
        for delay in [0.25, 0.75, 1.5, 3.0, 5.0] {
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
                guard let self, !NSApp.isActive else { return }
                self.bringToFront()
            }
        }
    }

    /// The wizard is a one-window app: closing it (red button, ⌘W) means "I'm
    /// done" — never a windowless process lingering in the Dock that swallows
    /// the next launch (the daemon's `open -W` would just poke it and nothing
    /// would appear).
    ///
    /// This is deliberately bound to OUR window rather than
    /// `applicationShouldTerminateAfterLastWindowClosed`: that delegate answers
    /// for *any* window, and the SwiftUI scene machinery opens and closes one of
    /// its own during launch — which quit the app ~2 s after it appeared.
    func windowWillClose(_ notification: Notification) {
        guard (notification.object as? NSWindow) === window else { return }
        NSApplication.shared.terminate(nil)
    }

    /// Dock click / second `open` while running: show the window we have.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { window?.makeKeyAndOrderFront(nil) }
        bringToFront()
        return true
    }

    private func makeWindow() -> NSWindow {
        let hosting = NSHostingView(rootView: RootView().environmentObject(state))
        // The window follows SwiftUI's ideal size: 520×440, growing to 620 tall
        // while the checkout form is showing (see RootView).
        hosting.sizingOptions = [.preferredContentSize]
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 440),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        w.title = "Whisper Push"
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        w.isMovableByWindowBackground = true
        w.isReleasedWhenClosed = false
        w.contentView = hosting
        w.delegate = self // see windowWillClose: closing this window ends the app
        if isPaymentPopup {
            // Elevate to a floating popup so it stays on top even after the
            // user clicks back into another app, and follow them onto whatever
            // Space is active (the daemon can fire this from any context).
            // Otherwise the checkout is easy to lose behind the window that had
            // focus.
            w.level = .floating
            w.collectionBehavior.insert(.moveToActiveSpace)
            w.collectionBehavior.insert(.fullScreenAuxiliary)
        }
        center(w, on: screenWithMouse())
        return w
    }

    /// Centre on the screen the user is looking at (the one under the pointer),
    /// not necessarily the main display — the daemon fires the modal from
    /// wherever the user clicked the menu bar.
    private func center(_ w: NSWindow, on screen: NSScreen?) {
        guard let screen else { w.center(); return }
        let vf = screen.visibleFrame
        let size = w.frame.size
        w.setFrameOrigin(NSPoint(x: vf.midX - size.width / 2,
                                 y: vf.midY - size.height / 2 + vf.height * 0.08))
    }

    private func screenWithMouse() -> NSScreen? {
        let p = NSEvent.mouseLocation
        return NSScreen.screens.first { NSMouseInRect(p, $0.frame, false) } ?? NSScreen.main
    }

    /// Force the wizard window frontmost and key. `orderFrontRegardless()` is the
    /// key call: it raises the window above other apps' windows even when macOS
    /// denied activation (common for a process spawned by a background agent).
    private func bringToFront() {
        NSApp.activate() // honours the daemon's activation yield (macOS 14+)
        NSApp.activate(ignoringOtherApps: true)
        if let window {
            window.makeKeyAndOrderFront(nil)
            window.orderFrontRegardless()
        }
    }
}

@main
struct OnboardingApp: App {
    @NSApplicationDelegateAdaptor(OnboardingAppDelegate.self) private var appDelegate

    var body: some Scene {
        // The delegate owns the real window (see above). SwiftUI still needs a
        // scene to run the app lifecycle and provide the standard main menu
        // (Edit → Paste must exist for ⌘V to reach the license-key field); a
        // Settings scene never opens on its own, and its menu item is removed.
        Settings { EmptyView() }
            .commands { CommandGroup(replacing: .appSettings) {} }
    }
}

/// The window's content: the wizard plus the designer's ⌘←/⌘→ sweep shortcuts.
struct RootView: View {
    @EnvironmentObject var state: OnboardingState

    // The wizard is a compact 440 pt tall everywhere except the checkout, where
    // the payment form is taller — grow the window there so it fits with no
    // scroll (the hosting view's `preferredContentSize` sizing follows this).
    private let baseHeight: CGFloat = 440
    private let checkoutHeight: CGFloat = 620

    var body: some View {
        ZStack {
            ContentView()
            // Cmd+→ / Cmd+← let the designer sweep through the screens
            // without having to fill in every required field. Always on
            // — harmless in production (Rust hands the user advance via
            // taps anyway). The hidden buttons attach the shortcuts.
            Button(action: { state.advance() }) { Color.clear }
                .keyboardShortcut(.rightArrow, modifiers: .command)
                .frame(width: 0, height: 0)
                .opacity(0)
            Button(action: { state.sweep(-1) }) { Color.clear }
                .keyboardShortcut(.leftArrow, modifiers: .command)
                .frame(width: 0, height: 0)
                .opacity(0)
        }
        .frame(width: 520, height: state.expandedForCheckout ? checkoutHeight : baseHeight)
        .fixedSize()
        // The wizard is designed for a light, branded surface (racing-green
        // text on light). Pin a light appearance so dark-mode Macs don't get
        // dark-on-dark (the contrast bug); also gives every screen a
        // consistent look in the DMG regardless of system setting.
        .preferredColorScheme(.light)
    }
}

struct ContentView: View {
    @EnvironmentObject var state: OnboardingState

    var body: some View {
        // Standalone payment modal (menu bar → License → Subscribe…).
        if state.licenseOnly {
            return AnyView(LicenseView())
        }
        return AnyView(fullWizard)
    }

    private var fullWizard: some View {
        ZStack(alignment: .top) {
            Group {
                switch state.currentStep {
                case .welcome:
                    WelcomeView()
                case .license:
                    LicenseView()
                case .permissions:
                    PermissionsView()
                case .model:
                    ModelPickerView()
                case .download:
                    DownloadView()
                case .ready:
                    ReadyView()
                }
            }
            .transition(.asymmetric(
                insertion: .move(edge: .trailing).combined(with: .opacity),
                removal: .move(edge: .leading).combined(with: .opacity)
            ))

            // Visible-only in design-preview mode. Tiny floating step
            // indicator so the designer always knows which screen they
            // are on while sweeping with Cmd+arrows.
            if state.isDesignPreview {
                DesignPreviewBadge(state: state)
                    .padding(.top, 8)
            }
        }
    }
}

/// Small pill at the top of the wizard, only shown when running with
/// `--design-preview`. Echoes the current step and the keyboard shortcuts.
private struct DesignPreviewBadge: View {
    @ObservedObject var state: OnboardingState

    private var stepName: String {
        switch state.currentStep {
        case .welcome:     return "1/6 Welcome"
        case .permissions: return "2/6 Permissions"
        case .license:     return "3/6 Subscription"
        case .model:       return "4/6 Model Picker"
        case .download:    return "5/6 Download"
        case .ready:       return "6/6 Ready"
        }
    }

    var body: some View {
        Text("PREVIEW · \(stepName) · ⌘← ⌘→")
            .font(.system(size: 10, weight: .semibold, design: .monospaced))
            .foregroundStyle(Color.brandCream)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(
                Capsule().fill(Color.brandGreen.opacity(0.85))
            )
    }
}
