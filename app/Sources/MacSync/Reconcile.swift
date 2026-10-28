import AppKit
import SwiftUI

// Folder reconcile ("Ordnerabgleich"): an older copy (ALT) next to the current
// one (NEU), see docs/reconcile.md. The comparison and the actions run in
// `macsync dupes`; this side shows the tree and collects the plan.

// MARK: - JSON of `macsync dupes ALT NEU --json --files` (crates/dupes/src/compare.rs)

enum FileState: String, Decodable, CaseIterable, Sendable {
    case same, updated, moved, oldNewer = "old_newer", unclear, onlyOld = "only_old", onlyNew = "only_new", unreadable

    init(from decoder: Decoder) throws {
        self = FileState(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unreadable
    }

    var symbol: String {
        switch self {
        case .same: return "✅"
        case .updated: return "🔼"
        case .moved: return "🔀"
        case .oldNewer: return "❗"
        case .unclear: return "❓"
        case .onlyOld: return "🟥"
        case .onlyNew: return "➕"
        case .unreadable: return "❌"
        }
    }

    var title: String {
        switch self {
        case .same: return "gleich"
        case .updated: return "in NEU neuer"
        case .moved: return "in NEU verschoben"
        case .oldNewer: return "in ALT neuer"
        case .unclear: return "anderer Inhalt, gleiches Datum"
        case .onlyOld: return "nur in ALT"
        case .onlyNew: return "nur in NEU"
        case .unreadable: return "nicht lesbar"
        }
    }

    /// Deleting ALT would lose something.
    var risky: Bool { self == .oldNewer || self == .unclear || self == .onlyOld || self == .unreadable }

    /// Order of the filter chips: what needs attention first.
    static let chipOrder: [FileState] = [.oldNewer, .unclear, .onlyOld, .unreadable, .moved, .updated, .onlyNew, .same]
}

enum FolderState: String, Decodable, Sendable {
    case identical, deletable, check, onlyOld = "only_old", onlyNew = "only_new", unreadable

    init(from decoder: Decoder) throws {
        self = FolderState(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unreadable
    }

    var symbol: String {
        switch self {
        case .identical: return "✅"
        case .deletable: return "🚮"
        case .check: return "🟧"
        case .onlyOld: return "🟥"
        case .onlyNew: return "➕"
        case .unreadable: return "❌"
        }
    }

    var title: String {
        switch self {
        case .identical: return "identisch"
        case .deletable: return "kann weg – alles in NEU"
        case .check: return "prüfen"
        case .onlyOld: return "nur in ALT"
        case .onlyNew: return "nur in NEU"
        case .unreadable: return "nicht ganz lesbar"
        }
    }

    var deletable: Bool { self == .identical || self == .deletable }
}

struct DupesComparison: Decodable, Sendable {
    struct Skipped: Decodable, Sendable {
        var path: String
        var reason: String
    }

    struct Order: Decodable, Sendable {
        /// `confirmed`, `contradicted` or `unknown`.
        var status: String
        var newerInNew: Int
        var newerInOld: Int
        var oldLastMs: Int64
        var newLastMs: Int64
    }

    struct Counts: Decodable, Sendable {
        var same: Int
        var updated: Int
        var moved: Int
        var oldNewer: Int
        var unclear: Int
        var onlyOld: Int
        var onlyNew: Int
        var unreadable: Int

        var risky: Int { oldNewer + unclear + onlyOld + unreadable }
        var oldFiles: Int { same + updated + moved + oldNewer + unclear + onlyOld + unreadable }

        subscript(s: FileState) -> Int {
            switch s {
            case .same: return same
            case .updated: return updated
            case .moved: return moved
            case .oldNewer: return oldNewer
            case .unclear: return unclear
            case .onlyOld: return onlyOld
            case .onlyNew: return onlyNew
            case .unreadable: return unreadable
            }
        }
    }

    struct Folder: Decodable, Sendable {
        var path: String
        var name: String
        var status: FolderState
        var inOld: Bool
        var inNew: Bool
        var total: Counts
        var oldBytes: Int64
        var newBytes: Int64
        var children: [Int]
        var entries: [Int]
    }

    struct Entry: Decodable, Sendable {
        var path: String
        var state: FileState
        /// Size in ALT (in NEU for `only_new`).
        var size: Int64
        var mtimeOld: Int64?
        var mtimeNew: Int64?
        var movedTo: String?
        /// `only_old`: proposed target in NEU (missing = same path).
        var suggested: String?
        /// Spelling in NEU where it differs.
        var pathNew: String?
        var sizeNew: Int64?
    }

    var old: String
    var new: String
    var oldFiles: Int64
    var oldBytes: Int64
    var newFiles: Int64
    var newBytes: Int64
    var dataless: Int64?
    var skipped: [Skipped]
    var order: Order
    var deletableBytes: Int64
    var folders: [Folder]
    var entries: [Entry]
}

// MARK: - tree

/// One row of the shared tree: a relative path with its ALT and NEU side.
struct RNode: Identifiable, Sendable {
    let id: Int
    let parent: Int?
    let depth: Int
    let name: String
    let nameNew: String
    /// Relative to ALT (ALT spelling) and to NEU.
    let path: String
    let pathNew: String
    let folder: Int?
    let entry: Int?
    let fileState: FileState?
    let folderState: FolderState?
    let inOld: Bool
    let inNew: Bool
    var movedTo: String?
    var suggested: String?
    var children: [Int] = []
    var oldBytes: Int64?
    var newBytes: Int64?
    var oldMs: Int64?
    var newMs: Int64?
    /// Files here and below that would be lost with ALT.
    var open = 0

    var isDir: Bool { folder != nil }
    var symbol: String { fileState?.symbol ?? folderState?.symbol ?? "" }
    var stateTitle: String { fileState?.title ?? folderState?.title ?? "" }
}

enum ReconcileTree {
    /// Flattens folders and entries depth first (folders before files, by
    /// name); node 0 is the root.
    static func build(_ c: DupesComparison, complete: Bool) -> [RNode] {
        var b = Builder(c: c, complete: complete)
        _ = b.folder(0, parent: nil, depth: -1, pathNew: "")
        return b.nodes
    }

    private struct Builder {
        let c: DupesComparison
        /// `--files`: identical files are listed, so NEU sums are complete.
        let complete: Bool
        var nodes: [RNode] = []

        mutating func folder(_ fi: Int, parent: Int?, depth: Int, pathNew: String) -> Int {
            let f = c.folders[fi]
            let id = nodes.count
            nodes.append(RNode(id: id, parent: parent, depth: depth, name: f.name, nameNew: f.name, path: f.path,
                               pathNew: pathNew, folder: fi, entry: nil, fileState: nil, folderState: f.status,
                               inOld: f.inOld, inNew: f.inNew))
            var kids: [Int] = []
            for ci in f.children {
                let name = c.folders[ci].name
                kids.append(folder(ci, parent: id, depth: depth + 1, pathNew: pathNew.isEmpty ? name : pathNew + "/" + name))
            }
            let files = f.entries.sorted {
                Self.last(c.entries[$0].path).localizedStandardCompare(Self.last(c.entries[$1].path)) == .orderedAscending
            }
            for ei in files { kids.append(entry(ei, parent: id, depth: depth + 1)) }

            var n = nodes[id]
            n.children = kids
            n.oldBytes = f.inOld ? f.oldBytes : nil
            var newBytes: Int64 = 0
            for k in kids {
                let child = nodes[k]
                n.open += child.open
                newBytes += child.newBytes ?? 0
                n.oldMs = max(n.oldMs ?? 0, child.oldMs ?? 0)
                n.newMs = max(n.newMs ?? 0, child.newMs ?? 0)
            }
            if n.oldMs == 0 { n.oldMs = nil }
            if n.newMs == 0 { n.newMs = nil }
            n.newBytes = f.inNew && (complete || !f.inOld) ? newBytes : nil
            nodes[id] = n
            return id
        }

