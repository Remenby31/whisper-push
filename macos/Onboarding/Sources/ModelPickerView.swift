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
        case "ggml-large-v3-turbo-q5_0.bin", "ggml-small-q5_1.bin":
            return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent(modelFile).path)
        case "parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v3-int8":
            let parakeetDir = dataDir.appendingPathComponent("parakeet")
            guard FileManager.default.fileExists(atPath: parakeetDir.appendingPathComponent("vocab.txt").path) else {
                return false
            }
            let variant = (try? String(contentsOf: parakeetDir.appendingPathComponent(".variant"), encoding: .utf8))?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? "fp32"
            let wantsInt8 = modelFile.hasSuffix("-int8")
            return wantsInt8 ? (variant == "int8") : (variant == "fp32")
        case "voxtral-q4.gguf":
            return FileManager.default.fileExists(atPath: dataDir.appendingPathComponent("voxtral/voxtral-q4.gguf").path)
        default:
            return false
        }
    }

    var body: some View {
        let lowRAM = OnboardingState.totalRAMGB() <= 12
        let models = [
            ModelInfo(id: "parakeet-int8",
                      name: "Parakeet TDT v3 (int8)",
                      modelFile: "parakeet-tdt-0.6b-v3-int8",
                      size: "890 MB",
                      warning: nil,
                      alreadyDownloaded: Self.isModelDownloaded("parakeet-tdt-0.6b-v3-int8")),
            ModelInfo(id: "parakeet",
                      name: "Parakeet TDT v3 (fp32)",
                      modelFile: "parakeet-tdt-0.6b-v3",
                      size: "2.5 GB",
                      warning: nil,
                      alreadyDownloaded: Self.isModelDownloaded("parakeet-tdt-0.6b-v3")),
            ModelInfo(id: "whisper-small",
                      name: "Whisper Small (q5)",
                      modelFile: "ggml-small-q5_1.bin",
                      size: "181 MB",
                      warning: nil,
                      alreadyDownloaded: Self.isModelDownloaded("ggml-small-q5_1.bin")),
            ModelInfo(id: "whisper",
                      name: "Whisper Turbo (q5)",
                      modelFile: "ggml-large-v3-turbo-q5_0.bin",
                      size: "550 MB",
                      warning: nil,
                      alreadyDownloaded: Self.isModelDownloaded("ggml-large-v3-turbo-q5_0.bin")),
            ModelInfo(id: "voxtral-local",
                      name: "Voxtral Realtime",
                      modelFile: "voxtral-q4.gguf",
                      size: "2.3 GB",
                      warning: lowRAM ? "Needs 16 GB+ RAM" : nil,
                      alreadyDownloaded: Self.isModelDownloaded("voxtral-q4.gguf")),
        ]

        VStack(spacing: 12) {
            VStack(spacing: 2) {
                Text("Choose your engines")
                    .font(.system(size: 22, weight: .bold))
                    .foregroundStyle(Color.brandGreen)
                Text("Detected: \(state.hardwareName)")
                    .font(.caption)
                    .foregroundStyle(Color.brandGreen.opacity(0.6))
            }
            .padding(.top, 22)

            ScrollView(.vertical, showsIndicators: false) {
                VStack(spacing: 2) {
                    ForEach(models) { model in
                        let isSelected = state.selectedModels.contains(model.modelFile)
                        HStack(spacing: 12) {
                            BrandCheckbox(isOn: isSelected || model.alreadyDownloaded)

                            Text(model.name)
                                .font(.system(size: 13, weight: .medium))
                                .foregroundStyle(Color.brandGreen)
                                .lineLimit(1)

                            Spacer(minLength: 8)

                            if let warn = model.warning {
                                Image(systemName: "exclamationmark.triangle.fill")
                                    .font(.system(size: 11))
                                    .foregroundStyle(Color.brandGreen.opacity(0.5))
                                    .help(warn)
                            }

                            Text(model.alreadyDownloaded ? "Installed" : model.size)
                                .font(.system(size: 11, weight: .medium, design: .monospaced))
                                .foregroundStyle(Color.brandGreen.opacity(0.55))
                                .frame(minWidth: 60, alignment: .trailing)
                        }
                        .padding(.vertical, 8)
                        .contentShape(Rectangle())
                        .onTapGesture {
                            if !model.alreadyDownloaded {
                                state.toggleModel(model.modelFile)
                            }
                        }
                        .opacity(model.alreadyDownloaded ? 0.7 : 1.0)
                    }
                }
                .frame(maxWidth: 340)
                .frame(maxWidth: .infinity)
            }
            .frame(maxHeight: .infinity)

            let toDownload = models.filter { state.selectedModels.contains($0.modelFile) && !$0.alreadyDownloaded }
            if toDownload.isEmpty {
                Text("\(state.selectedModels.count) engine\(state.selectedModels.count == 1 ? "" : "s") selected. All installed.")
                    .font(.caption)
                    .foregroundStyle(Color.brandGreen.opacity(0.6))
            } else {
                let totalMB = toDownload.reduce(0) { $0 + sizeToMB($1.size) }
                Text("\(toDownload.count) to download · \(formatSize(totalMB))")
                    .font(.caption)
                    .foregroundStyle(Color.brandGreen.opacity(0.6))
            }

            let needsDownload = models.contains { state.selectedModels.contains($0.modelFile) && !$0.alreadyDownloaded }
            Button(action: { state.advance() }) {
                Text(needsDownload ? "Download & Continue" : "Continue")
            }
            .buttonStyle(BrandPrimaryButtonStyle(enabled: !state.selectedModels.isEmpty))
            .disabled(state.selectedModels.isEmpty)
            .padding(.horizontal, 60)
            .padding(.bottom, 14)
        }
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
