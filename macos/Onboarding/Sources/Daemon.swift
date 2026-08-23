import Foundation

/// The one way this wizard talks to the daemon binary.
///
/// Every screen shells out to `whisper-push <subcommand>` and reads a JSON line
/// back (license status, activation, permission probes). Doing that by hand in
/// each view duplicated the Process plumbing three times — and each copy carried
/// the same two ways to hang the whole window:
///
///  * `standardError = Pipe()` that nobody drains. The daemon logs to stderr; a
///    pipe with no reader fills at the 64 KB buffer and blocks the child
///    forever, so `waitUntilExit()` never returns.
///  * `waitUntilExit()` *before* reading stdout — the same deadlock the other
///    way round as soon as the output exceeds the buffer.
///
/// This runs on a background queue in every caller; it is deliberately
/// synchronous and bounded instead (see `timeout`), so a daemon that wedges
/// costs one worker, never the UI.
enum Daemon {
    /// Hard ceiling on a call. The daemon's own network timeout is 10 s, so this
    /// only fires when it is genuinely stuck; a wizard screen must never wait
    /// longer than this for an answer that is only used to draw a label.
    static let timeout: TimeInterval = 15

    /// Run `path args…` and parse the LAST line of stdout as a JSON object.
    /// `nil` when the binary is missing or unrunnable, it times out, or it
    /// answers something that isn't JSON — callers treat that as "unknown" and
    /// fall back to a safe default rather than guessing.
    static func json(_ path: String?, _ args: [String]) -> [String: Any]? {
        guard let data = run(path, args) else { return nil }
        let line = String(data: data, encoding: .utf8)?
            .split(separator: "\n").last.map(String.init) ?? ""
        return try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any]
    }

    /// Start `path args…` and return immediately, without waiting for it.
    ///
    /// For commands whose whole job is to take a long time: `--permissions-request
    /// mic` sits in a 30 s poll waiting for the user to answer the TCC prompt, so
    /// waiting for it from a button action would freeze the wizard for half a
    /// minute. The caller learns the outcome from the permission poller instead.
    static func spawn(_ path: String?, _ args: [String]) {
        guard let path, FileManager.default.isExecutableFile(atPath: path) else { return }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        // Discard both streams: nothing reads them, and a pipe nobody drains
        // would block the child once it filled.
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run()
    }

    /// Run `path args…` to completion and hand back its stdout. Blocking and
    /// bounded — call it off the main thread.
    @discardableResult
    static func run(_ path: String?, _ args: [String]) -> Data? {
        guard let path, FileManager.default.isExecutableFile(atPath: path) else { return nil }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice // never a pipe we don't drain
        do { try p.run() } catch { return nil }

        // Read to EOF first: the child can finish writing and exit while we are
        // still draining, but it can never block on a full pipe.
        let data = out.fileHandleForReading.readDataToEndOfFile()

        // EOF means the child closed stdout, which normally means it exited —
        // but a wedged child that forked or ignored SIGPIPE could still linger,
        // so bound the wait and kill rather than hang this queue forever.
        let deadline = Date().addingTimeInterval(timeout)
        while p.isRunning && Date() < deadline {
            usleep(20_000)
        }
        if p.isRunning {
            p.terminate()
            return nil
        }
        return data
    }
}
