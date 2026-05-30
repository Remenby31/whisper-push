import SwiftUI

/// Step 4: guide the user through granting the three macOS permissions
/// the daemon needs.
///
/// Architecture notes:
/// - TCC is per-binary (cdhash). The wizard binary now lives in its own
///   sub-bundle (com.whisper-push.onboarding), so it's NOT killed by the
///   "Quit and reopen" popup when the user toggles a daemon perm in
///   Settings — that popup targets com.whisper-push.app, which has no
///   running process during onboarding.
/// - State is probed by shelling out to `whisper-push --permissions-json`
///   every 1.5s. Each Grant click fires `whisper-push --permissions-request
///   <kind>` and the subprocess parks (for mic) until the popup is resolved.
struct PermissionsView: View {
    @EnvironmentObject var state: OnboardingState
    @StateObject private var poller = PermissionsPoller()

    var body: some View {
        VStack(spacing: 18) {
            VStack(spacing: 6) {
                Text("Grant permissions")
                    .font(.system(size: 24, weight: .bold))
                    .foregroundStyle(Color.brandGreen)
                Text("Whisper Push needs three permissions to listen and type for you. Grant them one at a time below.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 32)
            }
            .padding(.top, 24)

            VStack(spacing: 10) {
                PermissionRow(
                    icon: "mic.fill",
                    title: "Microphone",
                    rationale: "Capture your voice while you hold the hotkey.",
                    kind: .microphone,
                    state: poller.microphone,
                    daemonPath: state.daemonPath
                )
                PermissionRow(
                    icon: "accessibility",
                    title: "Accessibility",
                    rationale: "Paste the transcribed text wherever your cursor is.",
                    kind: .accessibility,
                    state: poller.accessibility,
                    daemonPath: state.daemonPath
                )
                PermissionRow(
                    icon: "keyboard",
                    title: "Input Monitoring",
                    rationale: "Detect the global hotkey press from any app.",
                    kind: .inputMonitoring,
                    state: poller.inputMonitoring,
                    daemonPath: state.daemonPath
                )
            }
            .padding(.horizontal, 32)

            Spacer(minLength: 4)

            statusFooter

            Button(action: { state.advance() }) {
                Text(poller.allGranted ? "Continue" : "Continue without all permissions")
            }
            .buttonStyle(BrandPrimaryButtonStyle(enabled: poller.allGranted))
            .padding(.horizontal, 60)
            .padding(.bottom, 18)
        }
        .onAppear { poller.start(daemonPath: state.daemonPath) }
        .onDisappear { poller.stop() }
    }

    @ViewBuilder
    private var statusFooter: some View {
        if poller.allGranted {
            Label("All set — ready to dictate.", systemImage: "checkmark.seal.fill")
                .foregroundStyle(Color.brandGreen)
                .font(.callout)
        } else if poller.probeAvailable {
            Text("Click Grant on each one. Status updates automatically.")
                .font(.caption)
                .foregroundStyle(.tertiary)
        } else {
            Text("Open each pane, toggle Whisper Push, then choose Continue.")
                .font(.caption)
                .foregroundStyle(.tertiary)
        }
    }
}

// MARK: - Row

private struct PermissionRow: View {
    let icon: String
    let title: String
    let rationale: String
    let kind: PermissionsPoller.Kind
    let state: PermissionsPoller.State
    let daemonPath: String?

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: icon)
                .font(.system(size: 18, weight: .medium))
                .foregroundStyle(Color.brandGreen)
                .frame(width: 26)

            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Color.brandGreen)
                Text(rationale)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }

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
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color.brandCream.opacity(0.45))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(Color.brandGreen.opacity(0.08), lineWidth: 1)
        )
    }

    /// Grant click → fire daemon subprocess for the prompt. For Mic in
    /// `.notRequested`, DO NOT open Settings — the native popup is the
    /// grant UI. For Mic in `.denied`, open Settings (popup won't reappear).
    /// For Accessibility / Input Monitoring, always open Settings (those
    /// require a manual toggle and have no popup equivalent).
    private func requestPermission() {
        if let path = daemonPath, FileManager.default.isExecutableFile(atPath: path) {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: path)
            p.arguments = ["--permissions-request", kind.cliName]
            p.standardOutput = Pipe()
            p.standardError = Pipe()
            try? p.run()
        }

        let shouldOpenSettings: Bool
        switch kind {
        case .microphone:
            shouldOpenSettings = (state == .denied)
        case .accessibility, .inputMonitoring:
            shouldOpenSettings = true
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
    @Published var probeAvailable: Bool = false

    private var timer: Timer?
    private var daemonPath: String?

    func start(daemonPath: String?) {
        self.daemonPath = daemonPath
        self.probeAvailable = daemonPath != nil

        guard let path = daemonPath, FileManager.default.isExecutableFile(atPath: path) else {
            return
        }

        poll()
        timer = Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.poll() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

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
