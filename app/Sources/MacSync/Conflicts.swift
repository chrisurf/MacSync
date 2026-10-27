import AppKit
import SwiftUI

/// One version of a conflicting path (`conflicts` request of the daemon).
struct ConflictSide: Decodable, Hashable {
    /// `present`, `deleted` or `missing`.
    var state: String
    var kind: String
    var size: Int64?
    var mtimeMs: Int64?
    /// Where this version can be opened here (file or conflict-store copy).
    var file: String?

    var modified: Date? { mtimeMs.map { Date(timeIntervalSince1970: Double($0) / 1000) } }
}

struct ConflictItem: Decodable, Identifiable, Hashable {
    var id: Int64
    var unit: String
    var path: String
    var localPath: String
    var conflictClass: String
    var createdMs: Int64
    var resolvedMs: Int64?
    var resolution: String?
    var paths: Int
    var canKeepBoth: Bool
    var local: ConflictSide
    var remote: ConflictSide

    enum CodingKeys: String, CodingKey {
        case id, unit, path, localPath, paths, canKeepBoth, local, remote, createdMs, resolvedMs, resolution
        case conflictClass = "class"
    }

    var isOpen: Bool { resolvedMs == nil }
    var name: String {
        if conflictClass == "git-state" { return "Git-Zustand" }
        return path.isEmpty ? (unit as NSString).lastPathComponent : (path as NSString).lastPathComponent
    }
    /// Folder shown under the name: unit plus parent directory.
    var folder: String {
        let parent = (path as NSString).deletingLastPathComponent
        let u = unit == "." ? "" : unit
        return [u, parent].filter { !$0.isEmpty }.joined(separator: "/")
    }
    var created: Date { Date(timeIntervalSince1970: Double(createdMs) / 1000) }
    var resolved: Date? { resolvedMs.map { Date(timeIntervalSince1970: Double($0) / 1000) } }
    var bothFiles: Bool { local.file != nil && remote.file != nil && local.kind == "file" && remote.kind == "file" }
}

struct ConflictList: Decodable {
    var peer: String?
    var conflicts: [ConflictItem]
}

/// Plain-language names and explanations of the conflict classes.
enum ConflictText {
    static func title(_ c: String) -> String {
        switch c {
        case "concurrent-modify": return "Auf beiden Macs geändert"
        case "kind-change": return "Datei / Ordner"
        case "git-state": return "Git-Zustand"
        case "case-collision": return "Groß-/Kleinschreibung"
        default: return c
        }
    }

    static func symbol(_ c: String) -> String {
        switch c {
        case "concurrent-modify": return "arrow.triangle.branch"
        case "kind-change": return "doc.on.doc"
        case "git-state": return "point.3.connected.trianglepath.dotted"
        case "case-collision": return "textformat"
        default: return "exclamationmark.triangle"
        }
    }

    static func explain(_ i: ConflictItem, peer: String) -> String {
        switch i.conflictClass {
        case "concurrent-modify":
            if i.local.state == "deleted" || i.local.state == "missing" {
                return "Hier gelöscht, auf \(peer) aber gleichzeitig geändert. MacSync hat nichts gelöscht und nichts überschrieben."
            }
            if i.remote.state == "deleted" {
                return "Auf \(peer) gelöscht, hier aber gleichzeitig geändert. MacSync hat die Datei hier behalten."
            }
            return "Die Datei wurde auf beiden Macs geändert, bevor sie abgeglichen werden konnte. Beide Fassungen sind erhalten: deine liegt an ihrem Platz, die von \(peer) in der Konfliktablage."
        case "kind-change":
            return "Auf einem Mac ist es eine Datei, auf dem anderen ein Ordner oder Link. Nur eines davon kann bestehen bleiben."
        case "git-state":
            return "HEAD, Branches oder der Index dieses Repositorys wurden auf beiden Macs geändert (\(i.paths) Einträge). Der Git-Zustand wird nur als Ganzes übernommen, damit das Repository nicht gemischt wird. Am besten auf einem Mac committen/pushen und dann dessen Stand übernehmen."
        case "case-collision":
            let here = (i.localPath as NSString).lastPathComponent
            return "Auf \(peer) heißt es „\(i.name)“, hier „\(here)“. Für macOS ist das derselbe Name. Meist wurde nur die Schreibweise geändert: die Version mit der gewünschten Schreibweise übernehmen."
        default:
            return "Beide Macs haben unterschiedliche Stände."
        }
    }