        mutating func entry(_ ei: Int, parent: Int, depth: Int) -> Int {
            let e = c.entries[ei]
            let id = nodes.count
            let inOld = e.state != .onlyNew
            let inNew = [.same, .updated, .oldNewer, .unclear, .onlyNew].contains(e.state)
            let pathNew = e.pathNew ?? e.path
            var n = RNode(id: id, parent: parent, depth: depth, name: Self.last(e.path), nameNew: Self.last(pathNew),
                          path: e.path, pathNew: pathNew, folder: nil, entry: ei, fileState: e.state, folderState: nil,
                          inOld: inOld, inNew: inNew)
            n.movedTo = e.movedTo
            n.suggested = e.suggested
            n.oldBytes = inOld ? e.size : nil
            n.newBytes = e.state == .onlyNew ? e.size : (inNew ? e.sizeNew ?? (e.state == .same ? e.size : nil) : nil)
            n.oldMs = inOld ? e.mtimeOld : nil
            n.newMs = inNew || e.state == .moved ? e.mtimeNew : nil // moved: date at its new place
            n.open = e.state.risky ? 1 : 0
            nodes.append(n)
            return id
        }

        static func last(_ p: String) -> String { p.split(separator: "/").last.map(String.init) ?? p }
    }

    /// Fills in NEU sizes the CLI did not report (older versions without `size_new`).
    static func statMissingSizes(_ c: inout DupesComparison) {
        let root = URL(fileURLWithPath: c.new)
        for i in c.entries.indices where c.entries[i].sizeNew == nil {
            let e = c.entries[i]
            guard [.updated, .oldNewer, .unclear].contains(e.state) else { continue }
            let url = root.appendingPathComponent(e.pathNew ?? e.path)
            if let s = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber {
                c.entries[i].sizeNew = s.int64Value
            }
        }
    }
}

// MARK: - plan

enum PlanOp: String, Sendable {
    case copy, move, replace, keepBoth = "keep_both", trashOld = "trash_old"

    static let all: [PlanOp] = [.copy, .move, .replace, .keepBoth, .trashOld]

    /// Button / menu title.
    var title: String {
        switch self {
        case .copy: return "Nach NEU kopieren"
        case .move: return "Nach NEU verschieben"
        case .replace: return "ALT übernehmen"
        case .keepBoth: return "Beide behalten"
        case .trashOld: return "In ALT löschen (Papierkorb)"
        }
    }

    /// Marker in the row.
    var short: String {
        switch self {
        case .copy: return "kopieren"
        case .move: return "verschieben"
        case .replace: return "ersetzen"
        case .keepBoth: return "beide"
        case .trashOld: return "löschen"
        }
    }

    /// Marker in the row once done.
    var done: String {
        switch self {
        case .copy: return "kopiert"
        case .move: return "verschoben"
        case .replace: return "ersetzt"
        case .keepBoth: return "beide behalten"
        case .trashOld: return "im Papierkorb"
        }
    }

    /// What lands in the Trash (confirmation).
    var trashes: String? {
        switch self {
        case .copy, .keepBoth: return nil
        case .move: return "die ALT-Quelle, nach bestätigter Kopie"
        case .replace: return "die bisherige NEU-Fassung"
        case .trashOld: return "ALT – nur wenn NEU von jeder Datei eine identische Kopie hat (BLAKE3)"
        }
    }
}

/// The two checkbox columns of the tree; the plan stays the source of truth.
enum PlanColumn: Sendable {
    case copy, move

    var title: String { self == .copy ? "Kopieren" : "Verschieben" }
}

/// State of a column header: none, some or all visible rows checked.
enum CheckState { case none, mixed, all }

struct PlanItem: Sendable {
    let op: PlanOp
    let dir: Bool
    /// Target relative to NEU.
    let to: String
    let bytes: Int64
    /// Files in it (1 for a file).
    var files = 1
}

/// One line of the plan overview.
struct PlanRow: Identifiable {
    let id: Int
    let path: String
    let item: PlanItem

    /// From 1 GB on a plan item deserves a second look.
    var large: Bool { item.bytes >= ReconcileModel.largeBytes }
}

struct LineInfo: Sendable {
    let id: Int
    let old: Int?
    let new: Int?
    let added: Int?
    let removed: Int?
}

struct ApplyResult: Identifiable, Sendable {
    let id: Int
    let path: String
    let to: String
    var op: PlanOp?
    /// `done`, `skipped`, `failed` or `cancelled`.
    let result: String
    let error: String?
    /// `cloud_unavailable`: Google Drive/iCloud did not deliver a placeholder file.
    var code: String?

    var cloudUnavailable: Bool { code == "cloud_unavailable" }
}

struct ApplyReport: Identifiable {
    let id = UUID()
    var results: [ApplyResult]
    var done = 0
    var skipped = 0
    var failed = 0
    /// Actions not run because the run was cancelled.
    var cancelled = 0
    var bytes: Int64 = 0
    /// Stopped with „Abbrechen“.
    var wasCancelled = false
    /// macsync confirmed the stop (summary line): no partial copy is left.
    var cleanStop = false
    /// Finished copies whose ALT side is still there: candidates for `trash_old`.
    var offers: [TrashOffer] = []
    /// The run itself failed (no summary line).
    var error: String?
}

@MainActor
private final class ReportBox {
    var report = ApplyReport(results: [])
}

/// `--apply PLAN --probe`: does the cloud deliver the placeholder files of the plan?
struct CloudProbe {
    /// nil: no placeholders in the plan.
    var ok: Bool?
    var error: String?
    var files = 0
    var bytes: Int64 = 0
    /// Per plan node, when macsync reports it per action: still in the cloud,
    /// and what macsync will really copy (a folder copy takes everything in
    /// it, also what the comparison leaves out).
    var perNode: [Int: Int64] = [:]
    var sizes: [Int: (bytes: Int64, files: Int)] = [:]
}

/// A copy that is now in ALT and NEU; ALT may go after a checksum check.
struct TrashOffer: Identifiable, Sendable {
    let id: Int
    let path: String
    let to: String
    let dir: Bool
    let bytes: Int64
}

/// One action of a run, as written to the plan file.
struct RunAction {
    let op: PlanOp
    let dir: Bool
    let path: String
    let to: String
    let bytes: Int64
    var expectOld: [String: Int64]?
    var expectNew: [String: Int64]?

