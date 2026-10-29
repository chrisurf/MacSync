import AppKit
import SwiftUI

/// One item of the daemon's `browse` answer (state on both Macs).
struct SyncItem: Decodable, Hashable {
    var name: String
    var unit: String
    var path: String
    var kind: String
    var isUnit: Bool
    var size: Int64?
    var mtimeMs: Int64?
    var local: String
    var remote: String
    var state: String
    var paused: Bool
    var conflictId: Int64?
    /// The other Mac changed `.git/config` or a hook of this repository.
    struct GitNotice: Decodable, Hashable {
        var path: String
        var atMs: Int64
    }

    var gitNotice: GitNotice?

    var isSynced: Bool { state == "synced" }
}

struct BrowseResult: Decodable {
    var online: Bool
    var peer: String?
    /// Unit of the listed directory; `nil` for the sync root.
    var unit: String?
    /// The items carry the other Mac's state.
    var remote: Bool?
    var items: [SyncItem]
}

/// One file or folder as the file system lists it.
struct DiskEntry: Hashable, Sendable {
    var name: String
    var kind: String
    var size: Int64?
    var mtime: Date?
    /// A folder with `.git` inside: a sync unit of its own.
    var isRepo: Bool
    /// Folder with sub-folders (disclosure arrow in the tree).
    var hasFolders = false
}

/// Directory listings straight from disk: instant, independent of the daemon.
enum Disk {
    /// Never shown – the same names the scanner skips (crates/fs/src/exclude.rs).
    static let hidden: Set<String> = [
        ".macsync", ".DS_Store", "Icon\r", ".Spotlight-V100", ".Trashes", ".fseventsd",
        ".TemporaryItems", ".DocumentRevisions-V100",
    ]