    static func resolution(_ r: String?, peer: String) -> String {
        switch r {
        case "local": return "Version dieses Macs behalten"
        case "remote": return "Version von \(peer) übernommen"
        case "both": return "Beide behalten"
        case "superseded": return "Erledigt sich selbst (überholt)"
        case let r?: return r
        default: return "offen"
        }
    }
}

@MainActor
final class ConflictsModel: ObservableObject {
    enum Filter: String, CaseIterable { case open = "Offen", resolved = "Erledigt" }

    @Published private(set) var items: [ConflictItem] = []
    @Published private(set) var peerName = "anderer Mac"
    @Published private(set) var loaded = false
    @Published var filter: Filter = .open { didSet { selection = []; refresh() } }
    /// Unit in the sidebar; `nil` = all.
    @Published var unit: String? { didSet { selection = [] } }
    @Published var kind: String? { didSet { selection = [] } }
    @Published var search = ""
    @Published var selection = Set<ConflictItem.ID>()
    @Published var error: String?
    /// Message while resolving, e.g. "Löse 192 Konflikte …".
    @Published private(set) var busy: String?
    @Published var notice: String?

    private var timer: Timer?

    /// Conflicts of the current filter (open or resolved).
    var listed: [ConflictItem] { items.filter { filter == .open ? $0.isOpen : !$0.isOpen } }

    var shown: [ConflictItem] {
        listed.filter { i in
            (unit == nil || i.unit == unit)
                && (kind == nil || i.conflictClass == kind)
                && (search.isEmpty || i.path.localizedCaseInsensitiveContains(search) || i.unit.localizedCaseInsensitiveContains(search))
        }
    }

    var units: [(name: String, count: Int)] {
        Dictionary(grouping: listed, by: \.unit).map { ($0.key, $0.value.count) }.sorted { $0.name < $1.name }
    }

    var kinds: [String] { Array(Set(listed.map(\.conflictClass))).sorted() }

    var selected: [ConflictItem] { shown.filter { selection.contains($0.id) } }