    var json: [String: Any] {
        var a: [String: Any] = ["op": op.rawValue, "kind": dir ? "dir" : "file", "path": path, "to": to]
        if let expectOld { a["expect_old"] = expectOld }
        if let expectNew { a["expect_new"] = expectNew }
        return a
    }
}

/// Where one plan action stands during and after a run.
enum RunState: Equatable {
    case waiting, running, done, skipped, failed, cancelled

    init(result: String) {
        switch result {
        case "done": self = .done
        case "skipped": self = .skipped
        case "cancelled": self = .cancelled
        default: self = .failed
        }
    }

    var finished: Bool { self != .waiting && self != .running }
}

/// A plan being executed (`--apply`), kept until the next comparison.
struct ApplyRun {
    /// Per action index (order of the plan file): path in ALT, operation, target.
    let paths: [String]
    let dirs: [Bool]
    let ops: [PlanOp]
    let tos: [String]
    /// From the `start` line, until then the comparison's sizes.
    var bytes: [Int64]
    var states: [RunState]
    var errors: [String?]
    var codes: [String?]
    /// To be downloaded from the cloud first (from the `start` line).
    var cloudBytes: [Int64]
    /// A `start` line came: progress by bytes, else by actions (older macsync).
    var byBytes = false
    var current: Int?
    /// Bytes of the running action so far, and the file it is at.
    var currentDone: Int64 = 0
    var file = ""
    var finished = 0
    /// Size of the actions done (actually copied).
    var doneBytes: Int64 = 0
    /// Of skipped, failed and cancelled actions: out of the total, so bar and
    /// ETA only count what really gets copied.
    var droppedBytes: Int64 = 0
    /// Copied bytes over the last seconds, for the rate.
    var samples: [(Date, Int64)] = []
    /// Last line from macsync: without one for a while rate and ETA are stale.
    var lastData = Date()
    var running = true
    var cancelling = false
    let started = Date()

    init(_ actions: [RunAction]) {
        paths = actions.map(\.path)
        dirs = actions.map(\.dir)
        ops = actions.map(\.op)
        tos = actions.map(\.to)
        bytes = actions.map(\.bytes)
        states = Array(repeating: .waiting, count: actions.count)
        errors = Array(repeating: nil, count: actions.count)
        codes = Array(repeating: nil, count: actions.count)
        cloudBytes = Array(repeating: 0, count: actions.count)
        current = actions.isEmpty ? nil : 0
        if !actions.isEmpty { states[0] = .running }
    }

    var total: Int { paths.count }
    var bytesTotal: Int64 { bytes.reduce(0, +) - droppedBytes }
    var processed: Int64 { doneBytes + currentDone }

    var fraction: Double {
        if byBytes {
            let t = bytesTotal
            return t > 0 ? min(1, Double(processed) / Double(t)) : Double(finished) / Double(max(total, 1))
        }
        return Double(finished) / Double(max(total, 1))
    }

    /// Of the action at `index` (running: its bytes so far).
    func fraction(_ index: Int) -> Double? {
        guard byBytes, index == current, bytes[index] > 0 else { return nil }
        return min(1, Double(currentDone) / Double(bytes[index]))
    }

    /// Bytes per second over the last ~10 s; nil until there are 2 s of data.
    var rate: Double? {
        guard let a = samples.first, let b = samples.last else { return nil }
        let dt = b.0.timeIntervalSince(a.0)
        guard dt >= 2 else { return nil }
        return Double(b.1 - a.1) / dt
    }

    var remaining: TimeInterval? {
        guard byBytes, let r = rate, r > 0 else { return nil }
        return Double(max(bytesTotal - processed, 0)) / r
    }

    mutating func sample() {
        let now = Date()
        lastData = now
        samples.append((now, doneBytes + currentDone))
        while samples.count > 2 && samples[1].0 < now.addingTimeInterval(-10) { samples.removeFirst() }
    }

    mutating func finish(_ index: Int, _ state: RunState, error: String?, code: String? = nil) {
        guard states.indices.contains(index), !states[index].finished else { return }
        states[index] = state
        errors[index] = error
        codes[index] = code
        finished += 1
        if state == .done { doneBytes += bytes[index] } else { droppedBytes += bytes[index] }
        if index == current {
            currentDone = 0
            file = ""
            current = index + 1 < total ? index + 1 : nil
            if let c = current, states[c] == .waiting, !cancelling { states[c] = .running }
        }
        sample()
    }
}

private final class LineCollector: @unchecked Sendable {
    private var lines: [String] = []
    private let lock = NSLock()

    func add(_ l: String) {
        lock.lock(); lines.append(l); lock.unlock()
    }

    var text: String {
        lock.lock(); defer { lock.unlock() }
        return lines.joined(separator: "\n")
    }
}

// MARK: - model

@MainActor
final class ReconcileModel: ObservableObject {
    enum Phase { case pick, ready }
    enum Mode: String, CaseIterable { case open = "Nur Offenes", all = "Alle" }

    @Published var oldPath: String?
    @Published var newPath: String?
    @Published private(set) var phase: Phase = .pick
    @Published private(set) var comparing = false
    @Published private(set) var started = Date()
    @Published private(set) var comparison: DupesComparison?
    /// `false` with an older macsync without `--files` (no identical files listed).
    @Published private(set) var complete = true
    @Published private(set) var nodes: [RNode] = []
    @Published private(set) var rows: [RNode] = []
    @Published private(set) var expanded = Set<Int>()
    /// Open folders while a chip or search is active; all matches start open.
    @Published private(set) var filterExpanded = Set<Int>()
    /// Chip, search and mode the filter folders were last opened for.
    private var filterKey: String?
    @Published var mode: Mode = .open { didSet { rebuild() } }
    @Published var chip: FileState? { didSet { rebuild() } }
    @Published var search = "" { didSet { rebuild() } }
    @Published var selection = Set<Int>() { didSet { if selection != oldValue { loadLines() } } }
    @Published private(set) var lines: LineInfo?
    @Published private(set) var plan: [Int: PlanItem] = [:] {
        didSet {
            probe = nil
            if mode == .open { rebuild() }
        }
    }
    /// Cloud check of the current plan (before „Ausführen“).
    @Published private(set) var probe: CloudProbe?
    @Published private(set) var probing = false
    private var probeProcess: Process?
    /// „Ausführen …“ was pressed and not yet answered.
    private var confirmWanted = false
    @Published var orderDismissed = false
    @Published var error: String?
    @Published var needsFullDiskAccess = false
    /// The plan being executed, or the last one until the next comparison.
    @Published private(set) var run: ApplyRun?
    /// Node id → action index of `run`.
    private var runIndex: [Int: Int] = [:]
    @Published var confirming = false
    @Published var showingPlan = false
    @Published var report: ApplyReport?
    @Published private(set) var recent: [[String]] = UserDefaults.standard.array(forKey: "reconcileRecent") as? [[String]] ?? []

    /// Called once a comparison has been loaded / a run has ended (screenshots).
    var onLoaded: (() -> Void)?
    var onReport: ((ApplyReport) -> Void)?
    private var process: Process?
    private var applyProcess: Process?
    /// Counts comparisons: a result of an outdated one is dropped.
    private var generation = 0

    var applying: Bool { run?.running == true }

    var canCompare: Bool {
        guard let o = oldPath, let n = newPath else { return false }
        return o != n && !comparing && !applying
    }

