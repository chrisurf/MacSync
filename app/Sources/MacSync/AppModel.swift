import AppKit
import Foundation
import ServiceManagement
import UserNotifications

/// Status as reported by `macsync status --json` / the daemon socket.
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
    struct Transfer: Decodable {
        var unit: String
        var filesTotal: Int64
        var filesDone: Int64
        var bytesTotal: Int64
        var bytesDone: Int64
        var startedMs: Int64
        var current: String
    }

    var units: [Unit]
    var version: String?
    /// Content transfer in progress (this Mac receives).
    var transfer: Transfer?
    /// Bytes served to the other Mac so far, and when the last ones went out.
    var servedBytes: Int64?
    var servedMs: Int64?

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

/// What is moving right now, for the progress display.
struct Activity: Equatable {
    enum Direction { case receiving, sending }
    var direction: Direction
    var unit: String = ""
    var current: String = ""
    var filesDone: Int64 = 0
    var filesTotal: Int64 = 0
    var bytesDone: Int64 = 0
    var bytesTotal: Int64 = 0
    /// Bytes per second (smoothed).
    var rate: Double = 0

    var fraction: Double? {
        bytesTotal > 0 ? min(1, Double(bytesDone) / Double(bytesTotal)) : (filesTotal > 0 ? Double(filesDone) / Double(filesTotal) : nil)
    }

    var remaining: TimeInterval? {
        guard direction == .receiving, rate > 0, bytesTotal > bytesDone else { return nil }
        return Double(bytesTotal - bytesDone) / rate
    }

    static func bytes(_ b: Int64) -> String { ByteCountFormatter.string(fromByteCount: b, countStyle: .file) }

    static func duration(_ t: TimeInterval) -> String {
        if t < 60 { return "unter 1 Min." }
        let f = DateComponentsFormatter()
        f.allowedUnits = t < 3600 ? [.minute] : [.hour, .minute]
        f.unitsStyle = .abbreviated
        return f.string(from: t) ?? ""
    }

    /// One line for the menu.
    func summary(peer: String) -> String {
        switch direction {
        case .sending:
            return "Sende an \(peer)" + (rate > 0 ? " · \(Activity.bytes(Int64(rate)))/s" : "")
        case .receiving:
            var s = "Empfange"
            if let f = fraction { s += " \(Int(f * 100)) %" }
            if let r = remaining { s += " · noch ca. \(Activity.duration(r))" }
            return s
        }
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
    @Published private(set) var activity: Activity?
    /// Frame of the turning menu bar arrows while syncing.
    @Published private(set) var spinFrame = 0
    private var spinTimer: Timer?
    private var lastSample: (bytes: Int64, served: Int64, at: Date)?
    private var rate: Double = 0
    @Published var lastError: String?
    @Published var launchAtLogin = SMAppService.mainApp.status == .enabled

    let daemon = DaemonProcess()
    private var timer: Timer?
    private var lastConflicts = 0

    var isConfigured: Bool { FileManager.default.fileExists(atPath: Paths.config.path) }
    var config: ConfigFile? { ConfigFile.load() }

    func launch() {
        CLI.migrate()
        if isConfigured {
            daemon.start()
            // Already set up (CLI or RepoSync 0.2.0): start at login like
            // the setup assistant would – once; the user can turn it off.
            // Not for test/demo setups with their own configuration.
            let standard = ProcessInfo.processInfo.environment["MACSYNC_CONFIG"] == nil
            if standard, !UserDefaults.standard.bool(forKey: "offeredLaunchAtLogin") {
                UserDefaults.standard.set(true, forKey: "offeredLaunchAtLogin")
                setLaunchAtLogin(true)
            }
        } else {
            SetupWindow.shared.show()
        }
        // `open -a MacSync --args --manager [Ordner]` opens the sync manager directly.
        let args = CommandLine.arguments
        if isConfigured, let i = args.firstIndex(of: "--manager") {
            let path = args.indices.contains(i + 1) && !args[i + 1].hasPrefix("-") ? args[i + 1] : nil
            ManagerWindow.shared.show(path: path)
        }
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
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
        activity = fromDaemon != nil ? measure(s) : nil
        state = Self.state(s, daemonRunning: fromDaemon != nil, starting: running, exit: daemon.lastExit)
        if state == .inSync || state == .offline, activity != nil { state = .syncing }
        updateSpin()
        notifyConflicts(s?.conflicts ?? 0)
    }

    /// Current transfer with a smoothed rate from consecutive samples.
    private func measure(_ s: Status?) -> Activity? {
        let now = Date()
        let served = s?.servedBytes ?? 0
        let received = s?.transfer?.bytesDone ?? 0
        defer { lastSample = (received, served, now) }
        let dt = lastSample.map { now.timeIntervalSince($0.at) } ?? 0
        if let t = s?.transfer {
            var a = Activity(direction: .receiving, unit: t.unit, current: t.current, filesDone: t.filesDone,
                             filesTotal: t.filesTotal, bytesDone: t.bytesDone, bytesTotal: t.bytesTotal)
            if let last = lastSample, dt > 0, received >= last.bytes {
                rate = rate == 0 ? Double(received - last.bytes) / dt : 0.7 * rate + 0.3 * Double(received - last.bytes) / dt
            } else {
                let elapsed = now.timeIntervalSince1970 - Double(t.startedMs) / 1000
                rate = elapsed > 0 ? Double(t.bytesDone) / elapsed : 0
            }
            a.rate = rate
            return a
        }
        // Sending: the other Mac fetched data from us within the last 3 s.
        if let ms = s?.servedMs, ms > 0, now.timeIntervalSince1970 - Double(ms) / 1000 < 3 {
            var a = Activity(direction: .sending)
            if let last = lastSample, dt > 0, served >= last.served {
                rate = 0.5 * rate + 0.5 * Double(served - last.served) / dt
            }
            a.rate = rate
            return a
        }
        rate = 0
        return nil
    }

    private func updateSpin() {
        let spinning = state == .syncing
        if spinning, spinTimer == nil {
            spinTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { _ in
                Task { @MainActor in AppModel.shared.spinFrame = (AppModel.shared.spinFrame + 1) % MenuIcon.frames }
            }
        } else if !spinning, let t = spinTimer {
            t.invalidate()
            spinTimer = nil
            spinFrame = 0
        }
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

    /// Symlink ~/.local/bin/macsync → the binary inside the app.
    func installCommandLineTool() -> String {
        guard let bin = Paths.binary else { return "macsync nicht gefunden" }
        let dir = Paths.home.appendingPathComponent(".local/bin")
        let link = dir.appendingPathComponent("macsync")
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            if (try? FileManager.default.destinationOfSymbolicLink(atPath: link.path)) != nil
                || FileManager.default.fileExists(atPath: link.path) {
                try FileManager.default.removeItem(at: link)
            }
            try FileManager.default.createSymbolicLink(at: link, withDestinationURL: bin)
            return "Installiert: ~/.local/bin/macsync"
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
