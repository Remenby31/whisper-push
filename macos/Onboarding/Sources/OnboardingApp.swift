import SwiftUI

@main
struct OnboardingApp: App {
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
