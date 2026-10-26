import AppKit
import SwiftUI

/// One row of the sync manager (`browse` request of the daemon).
struct SyncItem: Decodable, Identifiable, Hashable {
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

    var id: String { isUnit ? "unit:\(unit)" : "\(unit)\u{0}\(path)" }
    var isDir: Bool { kind == "dir" }
    var isSynced: Bool { state == "synced" }
    var existsHere: Bool { local == "present" }

    /// Location on this Mac.
    func url(root: String) -> URL {
        var u = URL(fileURLWithPath: Paths.expand(root))
        if unit != "." { u.appendPathComponent(unit) }
        if !path.isEmpty { u.appendPathComponent(path) }
        return u
    }
}

private struct BrowseResult: Decodable {
    var online: Bool
    var peer: String?
    var items: [SyncItem]
}

@MainActor
final class ManagerModel: ObservableObject {
    enum Filter: String, CaseIterable { case all = "Alle", pending = "Nicht synchron" }

    @Published var unit: String?
    @Published var dir = ""
    @Published private(set) var items: [SyncItem] = []
    @Published private(set) var online = false
    @Published private(set) var peerName = "anderer Mac"
    @Published private(set) var loading = false
    @Published var error: String?
    @Published var filter: Filter = .all
    @Published var search = ""
    @Published var selection = Set<SyncItem.ID>()

    private var timer: Timer?

    var root: String { AppModel.shared.config?.string("root") ?? "~" }

    var shown: [SyncItem] {
        items.filter { item in
            (filter == .all || !item.isSynced || item.paused)
                && (search.isEmpty || item.name.localizedCaseInsensitiveContains(search))
        }
    }

    var selected: [SyncItem] { items.filter { selection.contains($0.id) } }

    /// Breadcrumb: (title, unit, dir) from the root down to here.
    var crumbs: [(title: String, unit: String?, dir: String)] {
        var out: [(String, String?, String)] = [(URL(fileURLWithPath: Paths.expand(root)).lastPathComponent, nil, "")]
        guard let unit else { return out }
        var acc = ""
        for part in unit.split(separator: "/") {
            acc = acc.isEmpty ? String(part) : acc + "/" + part
            out.append((String(part), acc, ""))
        }
        var d = ""
        for part in dir.split(separator: "/") where !dir.isEmpty {
            d = d.isEmpty ? String(part) : d + "/" + part
            out.append((String(part), unit, d))
        }
        return out
    }