    var root: DupesComparison.Folder? { comparison?.folders.first }

    // MARK: comparison

    func swap() {
        (oldPath, newPath) = (newPath, oldPath)
    }

    func compare() {
        guard canCompare, let o = oldPath, let n = newPath else { return }
        remember(o, n)
        error = nil
        needsFullDiskAccess = false
        comparing = true
        started = Date()
        run(o, n, files: true)
    }

    func cancel() {
        generation += 1
        process?.terminate()
        process = nil
        comparing = false
    }

    private func run(_ o: String, _ n: String, files: Bool) {
        generation += 1
        let gen = generation
        let out = LineCollector()
        var args = ["dupes", o, n, "--json"]
        if files { args.append("--files") }
        process = CLI.stream(args, line: { out.add($0) }) { [weak self] status, err in
            let text = out.text
            Task { @MainActor in self?.finished(o, n, files: files, gen: gen, status: status, err: err, text: text) }
        }
    }

    private func finished(_ o: String, _ n: String, files: Bool, gen: Int, status: Int32, err: String, text: String) {
        guard gen == generation else { return }
        process = nil
        // Exit code 2 = usage error: a macsync without `--files`.
        if status == 2 && files {
            run(o, n, files: false)
            return
        }
        guard status == 0 else {
            comparing = false
            fail(err.isEmpty ? "Vergleich fehlgeschlagen (\(status))" : err)
            return
        }
        Task {
            let built: (DupesComparison, [RNode])? = await Task.detached {
                guard var c = try? Self.decoder.decode(DupesComparison.self, from: Data(text.utf8)), !c.folders.isEmpty else {
                    return nil
                }
                ReconcileTree.statMissingSizes(&c)
                return (c, ReconcileTree.build(c, complete: files))
            }.value
            guard gen == generation else { return }
            comparing = false
            guard let built else {
                fail("Unerwartete Ausgabe von macsync dupes")
                return
            }
            load(built.0, built.1, complete: files)
        }
    }

    private func fail(_ message: String) {
        error = message
        let m = message.lowercased()
        needsFullDiskAccess = m.contains("not permitted") || m.contains("permission denied") || m.contains("os error 1)")
    }

    private func load(_ c: DupesComparison, _ tree: [RNode], complete: Bool) {
        // Keep folders open across a re-comparison (after executing a plan).
        let keepOpen = phase == .ready ? Set(expanded.compactMap { nodes.indices.contains($0) ? nodes[$0].path : nil }) : nil
        comparison = c
        self.complete = complete
        nodes = tree
        plan = [:]
        // A run still going (ALT deleted while comparing) keeps its rows.
        if !applying { run = nil }
        reindexRun()
        selection = []
        orderDismissed = false
        if let keepOpen {
            expanded = Set(tree.filter { $0.isDir && keepOpen.contains($0.path) }.map(\.id))
        } else {
            // Open what needs a look; whole folders only in ALT stay closed (one action).
            expanded = Set(tree.filter { $0.folderState == .check || $0.folderState == .unreadable }.map(\.id))
        }
        needsFullDiskAccess = !c.skipped.isEmpty
        phase = .ready
        rebuild()
        onLoaded?()
        onLoaded = nil
    }

    func backToStart() {
        cancel()
        phase = .pick
    }

    private func remember(_ o: String, _ n: String) {
        guard ProcessInfo.processInfo.environment["MACSYNC_RECONCILE"] == nil else { return }
        recent.removeAll { $0 == [o, n] }
        recent.insert([o, n], at: 0)
        recent = Array(recent.prefix(5))
        UserDefaults.standard.set(recent, forKey: "reconcileRecent")
    }

    func use(_ pair: [String]) {
        guard pair.count == 2 else { return }
        oldPath = pair[0]
        newPath = pair[1]
    }

    nonisolated static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    // MARK: tree

    /// The visible rows: expanded folders, filtered by mode, chip and search.
    func rebuild() {
        guard !nodes.isEmpty else { rows = []; return }
        let include = filterSet()
        if let include {
            // A new filter opens all folders with matches; toggling keeps working.
            let key = "\(String(describing: chip))|\(search.trimmingCharacters(in: .whitespaces))|\(mode)"
            if key != filterKey { filterExpanded = include }
            filterKey = key
        } else {
            filterKey = nil
        }
        let open = include != nil ? filterExpanded : expanded
        var out: [RNode] = []
        func visit(_ id: Int) {
            for c in nodes[id].children {
                let n = nodes[c]
                if let include {
                    guard include.contains(c) else { continue }
                } else if mode == .open && n.open == 0 && plan[c] == nil {
                    continue
                }
                out.append(n)
                if n.isDir && open.contains(c) { visit(c) }
            }
        }
        visit(0)
        rows = out
    }

    /// With a chip or search: the matching nodes and their folders.
    private func filterSet() -> Set<Int>? {
        let q = search.trimmingCharacters(in: .whitespaces)
        guard chip != nil || !q.isEmpty else { return nil }
        var set = Set<Int>()
        for n in nodes where n.id != 0 {
            if let chip, n.fileState != chip { continue }
            if chip == nil && mode == .open && n.open == 0 && plan[n.id] == nil { continue }
            if !q.isEmpty && !n.name.localizedCaseInsensitiveContains(q) && !n.nameNew.localizedCaseInsensitiveContains(q) { continue }
            var p: Int? = n.id
            while let i = p, i != 0, set.insert(i).inserted { p = nodes[i].parent }
        }
        return set
    }

    var filtering: Bool { chip != nil || !search.trimmingCharacters(in: .whitespaces).isEmpty }

    func isExpanded(_ n: RNode) -> Bool { (filtering ? filterExpanded : expanded).contains(n.id) }

    func toggle(_ n: RNode) {
        guard n.isDir else { return }
        if filtering {
            if filterExpanded.remove(n.id) == nil { filterExpanded.insert(n.id) }
        } else if expanded.remove(n.id) == nil {
            expanded.insert(n.id)
        }
        rebuild()
    }

    func expandAll(_ open: Bool) {
        if filtering {
            filterExpanded = open ? filterSet() ?? [] : []
        } else {
            expanded = open ? Set(nodes.filter { $0.isDir && $0.id != 0 }.map(\.id)) : []
        }
        rebuild()
    }

    /// Selects and reveals a path (screenshots / tests).
    func select(path: String) {
        guard let n = nodes.first(where: { $0.path == path || $0.pathNew == path }) else { return }
        var p = n.parent
        while let i = p, i != 0 { expanded.insert(i); p = nodes[i].parent }
        rebuild()
        selection = [n.id]
    }

    func entry(_ n: RNode) -> DupesComparison.Entry? { n.entry.flatMap { comparison?.entries[$0] } }

    var selected: [RNode] { selection.sorted().compactMap { nodes.indices.contains($0) ? nodes[$0] : nil } }

    // MARK: files

    func oldURL(_ n: RNode) -> URL? {
        guard n.inOld, let c = comparison else { return nil }
        return n.path.isEmpty ? URL(fileURLWithPath: c.old) : URL(fileURLWithPath: c.old).appendingPathComponent(n.path)
    }

