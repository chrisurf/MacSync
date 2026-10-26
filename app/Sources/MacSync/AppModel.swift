import AppKit
import Foundation
import ServiceManagement
import UserNotifications

/// Status as reported by `reposync status --json` / the daemon socket.
struct Status: Decodable {
    struct Peer: Decodable {
        var address: String?
        var device: String?
        var name: String?
        var paired: String?
    }

    struct Unit: Decodable {
        var name: String
        var state: String
        var conflicts: Int
        var lastSync: String?
        var paused: Bool
    }

    var daemon: Bool?
    var paused: Bool?
    var name: String?
    var device: String?
    var mode: String?
    var root: String?
    var peer: Peer?
    var peerOnline: Bool?
    var peerLastContact: String?
    var peerError: String?
    var peerVersion: String?
    var units: [Unit]
    var version: String?

    var conflicts: Int { units.reduce(0) { $0 + $1.conflicts } }
    var lastSync: Date? {
        units.compactMap { $0.lastSync.flatMap(Status.parseDate) }.max()
    }

    static func parseDate(_ s: String) -> Date? {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.date(from: s) ?? ISO8601DateFormatter().date(from: s)
    }
}

enum SyncState: Equatable {
    case notConfigured, starting, stopped(String), paused, conflict(Int), offline, syncing, inSync

    var title: String {
        switch self {
        case .notConfigured: return "Nicht eingerichtet"
        case .starting: return "Startet …"
        case .stopped: return "Synchronisation gestoppt"
        case .paused: return "Pausiert"
        case .conflict(let n): return n == 1 ? "1 Konflikt" : "\(n) Konflikte"
        case .offline: return "Anderer Mac nicht erreichbar"
        case .syncing: return "Wird synchronisiert …"
        case .inSync: return "Synchron"
        }
    }
}

@MainActor
final class AppModel: ObservableObject {
    static let shared = AppModel()

    @Published private(set) var status: Status?
    @Published private(set) var state: SyncState = .starting
    @Published var lastError: String?
    @Published var launchAtLogin = SMAppService.mainApp.status == .enabled

    let daemon = DaemonProcess()
    private var timer: Timer?
    private var lastConflicts = 0

    var isConfigured: Bool { FileManager.default.fileExists(atPath: Paths.config.path) }
    var config: ConfigFile? { ConfigFile.load() }

    func launch() {
        if isConfigured {
            daemon.start()
            // Already set up (CLI or RepoSync 0.2.0): start at login like
            // the setup assistant would – once; the user can turn it off.
            if !UserDefaults.standard.bool(forKey: "offeredLaunchAtLogin") {
                UserDefaults.standard.set(true, forKey: "offeredLaunchAtLogin")
                setLaunchAtLogin(true)
            }
        } else {
            SetupWindow.shared.show()
        }
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { _ in
            Task { @MainActor in await AppModel.shared.refresh() }
        }
        Task { await refresh() }
    }

    func quit() {
        daemon.stop()
        NSApp.terminate(nil)
    }

    // MARK: status

    func refresh() async {
        guard isConfigured else {
            status = nil
            state = .notConfigured
            return
        }
        let running = daemon.isRunning
        let fromDaemon: Any? = await Task.detached { try? IPC.call(["cmd": "status"]) }.value
        var s: Status?
        if let obj = fromDaemon {
            s = decode(obj)
        } else if let out = try? await CLI.run(["status", "--json"]) {
            s = try? Self.decoder.decode(Status.self, from: Data(out.utf8))
        }
        status = s
        state = Self.state(s, daemonRunning: fromDaemon != nil, starting: running, exit: daemon.lastExit)
        notifyConflicts(s?.conflicts ?? 0)
    }

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    private func decode(_ obj: Any) -> Status? {
        guard let data = try? JSONSerialization.data(withJSONObject: obj) else { return nil }
        return try? Self.decoder.decode(Status.self, from: data)
    }

    private static func state(_ s: Status?, daemonRunning: Bool, starting: Bool, exit: String?) -> SyncState {
        guard daemonRunning, let s else {
            if starting { return .starting }
            return .stopped(exit ?? "")
        }
        if s.paused == true { return .paused }
        if s.conflicts > 0 { return .conflict(s.conflicts) }
        if s.peer == nil { return .notConfigured }
        if s.peerOnline != true { return .offline }
        if s.units.contains(where: { $0.state != "in sync" }) { return .syncing }
        return .inSync
    }

    // MARK: actions

    func syncNow() { send(["cmd": "sync", "units": NSNull(), "dry_run": false], timeout: 3600) }
    func pause() { send(["cmd": "pause", "unit": NSNull()]) }
    func resume() { send(["cmd": "resume", "unit": NSNull()]) }

    private func send(_ req: [String: Any], timeout: Int = 30) {
        Task {
            do {
                _ = try await Task.detached { try IPC.call(req, timeout: timeout) }.value
                lastError = nil
            } catch {
                lastError = error.localizedDescription
            }
            await refresh()
        }
    }

    func startSync() {
        daemon.start()
        Task { await refresh() }
    }

    func restartDaemon() {
        daemon.restart()
        Task { await refresh() }
    }

    /// Changes a top-level config value and restarts the daemon to apply it.
    func setConfig(_ key: String, _ value: String) {
        guard var c = config else { return }
        c.set(key, value)
        do {
            try c.save()
            restartDaemon()
        } catch {
            lastError = "Konfiguration nicht gespeichert: \(error.localizedDescription)"
        }
    }

    func unpair() async {
        guard let device = status?.peer?.device else { return }
        daemon.stop()
        do {
            _ = try await CLI.run(["unpair", device])
        } catch {
            lastError = error.localizedDescription
        }
        daemon.start()
        await refresh()
    }

    func setLaunchAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
        } catch {
            lastError = "Anmeldeobjekt: \(error.localizedDescription)"
        }
        launchAtLogin = SMAppService.mainApp.status == .enabled
    }

    func openLog() {
        NSWorkspace.shared.open(Paths.logFile)
    }

    func openRoot() {
        guard let root = status?.root ?? config?.string("root") else { return }
        NSWorkspace.shared.open(URL(fileURLWithPath: Paths.expand(root)))
    }

    /// Symlink ~/.local/bin/reposync → the binary inside the app.
    func installCommandLineTool() -> String {
        guard let bin = Paths.binary else { return "reposync nicht gefunden" }
        let dir = Paths.home.appendingPathComponent(".local/bin")
        let link = dir.appendingPathComponent("reposync")
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            if (try? FileManager.default.destinationOfSymbolicLink(atPath: link.path)) != nil
                || FileManager.default.fileExists(atPath: link.path) {
                try FileManager.default.removeItem(at: link)
            }
            try FileManager.default.createSymbolicLink(at: link, withDestinationURL: bin)
            return "Installiert: ~/.local/bin/reposync"
        } catch {
            return "Fehler: \(error.localizedDescription)"
        }
    }

    // MARK: notifications

    private func notifyConflicts(_ n: Int) {
        defer { lastConflicts = n }
        guard n > lastConflicts, UserDefaults.standard.object(forKey: "notifyConflicts") as? Bool ?? true else { return }
        let content = UNMutableNotificationContent()
        content.title = "MacSync: Konflikt"
        content.body = n == 1 ? "Eine Datei wurde auf beiden Macs geändert." : "\(n) Dateien wurden auf beiden Macs geändert."
        UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: "conflicts", content: content, trigger: nil))
    }

    func requestNotificationPermission() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }
}