    func start() {
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { _ in
            Task { @MainActor in ManagerWindow.shared.model?.refresh() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    func refresh() {
        guard !loading else { return }
        loading = true
        var req: [String: Any] = ["cmd": "browse", "dir": dir]
        req["unit"] = unit ?? NSNull()
        Task {
            defer { loading = false }
            do {
                guard let obj = try await Task.detached(operation: { try IPC.call(req, timeout: 30) }).value else {
                    error = "Die Synchronisation läuft nicht – „Synchronisation starten“ im Menü."
                    return
                }
                let data = try JSONSerialization.data(withJSONObject: obj)
                let d = JSONDecoder()
                d.keyDecodingStrategy = .convertFromSnakeCase
                let r = try d.decode(BrowseResult.self, from: data)
                items = r.items
                online = r.online
                peerName = r.peer ?? "anderer Mac"
                error = nil
                selection = selection.intersection(Set(items.map(\.id)))
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    func go(unit: String?, dir: String) {
        self.unit = unit
        self.dir = dir
        selection = []
        items = []
        refresh()
    }

    func open(_ item: SyncItem) {
        if item.isUnit {
            go(unit: item.unit, dir: "")
        } else if item.isDir {
            go(unit: item.unit, dir: item.path)
        } else if item.existsHere {
            NSWorkspace.shared.open(item.url(root: root))
        }
    }

    func up() {
        guard crumbs.count > 1 else { return }
        let c = crumbs[crumbs.count - 2]
        go(unit: c.unit, dir: c.dir)
    }

    // MARK: actions

    func syncNow(_ items: [SyncItem]) {
        let units = Array(Set(items.map(\.unit)))
        send(["cmd": "rescan"]) { [weak self] in
            self?.send(["cmd": "sync", "units": units, "dry_run": false])
        }
    }

    func setPaused(_ item: SyncItem, _ paused: Bool) {
        send(["cmd": paused ? "pause" : "resume", "unit": item.unit])
    }

    func resolve(_ item: SyncItem, keep choice: String) {
        guard let id = item.conflictId else { return }
        send(["cmd": "resolve", "id": id, "choice": choice])
    }

    func reveal(_ items: [SyncItem]) {
        let urls = items.filter(\.existsHere).map { $0.url(root: root) }
        if !urls.isEmpty { NSWorkspace.shared.activateFileViewerSelecting(urls) }
    }

    func trash(_ items: [SyncItem]) {
        let targets = items.filter(\.existsHere)
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
        NSWorkspace.shared.recycle(targets.map { $0.url(root: root) }) { [weak self] _, err in
            Task { @MainActor in
                if let err { self?.error = err.localizedDescription }
                self?.send(["cmd": "rescan"])
            }
        }
    }

    private func send(_ req: [String: Any], then: (() -> Void)? = nil) {
        Task {
            do {
                _ = try await Task.detached(operation: { try IPC.call(req, timeout: 3600) }).value
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
            then?()
            refresh()
        }
    }
}

// MARK: - view

struct ManagerView: View {
    @ObservedObject var model: ManagerModel
    @ObservedObject var app = AppModel.shared

    var body: some View {
        VStack(spacing: 0) {
            ProgressHeader(app: app, peer: model.peerName)
                .padding(12)
            Divider()
            HStack(spacing: 8) {
                Button { model.up() } label: { Image(systemName: "chevron.left") }
                    .disabled(model.crumbs.count < 2)
                ForEach(Array(model.crumbs.enumerated()), id: \.offset) { i, c in
                    if i > 0 { Image(systemName: "chevron.right").font(.caption).foregroundStyle(.secondary) }
                    Button(c.title) { model.go(unit: c.unit, dir: c.dir) }
                        .buttonStyle(.plain)
                        .fontWeight(i == model.crumbs.count - 1 ? .semibold : .regular)
                }
                Spacer()
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
            table
            Divider()
            footer.padding(10)
        }
        .frame(minWidth: 820, minHeight: 480)
    }

    private var table: some View {
        Table(model.shown, selection: $model.selection) {
            TableColumn("Name") { item in
                HStack(spacing: 6) {
                    Image(systemName: icon(item)).foregroundStyle(item.isDir ? Color.accentColor : .secondary)
                    Text(item.name).lineLimit(1)
                }
            }
            .width(min: 160, ideal: 220)
            TableColumn("Dieser Mac") { item in PresenceCell(presence: item.local, newer: item.state == "to-peer" || item.state == "delete-on-peer") }
                .width(min: 90, ideal: 110)
            TableColumn(model.peerName) { item in PresenceCell(presence: item.remote, newer: item.state == "from-peer" || item.state == "delete-here") }
                .width(min: 90, ideal: 130)
            TableColumn("Status") { item in StateCell(item: item, peer: model.peerName) }
                .width(min: 150, ideal: 190)
            TableColumn("Größe") { item in
                Text(item.size.map { Activity.bytes($0) } ?? "–").foregroundStyle(.secondary).monospacedDigit()
            }
            .width(min: 60, ideal: 80)
            TableColumn("Geändert") { item in
                Text(item.mtimeMs.map { Date(timeIntervalSince1970: Double($0) / 1000).formatted(date: .abbreviated, time: .shortened) } ?? "–")
                    .foregroundStyle(.secondary)
            }
            .width(min: 110, ideal: 140)
        }
        .contextMenu(forSelectionType: SyncItem.ID.self) { ids in
            menu(for: model.items.filter { ids.contains($0.id) })
        } primaryAction: { ids in
            if let item = model.items.first(where: { ids.contains($0.id) }) { model.open(item) }
        }
        .overlay {
            if model.shown.isEmpty {
                VStack(spacing: 6) {
                    if let e = model.error {
                        Label(e, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                    } else if model.loading && model.items.isEmpty {
                        ProgressView()
                    } else if model.filter == .pending {
                        Label("Hier ist alles synchron", systemImage: "checkmark.circle").foregroundStyle(.green)
                    } else {
                        Text("Leer").foregroundStyle(.secondary)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func menu(for items: [SyncItem]) -> some View {
        if items.count == 1, let i = items.first {
            if i.isDir || i.existsHere { Button(i.isDir ? "Öffnen" : "Öffnen") { model.open(i) } }
            if let _ = i.conflictId {
                Menu("Konflikt lösen") {
                    Button("Meine Version behalten") { model.resolve(i, keep: "local") }
                    Button("Version von \(model.peerName) übernehmen") { model.resolve(i, keep: "remote") }
                    Button("Beide behalten") { model.resolve(i, keep: "both") }
                }
            }
            if i.isUnit {
                Button(i.paused ? "Fortsetzen" : "Pausieren") { model.setPaused(i, !i.paused) }
            }
        }
        if !items.isEmpty {
            Button("Jetzt synchronisieren") { model.syncNow(items) }
            Button("Im Finder zeigen") { model.reveal(items) }.disabled(!items.contains(where: \.existsHere))
            Divider()
            Button("In den Papierkorb …", role: .destructive) { model.trash(items) }
                .disabled(!items.contains(where: \.existsHere))
        }
    }

    private var footer: some View {
        let pending = model.items.filter { !$0.isSynced }.count
        let sel = model.selected
        return HStack {
            Text("\(model.items.count) Einträge" + (pending > 0 ? " · \(pending) nicht synchron" : " · alles synchron"))
                .foregroundStyle(.secondary)
            if !model.online {
                Label("\(model.peerName) nicht erreichbar", systemImage: "wifi.exclamationmark").foregroundStyle(.orange)
            }
            Spacer()
            if sel.count == 1, let i = sel.first, i.isUnit {
                Button(i.paused ? "Fortsetzen" : "Pausieren") { model.setPaused(i, !i.paused) }
            }
            Button("Im Finder zeigen") { model.reveal(sel) }.disabled(!sel.contains(where: \.existsHere))
            Button("Jetzt synchronisieren") { model.syncNow(sel.isEmpty ? model.items : sel) }
                .keyboardShortcut("s", modifiers: [.command])
            Button { model.refresh() } label: { Image(systemName: "arrow.clockwise") }
                .help("Neu laden")
        }
    }

    private func icon(_ item: SyncItem) -> String {
        if item.isUnit { return "folder.fill" }
        switch item.kind {
        case "dir": return "folder"
        case "symlink": return "arrowshape.turn.up.right"
        default: return "doc"
        }
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
        if a.rate > 0 { s += " · \(Activity.bytes(Int64(a.rate)))/s" }
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
    let item: SyncItem
    let peer: String

    var body: some View {
        let (text, symbol, color) = describe()
        Label(text, systemImage: symbol).foregroundStyle(color).lineLimit(1)
    }

    private func describe() -> (String, String, Color) {
        if item.paused && !item.isSynced { return ("Pausiert", "pause.circle.fill", .orange) }
        switch item.state {
        case "synced": return ("Synchron", "checkmark.circle.fill", .green)
        case "to-peer": return ("Wird an \(peer) gesendet", "arrow.right.circle.fill", .blue)
        case "from-peer": return ("Wird von \(peer) geholt", "arrow.left.circle.fill", .blue)
        case "delete-on-peer": return ("Wird auf \(peer) gelöscht", "trash.circle.fill", .orange)
        case "delete-here": return ("Wird hier gelöscht", "trash.circle.fill", .orange)
        case "conflict": return ("Konflikt", "exclamationmark.triangle.fill", .red)
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

    func show() {
        if let w = window {
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let m = ManagerModel()
        let w = NSWindow(contentViewController: NSHostingController(rootView: ManagerView(model: m)))
        w.title = "MacSync – Synchronisations-Manager"
        w.styleMask = [.titled, .closable, .resizable, .miniaturizable]
        w.setContentSize(NSSize(width: 980, height: 580))
        w.isReleasedWhenClosed = false
        w.delegate = self
        w.setFrameAutosaveName("MacSyncManager")
        w.center()
        model = m
        window = w
        m.start()
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        // Documentation screenshots: MACSYNC_SNAPSHOT=/path.png renders the
        // window content once the listing has loaded.
        if let path = ProcessInfo.processInfo.environment["MACSYNC_SNAPSHOT"] {
            DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [weak w] in
                guard let view = w?.contentView, let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
                view.cacheDisplay(in: view.bounds, to: rep)
                try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
            }
        }
    }

    func windowWillClose(_ notification: Notification) {
        model?.stop()
        model = nil
        window = nil
    }
}