    func newURL(_ n: RNode) -> URL? {
        guard let c = comparison else { return nil }
        let root = URL(fileURLWithPath: c.new)
        if let m = n.movedTo { return root.appendingPathComponent(m) }
        guard n.inNew else { return nil }
        return n.pathNew.isEmpty ? root : root.appendingPathComponent(n.pathNew)
    }

    func showDiff(_ n: RNode) {
        guard !n.isDir else { return }
        FileCompareWindow.shared.show(old: oldURL(n), new: newURL(n), title: n.path)
    }

    func reveal(_ urls: [URL]) {
        if !urls.isEmpty { NSWorkspace.shared.activateFileViewerSelecting(urls) }
    }

    private func loadLines() {
        lines = nil
        guard selection.count == 1, let id = selection.first, nodes.indices.contains(id), !nodes[id].isDir else { return }
        let (a, b) = (oldURL(nodes[id]), newURL(nodes[id]))
        Task {
            let info = await Task.detached { () -> LineInfo in
                let delta = a.flatMap { a in b.flatMap { FileCompare.lineDelta(old: a, new: $0) } }
                return LineInfo(id: id, old: a.flatMap(FileCompare.lineCount), new: b.flatMap(FileCompare.lineCount),
                                added: delta?.added, removed: delta?.removed)
            }.value
            if selection == [id] { lines = info }
        }
    }

    static func openFullDiskAccess() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles") {
            NSWorkspace.shared.open(url)
        }
    }

    // MARK: plan

    /// Inside a folder that is copied or moved as a whole (in `plan`, default the current one).
    func covered(_ n: RNode, in plan: [Int: PlanItem]? = nil) -> Bool {
        let plan = plan ?? self.plan
        var p = n.parent
        while let i = p {
            if plan[i]?.dir == true { return true }
            p = nodes[i].parent
        }
        return false
    }

    func ops(for n: RNode, in plan: [Int: PlanItem]? = nil) -> [PlanOp] {
        guard !covered(n, in: plan) else { return [] }
        if n.isDir {
            guard n.id != 0, n.inOld else { return [] }
            if !n.inNew { return [.copy, .move] }
            // Only identical copies: macsync checks every file by checksum.
            return n.folderState == .identical ? [.trashOld] : []
        }
        switch n.fileState {
        case .onlyOld: return [.copy, .move]
        case .oldNewer, .unclear: return [.replace, .keepBoth]
        case .same, .moved: return [.trashOld]
        default: return []
        }
    }

    private func item(_ op: PlanOp, _ n: RNode) -> PlanItem {
        if n.isDir {
            let files = n.folder.flatMap { comparison?.folders[$0].total.oldFiles } ?? 0
            return PlanItem(op: op, dir: true, to: op == .trashOld ? n.pathNew : n.path, bytes: n.oldBytes ?? 0, files: files)
        }
        let to = op == .copy || op == .move ? n.suggested ?? n.path : op == .trashOld ? n.movedTo ?? n.pathNew : n.pathNew
        return PlanItem(op: op, dir: false, to: to, bytes: n.oldBytes ?? 0)
    }

    // MARK: checkbox columns

    /// What a checkbox in `column` plans for `n`, nil without one: „Kopieren“
    /// takes over the ALT version where NEU has the file too.
    func op(_ column: PlanColumn, for n: RNode) -> PlanOp? {
        let ops = ops(for: n)
        switch column {
        case .copy: return ops.contains(.copy) ? .copy : ops.contains(.replace) ? .replace : nil
        case .move: return ops.contains(.move) ? .move : nil
        }
    }

    func isChecked(_ column: PlanColumn, _ n: RNode) -> Bool {
        guard let p = plan[n.id] else { return false }
        return column == .copy ? p.op == .copy || p.op == .replace : p.op == .move
    }

    /// Inside a folder checked in `column`: shown as a grey check.
    func coveredBy(_ column: PlanColumn, _ n: RNode) -> Bool {
        var p = n.parent
        while let i = p {
            if let it = plan[i], it.dir { return column == .copy ? it.op == .copy : it.op == .move }
            p = nodes[i].parent
        }
        return false
    }

    /// Checking one column unchecks the other; checking it again removes the action.
    func toggle(_ column: PlanColumn, _ n: RNode) {
        if isChecked(column, n) {
            unplan([n])
        } else if let op = op(column, for: n) {
            setPlan(op, [n])
        }
    }

    /// The visible rows a header checkbox acts on (a closed folder is one row).
    private func eligible(_ column: PlanColumn) -> [RNode] {
        rows.filter { isChecked(column, $0) || op(column, for: $0) != nil }
    }

    func headerState(_ column: PlanColumn) -> CheckState {
        let rows = eligible(column)
        let on = rows.filter { isChecked(column, $0) }.count
        return on == 0 ? .none : on == rows.count ? .all : .mixed
    }

    /// None or some → all visible rows; all → none.
    func toggleAll(_ column: PlanColumn) {
        let rows = eligible(column)
        if headerState(column) == .all {
            unplan(rows)
        } else {
            check(column, rows)
        }
    }

    /// Space: „Kopieren“ for the selected rows, off when all are on.
    func toggleCopySelected() {
        let ns = selected.filter { isChecked(.copy, $0) || op(.copy, for: $0) != nil }
        guard !ns.isEmpty else { return }
        if ns.allSatisfy({ isChecked(.copy, $0) }) { unplan(ns) } else { check(.copy, ns) }
    }

    private func check(_ column: PlanColumn, _ ns: [RNode]) {
        guard !applying else { return }
        var p = plan
        // In tree order: a folder checked first covers its files.
        for n in ns.sorted(by: { $0.id < $1.id }) where !covered(n, in: p) {
            guard let op = op(column, for: n) else { continue }
            if n.isDir { removeBelow(n.id, from: &p) }
            p[n.id] = item(op, n)
        }
        plan = p
    }

    func setPlan(_ op: PlanOp, _ targets: [RNode]) {
        guard !applying else { return }
        var p = plan
        // `p`, not `plan`: a folder chosen together with its files covers them.
        for n in targets where ops(for: n, in: p).contains(op) {
            if n.isDir { removeBelow(n.id, from: &p) }
            p[n.id] = item(op, n)
        }
        plan = p
    }

    func unplan(_ targets: [RNode]) {
        guard !applying else { return }
        var p = plan
        for n in targets { p[n.id] = nil }
        plan = p
    }

    private func removeBelow(_ id: Int, from p: inout [Int: PlanItem]) {
        for c in nodes[id].children {
            p[c] = nil
            removeBelow(c, from: &p)
        }
    }

    /// Copy what is only in ALT, take over what is newer there; never for
    /// `unclear`, never deletes in ALT. Choices already made stay.
    func suggest() {
        guard !applying else { return }
        var p = plan
        func visit(_ id: Int, covered: Bool) {
            let n = nodes[id]
            var covered = covered || p[id]?.dir == true
            if !covered, n.isDir, n.id != 0, n.folderState == .onlyOld, !n.inNew,
               nodes[n.parent ?? 0].inNew || n.parent == 0, !hasSuggestion(below: id) {
                removeBelow(id, from: &p)
                p[id] = item(.copy, n)
                covered = true
            }
            if !covered, p[id] == nil {
                switch n.fileState {
                case .onlyOld: p[id] = item(.copy, n)
                case .oldNewer: p[id] = item(.replace, n)
                default: break
                }
            }
            for c in n.children { visit(c, covered: covered) }
        }
        visit(0, covered: false)
        plan = p
    }

