import SwiftUI
import AppKit

/// Promotes the process to a regular foreground app and activates its window.
/// Inside the shipped Onboarding.app bundle this is redundant (the .app's
/// Info.plist already sets the activation policy), but it's idempotent there.
/// In `make onboarding-preview` we launch the SPM binary directly without a
/// bundle wrapper, so without this the SwiftUI WindowGroup creates the window
/// but macOS never activates the process and the window is never shown.
final class OnboardingAppDelegate: NSObject, NSApplicationDelegate {
    /// The standalone payment modal (menu bar → License → Subscription, or the
    /// "Upgrade"/"Renew" notification button) is launched with `--license-only`.
    /// That popup must stay *above every other app* until the user acts on it —
    /// a normal window drops behind as soon as another app takes focus, so the
    /// user thinks nothing happened. The full first-launch wizard stays a normal
    /// window (it owns the whole session, so pinning it on top would be rude).
    private var isPaymentPopup: Bool {
        ProcessInfo.processInfo.arguments.contains("--license-only")
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        bringToFront()
        // The daemon that launches us is a menu-bar accessory, and a directly
        // exec'd binary can lose the activation race — the payment window then
        // opens *behind* whatever app is frontmost and looks like it never
        // opened. Re-assert once the run loop settles so it's reliably on top.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [weak self] in
            self?.bringToFront()
        }
    }

    /// Force the wizard window frontmost and key. `orderFrontRegardless()` is the
    /// key call: it raises the window above other apps' windows even when macOS
    /// denied activation (common for a process spawned by a background agent).
    private func bringToFront() {
        NSApp.activate(ignoringOtherApps: true)
        // WindowGroup creates the window before the delegate fires, so it's
        // already in NSApp.windows.
        if let window = NSApp.windows.first {
            if isPaymentPopup {
                // Elevate to a floating popup so it stays on top even after the
                // user clicks back into another app, and follow them onto
                // whatever Space is active (the daemon can fire this from any
                // context). Otherwise the checkout is easy to lose behind the
                // window that had focus.
                window.level = .floating
                window.collectionBehavior.insert(.moveToActiveSpace)
                window.collectionBehavior.insert(.fullScreenAuxiliary)
            }
            window.makeKeyAndOrderFront(nil)
            window.orderFrontRegardless()
        }
    }
}

@main
struct OnboardingApp: App {
    @NSApplicationDelegateAdaptor(OnboardingAppDelegate.self) private var appDelegate
    @StateObject private var state = OnboardingState()

    // The wizard is a compact 440 pt tall everywhere except the checkout, where
    // the payment form is taller — grow the window there so it fits with no
    // scroll (`.windowResizability(.contentSize)` makes the window follow this).
    private let baseHeight: CGFloat = 440
    private let checkoutHeight: CGFloat = 620

    var body: some Scene {
        WindowGroup {
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
            .environmentObject(state)
            .frame(width: 520, height: state.expandedForCheckout ? checkoutHeight : baseHeight)
            .fixedSize()
            // The wizard is designed for a light, branded surface (racing-green
            // text on light). Pin a light appearance so dark-mode Macs don't get
            // dark-on-dark (the contrast bug); also gives every screen a
            // consistent look in the DMG regardless of system setting.
            .preferredColorScheme(.light)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
    }
}

struct ContentView: View {
    @EnvironmentObject var state: OnboardingState

    var body: some View {
        // Standalone payment modal (menu bar → License → Subscription).
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
