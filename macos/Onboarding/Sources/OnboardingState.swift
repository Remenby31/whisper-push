import SwiftUI

/// Shared state across all onboarding screens.
@MainActor
class OnboardingState: ObservableObject {
    // Order: welcome → permissions → model → download → ready.
    // Permissions come BEFORE model so the user grants up-front (mic +
    // accessibility + input monitoring) while the daemon is fresh-installed
    // and not yet running — guarantees no "Quit and reopen" popup ever
    // fires, and the model picker / download appear only once setup is
    // committed.
    enum Step: Int, CaseIterable {
        case welcome, permissions, model, download, ready
    }

    @Published var currentStep: Step = .welcome

    // CLI args (from Rust)
    let hardwareName: String
    let recommendedBackend: String
    /// Path to the daemon binary, used by PermissionsView to probe
    /// TCC state via `--permissions-json`. nil in dev/fallback.
    let daemonPath: String?

    // User choices
    @Published var selectedModels: Set<String> = []
    @Published var autoStart = true

    /// The primary model (first selected, or the recommended one).
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

        // Pre-select models based on available disk space and RAM
        let freeDiskGB = Self.freeDiskSpaceGB()
        let ramGB = Self.totalRAMGB()
        var selected: Set<String> = [modelNameForBackend(self.recommendedBackend)]

        // Voxtral needs ~2.5GB RAM — skip on 8GB machines
        let canRunVoxtral = ramGB > 12

        if freeDiskGB > 10 {
            selected.insert("parakeet-tdt-0.6b-v3")
            selected.insert("ggml-large-v3-turbo-q5_0.bin")
            if canRunVoxtral {
                selected.insert("voxtral-q4.gguf")
            }
        } else if freeDiskGB > 5 {
            selected.insert("ggml-large-v3-turbo-q5_0.bin")
            selected.insert("parakeet-tdt-0.6b-v3")
        }

        self.selectedModels = selected
    }

    static func totalRAMGB() -> Double {
        return Double(ProcessInfo.processInfo.physicalMemory) / 1_000_000_000
    }

    private static func freeDiskSpaceGB() -> Double {
        guard let attrs = try? FileManager.default.attributesOfFileSystem(
            forPath: NSHomeDirectory()
        ),
        let freeBytes = attrs[.systemFreeSize] as? Int64 else {
            return 0
        }
        return Double(freeBytes) / 1_000_000_000
    }

    private static func argValue(_ args: [String], flag: String) -> String? {
        guard let idx = args.firstIndex(of: flag), idx + 1 < args.count else { return nil }
        return args[idx + 1]
    }

    func toggleModel(_ modelFile: String) {
        if selectedModels.contains(modelFile) {
            // Don't allow deselecting the last one
            if selectedModels.count > 1 {
                selectedModels.remove(modelFile)
            }
        } else {
            selectedModels.insert(modelFile)
        }
    }

    /// Write the user's choices as JSON to stdout (read by Rust)
    func finish() {
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

        // Skip download screen if all selected models are already installed
        if Step(rawValue: nextRaw) == .download && allSelectedModelsInstalled() {
            nextRaw += 1
        }

        if let next = Step(rawValue: nextRaw) {
            withAnimation(.easeInOut(duration: 0.3)) {
                currentStep = next
            }
        }
    }

    private func allSelectedModelsInstalled() -> Bool {
        let dataDir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("whisper-push/models")
        return selectedModels.allSatisfy { model in
            switch model {
            case "ggml-large-v3-turbo-q5_0.bin":
                return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent(model).path)
            case "parakeet-tdt-0.6b-v3":
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
    case "parakeet": return "parakeet-tdt-0.6b-v3"
    case "voxtral-local": return "voxtral-q4.gguf"
    default: return "ggml-large-v3-turbo-q5_0.bin"
    }
}

func backendDisplayName(_ model: String) -> String {
    if model.contains("parakeet") { return "Parakeet TDT" }
    if model.contains("voxtral") { return "Voxtral Realtime" }
    return "Whisper large-v3-turbo"
}
