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
    case copy, move, replace, keepBoth = "keep_both"

    /// Button / menu title.
    var title: String {
        switch self {
        case .copy: return "Nach NEU kopieren"
        case .move: return "Nach NEU verschieben"
        case .replace: return "ALT übernehmen"
        case .keepBoth: return "Beide behalten"
        }
    }

    /// Marker in the row.
    var short: String {
        switch self {
        case .copy: return "kopieren"
        case .move: return "verschieben"
        case .replace: return "ersetzen"
        case .keepBoth: return "beide"
        }
    }
}

struct PlanItem: Sendable {
    let op: PlanOp
    let dir: Bool
    /// Target relative to NEU.
    let to: String
    let bytes: Int64
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
    /// `done`, `skipped` or `failed`.
    let result: String
    let error: String?
}

struct ApplyReport: Identifiable {
    let id = UUID()
    var results: [ApplyResult]
    var done = 0
    var skipped = 0
    var failed = 0
    var bytes: Int64 = 0
    /// The run itself failed (no summary line).
    var error: String?
}

@MainActor
private final class ReportBox {
    var report = ApplyReport(results: [])
}

struct ApplyProgress {
    var total: Int
    var finished = 0
    var current = ""
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
    @Published var mode: Mode = .open { didSet { rebuild() } }
    @Published var chip: FileState? { didSet { rebuild() } }
    @Published var search = "" { didSet { rebuild() } }
    @Published var selection = Set<Int>() { didSet { if selection != oldValue { loadLines() } } }
    @Published private(set) var lines: LineInfo?
    @Published private(set) var plan: [Int: PlanItem] = [:] { didSet { if mode == .open { rebuild() } } }
    @Published var orderDismissed = false
    @Published var error: String?
    @Published var needsFullDiskAccess = false
    @Published private(set) var applying: ApplyProgress?
    @Published var report: ApplyReport?
    @Published private(set) var recent: [[String]] = UserDefaults.standard.array(forKey: "reconcileRecent") as? [[String]] ?? []

    /// Called once a comparison has been loaded (screenshots).
    var onLoaded: (() -> Void)?
    private var process: Process?
    private var cancelled = false

    var canCompare: Bool {
        guard let o = oldPath, let n = newPath else { return false }
        return o != n && !comparing && applying == nil
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
        cancelled = true
        process?.terminate()
        process = nil
        comparing = false
    }

    private func run(_ o: String, _ n: String, files: Bool) {
        cancelled = false
        let out = LineCollector()
        var args = ["dupes", o, n, "--json"]
        if files { args.append("--files") }
        process = CLI.stream(args, line: { out.add($0) }) { [weak self] status, err in
            let text = out.text
            Task { @MainActor in self?.finished(o, n, files: files, status: status, err: err, text: text) }
        }
    }

