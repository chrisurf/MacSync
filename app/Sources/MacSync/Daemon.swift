import Foundation

/// Runs `reposync daemon` as a child process. As a child of MacSync.app it
/// inherits the app's Local Network permission (ADR-0010).
@MainActor
final class DaemonProcess {
    private var process: Process?
    private var stopping = false
    private var restarts = 0
    private(set) var lastExit: String?

    var isRunning: Bool { process?.isRunning ?? false }

    func start() {
        guard !isRunning, let bin = Paths.binary else { return }
        // A daemon started elsewhere (Terminal, LaunchAgent) owns the socket.
        if IPC.isListening() { return }
        stopping = false
        let p = Process()
        p.executableURL = bin
        p.arguments = ["--config", Paths.config.path, "daemon"]
        var env = ProcessInfo.processInfo.environment
        env["REPOSYNC_LOG"] = env["REPOSYNC_LOG"] ?? "info"
        env["NO_COLOR"] = "1" // plain log file
        env["REPOSYNC_PARENT_PID"] = String(getpid()) // daemon exits with the app
        p.environment = env
        if let log = Self.openLog() {
            p.standardOutput = log
            p.standardError = log
        }
        p.terminationHandler = { [weak self] proc in
            let status = proc.terminationStatus
            Task { @MainActor in self?.exited(status) }
        }
        do {
            try p.run()
            process = p
            lastExit = nil
        } catch {
            lastExit = "Start fehlgeschlagen: \(error.localizedDescription)"
        }
    }

    func stop() {
        stopping = true
        guard let p = process, p.isRunning else { return }
        p.interrupt() // SIGINT: clean shutdown, like Ctrl-C
        let deadline = Date().addingTimeInterval(3)
        while p.isRunning && Date() < deadline { usleep(50_000) }
        if p.isRunning { p.terminate() }
        process = nil
    }

    func restart() {
        stop()
        restarts = 0
        start()
    }

    private func exited(_ status: Int32) {
        process = nil
        guard !stopping else { return }
        lastExit = "Synchronisation beendet (Status \(status)) – siehe Protokoll"
        restarts += 1
        // Back off: 2 s, 4 s, … up to 60 s.
        let delay = min(60.0, pow(2.0, Double(min(restarts, 6))))
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            if !self.stopping { self.start() }
        }
    }

    private static func openLog() -> FileHandle? {
        let url = Paths.logFile
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        // Keep the log small: start over above 5 MB.
        if let size = try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int, size > 5_000_000 {
            try? Data().write(to: url)
        }
        let h = try? FileHandle(forWritingTo: url)
        _ = try? h?.seekToEnd()
        return h
    }
}
