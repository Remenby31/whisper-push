import SwiftUI

struct DownloadView: View {
    @EnvironmentObject var state: OnboardingState
    @StateObject private var downloader = ModelDownloader()

    var body: some View {
        VStack(spacing: 20) {
            Spacer()

            // Plain logo — no wave-fill animation tied to progress; the
            // linear ProgressView below carries the progress signal.
            LogoSquircle()

            if downloader.isDone {
                Text("Downloads complete!")
                    .font(.system(size: 24, weight: .bold))
            } else {
                Text(downloader.statusText)
                    .font(.headline)

                ProgressView(value: downloader.totalProgress)
                    .progressViewStyle(.linear)
                    .tint(Color.brandGreen)
                    .padding(.horizontal, 60)

                HStack(spacing: 16) {
                    Text(downloader.currentFile)
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    if downloader.totalBytes > 0 {
                        Text("\(formatBytes(downloader.downloadedBytes)) / \(formatBytes(downloader.totalBytes))")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .monospacedDigit()
                    }
                }

                Text("\(Int(downloader.totalProgress * 100))%")
                    .font(.system(size: 14, weight: .medium, design: .monospaced))
                    .foregroundColor(.brandCitron)
            }

            Spacer()

            Button(action: { state.advance() }) {
                Text(downloader.isDone ? "Continue" : "Downloading...")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 8)
            }
            .buttonStyle(.borderedProminent)
            .tint(Color.brandGreen)
            .controlSize(.large)
            .disabled(!downloader.isDone)
            .padding(.horizontal, 60)
            .padding(.bottom, 24)
        }
        .padding(.top, 32)
        .onAppear {
            downloader.downloadAll(models: Array(state.selectedModels))
        }
    }
}

private func formatBytes(_ bytes: Int64) -> String {
    if bytes >= 1_000_000_000 {
        return String(format: "%.1f GB", Double(bytes) / 1_000_000_000)
    } else if bytes >= 1_000_000 {
        return String(format: "%.0f MB", Double(bytes) / 1_000_000)
    }
    return "\(bytes) B"
}

// MARK: - Downloader

@MainActor
class ModelDownloader: NSObject, ObservableObject, URLSessionDownloadDelegate {
    @Published var totalProgress: Double = 0
    @Published var statusText = "Preparing..."
    @Published var currentFile = ""
    @Published var isDone = false
    @Published var downloadedBytes: Int64 = 0
    @Published var totalBytes: Int64 = 0

    private var pendingDownloads: [(model: String, files: [(url: URL, dest: URL)])] = []
    private var currentModelIndex = 0
    private var currentFileIndex = 0
    private var totalFileCount = 0
    private var completedFileCount = 0
    private var session: URLSession!

    override init() {
        super.init()
        session = URLSession(
            configuration: .default,
            delegate: self,
            delegateQueue: .main
        )
    }

    func downloadAll(models: [String]) {
        let dataDir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("whisper-push/models")

        for model in models {
            let files = downloadFiles(for: model, dataDir: dataDir)
            let missing = files.filter { !FileManager.default.fileExists(atPath: $0.dest.path) }
            if !missing.isEmpty {
                pendingDownloads.append((model: model, files: missing))
            }
        }

        totalFileCount = pendingDownloads.reduce(0) { $0 + $1.files.count }

        if totalFileCount == 0 {
            statusText = "All models already downloaded"
            isDone = true
            return
        }

        downloadNext()
    }

    private func downloadFiles(for model: String, dataDir: URL) -> [(url: URL, dest: URL)] {
        switch model {
        case "ggml-large-v3-turbo-q5_0.bin":
            let dest = dataDir.appendingPathComponent("ggml-large-v3-turbo-q5_0.bin")
            let url = URL(string: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin")!
            return [(url, dest)]

        case "parakeet-tdt-0.6b-v3":
            let dir = dataDir.appendingPathComponent("parakeet")
            let base = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main"
            return [
                (URL(string: "\(base)/encoder-model.onnx")!, dir.appendingPathComponent("encoder-model.onnx")),
                (URL(string: "\(base)/encoder-model.onnx.data")!, dir.appendingPathComponent("encoder-model.onnx.data")),
                (URL(string: "\(base)/decoder_joint-model.onnx")!, dir.appendingPathComponent("decoder_joint-model.onnx")),
                (URL(string: "\(base)/vocab.txt")!, dir.appendingPathComponent("vocab.txt")),
            ]

        case "voxtral-q4.gguf":
            let dir = dataDir.appendingPathComponent("voxtral")
            let base = "https://huggingface.co/TrevorJS/voxtral-mini-realtime-gguf/resolve/main"
            return [
                (URL(string: "\(base)/voxtral-q4.gguf")!, dir.appendingPathComponent("voxtral-q4.gguf")),
                (URL(string: "\(base)/tekken.json")!, dir.appendingPathComponent("tekken.json")),
            ]

        default:
            return []
        }
    }

    private func downloadNext() {
        guard currentModelIndex < pendingDownloads.count else {
            isDone = true
            statusText = "Downloads complete!"
            return
        }

        let entry = pendingDownloads[currentModelIndex]
        guard currentFileIndex < entry.files.count else {
            currentModelIndex += 1
            currentFileIndex = 0
            downloadNext()
            return
        }

        let file = entry.files[currentFileIndex]
        statusText = "Downloading \(backendDisplayName(entry.model))..."
        currentFile = file.dest.lastPathComponent

        try? FileManager.default.createDirectory(
            at: file.dest.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )

        let task = session.downloadTask(with: file.url)
        task.resume()
    }

    // MARK: - URLSessionDownloadDelegate

    nonisolated func urlSession(
        _ session: URLSession,
        downloadTask: URLSessionDownloadTask,
        didWriteData bytesWritten: Int64,
        totalBytesWritten: Int64,
        totalBytesExpectedToWrite: Int64
    ) {
        DispatchQueue.main.async {
            self.downloadedBytes = totalBytesWritten
            self.totalBytes = totalBytesExpectedToWrite

            let fileProgress = totalBytesExpectedToWrite > 0
                ? Double(totalBytesWritten) / Double(totalBytesExpectedToWrite)
                : 0
            self.totalProgress = (Double(self.completedFileCount) + fileProgress) / Double(self.totalFileCount)
        }
    }

    nonisolated func urlSession(
        _ session: URLSession,
        downloadTask: URLSessionDownloadTask,
        didFinishDownloadingTo location: URL
    ) {
        DispatchQueue.main.async {
            let entry = self.pendingDownloads[self.currentModelIndex]
            let dest = entry.files[self.currentFileIndex].dest
            try? FileManager.default.moveItem(at: location, to: dest)

            self.completedFileCount += 1
            self.currentFileIndex += 1
            self.downloadedBytes = 0
            self.totalBytes = 0
            self.downloadNext()
        }
    }

    nonisolated func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        if let error = error {
            DispatchQueue.main.async {
                self.statusText = "Error: \(error.localizedDescription)"
            }
        }
    }
}