    private func hasSuggestion(below id: Int) -> Bool {
        nodes[id].children.contains { nodes[$0].suggested != nil || hasSuggestion(below: $0) }
    }

    func clearPlan() {
        guard !applying else { return }
        plan = [:]
    }

    nonisolated static let largeBytes: Int64 = 1_000_000_000

    /// The plan, largest first.
    var planRows: [PlanRow] {
        plan.map { PlanRow(id: $0.key, path: nodes[$0.key].path, item: sized($0.key)) }
            .sorted { ($0.item.bytes, $1.path) > ($1.item.bytes, $0.path) }
    }

    /// A plan item with macsync's sizes once the probe has them.
    private func sized(_ id: Int) -> PlanItem {
        let it = plan[id]!
        guard let s = probe?.sizes[id] else { return it }
        return PlanItem(op: it.op, dir: it.dir, to: it.to, bytes: s.bytes, files: s.files)
    }

    private var sizedPlan: [PlanItem] { plan.keys.map(sized) }

    var planBytes: Int64 { sizedPlan.reduce(0) { $0 + $1.bytes } }
    /// Written to NEU, and checked in ALT before it goes to the Trash.
    var planCopyBytes: Int64 { sizedPlan.reduce(0) { $0 + ($1.op == .trashOld ? 0 : $1.bytes) } }
    var planTrashBytes: Int64 { planBytes - planCopyBytes }
    var planFiles: Int { sizedPlan.reduce(0) { $0 + $1.files } }

    /// Free space on NEU's volume.
    var newFreeBytes: Int64? {
        guard let n = comparison?.new else { return nil }
        let v = try? URL(fileURLWithPath: n).resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
        return v?.volumeAvailableCapacityForImportantUsage
    }

