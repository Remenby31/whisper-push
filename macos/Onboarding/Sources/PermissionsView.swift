import SwiftUI

/// Step 2 of 5. Minimal text per user feedback: just the heading, three
/// permission rows (flat, no card backgrounds), and a CTA.
struct PermissionsView: View {
    @EnvironmentObject var state: OnboardingState
    @StateObject private var poller = PermissionsPoller()

    var body: some View {
        VStack(spacing: 16) {
            Text("Grant permissions")
                .font(.system(size: 24, weight: .bold))
                .foregroundStyle(Color.brandGreen)
                .padding(.top, 24)

            // Same column width as ModelPickerView's row list, so the two
            // adjacent steps line up visually instead of jumping width.
            VStack(spacing: 4) {
                PermissionRow(icon: "mic.fill",      title: "Microphone",       kind: .microphone,      state: poller.microphone,      daemonPath: state.daemonPath)
                PermissionRow(icon: "accessibility", title: "Accessibility",    kind: .accessibility,   state: poller.accessibility,   daemonPath: state.daemonPath)
                PermissionRow(icon: "keyboard",      title: "Input Monitoring", kind: .inputMonitoring, state: poller.inputMonitoring, daemonPath: state.daemonPath)
            }
            .frame(maxWidth: 340)
            .frame(maxWidth: .infinity)

            Spacer()

            Button(action: { state.advance() }) {
                Text(poller.allGranted ? "Continue" : "Continue without all permissions")
            }
            .buttonStyle(BrandPrimaryButtonStyle(enabled: poller.allGranted))
            .padding(.horizontal, 60)
            .padding(.bottom, 28)
        }
        .onAppear { poller.start(daemonPath: state.daemonPath, designPreview: state.isDesignPreview) }
        .onDisappear { poller.stop() }
    }
}

// MARK: - Row

private struct PermissionRow: View {
    let icon: String
    let title: String
    let kind: PermissionsPoller.Kind
    let state: PermissionsPoller.State
    let daemonPath: String?

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: icon)
                .font(.system(size: 17, weight: .medium))
                .foregroundStyle(Color.brandGreen)
                .frame(width: 24)

            Text(title)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Color.brandGreen)

            Spacer()

            if state == .granted {
                BrandRowBadge(text: "Granted")
            } else {
                Button(action: requestPermission) {
                    Text(state == .denied ? "Open Settings" : "Grant")
                }
                .buttonStyle(BrandRowButtonStyle(prominent: true))
            }
        }
        .padding(.vertical, 10)
    }

    private func requestPermission() {
        if let path = daemonPath, FileManager.default.isExecutableFile(atPath: path) {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: path)
            p.arguments = ["--permissions-request", kind.cliName]
            p.standardOutput = Pipe()
            p.standardError = Pipe()
            try? p.run()
        }
        // Mic 1-tap popup is the grant UI in .notRequested; only open
        // Settings for the other kinds, or when the user already denied.
        let shouldOpenSettings: Bool
        switch kind {
        case .microphone:                       shouldOpenSettings = (state == .denied)
        case .accessibility, .inputMonitoring:  shouldOpenSettings = true
        }
        guard shouldOpenSettings else { return }
        let pane = kind.settingsPane
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
            if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") {
                NSWorkspace.shared.open(url)
            }
        }
    }
}

// MARK: - Poller

@MainActor
final class PermissionsPoller: ObservableObject {
    enum State: String {
        case granted, denied
        case notRequested = "not_requested"
        case unknown
    }

    enum Kind {
        case microphone, accessibility, inputMonitoring
        var cliName: String {
            switch self {
            case .microphone: return "mic"
            case .accessibility: return "accessibility"
            case .inputMonitoring: return "input_monitoring"
            }
        }
        var settingsPane: String {
            switch self {
            case .microphone: return "Privacy_Microphone"
            case .accessibility: return "Privacy_Accessibility"
            case .inputMonitoring: return "Privacy_ListenEvent"
            }
        }
    }

    @Published var microphone: State = .unknown
    @Published var accessibility: State = .unknown
    @Published var inputMonitoring: State = .unknown
    @Published var allGranted: Bool = false

    private var timer: Timer?
    private var daemonPath: String?

    func start(daemonPath: String?, designPreview: Bool) {
        self.daemonPath = daemonPath
        if designPreview {
            // Show "Grant" on every row so the design preview lets the
            // designer eyeball the button state without faking grants.
            return
        }
        guard let path = daemonPath, FileManager.default.isExecutableFile(atPath: path) else { return }
        poll()
        timer = Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.poll() }
        }
    }

    func stop() { timer?.invalidate(); timer = nil }

    private func poll() {
        guard let path = daemonPath else { return }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = ["--permissions-json"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = Pipe()
        do { try p.run() } catch { return }
        p.waitUntilExit()
        let data = out.fileHandleForReading.readDataToEndOfFile()
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        microphone = parse(obj["microphone"])
        accessibility = parse(obj["accessibility"])
        inputMonitoring = parse(obj["input_monitoring"])
        allGranted = (obj["all_granted"] as? Bool) ?? false
    }

    private func parse(_ value: Any?) -> State {
        guard let s = value as? String, let parsed = State(rawValue: s) else { return .unknown }
        return parsed
    }
}
