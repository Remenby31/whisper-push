import SwiftUI

@main
struct OnboardingApp: App {
    @StateObject private var state = OnboardingState()

    var body: some Scene {
        WindowGroup {
            ContentView()
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
        Group {
            switch state.currentStep {
            case .welcome:
                WelcomeView()
            case .model:
                ModelPickerView()
            case .download:
                DownloadView()
            case .permissions:
                PermissionsView()
            case .ready:
                ReadyView()
            }
        }
        .transition(.asymmetric(
            insertion: .move(edge: .trailing).combined(with: .opacity),
            removal: .move(edge: .leading).combined(with: .opacity)
        ))
    }
}
