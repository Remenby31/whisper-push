import SwiftUI

struct ModelInfo: Identifiable {
    let id: String
    let name: String
    let modelFile: String
    let size: String
    let warning: String?
    let alreadyDownloaded: Bool
}

struct ModelPickerView: View {
    @EnvironmentObject var state: OnboardingState

    private static func isModelDownloaded(_ modelFile: String) -> Bool {
        let dataDir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("whisper-push/models")
        switch modelFile {
        case "ggml-large-v3-turbo-q5_0.bin":
            return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent(modelFile).path)
        case "parakeet-tdt-0.6b-v3":
            return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent("parakeet/vocab.txt").path)
        case "voxtral-q4.gguf":
            return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent("voxtral/voxtral-q4.gguf").path)
        default:
            return false
        }
    }

    var body: some View {
        let lowRAM = OnboardingState.totalRAMGB() <= 12
        let models = [
            ModelInfo(
                id: "parakeet",
                name: "Parakeet TDT v3",
                modelFile: "parakeet-tdt-0.6b-v3",
                size: "600 MB",
                warning: nil,
                alreadyDownloaded: Self.isModelDownloaded("parakeet-tdt-0.6b-v3")
            ),
            ModelInfo(
                id: "whisper",
                name: "Whisper large-v3-turbo",
                modelFile: "ggml-large-v3-turbo-q5_0.bin",
                size: "550 MB",
                warning: nil,
                alreadyDownloaded: Self.isModelDownloaded("ggml-large-v3-turbo-q5_0.bin")
            ),
            ModelInfo(
                id: "voxtral-local",
                name: "Voxtral Realtime",
                modelFile: "voxtral-q4.gguf",
                size: "2.3 GB",
                warning: lowRAM ? "Needs 16GB+ RAM" : nil,
                alreadyDownloaded: Self.isModelDownloaded("voxtral-q4.gguf")
            ),
        ]

        VStack(spacing: 16) {
            Spacer()

            VStack(spacing: 4) {
                Text("Choose your engines")
                    .font(.system(size: 24, weight: .bold))
                Text("Detected: \(state.hardwareName)")
                    .font(.body)
                    .foregroundStyle(.secondary)
            }

            VStack(spacing: 10) {
                ForEach(models) { model in
                    let isSelected = state.selectedModels.contains(model.modelFile)
                    HStack(spacing: 14) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(model.name)
                                .font(.headline)
                            HStack(spacing: 8) {
                                if model.alreadyDownloaded {
                                    Label("Installed", systemImage: "checkmark.circle.fill")
                                        .foregroundColor(.green)
                                } else {
                                    Label(model.size, systemImage: "arrow.down.circle")
                                }
                                if let warn = model.warning {
                                    Label(warn, systemImage: "exclamationmark.triangle")
                                        .foregroundColor(.orange)
                                }
                            }
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        }

                        Spacer()

                        if model.alreadyDownloaded {
                            Image(systemName: "checkmark.square.fill")
                                .font(.title2)
                                .foregroundColor(.green)
                        } else {
                            Image(systemName: isSelected ? "checkmark.square.fill" : "square")
                                .font(.title2)
                                .foregroundColor(isSelected ? .brandGreen : .gray)
                        }
                    }
                    .padding(.vertical, 12)
                    .padding(.horizontal, 16)
                    .background(
                        RoundedRectangle(cornerRadius: 10)
                            .fill(isSelected ? Color.brandGreen.opacity(0.08) : Color(nsColor: .windowBackgroundColor))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(isSelected ? Color.brandGreen.opacity(0.4) : Color.gray.opacity(0.2), lineWidth: 1)
                    )
                    .contentShape(Rectangle())
                    .onTapGesture {
                        if !model.alreadyDownloaded {
                            state.toggleModel(model.modelFile)
                        }
                    }
                    .opacity(model.alreadyDownloaded ? 0.85 : 1.0)
                }
            }
            .padding(.horizontal, 32)

            let toDownload = models.filter { state.selectedModels.contains($0.modelFile) && !$0.alreadyDownloaded }
            if toDownload.isEmpty {
                Text("\(state.selectedModels.count) engine\(state.selectedModels.count == 1 ? "" : "s") selected — all installed")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                let totalMB = toDownload.reduce(0) { $0 + sizeToMB($1.size) }
                Text("\(toDownload.count) to download — \(formatSize(totalMB))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            let needsDownload = models.contains { state.selectedModels.contains($0.modelFile) && !$0.alreadyDownloaded }
            Button(action: { state.advance() }) {
                Text(needsDownload ? "Download & Continue" : "Continue")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 8)
            }
            .buttonStyle(.borderedProminent)
            .tint(Color.brandGreen)
            .controlSize(.large)
            .disabled(state.selectedModels.isEmpty)
            .padding(.horizontal, 60)
            .padding(.bottom, 24)
        }
        .padding(.top, 24)
    }
}

private func sizeToMB(_ s: String) -> Int {
    if s.contains("GB") {
        return Int((Double(s.replacingOccurrences(of: " GB", with: "")) ?? 0) * 1000)
    }
    return Int(s.replacingOccurrences(of: " MB", with: "")) ?? 0
}

private func formatSize(_ mb: Int) -> String {
    if mb >= 1000 {
        return String(format: "%.1f GB", Double(mb) / 1000.0)
    }
    return "\(mb) MB"
}