    func start() {
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { _ in
            Task { @MainActor in ConflictsWindow.shared.model?.refresh() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    func refresh() {
        guard busy == nil else { return }
        let all = filter == .resolved
        Task {
            do {
                let obj = try await Task.detached(operation: { try IPC.call(["cmd": "conflicts", "all": all], timeout: 30) }).value
                guard let obj else {
                    error = "Die Synchronisation läuft nicht – „Synchronisation starten“ im Menü."
                    return
                }
                let d = JSONDecoder()
                d.keyDecodingStrategy = .convertFromSnakeCase
                let r = try d.decode(ConflictList.self, from: JSONSerialization.data(withJSONObject: obj))
                guard all == (filter == .resolved) else { return }
                items = r.conflicts.sorted { ($0.resolvedMs ?? $0.createdMs, $0.id) > ($1.resolvedMs ?? $1.createdMs, $1.id) }
                peerName = r.peer ?? "anderer Mac"
                error = nil
                loaded = true
                selection = selection.intersection(Set(items.map(\.id)))
                if let u = unit, !listed.contains(where: { $0.unit == u }) { unit = nil }
                if let k = kind, !listed.contains(where: { $0.conflictClass == k }) { kind = nil }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    func select(id: Int64) {
        filter = .open
        unit = nil
        kind = nil
        search = ""
        selection = [id]
    }

    func label(_ choice: String) -> String {
        switch choice {
        case "local": return "Version dieses Macs behalten"
        case "remote": return "Version von \(peerName) übernehmen"
        default: return "Beide behalten"
        }
    }

    /// Resolves the given conflicts; asks first when files are replaced or
    /// several conflicts are resolved at once.
    func resolve(_ targets: [ConflictItem], keep choice: String) {
        let targets = targets.filter(\.isOpen)
        guard !targets.isEmpty else { return }
        if targets.count > 1 || choice == "remote" {
            let alert = NSAlert()
            alert.messageText = targets.count == 1
                ? "„\(targets[0].name)“: \(label(choice))?"
                : "\(targets.count) Konflikte: \(label(choice))?"
            switch choice {
            case "remote":
                alert.informativeText = "Die Fassung dieses Macs wird ersetzt. Sie bleibt 14 Tage im MacSync-Papierkorb (.macsync/trash) wiederherstellbar."
            case "local":
                alert.informativeText = "\(peerName) übernimmt bei der nächsten Synchronisation die Fassung dieses Macs. Die Fassung von \(peerName) bleibt dort im MacSync-Papierkorb."
            default:
                alert.informativeText = "Die Fassung von \(peerName) wird als zusätzliche Datei „… .conflict-…“ neben deine gelegt."
            }
            alert.alertStyle = choice == "remote" ? .warning : .informational
            alert.addButton(withTitle: label(choice))
            alert.addButton(withTitle: "Abbrechen")
            guard alert.runModal() == .alertFirstButtonReturn else { return }
        }
        let ids = targets.map(\.id)
        busy = targets.count == 1 ? "Löse Konflikt …" : "Löse \(targets.count) Konflikte …"
        notice = nil
        Task {
            defer {
                busy = nil
                refresh()
                Task { await AppModel.shared.refresh() }
            }
            do {
                let req: [String: Any] = ["cmd": "resolve-many", "ids": ids, "choice": choice]
                let obj = try await Task.detached(operation: { try IPC.call(req, timeout: 3600) }).value
                guard let rows = obj as? [[String: Any]] else {
                    error = "Die Synchronisation läuft nicht – „Synchronisation starten“ im Menü."
                    return
                }
                let failed = rows.filter { $0["ok"] as? Bool != true }
                let done = rows.count - failed.count
                selection = []
                if failed.isEmpty {
                    error = nil
                    notice = done == 1 ? "Konflikt gelöst." : "\(done) Konflikte gelöst."
                } else {
                    let first = failed.first?["message"] as? String ?? ""
                    error = "\(failed.count) von \(rows.count) nicht gelöst: \(first)"
                }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    func open(_ path: String?) {
        guard let path else { return }
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    func reveal(_ path: String?) {
        guard let path else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    static let fileMerge = "/Applications/Xcode.app/Contents/Applications/FileMerge.app"

    /// FileMerge if Xcode is installed, otherwise both files in their app.
    func compare(_ i: ConflictItem) {
        guard let a = i.local.file, let b = i.remote.file else { return }
        if FileManager.default.fileExists(atPath: Self.fileMerge) {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/usr/bin/opendiff")
            p.arguments = [a, b]
            p.standardOutput = FileHandle.nullDevice
            p.standardError = FileHandle.nullDevice
            if (try? p.run()) != nil { return }
        }
        NSWorkspace.shared.open(URL(fileURLWithPath: a))
        NSWorkspace.shared.open(URL(fileURLWithPath: b))
    }
}

// MARK: - view

struct ConflictsView: View {
    @ObservedObject var model: ConflictsModel

    var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider()
            HStack(spacing: 0) {
                sidebar.frame(width: 210)
                Divider()
                table.frame(minWidth: 380)
                Divider()
                ConflictDetail(model: model)
                    .frame(width: 330)
            }
            Divider()
            footer.padding(10)
        }
        .frame(minWidth: 940, minHeight: 480)
    }

    private var toolbar: some View {
        HStack(spacing: 8) {
            Picker("", selection: $model.filter) {
                ForEach(ConflictsModel.Filter.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .frame(width: 180)
            Picker("Art", selection: $model.kind) {
                Text("Alle Arten").tag(String?.none)
                ForEach(model.kinds, id: \.self) { Text(ConflictText.title($0)).tag(String?.some($0)) }
            }
            .frame(width: 230)
            Spacer()
            TextField("Suchen", text: $model.search)
                .textFieldStyle(.roundedBorder)
                .frame(width: 180)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private var sidebar: some View {
        List(selection: Binding(get: { model.unit ?? "" }, set: { model.unit = ($0?.isEmpty ?? true) ? nil : $0 })) {
            Label {
                HStack { Text("Alle Ordner"); Spacer(); count(model.listed.count) }
            } icon: {
                Image(systemName: "tray.full")
            }
            .tag("")
            Section("Ordner") {
                ForEach(model.units, id: \.name) { u in
                    Label {
                        HStack { Text(u.name == "." ? "Hauptverzeichnis" : u.name).lineLimit(1).truncationMode(.middle); Spacer(); count(u.count) }
                    } icon: {
                        Image(systemName: "folder.fill").foregroundStyle(Color.accentColor)
                    }
                    .tag(u.name)
                }
            }
        }
        .listStyle(.sidebar)
    }

    private func count(_ n: Int) -> some View {
        Text("\(n)").monospacedDigit().foregroundStyle(.secondary)
    }

    private var table: some View {
        Table(model.shown, selection: $model.selection) {
            TableColumn("Datei") { i in
                VStack(alignment: .leading, spacing: 1) {
                    Text(i.name).lineLimit(1)
                    if !i.folder.isEmpty {
                        Text(i.folder).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                    }
                }
            }
            .width(min: 160, ideal: 230)
            TableColumn("Art") { i in
                Label(ConflictText.title(i.conflictClass), systemImage: ConflictText.symbol(i.conflictClass))
                    .foregroundStyle(i.isOpen ? Color.red : Color.secondary)
                    .lineLimit(1)
            }
            .width(min: 120, ideal: 160)
            TableColumn(model.filter == .open ? "Seit" : "Gelöst") { i in
                Text((i.resolved ?? i.created).formatted(date: .numeric, time: .shortened)).foregroundStyle(.secondary)
            }
            .width(min: 120, ideal: 130)
        }
        .contextMenu(forSelectionType: ConflictItem.ID.self) { ids in
            let items = model.shown.filter { ids.contains($0.id) && $0.isOpen }
            if !items.isEmpty {
                Button(model.label("local")) { model.resolve(items, keep: "local") }
                Button(model.label("remote")) { model.resolve(items, keep: "remote") }
                if items.allSatisfy(\.canKeepBoth) {
                    Button(model.label("both")) { model.resolve(items, keep: "both") }
                }
            }
            if items.count == 1, let i = items.first {
                Divider()
                Button("Im Finder zeigen") { model.reveal(i.local.file) }.disabled(i.local.file == nil)
                if i.bothFiles { Button("Vergleichen") { model.compare(i) } }
            }
        }
        .overlay {
            if model.shown.isEmpty {
                VStack(spacing: 6) {
                    if let e = model.error, !model.loaded {
                        Label(e, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                    } else if !model.loaded {
                        ProgressView()
                    } else if model.filter == .open && model.listed.isEmpty {
                        Label("Keine offenen Konflikte", systemImage: "checkmark.circle").foregroundStyle(.green)
                    } else {
                        Text("Keine Treffer").foregroundStyle(.secondary)
                    }
                }
            }
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            if let b = model.busy {
                ProgressView().controlSize(.small)
                Text(b)
            } else if let e = model.error, model.loaded {
                Label(e, systemImage: "exclamationmark.triangle").foregroundStyle(.orange).lineLimit(2)
            } else if let n = model.notice {
                Label(n, systemImage: "checkmark.circle").foregroundStyle(.green)
            } else {
                let n = model.shown.count
                Text(n == 1 ? "1 Konflikt" : "\(n) Konflikte").foregroundStyle(.secondary)
            }
            Spacer()
            Button("Alle auswählen") { model.selection = Set(model.shown.map(\.id)) }
                .keyboardShortcut("a", modifiers: [.command])
                .disabled(model.shown.isEmpty)
            Button("Im Manager zeigen") {
                if let i = model.selected.first { ManagerWindow.shared.show(path: ([i.unit == "." ? "" : i.unit, (i.localPath as NSString).deletingLastPathComponent]).filter { !$0.isEmpty }.joined(separator: "/")) }
            }
            .disabled(model.selected.count != 1)
            Button { model.refresh() } label: { Image(systemName: "arrow.clockwise") }
                .help("Neu laden")
        }
    }
}

/// Right-hand pane: one conflict in detail, or actions for a selection.
private struct ConflictDetail: View {
    @ObservedObject var model: ConflictsModel

    var body: some View {
        let sel = model.selected
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if sel.count == 1, let i = sel.first {
                    single(i)
                } else if sel.count > 1 {
                    several(sel)
                } else {
                    intro
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.5))
        .disabled(model.busy != nil)
    }

    private var intro: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Konflikte").font(.title3.bold())
            Text("Ein Konflikt entsteht, wenn dieselbe Datei auf beiden Macs geändert wurde, bevor MacSync abgleichen konnte. MacSync überschreibt dann nichts, sondern hält die Fassung von \(model.peerName) zurück, bis du entscheidest.")
            Text("Links einen Konflikt auswählen, um beide Fassungen zu vergleichen. Für viele gleichartige Konflikte: Ordner wählen, **Alle auswählen** (⌘A), dann gesammelt lösen.")
                .foregroundStyle(.secondary)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder
    private func single(_ i: ConflictItem) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(i.name).font(.title3.bold()).lineLimit(2)
            Text([i.unit == "." ? "" : i.unit, i.path].filter { !$0.isEmpty }.joined(separator: "/"))
                .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
        }
        Label(ConflictText.title(i.conflictClass), systemImage: ConflictText.symbol(i.conflictClass))
            .foregroundStyle(i.isOpen ? .red : .secondary)
            .font(.headline)
        Text(ConflictText.explain(i, peer: model.peerName))
            .fixedSize(horizontal: false, vertical: true)

        let newer = newerSide(i)
        SideBox(title: "Dieser Mac", side: i.local, newer: newer == .local,
                name: i.conflictClass == "case-collision" ? (i.localPath as NSString).lastPathComponent : nil,
                note: nil,
                open: { model.open(i.local.file) }, reveal: { model.reveal(i.local.file) },
                keep: i.isOpen ? { model.resolve([i], keep: "local") } : nil,
                keepTitle: "Diese behalten")
        SideBox(title: model.peerName, side: i.remote, newer: newer == .remote,
                name: i.conflictClass == "case-collision" ? i.name : nil,
                note: i.remote.file != nil ? "Kopie in der Konfliktablage" : nil,
                open: { model.open(i.remote.file) }, reveal: { model.reveal(i.remote.file) },
                keep: i.isOpen ? { model.resolve([i], keep: "remote") } : nil,
                keepTitle: "Diese übernehmen")

        if i.isOpen {
            HStack {
                if i.bothFiles {
                    Button { model.compare(i) } label: { Label("Vergleichen", systemImage: "rectangle.split.2x1") }
                        .help(FileManager.default.fileExists(atPath: ConflictsModel.fileMerge) ? "Mit FileMerge vergleichen" : "Beide Fassungen öffnen")
                }
                if i.canKeepBoth {
                    Button { model.resolve([i], keep: "both") } label: { Label("Beide behalten", systemImage: "plus.square.on.square") }
                        .help("Die Fassung von \(model.peerName) kommt als „… .conflict-…“ neben deine Datei")
                }
            }
        } else {
            Label("\(ConflictText.resolution(i.resolution, peer: model.peerName)) · \(i.resolved?.formatted(date: .abbreviated, time: .shortened) ?? "")",
                  systemImage: "checkmark.circle")
                .foregroundStyle(.green)
        }
        Text("Seit \(i.created.formatted(date: .abbreviated, time: .shortened)) · #\(i.id)")
            .font(.caption).foregroundStyle(.secondary)
    }

    @ViewBuilder
    private func several(_ sel: [ConflictItem]) -> some View {
        let open = sel.filter(\.isOpen)
        Text("\(sel.count) Konflikte ausgewählt").font(.title3.bold())
        let byKind = Dictionary(grouping: sel, by: \.conflictClass).sorted { $0.key < $1.key }
        VStack(alignment: .leading, spacing: 4) {
            ForEach(byKind, id: \.key) { k, v in
                Label("\(v.count) × \(ConflictText.title(k))", systemImage: ConflictText.symbol(k))
            }
        }
        if !open.isEmpty {
            if open.contains(where: { $0.conflictClass == "case-collision" }) {
                Text("Bei Groß-/Kleinschreibung: die Seite übernehmen, deren Schreibweise gelten soll – meist die neue.")
                    .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            VStack(alignment: .leading, spacing: 8) {
                Button { model.resolve(open, keep: "local") } label: {
                    Label("Alle: Version dieses Macs behalten", systemImage: "laptopcomputer").frame(maxWidth: .infinity, alignment: .leading)
                }
                Button { model.resolve(open, keep: "remote") } label: {
                    Label("Alle: Version von \(model.peerName) übernehmen", systemImage: "arrow.down.circle").frame(maxWidth: .infinity, alignment: .leading)
                }
                if open.allSatisfy(\.canKeepBoth) {
                    Button { model.resolve(open, keep: "both") } label: {
                        Label("Alle: beide behalten", systemImage: "plus.square.on.square").frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            .controlSize(.large)
        }
    }

    private enum Side { case local, remote, none }

    private func newerSide(_ i: ConflictItem) -> Side {
        guard let a = i.local.mtimeMs, let b = i.remote.mtimeMs, abs(a - b) > 1000 else { return .none }
        return a > b ? .local : .remote
    }
}

private struct SideBox: View {
    let title: String
    let side: ConflictSide
    let newer: Bool
    /// Spelling of the name on this side (case collisions).
    let name: String?
    let note: String?
    let open: () -> Void
    let reveal: () -> Void
    let keep: (() -> Void)?
    let keepTitle: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(title).font(.headline).lineLimit(1)
                if newer {
                    Text("neuer").font(.caption.bold()).padding(.horizontal, 6).padding(.vertical, 1)
                        .background(Capsule().fill(Color.blue.opacity(0.18))).foregroundStyle(.blue)
                }
                Spacer()
            }
            if let name { row("Name", name) }
            row("Zustand", state)
            if let s = side.size { row("Größe", Activity.bytes(s)) }
            if let d = side.modified { row("Geändert", d.formatted(date: .abbreviated, time: .shortened)) }
            if let note { Text(note).font(.caption).foregroundStyle(.secondary) }
            HStack {
                if side.file != nil {
                    Button("Öffnen", action: open)
                    Button { reveal() } label: { Image(systemName: "magnifyingglass") }.help("Im Finder zeigen")
                }
                Spacer()
                if let keep { Button(keepTitle, action: keep).buttonStyle(.borderedProminent) }
            }
            .controlSize(.small)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .textBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(newer ? Color.blue.opacity(0.5) : Color(nsColor: .separatorColor)))
    }

    private var state: String {
        switch side.state {
        case "present":
            switch side.kind {
            case "dir": return "Ordner"
            case "symlink": return "Link"
            default: return "vorhanden"
            }
        case "deleted": return "gelöscht"
        default: return "fehlt"
        }
    }

    private func row(_ k: String, _ v: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(k).foregroundStyle(.secondary).frame(width: 70, alignment: .leading)
            Text(v).lineLimit(1).truncationMode(.middle)
        }
        .font(.callout)
    }
}

/// The conflicts window.
@MainActor
final class ConflictsWindow: NSObject, NSWindowDelegate {
    static let shared = ConflictsWindow()
    private var window: NSWindow?
    private(set) var model: ConflictsModel?

    func show(select id: Int64? = nil) {
        if let w = window, let m = model {
            if let id { m.select(id: id) }
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let m = ConflictsModel()
        let w = NSWindow(contentViewController: NSHostingController(rootView: ConflictsView(model: m)))
        w.title = "MacSync – Konflikte"
        w.styleMask = [.titled, .closable, .resizable, .miniaturizable]
        w.setContentSize(NSSize(width: 1080, height: 600))
        w.isReleasedWhenClosed = false
        w.delegate = self
        let snapshot = ProcessInfo.processInfo.environment["MACSYNC_SNAPSHOT"]
        if snapshot == nil { w.setFrameAutosaveName("MacSyncConflicts") }
        w.center()
        model = m
        window = w
        m.start()
        if let id { m.select(id: id) }
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        // Documentation screenshots (see ManagerWindow).
        if let snapshot {
            DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [weak w] in
                guard let w, let frame = w.contentView?.superview else { return }
                ManagerWindow.snapshot(frame, to: URL(fileURLWithPath: snapshot))
            }
        }
    }

    func windowWillClose(_ notification: Notification) {
        model?.stop()
        model = nil
        window = nil
    }
}
