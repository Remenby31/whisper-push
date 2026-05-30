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
            // Both variants share models/parakeet/ on disk. To distinguish
            // which one is installed we read models/parakeet/.variant
            // (written at download time): "int8" or "fp32" (default).
            let parakeetDir = dataDir.appendingPathComponent("parakeet")
            guard FileManager.default.fileExists(atPath: parakeetDir.appendingPathComponent("vocab.txt").path) else {
                return false
            }
            let installedVariant = (try? String(contentsOf: parakeetDir.appendingPathComponent(".variant"), encoding: .utf8))?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? "fp32"
            let wantsInt8 = modelFile.hasSuffix("-int8")
            return wantsInt8 ? (installedVariant == "int8") : (installedVariant == "fp32")
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
                id: "parakeet-int8",
                name: "Parakeet TDT v3 (int8)",
                modelFile: "parakeet-tdt-0.6b-v3-int8",
                // Quantized to int8 — 8-bit integer weights instead of fp32.
                // ~3× smaller, ~2× less RAM, WER ≤ +1% in practice.
                size: "890 MB",
                warning: nil,
                alreadyDownloaded: Self.isModelDownloaded("parakeet-tdt-0.6b-v3-int8")
            ),
            ModelInfo(
                id: "parakeet",
                name: "Parakeet TDT v3 (fp32)",
                modelFile: "parakeet-tdt-0.6b-v3",
                // Full-precision fp32 ONNX. Heavier but the "reference" model
                // — keep around for quality-sensitive comparisons.
                size: "2.5 GB",
                warning: nil,
                alreadyDownloaded: Self.isModelDownloaded("parakeet-tdt-0.6b-v3")
            ),
            ModelInfo(
                id: "whisper-small",
                name: "Whisper Small (q5)",
                modelFile: "ggml-small-q5_1.bin",
                // 244M params quantized to q5 — minimum viable Whisper for
                // light dictation, multilingual, runs comfortably on any
                // Apple Silicon.
                size: "181 MB",
                warning: nil,
                alreadyDownloaded: Self.isModelDownloaded("ggml-small-q5_1.bin")
            ),
            ModelInfo(
                id: "whisper",
                name: "Whisper large-v3-turbo (q5)",
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

        VStack(spacing: 12) {
            VStack(spacing: 2) {
                Text("Choose your engines")
                    .font(.system(size: 22, weight: .bold))
                Text("Detected: \(state.hardwareName)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(.top, 18)

            // Scrollable model list — keeps the title + summary + Continue
            // button visible even when more rows are present than fit.
            ScrollView(.vertical, showsIndicators: false) {
                VStack(spacing: 6) {
                    ForEach(models) { model in
                        let isSelected = state.selectedModels.contains(model.modelFile)
                        HStack(spacing: 10) {
                            // Selection box (left)
                            if model.alreadyDownloaded {
                                Image(systemName: "checkmark.circle.fill")
                                    .font(.system(size: 16))
                                    .foregroundColor(.green)
                            } else {
                                Image(systemName: isSelected ? "checkmark.square.fill" : "square")
                                    .font(.system(size: 16))
                                    .foregroundColor(isSelected ? .brandGreen : .gray.opacity(0.5))
                            }

                            // Name (single line)
                            Text(model.name)
                                .font(.system(size: 13, weight: .medium))
                                .lineLimit(1)

                            Spacer(minLength: 8)

                            // Warning (if any)
                            if let warn = model.warning {
                                Image(systemName: "exclamationmark.triangle.fill")
                                    .font(.system(size: 11))
                                    .foregroundColor(.orange)
                                    .help(warn)
                            }

                            // Size or "Installed"
                            Text(model.alreadyDownloaded ? "Installed" : model.size)
                                .font(.system(size: 11, weight: .medium, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .frame(minWidth: 60, alignment: .trailing)
                        }
                        .padding(.vertical, 7)
                        .padding(.horizontal, 12)
                        .background(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .fill(isSelected ? Color.brandGreen.opacity(0.07) : Color.brandCream.opacity(0.35))
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .stroke(isSelected ? Color.brandGreen.opacity(0.35) : Color.brandGreen.opacity(0.08), lineWidth: 1)
                        )
                        .contentShape(Rectangle())
                        .onTapGesture {
                            if !model.alreadyDownloaded {
                                state.toggleModel(model.modelFile)
                            }
                        }
                        .opacity(model.alreadyDownloaded ? 0.7 : 1.0)
                    }
                }
                .padding(.horizontal, 28)
                .padding(.vertical, 2)
            }
            .frame(maxHeight: .infinity)

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

            let needsDownload = models.contains { state.selectedModels.contains($0.modelFile) && !$0.alreadyDownloaded }
            Button(action: { state.advance() }) {
                Text(needsDownload ? "Download & Continue" : "Continue")
            }
            .buttonStyle(BrandPrimaryButtonStyle(enabled: !state.selectedModels.isEmpty))
            .disabled(state.selectedModels.isEmpty)
            .padding(.horizontal, 60)
            .padding(.bottom, 18)
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