    /// From the confirmation to the plan overview.
    func reviewPlan() {
        confirmWanted = false
        confirming = false
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in self?.showingPlan = true }
    }

    /// macsync's reason in German where the user has to act on it.
    nonisolated static func reason(_ error: String) -> String {
        if error.contains(".git differs in NEU") {
            return "Git-Historie weicht ab (.git) – ALT könnte Commits/Branches haben, die NEU fehlen"
        }
        return error
    }

    /// „Google Drive“, „iCloud“, „Dropbox“ … for ALT in cloud storage.
    var cloudName: String {
        guard let o = comparison?.old else { return "Die Cloud" }
        if o.contains("/Library/Mobile Documents/") { return "iCloud" }
        guard let r = o.range(of: "/Library/CloudStorage/") else { return "Die Cloud" }
        let provider = o[r.upperBound...].split(separator: "/").first.map { String($0.split(separator: "-").first ?? $0) } ?? ""
        switch provider {
        case "GoogleDrive": return "Google Drive"
        case "OneDrive": return "OneDrive"
        case "": return "Die Cloud"
        default: return provider
        }
    }

    /// ALT in Google Drive, iCloud & Co.: files may be downloaded first.
    var oldInCloud: Bool {
        guard let o = comparison?.old else { return false }
        return o.contains("/Library/CloudStorage/") || o.contains("/Library/Mobile Documents/")
    }

    var planCounts: [(PlanOp, Int)] {
        PlanOp.all.compactMap { op in
            let n = plan.values.filter { $0.op == op }.count
            return n > 0 ? (op, n) : nil
        }
    }

    var planSummary: String {
        guard !plan.isEmpty else { return "Plan: leer – Aktionen über das Kontextmenü oder „Vorschläge“" }
        let parts = planCounts.map { "\($0.1) \($0.0.short)" }
        return "Plan: " + parts.joined(separator: " · ") + " · " + Activity.bytes(planBytes)
    }

    // MARK: execute

    /// What the plan does, one line per kind of action (confirmation).
    var planEffects: [String] {
        planCounts.map { op, n in
            switch op {
            case .copy: return "\(n) nach NEU kopieren"
            case .move: return "\(n) nach NEU verschieben"
            case .replace: return "\(n) NEU-Datei\(n == 1 ? "" : "en") durch die ALT-Fassung ersetzen"
            case .keepBoth: return "\(n) ALT-Fassung\(n == 1 ? "" : "en") zusätzlich neben die NEU-Datei legen („Name (ALT JJJJ-MM-TT)“)"
            case .trashOld: return "\(n) in ALT löschen"
            }
        }
    }

    /// Checks the cloud first, then asks (sheet), see `start()`.
    func execute() {
        guard comparison != nil, !plan.isEmpty, !applying, !comparing else { return }
        showingPlan = false
        confirmWanted = true
        checkCloud()
        // A quick check needs no spinner; a slow one shows it in the sheet.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
            guard let self, self.confirmWanted, self.probing else { return }
            self.confirming = true
        }
        if !probing { confirming = true }
    }

    func cancelConfirm() {
        confirmWanted = false
        confirming = false
        probeProcess?.terminate()
    }

    /// `--probe`: reads only the smallest placeholder file of the plan. An
    /// older macsync without it is skipped silently.
    func checkCloud() {
        guard let c = comparison, !probing else { return }
        let ordered = planOrder
        guard let file = writePlan(planActions(ordered)) else { return }
        probe = nil
        probing = true
        let out = LineCollector()
        probeProcess = CLI.stream(["dupes", c.old, c.new, "--apply", file.path, "--probe", "--json"], line: { out.add($0) }) { [weak self] _, _ in
            let text = out.text
            Task { @MainActor in
                try? FileManager.default.removeItem(at: file)
                self?.probed(text, ordered: ordered)
            }
        }
        if probeProcess == nil { probing = false }
    }

    private func probed(_ text: String, ordered: [Int]) {
        probeProcess = nil
        let obj = text.split(separator: "\n").lazy
            .compactMap { (try? JSONSerialization.jsonObject(with: Data($0.utf8))) as? [String: Any] }
            .first { $0["probe"] as? Bool == true }
        var p: CloudProbe?
        if let obj {
            var q = CloudProbe(ok: obj["cloud_ok"] as? Bool, error: obj["error"] as? String,
                               files: obj["cloud_files"] as? Int ?? 0, bytes: (obj["cloud_bytes"] as? NSNumber)?.int64Value ?? 0)
            for a in obj["actions"] as? [[String: Any]] ?? [] {
                guard let i = a["index"] as? Int, ordered.indices.contains(i) else { continue }
                if let b = a["cloud_bytes"] as? NSNumber, b.int64Value > 0 { q.perNode[ordered[i]] = b.int64Value }
                if let b = a["bytes"] as? NSNumber, let f = a["files"] as? Int { q.sizes[ordered[i]] = (b.int64Value, f) }
            }
            p = q
        }
        probe = p // after `plan` may have changed: its didSet cleared the old one
        probing = false
        if confirmWanted { confirming = true }
    }

    /// Plan file order; deleting in ALT last: by then the copies before it are done.
    private var planOrder: [Int] {
        plan.keys.sorted {
            (plan[$0]!.op == .trashOld ? 1 : 0, $0) < (plan[$1]!.op == .trashOld ? 1 : 0, $1)
        }
    }

    func start() {
        confirmWanted = false
        confirming = false
        probeProcess?.terminate()
        guard comparison != nil, !plan.isEmpty, !applying, !comparing else { return }
        launch(planActions(planOrder))
    }

    private func planActions(_ ordered: [Int]) -> [RunAction] {
        ordered.map { id in
            let n = nodes[id], it = plan[id]!
            var a = RunAction(op: it.op, dir: it.dir, path: n.path, to: it.to, bytes: it.bytes)
            if !it.dir, it.op != .trashOld, let e = entry(n) {
                if let m = e.mtimeOld { a.expectOld = ["size": e.size, "mtime_ms": m] }
                if it.op == .replace || it.op == .keepBoth, let s = n.newBytes, let m = e.mtimeNew {
                    a.expectNew = ["size": s, "mtime_ms": m]
                }
            }
            return a
        }
    }

    /// „In ALT löschen“ after a run: what was copied goes from ALT to the
    /// Trash, after macsync found identical copies in NEU.
    func trashOld(_ offers: [TrashOffer], confirmed: Bool = false) {
        guard comparison != nil, !offers.isEmpty, !applying else { return }
        if !confirmed && !confirmTrash(offers) { return }
        report = nil
        launch(offers.map { RunAction(op: .trashOld, dir: $0.dir, path: $0.path, to: $0.to, bytes: $0.bytes) })
    }

    private func confirmTrash(_ offers: [TrashOffer]) -> Bool {
        let alert = NSAlert()
        alert.messageText = offers.count == 1 ? "„\(offers[0].path)“ in ALT löschen?" : "\(offers.count) Einträge in ALT löschen?"
        var lines = offers.prefix(8).map { "• \($0.path) (\(Activity.bytes($0.bytes)))" }
        if offers.count > 8 { lines.append("… und \(offers.count - 8) weitere") }
        lines.append("")
        lines.append("Diese Ordner/Dateien kommen aus ALT in den macOS-Papierkorb – nur wenn NEU von jeder Datei eine identische Kopie hat (Prüfsumme BLAKE3). NEU wird nicht verändert.")
        if oldInCloud {
            lines.append("")
            lines.append("ALT liegt in \(cloudName): für die Prüfung werden die Dateien gelesen und dabei heruntergeladen.")
        }
        alert.informativeText = lines.joined(separator: "\n")
        alert.alertStyle = .warning
        alert.addButton(withTitle: "In ALT löschen")
        alert.addButton(withTitle: "Abbrechen")
        return alert.runModal() == .alertFirstButtonReturn
    }

    /// Writes the plan file and runs `macsync dupes ALT NEU --apply`.
    private func launch(_ actions: [RunAction]) {
        guard let c = comparison, !actions.isEmpty, let file = writePlan(actions) else { return }
        run = ApplyRun(actions)
        reindexRun()
        run?.sample()
        let box = ReportBox()
        applyProcess = CLI.stream(["dupes", c.old, c.new, "--apply", file.path, "--json"], line: { [weak self] l in
            Task { @MainActor in self?.applyLine(l, report: &box.report) }
        }) { [weak self] status, err in
            Task { @MainActor in
                try? FileManager.default.removeItem(at: file)
                self?.applied(status: status, err: err, report: box.report)
            }
        }
    }

    private func writePlan(_ actions: [RunAction]) -> URL? {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("macsync-plan-\(UUID().uuidString).json")
        do {
            try JSONSerialization.data(withJSONObject: ["actions": actions.map(\.json)], options: [.prettyPrinted]).write(to: file)
            return file
        } catch {
            self.error = "Plan nicht gespeichert: \(error.localizedDescription)"
            return nil
        }
    }

    /// Rows of the run by path: they survive a re-comparison.
    private func reindexRun() {
        runIndex = [:]
        guard let run else { return }
        let index = Dictionary(run.paths.enumerated().map { ($1, $0) }, uniquingKeysWith: { a, _ in a })
        for n in nodes where n.id != 0 {
            if let i = index[n.path] { runIndex[n.id] = i }
        }
    }

    /// SIGTERM: macsync stops the running action, removes its partial copy
    /// and reports the rest as `cancelled`.
    func cancelRun() {
        guard applying, run?.cancelling == false else { return }
        run?.cancelling = true
        applyProcess?.terminate()
    }

    private func applyLine(_ l: String, report: inout ApplyReport) {
        guard let obj = (try? JSONSerialization.jsonObject(with: Data(l.utf8))) as? [String: Any] else { return }
        if obj["summary"] as? Bool == true {
            report.done = obj["done"] as? Int ?? 0
            report.skipped = obj["skipped"] as? Int ?? 0
            report.failed = obj["failed"] as? Int ?? 0
            report.bytes = (obj["bytes"] as? NSNumber)?.int64Value ?? 0
            report.wasCancelled = obj["cancelled"] as? Bool ?? false
            report.cleanStop = report.wasCancelled
            return
        }
        if obj["start"] as? Bool == true {
            guard var r = run else { return }
            for a in obj["actions"] as? [[String: Any]] ?? [] {
                guard let i = a["index"] as? Int, r.bytes.indices.contains(i) else { continue }
                if let b = a["bytes"] as? NSNumber { r.bytes[i] = b.int64Value }
                if let b = a["cloud_bytes"] as? NSNumber { r.cloudBytes[i] = b.int64Value }
            }
            r.byBytes = true
            run = r
            return
        }
        if obj["progress"] as? Bool == true {
            guard var r = run, let i = obj["index"] as? Int, r.states.indices.contains(i), !r.states[i].finished else { return }
            if r.current != i {
                if let c = r.current, r.states[c] == .running { r.states[c] = .waiting }
                r.current = i
            }
            if !r.cancelling { r.states[i] = .running }
            r.currentDone = (obj["bytes_done"] as? NSNumber)?.int64Value ?? 0
            r.file = obj["file"] as? String ?? ""
            r.sample()
            run = r
            return
        }
        let i = obj["index"] as? Int ?? report.results.count
        let result = ApplyResult(id: i, path: obj["path"] as? String ?? "", to: obj["to"] as? String ?? "",
                                 op: run.flatMap { $0.ops.indices.contains(i) ? $0.ops[i] : nil },
                                 result: obj["result"] as? String ?? "failed", error: obj["error"] as? String,
                                 code: obj["code"] as? String)
        report.results.append(result)
        run?.finish(i, RunState(result: result.result), error: result.error, code: result.code)
    }

    private func applied(status: Int32, err: String, report: ApplyReport) {
        applyProcess = nil
        var r = report
        let cancelling = run?.cancelling == true
        if var run {
            // Without an outcome line (older macsync stopped by SIGTERM, crash).
            for i in run.states.indices where !run.states[i].finished {
                let state: RunState = cancelling ? .cancelled : .failed
                let error = cancelling ? nil : (err.isEmpty ? "keine Rückmeldung von macsync" : err)
                run.finish(i, state, error: error)
                if !r.results.isEmpty || cancelling {
                    r.results.append(ApplyResult(id: i, path: run.paths[i], to: run.tos[i], op: run.ops[i],
                                                 result: state == .cancelled ? "cancelled" : "failed", error: error))
                }
            }
            run.running = false
            run.cancelling = false
            self.run = run
            if r.bytes == 0 {
                r.bytes = run.states.indices.reduce(0) { $0 + (run.states[$1] == .done && run.ops[$1] != .trashOld ? run.bytes[$1] : 0) }
            }
            // ALT is still there next to its new copy: offer to delete it.
            r.offers = r.results.compactMap { res in
                guard res.result == "done", let op = res.op, [.copy, .replace, .keepBoth].contains(op),
                      run.paths.indices.contains(res.id) else { return nil }
                return TrashOffer(id: res.id, path: run.paths[res.id], to: res.to.isEmpty ? run.tos[res.id] : res.to,
                                  dir: run.dirs[res.id], bytes: run.bytes[res.id])
            }
        }
        r.wasCancelled = r.wasCancelled || cancelling
        // From the outcome lines: also right when the summary is missing.
        let count = { (result: String) in r.results.filter { $0.result == result }.count }
        (r.done, r.skipped, r.failed, r.cancelled) = (count("done"), count("skipped"), count("failed"), count("cancelled"))
        if r.results.isEmpty && status != 0 && !cancelling {
            r.error = status == 2 ? "Diese macsync-Version kann Pläne noch nicht ausführen (--apply fehlt)." : err
        }
        self.report = r
        onReport?(r)
        if r.results.contains(where: { $0.result != "cancelled" }) {
            plan = [:]
            // What was done changes the picture: compare again (also when a
            // comparison started during the run, it is outdated now).
            if comparing { cancel() }
            compare()
        }
        // Nothing happened: the plan stays; the rows show „abgebrochen“ until
        // the result is closed (`closeReport`).
    }

    func closeReport() {
        report = nil
        if !applying, !plan.isEmpty {
            run = nil
            runIndex = [:]
        }
    }

    /// Run state of a row: its own action or that of the folder it is in.
    func runState(_ n: RNode) -> (state: RunState, index: Int)? {
        guard let run else { return nil }
        var p: Int? = n.id
        while let id = p {
            if let i = runIndex[id] { return (run.states[i], i) }
            p = nodes[id].parent
        }
        return nil
    }
}