    /// `nil` if the folder does not exist (here).
    static func list(_ dir: URL) -> [DiskEntry]? {
        let keys: [URLResourceKey] = [.isDirectoryKey, .isSymbolicLinkKey, .fileSizeKey, .contentModificationDateKey]
        guard let urls = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: keys) else {
            return nil
        }
        return urls.compactMap { u in
            let name = u.lastPathComponent
            guard !hidden.contains(name), let v = try? u.resourceValues(forKeys: Set(keys)) else { return nil }
            let kind = v.isSymbolicLink == true ? "symlink" : (v.isDirectory == true ? "dir" : "file")
            return DiskEntry(name: name, kind: kind, size: kind == "file" ? v.fileSize.map(Int64.init) : nil,
                             mtime: v.contentModificationDate,
                             isRepo: kind == "dir" && FileManager.default.fileExists(atPath: u.appendingPathComponent(".git").path))
        }
    }

    /// Sub-folders for the tree, each with whether it has sub-folders itself.
    static func folders(_ dir: URL) -> [DiskEntry] {
        (list(dir) ?? []).filter { $0.kind == "dir" }.map { e in
            var e = e
            e.hasFolders = hasFolders(dir.appendingPathComponent(e.name))
            return e
        }
        .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    private static func hasFolders(_ dir: URL) -> Bool {
        guard let it = FileManager.default.enumerator(at: dir, includingPropertiesForKeys: [.isDirectoryKey],
                                                      options: [.skipsSubdirectoryDescendants]) else { return false }
        for case let u as URL in it where !hidden.contains(u.lastPathComponent) {
            if (try? u.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory == true { return true }
        }
        return false
    }

    /// Unit of a root-relative folder as the daemon would resolve it: the
    /// deepest repository on the way, else the top-level folder.
    static func unit(of path: String, root: URL) -> String? {
        let parts = path.split(separator: "/").map(String.init)
        guard !parts.isEmpty else { return nil }
        for n in stride(from: parts.count, to: 1, by: -1) {
            let p = parts.prefix(n).joined(separator: "/")
            if FileManager.default.fileExists(atPath: root.appendingPathComponent(p).appendingPathComponent(".git").path) {
                return p
            }
        }
        return parts[0]
    }
}

/// One row of the manager: the disk entry merged with the daemon's state.
struct ManagerRow: Identifiable, Hashable {
    var id: String { name }
    var name: String
    /// Relative to the sync root.
    var path: String
    var kind: String
    /// Exists on this Mac (from the file system, not the database).
    var onDisk: Bool
    var size: Int64?
    var mtime: Date?
    var isUnit: Bool
    /// Unit for sync actions (from the daemon, or derived from the folders).
    var unit: String
    /// The daemon's view; `nil`: not loaded (yet) or not known to the daemon.
    var item: SyncItem?

    var isDir: Bool { kind == "dir" }
    var paused: Bool { item?.paused ?? false }
}

@MainActor
final class ManagerModel: ObservableObject {
    enum Filter: String, CaseIterable { case all = "Alle", pending = "Nicht synchron" }

    /// How far the status of the current folder has arrived.
    enum Phase: Equatable {
        /// Nothing from the daemon yet.
        case loading
        /// This Mac's state (fast step); the other Mac's state follows.
        case local
        /// Complete; `remote`: with the other Mac's state.
        case done(remote: Bool)
        case noDaemon
        case failed
    }

    /// Root-relative folder shown (`""` = the sync root).
    @Published private(set) var path = ""
    @Published private(set) var rows: [ManagerRow] = []
    /// The disk listing of `path` has arrived.
    @Published private(set) var listed = false
    /// `path` exists on this Mac.
    @Published private(set) var exists = true
    @Published private(set) var phase: Phase = .loading
    @Published private(set) var online = false
    @Published private(set) var peerName = "anderer Mac"
    @Published var error: String?
    @Published var filter: Filter = .all
    @Published var search = ""
    @Published var selection = Set<ManagerRow.ID>()
    /// Folder tree on the left (root = the sync root), read from disk.
    let tree: TreeNode
    @Published var treeSelection: String? = ""

    let rootURL: URL
    private var disk: [DiskEntry]?
    /// Unit of `path` derived from the folders (until the daemon tells).
    private var diskUnit: String?
    private var status: BrowseResult?
    /// Bumped on navigation: late answers for another folder are dropped.
    private var generation = 0
    private var diskTask: Task<Void, Never>?
    private var statusTask: Task<Void, Never>?
    private var timer: Timer?
    private var openedAt = Date()
    private var reportedOpen = false

    init() {
        rootURL = URL(fileURLWithPath: Paths.expand(AppModel.shared.config?.string("root") ?? "~"))
        tree = TreeNode(title: rootURL.lastPathComponent, path: "", isUnit: false, root: rootURL)
    }

    var shown: [ManagerRow] {
        rows.filter { row in
            (filter == .all || isPending(row))
                && (search.isEmpty || row.name.localizedCaseInsensitiveContains(search))
        }
    }

    var selected: [ManagerRow] { rows.filter { selection.contains($0.id) } }

    /// The other Mac's state is loaded.
    var remoteKnown: Bool { phase == .done(remote: true) }
    var statusLoading: Bool { phase == .loading || phase == .local }
    var peerUnreachable: Bool { phase == .done(remote: false) }

    func isPending(_ row: ManagerRow) -> Bool {
        guard let i = row.item else { return !row.onDisk }
        return !row.onDisk || i.paused || i.conflictId != nil || (remoteKnown && !i.isSynced)
    }

    /// Breadcrumb: (title, path) from the root down to here.
    var crumbs: [(title: String, path: String)] {
        var out = [(rootURL.lastPathComponent, "")]
        var acc = ""
        for part in path.split(separator: "/") {
            acc = acc.isEmpty ? String(part) : acc + "/" + part
            out.append((String(part), acc))
        }
        return out
    }

    func url(_ path: String) -> URL { path.isEmpty ? rootURL : rootURL.appendingPathComponent(path) }

    func start(path: String?) {
        tree.expanded = true
        openedAt = Date()
        go(path ?? "")
        timer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { _ in
            Task { @MainActor in ManagerWindow.shared.model?.refresh() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        diskTask?.cancel()
        statusTask?.cancel()
    }

    /// Opens a root-relative folder such as `Projekte/website/src`.
    func go(_ newPath: String) {
        let p = newPath.split(separator: "/").filter { $0 != "." && !$0.isEmpty }.joined(separator: "/")
        generation += 1
        diskTask?.cancel()
        statusTask?.cancel()
        diskTask = nil
        statusTask = nil
        path = p
        disk = nil
        diskUnit = nil
        status = nil
        listed = false
        exists = true
        phase = .loading
        error = nil
        selection = []
        rows = []
        treeSelection = p
        Task { await revealInTree() }
        refresh()
    }

    /// Re-reads disk and status; a step still running for this folder is not
    /// started twice.
    func refresh() {
        if diskTask == nil { diskTask = Task { await loadDisk() } }
        if statusTask == nil { statusTask = Task { await loadStatus() } }
    }

    private func loadDisk() async {
        let g = generation, p = path, dir = url(p), root = rootURL
        defer { if g == generation { diskTask = nil } }
        let (entries, unit) = await Task.detached { (Disk.list(dir), Disk.unit(of: p, root: root)) }.value
        guard g == generation else { return }
        disk = entries ?? []
        diskUnit = unit
        exists = entries != nil
        listed = true
        rebuild()
        if !reportedOpen, ProcessInfo.processInfo.environment["MACSYNC_SNAPSHOT"] != nil {
            reportedOpen = true
            let ms = Int(Date().timeIntervalSince(openedAt) * 1000)
            FileHandle.standardError.write(Data("manager: list visible after \(ms) ms (\(rows.count) rows)\n".utf8))
        }
        if let node = tree.node(p), node.children != nil { await node.load() }
    }

    /// Status in two steps: this Mac (fast, no network), then the other Mac
    /// (only over an open connection; the daemon never waits for it).
    private func loadStatus() async {
        let g = generation, p = path
        defer { if g == generation { statusTask = nil } }
        var step = "local"
        do {
            if !remoteKnown {
                guard let r = try await Self.fetch(path: p, local: true, timeout: 5) else {
                    if g == generation { noDaemon() }
                    return
                }
                guard g == generation else { return }
                if phase == .loading || phase == .noDaemon || phase == .failed { apply(r, phase: .local) }
            }
            step = "remote"
            guard let r = try await Self.fetch(path: p, local: false, timeout: 20) else {
                if g == generation { noDaemon() }
                return
            }
            guard g == generation else { return }
            apply(r, phase: .done(remote: r.remote ?? r.online))
            error = nil
        } catch {
            guard g == generation, !(error is CancellationError) else { return }
            if step == "remote" {
                self.error = "Status des anderen Mac nicht geladen: " + error.localizedDescription
                if phase == .local { phase = .done(remote: false) }
            } else {
                self.error = error.localizedDescription
                if status == nil { phase = .failed }
            }
        }
    }

    private func noDaemon() {
        status = nil
        phase = .noDaemon
        error = nil
        rebuild()
    }

    private func apply(_ r: BrowseResult, phase: Phase) {
        status = r
        self.phase = phase
        online = r.online
        peerName = r.peer ?? "anderer Mac"
        rebuild()
    }

    /// One folder from the daemon; `nil` if the daemon does not run.
    static func fetch(path: String, local: Bool, timeout: Int) async throws -> BrowseResult? {
        let req: [String: Any] = ["cmd": "browse", "path": path, "local": local]
        guard let obj = try await Task.detached(operation: { try IPC.call(req, timeout: timeout) }).value else { return nil }
        let data = try JSONSerialization.data(withJSONObject: obj)
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return try d.decode(BrowseResult.self, from: data)
    }

    /// Merges the disk listing with the daemon's items (by name). Items the
    /// daemon knows but the disk does not (only on the other Mac, deleted)
    /// are appended.
    private func rebuild() {
        guard let disk else { return }
        var byName = [String: SyncItem]()
        for i in status?.items ?? [] where byName[i.name] == nil && !Disk.hidden.contains(i.name) { byName[i.name] = i }
        let dirUnit = status?.unit ?? diskUnit
        let join = { (name: String) in self.path.isEmpty ? name : self.path + "/" + name }
        var out: [ManagerRow] = []
        for e in disk {
            let item = byName.removeValue(forKey: e.name)
            let isUnit = item?.isUnit ?? (e.kind == "dir" && (path.isEmpty || e.isRepo))
            let unit = item?.unit ?? (isUnit ? join(e.name) : (dirUnit ?? "."))
            out.append(ManagerRow(name: e.name, path: join(e.name), kind: e.kind, onDisk: true, size: e.size,
                                  mtime: e.mtime, isUnit: isUnit, unit: unit, item: item))
        }
        for i in byName.values {
            out.append(ManagerRow(name: i.name, path: join(i.name), kind: i.kind, onDisk: false, size: i.size,
                                  mtime: i.mtimeMs.map { Date(timeIntervalSince1970: Double($0) / 1000) },
                                  isUnit: i.isUnit, unit: i.unit, item: i))
        }
        out.sort { a, b in
            a.isDir != b.isDir ? a.isDir : a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
        if out != rows { rows = out }
        let ids = Set(out.map(\.id))
        if !selection.isSubset(of: ids) { selection.formIntersection(ids) }
        tree.node(path)?.setStates(from: out, known: remoteKnown)
    }

    /// Expands the tree down to the current folder (loading as needed).
    func revealInTree() async {
        var node = tree
        for c in crumbs.dropFirst() {
            if node.children == nil { await node.load() }
            node.expanded = true
            guard let next = node.children?.first(where: { $0.id == c.path }) else { return }
            node = next
        }
    }

    func open(_ row: ManagerRow) {
        if row.isDir {
            go(row.path)
        } else if row.onDisk {
            NSWorkspace.shared.open(url(row.path))
        }
    }

    func openSelection() {
        let sel = selected
        if sel.count == 1, let r = sel.first { open(r) } else { sel.filter { !$0.isDir && $0.onDisk }.forEach(open) }
    }

    func up() {
        guard crumbs.count > 1 else { return }
        go(crumbs[crumbs.count - 2].path)
    }

    // MARK: actions

    func syncNow(_ rows: [ManagerRow]) {
        var units = Array(Set(rows.map(\.unit)))
        if units.isEmpty, let u = status?.unit ?? diskUnit { units = [u] }
        send(["cmd": "rescan"]) { [weak self] in
            self?.send(["cmd": "sync", "units": units, "dry_run": false])
        }
    }

    /// Synchronise these next, ahead of everything else (both directions).
    func putFirst(_ rows: [ManagerRow]) {
        for r in rows {
            send(["cmd": "prioritize", "path": r.path])
        }
    }

    /// `here`: this Mac's state of the rows replaces the other Mac's;
    /// otherwise the other Mac's state replaces this one. Asks first; the
    /// replaced versions stay in the MacSync trash of the Mac that changes.
    func overwrite(_ rows: [ManagerRow], here: Bool) {
        guard !rows.isEmpty else { return }
        let names = rows.count == 1 ? "„\(rows[0].name)“" : "\(rows.count) Einträge"
        let alert = NSAlert()
        if here {
            alert.messageText = "\(names) auf \(peerName) überschreiben?"
            alert.informativeText = "Der Stand dieses Macs gilt: Abweichende Dateien auf \(peerName) werden ersetzt, Dateien, die es nur dort gibt, entfernt. Die bisherigen Fassungen bleiben 14 Tage im MacSync-Papierkorb auf \(peerName). Offene Konflikte darin werden zugunsten dieses Macs gelöst."
            alert.addButton(withTitle: "Auf \(peerName) überschreiben")
        } else {
            alert.messageText = "\(names) von \(peerName) übernehmen?"
            alert.informativeText = "Der Stand von \(peerName) gilt: Abweichende Dateien auf diesem Mac werden ersetzt, Dateien, die es nur hier gibt, entfernt. Die bisherigen Fassungen bleiben 14 Tage im MacSync-Papierkorb auf diesem Mac. Offene Konflikte darin werden zugunsten von \(peerName) gelöst."
            alert.addButton(withTitle: "Von \(peerName) übernehmen")
        }
        alert.addButton(withTitle: "Abbrechen")
        alert.alertStyle = .warning
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        for r in rows {
            send(["cmd": "win", "path": r.path, "here": here])
        }
    }

    func dismissGitNotice(_ row: ManagerRow) {
        send(["cmd": "dismiss-git-notice", "unit": row.unit])
    }

    func setPaused(_ row: ManagerRow, _ paused: Bool) {
        send(["cmd": paused ? "pause" : "resume", "unit": row.unit])
    }

    func resolve(_ row: ManagerRow, keep choice: String) {
        guard let id = row.item?.conflictId else { return }
        send(["cmd": "resolve", "id": id, "choice": choice])
    }

    func reveal(_ rows: [ManagerRow]) {
        let urls = rows.filter(\.onDisk).map { url($0.path) }
        if !urls.isEmpty { NSWorkspace.shared.activateFileViewerSelecting(urls) }
    }

    func copyPath(_ rows: [ManagerRow]) {
        let paths = (rows.isEmpty ? [url(path)] : rows.map { url($0.path) }).map(\.path)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(paths.joined(separator: "\n"), forType: .string)
    }

    func trash(_ rows: [ManagerRow]) {
        let targets = rows.filter(\.onDisk)
        guard !targets.isEmpty else { return }
        let alert = NSAlert()
        alert.messageText = targets.count == 1
            ? "„\(targets[0].name)“ in den Papierkorb legen?"
            : "\(targets.count) Objekte in den Papierkorb legen?"
        alert.informativeText = "Sie werden auch auf \(peerName) gelöscht. Dort landen sie im MacSync-Papierkorb (14 Tage wiederherstellbar)."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "In den Papierkorb")
        alert.addButton(withTitle: "Abbrechen")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        NSWorkspace.shared.recycle(targets.map { url($0.path) }) { [weak self] _, err in
            Task { @MainActor in
                if let err { self?.error = err.localizedDescription }
                self?.send(["cmd": "rescan"])
            }
        }
    }

    private func send(_ req: [String: Any], then: (() -> Void)? = nil) {
        Task {
            do {
                let running = try await Task.detached(operation: { try IPC.call(req, timeout: 3600) != nil || IPC.isListening() }).value
                error = running ? nil : "Die Synchronisation läuft nicht – „Synchronisation starten“ im Menü."
            } catch {
                self.error = error.localizedDescription
            }
            then?()
            refresh()
        }
    }
}

// MARK: - folder tree

/// A folder in the tree on the left; sub-folders are read from disk when
/// expanded, the status dots come from the listing of the folder.
@MainActor
final class TreeNode: ObservableObject, Identifiable {
    /// Root-relative path.
    let id: String
    @Published var title: String
    @Published var isUnit: Bool
    @Published var hasFolders = true
    @Published var state = "synced"
    @Published var paused = false
    /// `nil` = not loaded yet.
    @Published private(set) var children: [TreeNode]?
    @Published var expanded = false {
        didSet { if expanded && children == nil { Task { await load() } } }
    }
    let root: URL

    init(title: String, path: String, isUnit: Bool, root: URL) {
        self.id = path
        self.title = title
        self.isUnit = isUnit
        self.root = root
    }

    func load() async {
        let dir = id.isEmpty ? root : root.appendingPathComponent(id)
        let folders = await Task.detached { Disk.folders(dir) }.value
        let old = Dictionary(uniqueKeysWithValues: (children ?? []).map { ($0.id, $0) })
        let next = folders.map { f -> TreeNode in
            let p = id.isEmpty ? f.name : id + "/" + f.name
            let n = old[p] ?? TreeNode(title: f.name, path: p, isUnit: id.isEmpty || f.isRepo, root: root)
            n.hasFolders = f.hasFolders
            return n
        }
        if next.map(\.id) != children?.map(\.id) { children = next }
        hasFolders = !next.isEmpty
    }

    /// Status dots of the sub-folders from the rows of this folder.
    func setStates(from rows: [ManagerRow], known: Bool) {
        let byName = Dictionary(rows.map { ($0.name, $0) }, uniquingKeysWith: { a, _ in a })
        for c in children ?? [] {
            guard let i = byName[c.title]?.item else { continue }
            let s = i.conflictId != nil ? "conflict" : (known ? i.state : "synced")
            if c.state != s { c.state = s }
            if c.paused != i.paused { c.paused = i.paused }
            if c.isUnit != i.isUnit { c.isUnit = i.isUnit }
        }
    }

    func node(_ id: String) -> TreeNode? {
        if self.id == id { return self }
        guard id.hasPrefix(self.id) else { return nil }
        for c in children ?? [] { if let n = c.node(id) { return n } }
        return nil
    }
}

private struct FolderTree: View {
    @ObservedObject var model: ManagerModel

    var body: some View {
        List(selection: Binding(
            get: { model.treeSelection },
            set: { id in
                guard let id, id != model.treeSelection else { return }
                model.go(id)
            })) {
            TreeRow(node: model.tree)
        }
        .listStyle(.sidebar)
    }
}

private struct TreeRow: View {
    @ObservedObject var node: TreeNode

    var body: some View {
        if !node.hasFolders || node.children?.isEmpty == true {
            label
        } else {
            DisclosureGroup(isExpanded: $node.expanded) {
                ForEach(node.children ?? []) { TreeRow(node: $0) }
            } label: {
                label
            }
        }
    }

    private var label: some View {
        HStack(spacing: 6) {
            Image(systemName: node.id.isEmpty ? "externaldrive.fill" : (node.isUnit ? "folder.fill" : "folder"))
                .foregroundStyle(Color.accentColor)
            Text(node.title).lineLimit(1)
            Spacer(minLength: 4)
            if node.paused {
                Image(systemName: "pause.circle.fill").foregroundStyle(.orange).font(.caption)
            } else if node.state != "synced" {
                Circle().fill(dot).frame(width: 7, height: 7).help(node.state)
            }
        }
        .tag(node.id)
    }

    private var dot: Color {
        switch node.state {
        case "conflict": return .red
        case "delete-on-peer", "delete-here": return .orange
        case "unknown": return .gray
        default: return .blue
        }
    }
}

/// Draggable vertical divider that sets the width of the view on its left.
private struct SplitDivider: View {
    @Binding var width: Double
    let range: ClosedRange<Double>
    @State private var start: Double?

    var body: some View {
        Rectangle()
            .fill(Color(nsColor: .separatorColor))
            .frame(width: 1)
            .padding(.horizontal, 3) // wider grab area
            .contentShape(Rectangle())
            .onHover { inside in
                if inside { NSCursor.resizeLeftRight.push() } else { NSCursor.pop() }
            }
            .gesture(
                DragGesture(minimumDistance: 1, coordinateSpace: .global)
                    .onChanged { g in
                        let base = start ?? width
                        start = base
                        width = min(range.upperBound, max(range.lowerBound, base + g.translation.width))
                    }
                    .onEnded { _ in start = nil }
            )
            .padding(.horizontal, -3)
    }
}

// MARK: - view

struct ManagerView: View {
    @ObservedObject var model: ManagerModel
    @ObservedObject var app = AppModel.shared
    @AppStorage("managerShowTree") private var showTree = true
    /// About a fifth of the default window, like the Finder sidebar.
    @AppStorage("managerTreeWidth") private var treeWidth = 210.0

    var body: some View {
        VStack(spacing: 0) {
            ProgressHeader(app: app, peer: model.peerName)
                .padding(12)
            Divider()
            HStack(spacing: 0) {
                if showTree {
                    FolderTree(model: model)
                        .frame(width: treeWidth)
                    SplitDivider(width: $treeWidth, range: 150...460)
                }
                VStack(spacing: 0) {
                    toolbar
                    hints
                    table
                }
                .frame(minWidth: 560)
            }
            Divider()
            footer.padding(10)
        }
        .frame(minWidth: 820, minHeight: 480)
    }

    private var toolbar: some View {
        HStack(spacing: 8) {
            Button { withAnimation { showTree.toggle() } } label: { Image(systemName: "sidebar.left") }
                .help(showTree ? "Verzeichnisbaum ausblenden" : "Verzeichnisbaum einblenden")
                .keyboardShortcut("s", modifiers: [.command, .control])
            Button { model.up() } label: { Image(systemName: "chevron.left") }
                .disabled(model.crumbs.count < 2)
                .help("Übergeordneter Ordner (⌘↑)")
                .keyboardShortcut(.upArrow, modifiers: .command)
            // ⌘↓ opens the selection, like in the Finder.
            Button("") { model.openSelection() }
                .keyboardShortcut(.downArrow, modifiers: .command)
                .frame(width: 0, height: 0)
                .opacity(0)
            ForEach(Array(model.crumbs.enumerated()), id: \.offset) { i, c in
                if i > 0 { Image(systemName: "chevron.right").font(.caption).foregroundStyle(.secondary) }
                Button(c.title) { model.go(c.path) }
                    .buttonStyle(.plain)
                    .fontWeight(i == model.crumbs.count - 1 ? .semibold : .regular)
                    .lineLimit(1)
                    .fixedSize()
            }
            Spacer()
            if let n = app.status?.conflicts, n > 0 {
                Button { ConflictsWindow.shared.show() } label: {
                    Label(n == 1 ? "1 Konflikt" : "\(n) Konflikte", systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                }
                .help("Alle Konflikte anzeigen und lösen (⌘K)")
            }
            Picker("", selection: $model.filter) {
                ForEach(ManagerModel.Filter.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .frame(width: 200)
            TextField("Suchen", text: $model.search)
                .textFieldStyle(.roundedBorder)
                .frame(width: 160)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    /// Slim notes above the list; they never replace it.
    @ViewBuilder
    private var hints: some View {
        if let e = model.error {
            HintBar(text: e, symbol: "exclamationmark.triangle.fill", color: .orange)
        }
        if model.phase == .noDaemon {
            HintBar(text: "Synchronisation läuft nicht – Dateien ohne Status", symbol: "pause.circle.fill", color: .secondary,
                    action: ("Synchronisation starten", {
                        app.startSync()
                        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { model.refresh() }
                    }))
        } else if app.status?.network?.hidden == true {
            // Hidden in an unknown network: the peer can't reach this Mac,
            // so say why and offer the fix right here.
            HintBar(text: "Unbekanntes Netz – MacSync ist hier unsichtbar, die Macs finden sich nicht. Vertraust du diesem Netz?",
                    symbol: "lock.shield", color: .orange,
                    action: ("Diesem Netz vertrauen", {
                        app.trustNetwork()
                        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { model.refresh() }
                    }))
        } else if model.peerUnreachable {
            HintBar(text: "\(model.peerName) nicht erreichbar – Status des anderen Mac unbekannt",
                    symbol: "wifi.exclamationmark", color: .orange)
        }
    }

    private var table: some View {
        Table(model.shown, selection: $model.selection) {
            TableColumn("Name") { row in
                HStack(spacing: 6) {
                    Image(systemName: icon(row)).foregroundStyle(row.isDir ? Color.accentColor : .secondary)
                    Text(row.name).lineLimit(1)
                }
                .opacity(row.onDisk ? 1 : 0.5)
                .help(row.onDisk ? row.path : "Nicht auf diesem Mac")
            }
            .width(min: 160, ideal: 220)
            TableColumn("Dieser Mac") { row in
                if row.onDisk {
                    let newer = model.remoteKnown && (row.item?.state == "to-peer" || row.item?.state == "delete-on-peer")
                    PresenceCell(presence: "present", newer: newer)
                } else {
                    PresenceCell(presence: row.item?.local == "deleted" || row.item?.local == "present" ? "deleted" : "missing",
                                 newer: model.remoteKnown && row.item?.state == "delete-on-peer")
                }
            }
            .width(min: 90, ideal: 110)
            TableColumn(model.peerName) { row in remoteCell(row) }
                .width(min: 90, ideal: 130)
            TableColumn("Status") { row in
                StateCell(row: row, phase: model.phase, peer: model.peerName, first: isFirst(row))
            }
                .width(min: 150, ideal: 190)
            TableColumn("Größe") { row in
                Text(row.size.map { Activity.bytes($0) } ?? "–").foregroundStyle(.secondary).monospacedDigit()
            }
            .width(min: 60, ideal: 80)
            TableColumn("Geändert") { row in
                Text(row.mtime.map { $0.formatted(date: .abbreviated, time: .shortened) } ?? "–")
                    .foregroundStyle(.secondary)
            }
            .width(min: 110, ideal: 140)
        }
        .contextMenu(forSelectionType: ManagerRow.ID.self) { ids in
            menu(for: model.rows.filter { ids.contains($0.id) })
        } primaryAction: { ids in
            let rows = model.rows.filter { ids.contains($0.id) }
            if rows.count == 1, let r = rows.first { model.open(r) } else { rows.filter { !$0.isDir }.forEach(model.open) }
        }
        .onKeyPress(.return) {
            model.openSelection()
            return .handled
        }
        .overlay {
            if model.listed, model.shown.isEmpty {
                VStack(spacing: 6) {
                    if !model.search.isEmpty {
                        Text("Keine Treffer").foregroundStyle(.secondary)
                    } else if model.filter == .pending {
                        if model.remoteKnown {
                            Label("Hier ist alles synchron", systemImage: "checkmark.circle").foregroundStyle(.green)
                        } else {
                            Text("Keine bekannten Abweichungen").foregroundStyle(.secondary)
                        }
                    } else if !model.exists {
                        Text("Ordner nicht auf diesem Mac").foregroundStyle(.secondary)
                    } else {
                        Text("Leer").foregroundStyle(.secondary)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func remoteCell(_ row: ManagerRow) -> some View {
        switch model.phase {
        case .loading, .local:
            Text("…").foregroundStyle(.tertiary).help("Status wird geladen …")
        case .noDaemon, .failed:
            Text("–").foregroundStyle(.tertiary)
        case .done(remote: false):
            PresenceCell(presence: "unknown", newer: false)
        case .done(remote: true):
            if let i = row.item {
                PresenceCell(presence: i.remote, newer: i.state == "from-peer" || i.state == "delete-here")
            } else {
                Text("–").foregroundStyle(.tertiary)
            }
        }
    }

    /// Put first by the user and not synchronised yet: the chosen path, a
    /// queued unit, or a folder holding one.
    private func isFirst(_ row: ManagerRow) -> Bool {
        guard let q = app.status?.priority, !q.isEmpty else { return false }
        return q.contains { $0.path == row.path || $0.unit == row.path || $0.unit.hasPrefix(row.path + "/") }
    }

    @ViewBuilder
    private func menu(for rows: [ManagerRow]) -> some View {
        if rows.count == 1, let r = rows.first {
            if r.isDir || r.onDisk { Button("Öffnen") { model.open(r) } }
        }
        if !rows.isEmpty {
            Button("Im Finder zeigen") { model.reveal(rows) }.disabled(!rows.contains(where: \.onDisk))
            Button("Pfad kopieren") { model.copyPath(rows) }
            Divider()
        }
        if rows.count == 1, let r = rows.first {
            if let id = r.item?.conflictId {
                Button("Konflikt ansehen …") { ConflictsWindow.shared.show(select: id) }
                Menu("Konflikt lösen") {
                    Button("Meine Version behalten") { model.resolve(r, keep: "local") }
                    Button("Version von \(model.peerName) übernehmen") { model.resolve(r, keep: "remote") }
                    Button("Beide behalten") { model.resolve(r, keep: "both") }
                }
            }
            if r.isUnit {
                Button(r.paused ? "Fortsetzen" : "Pausieren") { model.setPaused(r, !r.paused) }
            }
            if r.item?.gitNotice != nil {
                Button("Git-Hinweis ausblenden") { model.dismissGitNotice(r) }
            }
        }
        if !rows.isEmpty {
            Divider()
            Button("Diesen Stand auf \(model.peerName) übertragen …") { model.overwrite(rows, here: true) }
                .help("Dieser Mac gilt: \(model.peerName) übernimmt genau diesen Stand (bisherige Fassungen dort in den MacSync-Papierkorb)")
            Button("Stand von \(model.peerName) übernehmen …") { model.overwrite(rows, here: false) }
                .help("\(model.peerName) gilt: dieser Mac übernimmt genau dessen Stand (bisherige Fassungen hier in den MacSync-Papierkorb)")
            Divider()
        }
        if !rows.isEmpty {
            Button("Jetzt synchronisieren") { model.syncNow(rows) }
            Button("Als Nächstes synchronisieren") { model.putFirst(rows) }
                .help("Kommt direkt nach dem Ordner, der gerade übertragen wird – auf beiden Macs")
            Divider()
            Button("In den Papierkorb …", role: .destructive) { model.trash(rows) }
                .disabled(!rows.contains(where: \.onDisk))
        }
    }

    private var footerText: String {
        guard model.listed else { return "Ordner wird gelesen …" }
        let n = model.rows.count
        var s = n == 1 ? "1 Eintrag" : "\(n) Einträge"
        switch model.phase {
        case .loading, .local: s += " · Status wird geladen …"
        case .noDaemon: s += " · Synchronisation läuft nicht"
        case .failed: s += " · Status nicht verfügbar"
        case .done(remote: false): s += " · Status des anderen Mac unbekannt"
        case .done(remote: true):
            let pending = model.rows.filter { r in r.item.map { !$0.isSynced } ?? !r.onDisk }.count
            s += pending > 0 ? " · \(pending) nicht synchron" : " · alles synchron"
        }
        return s
    }

    private var footer: some View {
        let sel = model.selected
        return HStack(spacing: 8) {
            Text(footerText).foregroundStyle(.secondary)
            if model.statusLoading {
                ProgressView().controlSize(.small)
            }
            Spacer()
            Text(app.versionLine).font(.caption).foregroundStyle(.tertiary)
            if sel.count == 1, let r = sel.first, r.isUnit {
                Button(r.paused ? "Fortsetzen" : "Pausieren") { model.setPaused(r, !r.paused) }
            }
            Button("Im Finder zeigen") { model.reveal(sel.isEmpty ? [] : sel) }.disabled(!sel.contains(where: \.onDisk))
            Button("Jetzt synchronisieren") { model.syncNow(sel.isEmpty ? model.rows : sel) }
                .keyboardShortcut("s", modifiers: [.command])
            Button { model.refresh() } label: { Image(systemName: "arrow.clockwise") }
                .help("Neu laden")
        }
    }

    private func icon(_ row: ManagerRow) -> String {
        if row.isUnit { return "folder.fill" }
        switch row.kind {
        case "dir": return "folder"
        case "symlink": return "arrowshape.turn.up.right"
        default: return "doc"
        }
    }
}

private struct HintBar: View {
    let text: String
    let symbol: String
    let color: Color
    var action: (title: String, run: () -> Void)?

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: symbol).foregroundStyle(color)
            Text(text).foregroundStyle(.secondary).lineLimit(2)
            Spacer(minLength: 0)
            if let action {
                Button(action.title, action: action.run).controlSize(.small)
            }
        }
        .font(.callout)
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
        .background(color.opacity(0.08))
    }
}

/// Transfer progress on top of the manager.
private struct ProgressHeader: View {
    @ObservedObject var app: AppModel
    let peer: String

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(nsImage: MenuIcon.image(for: app.state, frame: app.spinFrame))
            VStack(alignment: .leading, spacing: 4) {
                if let a = app.activity {
                    Text(a.summary(peer: peer)).font(.headline)
                    if a.direction == .receiving {
                        ProgressView(value: a.fraction ?? 0)
                        Text(detail(a)).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                    }
                } else {
                    Text(app.state.title).font(.headline)
                    if let d = app.status?.lastSync {
                        Text("Zuletzt synchronisiert " + RelativeDateTimeFormatter().localizedString(for: d, relativeTo: Date()))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            Spacer()
        }
    }

    private func detail(_ a: Activity) -> String {
        var s = "\(a.filesDone) von \(a.filesTotal) Dateien · \(Activity.bytes(a.bytesDone)) von \(Activity.bytes(a.bytesTotal))"
        if a.rate > 0, !a.stalled { s += " · \(Activity.bytes(Int64(a.rate)))/s" }
        if !a.current.isEmpty { s += " · \(a.unit == "." ? "" : a.unit + "/")\(a.current)" }
        return s
    }
}

private struct PresenceCell: View {
    let presence: String
    /// This side holds the newer state (version or deletion).
    let newer: Bool

    var body: some View {
        switch presence {
        case "present":
            Label(newer ? "neuer" : "vorhanden", systemImage: newer ? "circle.fill" : "checkmark")
                .foregroundStyle(newer ? Color.blue : Color.green)
        case "deleted":
            Label("gelöscht", systemImage: "trash").foregroundStyle(newer ? Color.blue : Color.secondary)
        case "missing":
            Label("fehlt", systemImage: "minus").foregroundStyle(.secondary)
        default:
            Label("unbekannt", systemImage: "questionmark").foregroundStyle(.secondary)
        }
    }
}

private struct StateCell: View {
    let row: ManagerRow
    let phase: ManagerModel.Phase
    let peer: String
    var first = false

    var body: some View {
        if let d = describe() {
            HStack(spacing: 6) {
                if first {
                    Image(systemName: "arrow.up.to.line.circle.fill").foregroundStyle(.blue)
                        .help("Vorgezogen: wird als Nächstes synchronisiert")
                }
                Label(d.text, systemImage: d.symbol).foregroundStyle(d.color).lineLimit(1)
                    .help(longText ?? d.text)
                if let n = row.item?.gitNotice {
                    Image(systemName: "exclamationmark.shield.fill").foregroundStyle(.orange)
                        .help(gitNoticeText(n))
                }
            }
        } else if phase == .loading || phase == .local {
            HStack(spacing: 5) {
                ProgressView().controlSize(.mini)
                Text("Status wird geladen …").foregroundStyle(.secondary).lineLimit(1)
            }
        } else {
            Text("–").foregroundStyle(.tertiary).help(phase == .noDaemon ? "Synchronisation läuft nicht" : "Kein Status")
        }
    }

    private func gitNoticeText(_ n: SyncItem.GitNotice) -> String {
        let when = Date(timeIntervalSince1970: Double(n.atMs) / 1000).formatted(date: .abbreviated, time: .shortened)
        let what = n.path == ".git/config" ? "die Git-Konfiguration (.git/config)" : "den Git-Hook \(n.path)"
        return "\(peer) hat \(what) dieses Repositorys geändert (\(when)). MacSync führt daraus nichts aus; Git selbst schon – prüfe die Änderung, wenn du sie nicht erwartet hast."
    }

    /// Spelled out with the name of the other Mac (tooltip).
    private var longText: String? {
        guard let i = row.item else { return "Neu oder von der Synchronisation ausgeschlossen" }
        switch i.state {
        case "to-peer": return "Diese Version wird an \(peer) gesendet"
        case "from-peer": return "Die neuere Version wird von \(peer) geholt"
        case "delete-on-peer": return "Hier gelöscht – wird auch auf \(peer) gelöscht"
        case "delete-here": return "Auf \(peer) gelöscht – wird auch hier gelöscht"
        default: return nil
        }
    }

    /// `nil`: nothing known (yet).
    private func describe() -> (text: String, symbol: String, color: Color)? {
        let known = phase == .done(remote: true)
        guard let i = row.item else {
            // Not in the daemon's database: new since the last scan, or excluded.
            switch phase {
            case .local, .done: return ("Noch nicht erfasst", "circle.dashed", .secondary)
            default: return nil
            }
        }
        if i.conflictId != nil || i.state == "conflict" { return ("Konflikt", "exclamationmark.triangle.fill", .red) }
        if i.paused && !(known && i.isSynced) { return ("Pausiert", "pause.circle.fill", .orange) }
        if !known {
            guard case .done = phase else { return nil }
            if !row.onDisk { return ("Hier gelöscht", "trash.circle", .secondary) }
            return ("Status unbekannt", "questionmark.circle", .secondary)
        }
        switch i.state {
        case "synced": return ("Synchron", "checkmark.circle.fill", .green)
        case "to-peer": return ("Wird gesendet", "arrow.right.circle.fill", .blue)
        case "from-peer" where !row.onDisk: return ("Nur auf \(peer)", "arrow.left.circle.fill", .blue)
        case "from-peer": return ("Wird geholt", "arrow.left.circle.fill", .blue)
        case "delete-on-peer": return ("Wird dort gelöscht", "trash.circle.fill", .orange)
        case "delete-here": return ("Wird hier gelöscht", "trash.circle.fill", .orange)
        case "differs": return ("Änderungen", "arrow.triangle.2.circlepath.circle.fill", .blue)
        default: return ("Unbekannt", "questionmark.circle", .secondary)
        }
    }
}

/// The sync manager window.
@MainActor
final class ManagerWindow: NSObject, NSWindowDelegate {
    static let shared = ManagerWindow()
    private var window: NSWindow?
    private(set) var model: ManagerModel?

    func show(path: String? = nil) {
        if let w = window {
            if let path, let m = model { m.go(path) }
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let m = ManagerModel()
        let w = NSWindow(contentViewController: NSHostingController(rootView: ManagerView(model: m)))
        w.title = "MacSync – Synchronisations-Manager"
        w.styleMask = [.titled, .closable, .resizable, .miniaturizable]
        w.setContentSize(NSSize(width: 1100, height: 600))
        w.isReleasedWhenClosed = false
        w.delegate = self
        if ProcessInfo.processInfo.environment["MACSYNC_SNAPSHOT"] == nil {
            w.setFrameAutosaveName("MacSyncManager")
        }
        w.center()
        model = m
        window = w
        m.start(path: path)
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        // Documentation screenshots: MACSYNC_SNAPSHOT=/path.png renders the
        // window content once the listing has loaded (after
        // MACSYNC_SNAPSHOT_DELAY seconds, default 4).
        let env = ProcessInfo.processInfo.environment
        if let path = env["MACSYNC_SNAPSHOT"] {
            let delay = env["MACSYNC_SNAPSHOT_DELAY"].flatMap(Double.init) ?? 4
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak w] in
                guard let w, let frame = w.contentView?.superview else { return }
                ManagerWindow.snapshot(frame, to: URL(fileURLWithPath: path))
            }
        }
    }

    /// Renders a view hierarchy (incl. title bar) at 2x via its layers.
    static func snapshot(_ view: NSView, to url: URL) {
        let scale: CGFloat = 2
        let size = view.bounds.size
        guard let layer = view.layer,
              let ctx = CGContext(data: nil, width: Int(size.width * scale), height: Int(size.height * scale),
                                  bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return }
        ctx.scaleBy(x: scale, y: scale)
        if !view.isFlipped {
            // layer.render draws in the layer's coordinate system
        }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: false)
        layer.render(in: ctx)
        NSGraphicsContext.restoreGraphicsState()
        guard let img = ctx.makeImage() else { return }
        let rep = NSBitmapImageRep(cgImage: img)
        try? rep.representation(using: .png, properties: [:])?.write(to: url)
    }

    func windowWillClose(_ notification: Notification) {
        model?.stop()
        model = nil
        window = nil
    }
}
