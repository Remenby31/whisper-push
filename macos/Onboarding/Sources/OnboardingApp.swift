import SwiftUI
import AppKit

/// Promotes the process to a regular foreground app and activates its window.
/// Inside the shipped Onboarding.app bundle this is redundant (the .app's
/// Info.plist already sets the activation policy), but it's idempotent there.
/// In `make onboarding-preview` we launch the SPM binary directly without a
/// bundle wrapper, so without this the SwiftUI WindowGroup creates the window
/// but macOS never activates the process and the window is never shown.
final class OnboardingAppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        // Bring the first window to the front (WindowGroup creates it before
        // this delegate fires, so it's already in NSApp.windows).
        NSApp.windows.first?.makeKeyAndOrderFront(nil)
    }
}

@main
struct OnboardingApp: App {
    @NSApplicationDelegateAdaptor(OnboardingAppDelegate.self) private var appDelegate
    @StateObject private var state = OnboardingState()

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
            .frame(width: 520, height: 440)
            .fixedSize()
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
    }
}

struct ContentView: View {
    @EnvironmentObject var state: OnboardingState

    var body: some View {
        ZStack(alignment: .top) {
            Group {
                switch state.currentStep {
                case .welcome:
                    WelcomeView()
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
        case .welcome:     return "1/5 Welcome"
        case .permissions: return "2/5 Permissions"
        case .model:       return "3/5 Model Picker"
        case .download:    return "4/5 Download"
        case .ready:       return "5/5 Ready"
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