    private func finished(_ o: String, _ n: String, files: Bool, status: Int32, err: String, text: String) {
        guard !cancelled else { return }
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
                if n.isDir && (include != nil || expanded.contains(c)) { visit(c) }
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

    func isExpanded(_ n: RNode) -> Bool { filtering || expanded.contains(n.id) }

    func toggle(_ n: RNode) {
        guard n.isDir, !filtering else { return }
        if expanded.contains(n.id) { expanded.remove(n.id) } else { expanded.insert(n.id) }
        rebuild()
    }

    func expandAll(_ open: Bool) {
        expanded = open ? Set(nodes.filter { $0.isDir && $0.id != 0 }.map(\.id)) : []
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
        if n.isDir { return n.inOld && !n.inNew && n.id != 0 ? [.copy, .move] : [] }
        switch n.fileState {
        case .onlyOld: return [.copy, .move]
        case .oldNewer, .unclear: return [.replace, .keepBoth]
        default: return []
        }
    }

    private func item(_ op: PlanOp, _ n: RNode) -> PlanItem {
        if n.isDir { return PlanItem(op: op, dir: true, to: n.path, bytes: n.oldBytes ?? 0) }
        let to = op == .copy || op == .move ? n.suggested ?? n.path : n.pathNew
        return PlanItem(op: op, dir: false, to: to, bytes: n.oldBytes ?? 0)
    }

    func setPlan(_ op: PlanOp, _ targets: [RNode]) {
        var p = plan
        // `p`, not `plan`: a folder chosen together with its files covers them.
        for n in targets where ops(for: n, in: p).contains(op) {
            if n.isDir { removeBelow(n.id, from: &p) }
            p[n.id] = item(op, n)
        }
        plan = p
    }

    func unplan(_ targets: [RNode]) {
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
    /// `unclear`. Choices already made stay.
    func suggest() {
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

    func clearPlan() { plan = [:] }

    var planCounts: [(PlanOp, Int)] {
        [PlanOp.copy, .move, .replace, .keepBoth].compactMap { op in
            let n = plan.values.filter { $0.op == op }.count
            return n > 0 ? (op, n) : nil
        }
    }

    var planSummary: String {
        guard !plan.isEmpty else { return "Plan: leer – Aktionen über das Kontextmenü oder „Vorschläge“" }
        let parts = planCounts.map { "\($0.1) \($0.0.short)" }
        let bytes = plan.values.reduce(Int64(0)) { $0 + $1.bytes }
        return "Plan: " + parts.joined(separator: " · ") + " · " + Activity.bytes(bytes)
    }

    // MARK: execute

    func execute() {
        guard let c = comparison, !plan.isEmpty, applying == nil, !comparing else { return }
        let alert = NSAlert()
        alert.messageText = plan.count == 1 ? "1 Aktion ausführen?" : "\(plan.count) Aktionen ausführen?"
        var lines: [String] = []
        for (op, n) in planCounts {
            switch op {
            case .copy: lines.append("• \(n) nach NEU kopieren")
            case .move: lines.append("• \(n) nach NEU verschieben – die ALT-Quelle kommt nach bestätigter Kopie in den Papierkorb")
            case .replace: lines.append("• \(n) NEU-Datei\(n == 1 ? "" : "en") durch die ALT-Fassung ersetzen – die bisherige NEU-Fassung kommt in den Papierkorb")
            case .keepBoth: lines.append("• \(n) ALT-Fassung\(n == 1 ? "" : "en") zusätzlich neben die NEU-Datei legen („Name (ALT JJJJ-MM-TT)“)")
            }
        }
        lines.append("")
        lines.append("Nichts wird überschrieben oder endgültig gelöscht. Was sich seit dem Vergleich geändert hat, wird übersprungen.")
        alert.informativeText = lines.joined(separator: "\n")
        alert.alertStyle = plan.values.contains { $0.op == .move || $0.op == .replace } ? .warning : .informational
        alert.addButton(withTitle: "Ausführen")
        alert.addButton(withTitle: "Abbrechen")
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        let ordered = plan.keys.sorted()
        var actions: [[String: Any]] = []
        for id in ordered {
            let n = nodes[id], it = plan[id]!
            var a: [String: Any] = ["op": it.op.rawValue, "kind": it.dir ? "dir" : "file", "path": n.path, "to": it.to]
            if !it.dir, let e = entry(n) {
                if let m = e.mtimeOld { a["expect_old"] = ["size": e.size, "mtime_ms": m] }
                if it.op == .replace || it.op == .keepBoth, let s = n.newBytes, let m = e.mtimeNew {
                    a["expect_new"] = ["size": s, "mtime_ms": m]
                }
            }
            actions.append(a)
        }
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("macsync-plan-\(UUID().uuidString).json")
        do {
            try JSONSerialization.data(withJSONObject: ["actions": actions], options: [.prettyPrinted]).write(to: file)
        } catch {
            self.error = "Plan nicht gespeichert: \(error.localizedDescription)"
            return
        }
        let paths = ordered.map { nodes[$0].path }
        applying = ApplyProgress(total: actions.count, current: paths.first ?? "")
        let box = ReportBox()
        _ = CLI.stream(["dupes", c.old, c.new, "--apply", file.path, "--json"], line: { [weak self] l in
            Task { @MainActor in self?.applyLine(l, paths: paths, report: &box.report) }
        }) { [weak self] status, err in
            Task { @MainActor in
                try? FileManager.default.removeItem(at: file)
                self?.applied(status: status, err: err, report: box.report)
            }
        }
    }

    private func applyLine(_ l: String, paths: [String], report: inout ApplyReport) {
        guard let obj = (try? JSONSerialization.jsonObject(with: Data(l.utf8))) as? [String: Any] else { return }
        if obj["summary"] as? Bool == true {
            report.done = obj["done"] as? Int ?? 0
            report.skipped = obj["skipped"] as? Int ?? 0
            report.failed = obj["failed"] as? Int ?? 0
            report.bytes = (obj["bytes"] as? NSNumber)?.int64Value ?? 0
            return
        }
        let i = obj["index"] as? Int ?? report.results.count
        report.results.append(ApplyResult(id: i, path: obj["path"] as? String ?? "", to: obj["to"] as? String ?? "",
                                          result: obj["result"] as? String ?? "failed", error: obj["error"] as? String))
        applying?.finished = report.results.count
        applying?.current = paths.indices.contains(i + 1) ? paths[i + 1] : ""
    }

    private func applied(status: Int32, err: String, report: ApplyReport) {
        applying = nil
        var r = report
        if r.results.isEmpty && status != 0 {
            r.error = status == 2 ? "Diese macsync-Version kann Pläne noch nicht ausführen (--apply fehlt)." : err
        }
        self.report = r
        if !r.results.isEmpty {
            plan = [:]
            // What was done changes the picture: compare again.
            compare()
        }
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
            let take = { [weak w] in
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                    guard let w, let frame = w.contentView?.superview else { return }
                    ManagerWindow.snapshot(frame, to: URL(fileURLWithPath: snapshot))
                }
            }
            if m.comparing {
                m.onLoaded = { [weak m] in
                    if env["MACSYNC_RECONCILE_FILTER"] == "all" { m?.mode = .all }
                    if env["MACSYNC_RECONCILE_SUGGEST"] != nil { m?.suggest() }
                    // Select once the table shows the new rows.
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                        if let p = env["MACSYNC_RECONCILE_SELECT"] { m?.select(path: p) }
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
        model = nil
        window = nil
    }
}