// MARK: - window

/// The folder reconcile window.
@MainActor
final class ReconcileWindow: NSObject, NSWindowDelegate {
    static let shared = ReconcileWindow()
    private var window: NSWindow?
    private(set) var model: ReconcileModel?

    /// With both folders given, the comparison starts right away.
    func show(old: String? = nil, new: String? = nil) {
        let m = model ?? ReconcileModel()
        if let old { m.oldPath = old }
        if let new { m.newPath = new }
        if old != nil && new != nil { m.compare() }
        if let w = window {
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let host = NSHostingController(rootView: ReconcileView(model: m))
        host.sizingOptions = [.minSize] // keep the window size below, not the minimum
        let w = NSWindow(contentViewController: host)
        w.title = "MacSync – Ordnerabgleich"
        w.styleMask = [.titled, .closable, .resizable, .miniaturizable]
        w.setContentSize(NSSize(width: 1180, height: 720))
        w.isReleasedWhenClosed = false
        w.delegate = self
        let snapshot = ProcessInfo.processInfo.environment["MACSYNC_SNAPSHOT"]
        if snapshot == nil { w.setFrameAutosaveName("MacSyncReconcile") }
        w.center()
        model = m
        window = w
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        // Screenshots (see ManagerWindow): after the comparison has loaded, or
        // of the start screen when not both folders are given.
        if let snapshot {
            let env = ProcessInfo.processInfo.environment
            // MACSYNC_RECONCILE_DELAY="4,9": one shot per delay (name-4.png, name-9.png).
            let delays = (env["MACSYNC_RECONCILE_DELAY"] ?? "").split(separator: ",").compactMap { Double($0) }
            let take = { [weak w] in
                for d in delays.isEmpty ? [1.5] : delays {
                    DispatchQueue.main.asyncAfter(deadline: .now() + d) {
                        guard let w, let frame = w.contentView?.superview else { return }
                        let base = URL(fileURLWithPath: snapshot)
                        let url = delays.count > 1
                            ? base.deletingPathExtension().appendingPathExtension("\(Int(d))").appendingPathExtension("png") : base
                        // A sheet or popover is a window of its own: it goes next to it as name-sheet.png.
                        let popover = NSApp.windows.first { $0.isVisible && $0.className.contains("Popover") }
                        if let v = (w.attachedSheet ?? popover)?.contentView?.superview {
                            ManagerWindow.snapshot(v, to: url.deletingPathExtension().appendingPathExtension("sheet.png"))
                        }
                        ManagerWindow.snapshot(frame, to: url)
                    }
                }
            }
            if m.comparing {
                m.onLoaded = { [weak m] in
                    if env["MACSYNC_RECONCILE_FILTER"] == "all" { m?.mode = .all }
                    if env["MACSYNC_RECONCILE_SUGGEST"] != nil { m?.suggest() }
                    // Select once the table shows the new rows.
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                        if let p = env["MACSYNC_RECONCILE_SELECT"] { m?.select(path: p) }
                        // Checkboxes: "copy:PATH,move:PATH,all:copy" (comma separated).
                        for c in (env["MACSYNC_RECONCILE_CHECK"] ?? "").split(separator: ",") {
                            let parts = c.split(separator: ":", maxSplits: 1).map(String.init)
                            guard let m, parts.count == 2 else { continue }
                            if parts[0] == "all" {
                                m.toggleAll(parts[1] == "move" ? .move : .copy)
                            } else if let n = m.nodes.first(where: { $0.path == parts[1] }) {
                                m.toggle(parts[0] == "move" ? .move : .copy, n)
                            }
                        }
                        // Paths (comma separated) planned for „In ALT löschen“.
                        for p in (env["MACSYNC_RECONCILE_TRASHPLAN"] ?? "").split(separator: ",") {
                            if let m, let n = m.nodes.first(where: { $0.path == p }) { m.setPlan(.trashOld, [n]) }
                        }
                        // After the run: delete every offered copy in ALT, without asking.
                        if env["MACSYNC_RECONCILE_TRASH"] != nil {
                            m?.onReport = { [weak m] r in
                                m?.onReport = nil
                                DispatchQueue.main.asyncAfter(deadline: .now() + 2) { m?.trashOld(r.offers, confirmed: true) }
                            }
                        }
                        // confirm | plan | execute (without asking), optionally cancelled after n s.
                        switch env["MACSYNC_RECONCILE_SHOW"] {
                        case "confirm": m?.execute()
                        case "plan": m?.showingPlan = true
                        case "execute": m?.start()
                        default: break
                        }
                        if let s = env["MACSYNC_RECONCILE_CANCEL"].flatMap(Double.init) {
                            DispatchQueue.main.asyncAfter(deadline: .now() + s) { m?.cancelRun() }
                        }
                        take()
                    }
                }
            } else {
                take()
            }
        }
    }

    func windowWillClose(_ notification: Notification) {
        model?.cancel()
        model?.cancelRun()
        model = nil
        window = nil
    }
}
