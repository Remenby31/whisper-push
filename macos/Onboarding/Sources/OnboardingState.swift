import SwiftUI

/// Shared state across all onboarding screens.
@MainActor
class OnboardingState: ObservableObject {
    /// Order: welcome → permissions → model → download → ready.
    /// Permissions come BEFORE model so grants happen up-front while the
    /// daemon isn't running, guaranteeing no "Quit and reopen" popup.
    enum Step: Int, CaseIterable {
        // Paywall (license) comes AFTER permissions so the user is set up first.
        case welcome, permissions, license, model, download, ready

        static func from(name: String) -> Step? {
            switch name {
            case "welcome": return .welcome
            case "license": return .license
            case "permissions": return .permissions
            case "model": return .model
            case "download": return .download
            case "ready": return .ready
            default: return nil
            }
        }
    }

    @Published var currentStep: Step = .welcome

    // CLI args (from Rust)
    let hardwareName: String
    let recommendedBackend: String
    /// Path to the daemon binary, used by PermissionsView to probe TCC
    /// state via `--permissions-json`. nil in dev / design preview.
    let daemonPath: String?
    /// Design preview mode (driven by the `onboarding-design` Claude skill
    /// via `make onboarding-preview`). When true the wizard never calls
    /// the daemon, never downloads, and `finish()` does not emit JSON.
    let isDesignPreview: Bool
    /// Standalone license/payment modal (menu bar → License → Subscription).
    /// Shows only LicenseView; its buttons close the window instead of advancing.
    let licenseOnly: Bool
    /// Open the modal directly on the activate (enter-your-key) screen instead of
    /// the paywall (menu bar → Activate with License Key…). Implies `licenseOnly`.
    let startActivate: Bool

    // User choices
    @Published var selectedModels: Set<String> = []
    @Published var autoStart = true

    /// The embedded payment form needs more vertical room than the rest of the
    /// wizard; LicenseView raises this while the checkout is showing so the whole
    /// form fits without scrolling. Drives the window height in OnboardingApp.
    @Published var expandedForCheckout = false

    var primaryModel: String {
        let recommended = modelNameForBackend(recommendedBackend)
        if selectedModels.contains(recommended) {
            return recommended
        }
        return selectedModels.first ?? recommended
    }

    init() {
        let args = ProcessInfo.processInfo.arguments
        self.hardwareName = Self.argValue(args, flag: "--hardware") ?? "Unknown"
        self.recommendedBackend = Self.argValue(args, flag: "--recommended") ?? "parakeet"
        self.daemonPath = Self.argValue(args, flag: "--daemon-path")
        self.isDesignPreview = args.contains("--design-preview")
        self.licenseOnly = args.contains("--license-only")
        self.startActivate = args.contains("--activate")

        let freeDiskGB = Self.freeDiskSpaceGB()
        let ramGB = Self.totalRAMGB()
        var selected: Set<String> = [modelNameForBackend(self.recommendedBackend)]

        let canRunVoxtral = ramGB > 12

        if freeDiskGB > 10 {
            selected.insert("parakeet-tdt-0.6b-v3-int8")
            selected.insert("ggml-large-v3-turbo-q5_0.bin")
            if canRunVoxtral {
                selected.insert("voxtral-q4.gguf")
            }
        } else if freeDiskGB > 5 {
            selected.insert("ggml-large-v3-turbo-q5_0.bin")
            selected.insert("parakeet-tdt-0.6b-v3-int8")
        }

        self.selectedModels = selected

        // Optional jump to a specific step (used by design preview + Rust
        // resume after a kill).
        if let stepArg = Self.argValue(args, flag: "--start-at"),
           let step = Step.from(name: stepArg) {
            self.currentStep = step
        }
        // Standalone payment modal: land directly on the license screen.
        if self.licenseOnly {
            self.currentStep = .license
        }
    }

    static func totalRAMGB() -> Double {
        return Double(ProcessInfo.processInfo.physicalMemory) / 1_000_000_000
    }

    private static func freeDiskSpaceGB() -> Double {
        guard let attrs = try? FileManager.default.attributesOfFileSystem(forPath: NSHomeDirectory()),
              let freeBytes = attrs[.systemFreeSize] as? Int64 else { return 0 }
        return Double(freeBytes) / 1_000_000_000
    }

    private static func argValue(_ args: [String], flag: String) -> String? {
        guard let idx = args.firstIndex(of: flag), idx + 1 < args.count else { return nil }
        return args[idx + 1]
    }

    func toggleModel(_ modelFile: String) {
        if selectedModels.contains(modelFile) {
            if selectedModels.count > 1 {
                selectedModels.remove(modelFile)
            }
        } else {
            selectedModels.insert(modelFile)
        }
    }

    func finish() {
        guard !isDesignPreview else {
            NSApplication.shared.terminate(nil)
            return
        }
        let result: [String: Any] = [
            "model": primaryModel,
            "download": Array(selectedModels),
            "auto_start": autoStart
        ]
        if let data = try? JSONSerialization.data(withJSONObject: result),
           let json = String(data: data, encoding: .utf8) {
            print(json)
        }
        NSApplication.shared.terminate(nil)
    }

    func advance() {
        var nextRaw = currentStep.rawValue + 1
        if Step(rawValue: nextRaw) == .download && allSelectedModelsInstalled() {
            nextRaw += 1
        }
        if let next = Step(rawValue: nextRaw) {
            withAnimation(.easeInOut(duration: 0.3)) {
                currentStep = next
            }
        }
    }

    /// Cmd+→ / Cmd+← sweep used by design preview.
    func sweep(_ direction: Int) {
        let raw = currentStep.rawValue + direction
        if let next = Step(rawValue: raw) {
            withAnimation(.easeInOut(duration: 0.25)) {
                currentStep = next
            }
        }
    }

    private func allSelectedModelsInstalled() -> Bool {
        let dataDir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("whisper-push/models")
        return selectedModels.allSatisfy { model in
            switch model {
            case "ggml-large-v3-turbo-q5_0.bin", "ggml-small-q5_1.bin":
                return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent(model).path)
            case "parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v3-int8":
                return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent("parakeet/vocab.txt").path)
            case "voxtral-q4.gguf":
                return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent("voxtral/voxtral-q4.gguf").path)
            default:
                return false
            }
        }
    }
}

func modelNameForBackend(_ backend: String) -> String {
    switch backend {
    case "parakeet": return "parakeet-tdt-0.6b-v3-int8"
    case "voxtral-local": return "voxtral-q4.gguf"
    default: return "ggml-large-v3-turbo-q5_0.bin"
    }
}

func backendDisplayName(_ model: String) -> String {
    if model == "parakeet-tdt-0.6b-v3-int8" { return "Parakeet TDT (int8)" }
    if model.contains("parakeet") { return "Parakeet TDT (fp32)" }
    if model.contains("voxtral") { return "Voxtral Realtime" }
    if model.contains("small") { return "Whisper Small" }
    return "Whisper Turbo"
}
