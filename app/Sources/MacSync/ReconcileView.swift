import AppKit
import SwiftUI

// Views of the folder reconcile window (model in Reconcile.swift).

struct ReconcileView: View {
    @ObservedObject var model: ReconcileModel

    var body: some View {
        Group {
            if model.phase == .pick {
                ReconcilePicker(model: model)
            } else {
                ReconcileResult(model: model)
            }
        }
        .frame(minWidth: 980, minHeight: 560)
        .sheet(item: $model.report) { r in
            ReconcileReportView(model: model, report: r) { model.closeReport() }
        }
    }
}

private func date(_ ms: Int64?) -> String {
    guard let ms else { return "" }
    return Date(timeIntervalSince1970: Double(ms) / 1000).formatted(date: .numeric, time: .shortened)
}

/// Elapsed seconds while something runs.
private struct Elapsed: View {
    let since: Date

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { ctx in
            Text("\(Int(ctx.date.timeIntervalSince(since))) s").monospacedDigit().foregroundStyle(.secondary)
        }
    }
}

/// Error with the hint to Full Disk Access where it helps.
private struct ReconcileError: View {
    @ObservedObject var model: ReconcileModel

    var body: some View {
        if model.error != nil || model.needsFullDiskAccess {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 4) {
                    if let e = model.error { Text(e).textSelection(.enabled) }
                    if model.needsFullDiskAccess {
                        if let n = model.comparison?.skipped.count, n > 0, model.error == nil {
                            Text(n == 1 ? "1 Pfad war nicht lesbar – dort ist keine Aussage möglich (❌)."
                                 : "\(n) Pfade waren nicht lesbar – dort ist keine Aussage möglich (❌).")
                        }
                        Text("Für Ordner wie Google Drive, iCloud oder Dokumente braucht MacSync den Festplattenvollzugriff.")
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer()
                if model.needsFullDiskAccess {
                    Button("Festplattenvollzugriff öffnen …") { ReconcileModel.openFullDiskAccess() }
                }
            }
            .font(.callout)
            .padding(10)
            .background(RoundedRectangle(cornerRadius: 8).fill(Color.orange.opacity(0.1)))
        }
    }
}

// MARK: - start screen

private struct ReconcilePicker: View {
    @ObservedObject var model: ReconcileModel

    var body: some View {
        VStack(spacing: 20) {
            VStack(spacing: 6) {
                Text("Ordnerabgleich").font(.title2.weight(.semibold))
                Text("Vergleicht eine ältere Kopie mit der aktuellen und übernimmt, was in NEU fehlt oder in ALT neuer ist.\nNichts wird überschrieben oder endgültig gelöscht.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
            }
            HStack(spacing: 14) {
                FolderPane(title: "ALT", subtitle: "ältere Kopie", path: $model.oldPath)
                Button { model.swap() } label: { Image(systemName: "arrow.left.arrow.right").frame(width: 20, height: 20) }
                    .help("Seiten tauschen")
                FolderPane(title: "NEU", subtitle: "aktuelle Kopie", path: $model.newPath)
            }
            .frame(maxWidth: 860)
            ReconcileError(model: model).frame(maxWidth: 860)
            HStack(spacing: 10) {
                if model.comparing {
                    ProgressView().controlSize(.small)
                    Text("Vergleiche …")
                    Elapsed(since: model.started)
                    Button("Abbrechen") { model.cancel() }
                }
                Button("Vergleichen") { model.compare() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!model.canCompare)
            }
            if model.oldPath != nil, model.oldPath == model.newPath {
                Text("ALT und NEU sind derselbe Ordner.").foregroundStyle(.orange)
            }
            if !model.recent.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Zuletzt verglichen").font(.headline)
                    ForEach(model.recent, id: \.self) { pair in
                        Button { model.use(pair) } label: {
                            HStack(spacing: 6) {
                                Text(Paths.display(pair.first ?? "")).lineLimit(1).truncationMode(.head)
                                Image(systemName: "arrow.right").foregroundStyle(.secondary)
                                Text(Paths.display(pair.last ?? "")).lineLimit(1).truncationMode(.head)
                            }
                        }
                        .buttonStyle(.link)
                    }
                }
                .frame(maxWidth: 860, alignment: .leading)
            }
            Spacer()
        }
        .padding(28)
    }
}

/// One side of the start screen: path, „Auswählen …“, drop target.
private struct FolderPane: View {
    let title: String
    let subtitle: String
    @Binding var path: String?
    @State private var hovering = false

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 6) {
                Text(title).font(.headline)
                Text("(\(subtitle))").foregroundStyle(.secondary)
            }
            Image(systemName: path == nil ? "folder.badge.plus" : "folder.fill")
                .font(.system(size: 38))
                .foregroundStyle(Color.accentColor)
            if let path {
                Text(URL(fileURLWithPath: path).lastPathComponent).fontWeight(.medium).lineLimit(1)
                Text(Paths.display(path)).font(.caption).foregroundStyle(.secondary)
                    .lineLimit(2).truncationMode(.head).multilineTextAlignment(.center)
            } else {
                Text("Ordner hierher ziehen").foregroundStyle(.secondary)
                Text(" ").font(.caption)
            }
            Button("Auswählen …") { choose() }
        }
        .padding(18)
        .frame(maxWidth: .infinity, minHeight: 210)
        .background(RoundedRectangle(cornerRadius: 12).fill(hovering ? Color.accentColor.opacity(0.12) : Color(nsColor: .controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 12)
            .strokeBorder(hovering ? Color.accentColor : Color(nsColor: .separatorColor), style: StrokeStyle(lineWidth: hovering ? 2 : 1, dash: [6, 4])))
        .dropDestination(for: URL.self) { urls, _ in
            guard let u = urls.first, (try? u.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory == true else { return false }
            path = u.path
            return true
        } isTargeted: { hovering = $0 }
    }

    private func choose() {
        let p = NSOpenPanel()
        p.canChooseDirectories = true
        p.canChooseFiles = false
        p.allowsMultipleSelection = false
        p.prompt = "Auswählen"
        p.message = "\(title) (\(subtitle)) auswählen"
        if let path { p.directoryURL = URL(fileURLWithPath: path).deletingLastPathComponent() }
        if p.runModal() == .OK, let u = p.url { path = u.path }
    }
}

// MARK: - result

private struct ReconcileResult: View {
    @ObservedObject var model: ReconcileModel

    var body: some View {
        VStack(spacing: 0) {
            topBar.padding(.horizontal, 12).padding(.vertical, 8)
            Divider()
            VStack(alignment: .leading, spacing: 8) {
                summary
                orderBanner
                ReconcileError(model: model)
                toolbar
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            Divider()
            ReconcileTable(model: model)
                .overlay {
                    if model.comparing {
                        VStack(spacing: 8) {
                            ProgressView()
                            HStack { Text("Vergleiche …"); Elapsed(since: model.started) }
                            Button("Abbrechen") { model.cancel() }
                        }
                        .padding(20)
                        .background(RoundedRectangle(cornerRadius: 10).fill(.regularMaterial))
                    } else if model.rows.isEmpty {
                        emptyState
                    }
                }
            if !model.selection.isEmpty {
                Divider()
                ReconcileDetail(model: model).padding(12)
            }
            Divider()
            if let run = model.run, run.running {
                ReconcileProgress(model: model, run: run).padding(12)
            } else {
                planBar.padding(10)
            }
        }
        .sheet(isPresented: $model.confirming) {
            ReconcileConfirmView(model: model)
        }
    }

    private var topBar: some View {
        HStack(spacing: 10) {
            Button { model.backToStart() } label: { Label("Andere Ordner …", systemImage: "chevron.left") }
                .disabled(model.applying)
            Spacer(minLength: 8)
            side("ALT", model.comparison?.old ?? model.oldPath, files: model.comparison?.oldFiles, bytes: model.comparison?.oldBytes,
                 last: model.comparison?.order.oldLastMs)
            Button {
                model.swap()
                model.compare()
            } label: { Image(systemName: "arrow.left.arrow.right") }
                .help("Seiten tauschen und neu vergleichen")
                .disabled(model.comparing || model.applying)
            side("NEU", model.comparison?.new ?? model.newPath, files: model.comparison?.newFiles, bytes: model.comparison?.newBytes,
                 last: model.comparison?.order.newLastMs)
            Spacer(minLength: 8)
            Button { model.compare() } label: { Image(systemName: "arrow.clockwise") }
                .help("Neu vergleichen")
                .disabled(!model.canCompare)
        }
    }

    private func side(_ title: String, _ path: String?, files: Int64?, bytes: Int64?, last: Int64?) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            HStack(spacing: 6) {
                Text(title).fontWeight(.semibold)
                Text(Paths.display(path ?? "")).lineLimit(1).truncationMode(.head)
            }
            if let files, let bytes {
                Text("\(files) Dateien · \(Activity.bytes(bytes))" + (last.map { $0 > 0 ? " · zuletzt \(date($0))" : "" } ?? ""))
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: 380, alignment: .leading)
    }

    @ViewBuilder
    private var summary: some View {
        if let c = model.comparison, let root = model.root {
            let tops = root.children.filter { c.folders[$0].inOld }
            let ok = tops.filter { c.folders[$0].status.deletable }.count
            HStack(spacing: 8) {
                switch root.status {
                case .identical:
                    Text("✅ ALT ist identisch mit NEU – kann ganz weg (\(Activity.bytes(root.oldBytes)))")
                case .deletable:
                    Text("🚮 ALT kann ganz weg – NEU enthält alles, teils neuer (\(Activity.bytes(root.oldBytes)))")
                default:
                    Text("🟧 ALT kann noch nicht ganz weg")
                    Text("· \(ok) von \(tops.count) Ordnern können weg (\(Activity.bytes(c.deletableBytes))) · \(root.total.risky) offen")
                        .foregroundStyle(.secondary)
                }
            }
            .font(.headline)
        }
    }

    @ViewBuilder
    private var orderBanner: some View {
        if let o = model.comparison?.order, !model.orderDismissed,
           o.status == "contradicted" || (o.status == "unknown" && o.newerInOld > 0) {
            let changed = o.newerInNew + o.newerInOld
            HStack(alignment: .top, spacing: 10) {
                Text("⚠️")
                VStack(alignment: .leading, spacing: 2) {
                    Text(o.status == "contradicted"
                         ? "Reihenfolge fraglich: \(o.newerInOld) von \(changed) geänderten Dateien sind in ALT neuer. Ist ALT die aktuelle Kopie?"
                         : "Reihenfolge unklar: \(o.newerInNew) geänderte Dateien sind in NEU neuer, \(o.newerInOld) in ALT.")
                        .fontWeight(.medium)
                    Text("Letzte Änderung: ALT \(date(o.oldLastMs)) · NEU \(date(o.newLastMs))")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Seiten tauschen") {
                    model.swap()
                    model.compare()
                }
                .disabled(model.comparing || model.applying)
                Button("So lassen") { model.orderDismissed = true }
            }
            .font(.callout)
            .padding(10)
            .background(RoundedRectangle(cornerRadius: 8).fill(Color.yellow.opacity(0.15)))
        }
    }

    private var toolbar: some View {
        HStack(spacing: 8) {
            Picker("", selection: $model.mode) {
                ForEach(ReconcileModel.Mode.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 190)
            if let t = model.root?.total {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(FileState.chipOrder.filter { t[$0] > 0 }, id: \.self) { s in
                            chip(s, t[s])
                        }
                    }
                }
            }
            Spacer(minLength: 4)
            Menu {
                Button("Alle aufklappen") { model.expandAll(true) }
                Button("Alle zuklappen") { model.expandAll(false) }
            } label: { Image(systemName: "list.bullet.indent") }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .help("Ordner auf- oder zuklappen")
            TextField("Suchen", text: $model.search)
                .textFieldStyle(.roundedBorder)
                .frame(width: 170)
        }
    }

    private func chip(_ s: FileState, _ n: Int) -> some View {
        let on = model.chip == s
        return Button { model.chip = on ? nil : s } label: {
            Text("\(s.symbol) \(n)")
                .monospacedDigit()
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(Capsule().fill(on ? Color.accentColor.opacity(0.25) : Color(nsColor: .quaternaryLabelColor).opacity(0.5)))
                .overlay(Capsule().stroke(on ? Color.accentColor : .clear))
        }
        .buttonStyle(.plain)
        .help(s.title + (on ? " – Filter aufheben" : " – nur diese zeigen"))
    }

    @ViewBuilder
    private var emptyState: some View {
        if model.comparison != nil {
            if model.filtering {
                Text("Keine Treffer").foregroundStyle(.secondary)
            } else if model.mode == .open {
                VStack(spacing: 6) {
                    Label("Nichts offen – ALT enthält nichts, was in NEU fehlt", systemImage: "checkmark.circle")
                        .foregroundStyle(.green)
                    Button("Alle zeigen") { model.mode = .all }.buttonStyle(.link)
                }
            } else {
                Text("Leer").foregroundStyle(.secondary)
            }
        }
    }

    private var planBar: some View {
        HStack(spacing: 10) {
            Image(systemName: "list.bullet.clipboard").foregroundStyle(model.plan.isEmpty ? Color.secondary : Color.accentColor)
            Text(model.planSummary).foregroundStyle(model.plan.isEmpty ? .secondary : .primary)
            if !model.complete {
                Text("· gleiche Dateien nicht aufgelistet (ältere macsync-Version)").font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button("Plan anzeigen …") { model.showingPlan.toggle() }
                .disabled(model.plan.isEmpty)
                .popover(isPresented: $model.showingPlan, arrowEdge: .top) { PlanOverview(model: model) }
            Button("Vorschläge") { model.suggest() }
                .help("Nur in ALT → kopieren, in ALT neuer → ALT übernehmen; Unklares bleibt deine Entscheidung")
                .disabled(model.comparison == nil)
            Button("Plan leeren") { model.clearPlan() }.disabled(model.plan.isEmpty)
            Button("Ausführen …") { model.execute() }
                .buttonStyle(.borderedProminent)
                .disabled(model.plan.isEmpty || model.applying || model.comparing)
        }
    }
}

// MARK: - tree table

private struct ReconcileTable: View {
    @ObservedObject var model: ReconcileModel
    /// Header clicks of the checkbox columns arrive as sort requests (SwiftUI
    /// tables on macOS 14 only have text headers); nothing is sorted.
    @State private var sort: [KeyPathComparator<RNode>] = []

    var body: some View {
        Table(model.rows, selection: $model.selection, sortOrder: $sort) {
            TableColumn("ALT") { n in NameCell(model: model, node: n, old: true) }
                .width(min: 160, ideal: 220)
            TableColumn("Größe") { n in size(n.inOld, n.oldBytes, planned: model.plan[n.id] != nil) }
                .width(min: 56, ideal: 66)
            TableColumn("Geändert") { n in Text(n.inOld ? date(n.oldMs) : "").foregroundStyle(.secondary) }
                .width(min: 90, ideal: 104)
            TableColumn("") { n in StateCell(model: model, node: n) }
                .width(min: 90, ideal: 120)
            TableColumn(header(.copy), value: \RNode.copyColumn) { n in CheckCell(model: model, node: n, column: .copy) }
                .width(min: 74, ideal: 84)
            TableColumn(header(.move), value: \RNode.moveColumn) { n in CheckCell(model: model, node: n, column: .move) }
                .width(min: 84, ideal: 96)
            TableColumn("NEU") { n in NameCell(model: model, node: n, old: false) }
                .width(min: 160, ideal: 210)
            TableColumn("Größe") { n in size(n.inNew, n.newBytes) }
                .width(min: 56, ideal: 66)
            TableColumn("Geändert") { n in Text(n.inNew ? date(n.newMs) : "").foregroundStyle(.secondary) }
                .width(min: 90, ideal: 104)
        }
        .onChange(of: sort) { _, new in
            guard let c = new.first else { return }
            sort = []
            if !model.applying { model.toggleAll(c.keyPath == \RNode.copyColumn ? .copy : .move) }
        }
        .onKeyPress(.space) {
            guard !model.applying, !model.selection.isEmpty else { return .ignored }
            model.toggleCopySelected()
            return .handled
        }
        .contextMenu(forSelectionType: Int.self) { ids in
            ReconcileActions(model: model, targets: ids.sorted().map { model.nodes[$0] }, menu: true)
        } primaryAction: { ids in
            guard ids.count == 1, let id = ids.first else { return }
            let n = model.nodes[id]
            if n.isDir { model.toggle(n) } else { model.showDiff(n) }
        }
    }

    /// Header with the state of its checkboxes: ☐ none, ⊟ some, ☑ all visible rows.
    private func header(_ column: PlanColumn) -> Text {
        let glyph: String
        switch model.headerState(column) {
        case .none: glyph = "☐"
        case .mixed: glyph = "⊟"
        case .all: glyph = "☑"
        }
        return Text("\(glyph) \(column.title)")
    }

    /// Planned and from 1 GB on: orange, so a huge folder is not overlooked.
    @ViewBuilder
    private func size(_ present: Bool, _ b: Int64?, planned: Bool = false) -> some View {
        if present, let b, planned, b >= ReconcileModel.largeBytes {
            Text(Activity.bytes(b)).fontWeight(.semibold).foregroundStyle(.orange).monospacedDigit()
        } else {
            Text(present ? b.map { Activity.bytes($0) } ?? "" : "").foregroundStyle(.secondary).monospacedDigit()
        }
    }
}

private extension RNode {
    /// Sort keys only to tell the checkbox column headers apart.
    var copyColumn: Int { id }
    var moveColumn: Int { id }
}

/// „Kopieren“ / „Verschieben“ checkbox of a row; empty where not possible.
private struct CheckCell: View {
    @ObservedObject var model: ReconcileModel
    let node: RNode
    let column: PlanColumn

    var body: some View {
        HStack(spacing: 4) {
            if model.coveredBy(column, node) {
                Toggle("", isOn: .constant(true)).labelsHidden().disabled(true)
                    .help("mit Ordner – den Ordner abwählen, um einzelne Dateien zu wählen")
            } else if let op = model.op(column, for: node) {
                Toggle("", isOn: Binding(get: { model.isChecked(column, node) }, set: { _ in model.toggle(column, node) }))
                    .labelsHidden()
                    .disabled(model.applying)
                    .help(help(op))
                if model.isChecked(column, node), let b = node.oldBytes, b >= ReconcileModel.largeBytes {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                        .help("\(Activity.bytes(b)) – wirklich alles?")
                }
            }
        }
    }

    private func help(_ op: PlanOp) -> String {
        switch op {
        case .replace: return "ALT-Fassung übernehmen – die NEU-Fassung kommt in den Papierkorb"
        case .move: return "Nach NEU verschieben – die ALT-Quelle kommt nach bestätigter Kopie in den Papierkorb"
        default: return "Nach NEU kopieren"
        }
    }
}

private struct NameCell: View {
    @ObservedObject var model: ReconcileModel
    let node: RNode
    let old: Bool

    var body: some View {
        HStack(spacing: 4) {
            Color.clear.frame(width: CGFloat(max(node.depth, 0)) * 14, height: 1)
            if old && node.isDir && !node.children.isEmpty {
                Button { model.toggle(node) } label: {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 9, weight: .bold))
                        .rotationEffect(.degrees(model.isExpanded(node) ? 90 : 0))
                        .frame(width: 12, height: 14)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
            } else {
                Color.clear.frame(width: 12, height: 1)
            }
            if present {
                Image(systemName: node.isDir ? "folder.fill" : "doc")
                    .foregroundStyle(node.isDir ? Color.accentColor : .secondary)
                    .frame(width: 16)
                Text(old ? node.name : node.nameNew)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .foregroundStyle(quiet ? .secondary : .primary)
            } else {
                missing
            }
        }
    }

    private var present: Bool { old ? node.inOld : node.inNew }

    /// Identical things recede.
    private var quiet: Bool { node.fileState == .same || node.folderState == .identical }

    @ViewBuilder
    private var missing: some View {
        Text("—").foregroundStyle(.tertiary).frame(width: 16)
        if !old {
            if let to = model.plan[node.id]?.to ?? model.runState(node).flatMap({ s in
                model.run.flatMap { $0.paths[s.index] == node.path ? $0.tos[s.index] : nil }
            }) {
                // Same place: the name is enough; elsewhere the whole target.
                Text("→ " + (to == node.path ? node.name : to))
                    .foregroundStyle(Color.accentColor).lineLimit(1).truncationMode(.head)
            } else if let m = node.movedTo {
                Text("→ \(m)").foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                    .help("Verschoben nach \(m)")
            } else if let s = node.suggested {
                Text("→ \(s)").foregroundStyle(.secondary).italic().lineLimit(1).truncationMode(.head)
                    .help("Vorschlag: dort liegen die meisten Dateien dieses Ordners in NEU")
            }
        }
    }
}

private struct StateCell: View {
    @ObservedObject var model: ReconcileModel
    let node: RNode

    var body: some View {
        HStack(spacing: 4) {
            Text(node.symbol)
            if let s = model.runState(node), let run = model.run {
                RunBadge(state: s.state, op: run.ops[s.index], fraction: run.fraction(s.index),
                         cloud: run.codes[s.index] == "cloud_unavailable" ? model.cloudName : nil)
            } else if let p = model.plan[node.id], p.op == .keepBoth || p.op == .trashOld {
                // Copy, move and replace show as checkboxes.
                OpBadge(op: p.op)
            } else if model.covered(node) {
                Text("mit Ordner").font(.caption).foregroundStyle(Color.accentColor)
            } else if node.isDir && node.open > 0 && node.folderState != .onlyOld {
                Text("\(node.open) offen").font(.caption).foregroundStyle(.secondary)
            }
        }
        .help(help)
    }

    private var help: String {
        if let s = model.runState(node), let e = model.run?.errors[s.index] { return ReconcileModel.reason(e) }
        return node.stateTitle
    }
}

/// Status of a plan action in its row while and after it runs.
private struct RunBadge: View {
    let state: RunState
    let op: PlanOp
    let fraction: Double?
    /// Failed because the cloud did not deliver: its name.
    var cloud: String?

    var body: some View {
        switch state {
        case .waiting:
            Text("wartet").font(.caption).foregroundStyle(.secondary)
        case .running:
            HStack(spacing: 4) {
                if let fraction {
                    ProgressView(value: fraction).progressViewStyle(.circular).controlSize(.mini)
                    Text("läuft \(Int(fraction * 100)) %").monospacedDigit()
                } else {
                    ProgressView().controlSize(.mini)
                    Text("läuft")
                }
            }
            .font(.caption.weight(.medium))
            .foregroundStyle(Color.accentColor)
        case .done:
            Text("✓ " + op.done).font(.caption.weight(.medium)).foregroundStyle(.green)
                .help(op == .trashOld ? "Prüfsumme identisch – ALT liegt im Papierkorb" : "")
        case .skipped:
            Text("übersprungen").font(.caption).foregroundStyle(.orange)
        case .failed:
            if let cloud {
                Text("\(cloud) liefert nicht").font(.caption.weight(.medium)).foregroundStyle(.red)
            } else {
                Text("Fehler").font(.caption.weight(.medium)).foregroundStyle(.red)
            }
        case .cancelled:
            Text("abgebrochen").font(.caption).foregroundStyle(.secondary)
        }
    }
}

/// Plan actions for the selected rows (context menu or detail pane).
private struct ReconcileActions: View {
    @ObservedObject var model: ReconcileModel
    let targets: [RNode]
    let menu: Bool

    var body: some View {
        // While a plan runs it cannot be changed.
        let editable = model.applying ? [] : targets
        // Copy, move and replace are the checkbox columns.
        let ops = [PlanOp.keepBoth, .trashOld].compactMap { op -> (PlanOp, [RNode])? in
            let ns = editable.filter { model.ops(for: $0).contains(op) }
            return ns.isEmpty ? nil : (op, ns)
        }
        let planned = editable.filter { model.plan[$0.id] != nil }
        let keepNew = editable.contains { $0.fileState == .oldNewer || $0.fileState == .unclear }
        ForEach(ops, id: \.0) { op, ns in
            Button(title(op, ns)) { model.setPlan(op, ns) }
        }
        if keepNew {
            Button("NEU behalten") { model.unplan(targets) }
                .help("Nichts tun: die NEU-Fassung bleibt")
        } else if !planned.isEmpty {
            Button(planned.count > 1 && targets.count > 1 ? "Aus dem Plan nehmen (\(planned.count))" : "Aus dem Plan nehmen") {
                model.unplan(planned)
            }
        }
        if menu {
            if targets.count == 1, let n = targets.first {
                if !ops.isEmpty || keepNew || !planned.isEmpty { Divider() }
                if !n.isDir { Button("Unterschiede zeigen") { model.showDiff(n) } }
                if let u = model.oldURL(n) { Button("ALT im Finder zeigen") { model.reveal([u]) } }
                if let u = model.newURL(n) { Button("NEU im Finder zeigen") { model.reveal([u]) } }
            } else if targets.count > 1 {
                Divider()
                Button("Im Finder zeigen") {
                    model.reveal(targets.compactMap { model.oldURL($0) ?? model.newURL($0) })
                }
            }
        }
    }

    private func title(_ op: PlanOp, _ ns: [RNode]) -> String {
        var t = op.title
        if ns.count == 1, let n = ns.first, n.isDir { t += " (ganzer Ordner)" }
        if targets.count > 1 { t += " (\(ns.count))" }
        return t
    }
}

// MARK: - detail

private struct ReconcileDetail: View {
    @ObservedObject var model: ReconcileModel

    var body: some View {
        let sel = model.selected
        if sel.count == 1, let n = sel.first {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top, spacing: 14) {
                    SideBox(title: "ALT", present: n.inOld, size: n.oldBytes, ms: n.oldMs, lines: lines(n)?.old,
                            path: n.path, folder: n.isDir)
                    VStack(spacing: 4) {
                        Text(n.symbol).font(.title)
                        Text(n.stateTitle).font(.callout).multilineTextAlignment(.center)
                        // From ALT's side: lines only in ALT +, lines only in NEU −.
                        if let l = lines(n), let a = l.added, let r = l.removed, a + r > 0 {
                            Text("ALT: +\(r) / −\(a) Zeilen gegenüber NEU").font(.caption).monospacedDigit().foregroundStyle(.secondary)
                        }
                        if let p = model.plan[n.id] {
                            Text("Plan: \(p.op.short)").font(.caption.weight(.medium)).foregroundStyle(Color.accentColor)
                        }
                    }
                    .frame(width: 170)
                    SideBox(title: "NEU", present: n.inNew || n.movedTo != nil, size: n.movedTo != nil ? model.entry(n)?.sizeNew ?? n.oldBytes : n.newBytes,
                            ms: n.newMs, lines: lines(n)?.new, path: n.movedTo ?? model.plan[n.id]?.to ?? n.pathNew,
                            folder: n.isDir, note: note(n))
                }
                HStack(spacing: 8) {
                    if !n.isDir {
                        Button("Unterschiede zeigen") { model.showDiff(n) }
                            .disabled(model.oldURL(n) == nil && model.newURL(n) == nil)
                    }
                    Button("ALT im Finder zeigen") { model.oldURL(n).map { model.reveal([$0]) } }
                        .disabled(model.oldURL(n) == nil)
                    Button("NEU im Finder zeigen") { model.newURL(n).map { model.reveal([$0]) } }
                        .disabled(model.newURL(n) == nil)
                    Spacer()
                    ReconcileActions(model: model, targets: [n], menu: false)
                }
            }
        } else {
            HStack(spacing: 8) {
                Text("\(sel.count) ausgewählt").foregroundStyle(.secondary)
                Spacer()
                ReconcileActions(model: model, targets: sel, menu: false)
            }
        }
    }

    private func lines(_ n: RNode) -> LineInfo? {
        model.lines?.id == n.id ? model.lines : nil
    }

    private func note(_ n: RNode) -> String? {
        if n.movedTo != nil { return "an anderem Ort" }
        if model.plan[n.id] != nil, !n.inNew { return "Ziel des Plans" }
        if n.suggested != nil, !n.inNew { return "Vorschlag" }
        return nil
    }
}

private struct SideBox: View {
    let title: String
    let present: Bool
    let size: Int64?
    let ms: Int64?
    let lines: Int?
    let path: String
    let folder: Bool
    var note: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(title).font(.headline)
                if let note { Text(note).font(.caption).foregroundStyle(.secondary) }
            }
            if present || note != nil {
                row("Pfad", path.isEmpty ? "/" : path)
            }
            if present {
                row("Größe", size.map { Activity.bytes($0) } ?? "–")
                row(folder ? "Zuletzt" : "Geändert", ms.map { date($0) } ?? "–")
                if !folder { row("Zeilen", lines.map { "\($0)" } ?? "–") }
            } else if note == nil {
                Text("— nicht vorhanden").foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color(nsColor: .separatorColor)))
    }

    private func row(_ k: String, _ v: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(k).foregroundStyle(.secondary).frame(width: 62, alignment: .leading)
            Text(v).lineLimit(1).truncationMode(.middle).textSelection(.enabled)
        }
        .font(.callout)
    }
}

// MARK: - plan overview, confirmation, progress

/// Duration as „1 Std. 40 Min.“, „12 Min.“, „40 s“.
private func duration(_ t: TimeInterval) -> String {
    if t < 60 { return "\(max(Int(t.rounded()), 1)) s" }
    let m = Int((t / 60).rounded())
    if m < 60 { return "\(m) Min." }
    return m % 60 == 0 ? "\(m / 60) Std." : "\(m / 60) Std. \(m % 60) Min."
}

private func files(_ n: Int) -> String { n == 1 ? "1 Datei" : "\(n) Dateien" }

/// Size, from 1 GB on in orange with a warning sign.
private struct SizeText: View {
    let bytes: Int64

    var body: some View {
        if bytes >= ReconcileModel.largeBytes {
            Label(Activity.bytes(bytes), systemImage: "exclamationmark.triangle.fill")
                .fontWeight(.semibold)
                .foregroundStyle(.orange)
                .monospacedDigit()
        } else {
            Text(Activity.bytes(bytes)).foregroundStyle(.secondary).monospacedDigit()
        }
    }
}

/// Plan marker; deleting in ALT in red.
private struct OpBadge: View {
    let op: PlanOp

    var body: some View {
        let color = op == .trashOld ? Color.red : Color.accentColor
        Text(op.short)
            .font(.caption.weight(.medium))
            .foregroundStyle(color)
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(Capsule().fill(color.opacity(0.15)))
    }
}

/// One planned action: what, where to, how big.
private struct PlanLine: View {
    let row: PlanRow
    /// Still in the cloud (from the probe).
    var cloud: Int64 = 0

    var body: some View {
        HStack(spacing: 8) {
            OpBadge(op: row.item.op).frame(width: 84, alignment: .leading)
            Image(systemName: row.item.dir ? "folder.fill" : "doc")
                .foregroundStyle(row.item.dir ? Color.accentColor : .secondary)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 1) {
                Text(row.path).lineLimit(1).truncationMode(.middle)
                if row.item.op == .trashOld {
                    Text("in den Papierkorb (ALT)" + (row.item.to != row.path ? " · Kopie in NEU: \(row.item.to)" : ""))
                        .font(.caption).foregroundStyle(.red).lineLimit(1).truncationMode(.middle)
                } else if row.item.to != row.path {
                    Text("→ \(row.item.to)").font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                }
            }
            Spacer(minLength: 8)
            if cloud > 0 {
                Label(Activity.bytes(cloud), systemImage: "icloud.and.arrow.down")
                    .font(.caption).foregroundStyle(.secondary).monospacedDigit()
                    .help("\(Activity.bytes(cloud)) davon liegen nur in der Cloud und werden erst geladen")
            }
            if row.item.dir {
                Text(files(row.item.files)).font(.caption).foregroundStyle(.secondary).monospacedDigit()
            }
            SizeText(bytes: row.item.bytes).frame(minWidth: 76, alignment: .trailing)
        }
    }
}

/// „Plan anzeigen“: all planned actions, largest first, each removable.
private struct PlanOverview: View {
    @ObservedObject var model: ReconcileModel

    var body: some View {
        let rows = model.planRows
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Plan").font(.headline)
                Text("nach Größe").foregroundStyle(.secondary)
                Spacer()
                if rows.contains(where: \.large) {
                    Text("\(rows.filter(\.large).count) ab 1 GB").font(.caption).foregroundStyle(.orange)
                }
            }
            .padding(12)
            Divider()
            if rows.isEmpty {
                Text("Der Plan ist leer.").foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 80)
            } else {
                List(rows) { r in
                    HStack(spacing: 6) {
                        PlanLine(row: r, cloud: model.probe?.perNode[r.id] ?? 0)
                        Button { model.unplan([model.nodes[r.id]]) } label: {
                            Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .help("Aus dem Plan nehmen")
                    }
                    .listRowBackground(r.large ? Color.orange.opacity(0.08) : Color.clear)
                }
                .listStyle(.plain)
            }
            Divider()
            HStack {
                Text(rows.count == 1 ? "1 Aktion" : "\(rows.count) Aktionen").fontWeight(.medium)
                Spacer()
                Text(files(model.planFiles)).foregroundStyle(.secondary).monospacedDigit()
                Text("Gesamt").foregroundStyle(.secondary)
                SizeText(bytes: model.planBytes).frame(minWidth: 76, alignment: .trailing)
                Color.clear.frame(width: 16, height: 1)
            }
            .padding(12)
        }
        .frame(width: 600, height: min(520, CGFloat(130 + max(rows.count, 3) * 26)))
    }
}

/// The cloud app does not deliver placeholder files: what to do.
private struct CloudHint: View {
    @ObservedObject var model: ReconcileModel
    let error: String?
    /// „Erneut prüfen“ (before running).
    let retry: Bool

    var body: some View {
        let name = model.cloudName
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "icloud.slash").font(.title2).foregroundStyle(.red)
            VStack(alignment: .leading, spacing: 4) {
                Text("\(name) liefert die Dateien gerade nicht.").fontWeight(.semibold)
                if let error {
                    Text(error).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                        .lineLimit(2).truncationMode(.middle)
                }
                Text("1. \(name) starten und warten, bis es verbunden ist")
                Text("Tipp: Ordner im Finder per Rechtsklick → „\(name == "iCloud" ? "Jetzt laden" : "Offline verfügbar")“ vorher herunterladen")
                    .foregroundStyle(.secondary)
                HStack(spacing: 8) {
                    if let c = model.comparison {
                        Button("ALT im Finder zeigen") { model.reveal([URL(fileURLWithPath: c.old)]) }
                    }
                    if retry {
                        Button("Erneut prüfen") { model.checkCloud() }.disabled(model.probing)
                    }
                }
                .padding(.top, 2)
            }
            Spacer(minLength: 0)
        }
        .font(.callout)
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.red.opacity(0.1)))
    }
}

/// Before „Ausführen“: how much, the largest items, what can take long.
private struct ReconcileConfirmView: View {
    @ObservedObject var model: ReconcileModel

    var body: some View {
        let rows = model.planRows
        let total = model.planCopyBytes
        let trash = model.planTrashBytes
        let free = model.newFreeBytes
        VStack(alignment: .leading, spacing: 12) {
            Text(rows.count == 1 ? "1 Aktion ausführen?" : "\(rows.count) Aktionen ausführen?").font(.title3.weight(.semibold))
            HStack(spacing: 6) {
                Text("\(files(model.planFiles)) ·")
                if total > 0 || trash == 0 {
                    SizeText(bytes: total)
                    Text("nach NEU").foregroundStyle(.secondary)
                }
                if trash > 0 {
                    if total > 0 { Text("·") }
                    Text("\(Activity.bytes(trash)) in ALT prüfen und löschen").foregroundStyle(.red)
                }
                if let free, total > 0 { Text("· in NEU frei: \(Activity.bytes(free))").foregroundStyle(.secondary) }
            }
            if model.probing {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text("Prüfe \(model.cloudName) …").foregroundStyle(.secondary)
                }
            } else if let p = model.probe, p.bytes > 0 {
                Label("davon \(Activity.bytes(p.bytes)) aus der Cloud zu laden (\(files(p.files)))", systemImage: "icloud.and.arrow.down")
                    .foregroundStyle(.secondary)
            }
            if let p = model.probe, p.ok == false {
                CloudHint(model: model, error: p.error, retry: true)
            }
            if total >= ReconcileModel.largeBytes || model.oldInCloud || (free.map { $0 < total } ?? false) {
                VStack(alignment: .leading, spacing: 6) {
                    if let free, free < total {
                        Label("In NEU ist nicht genug Platz frei (\(Activity.bytes(free)) für \(Activity.bytes(total))).",
                              systemImage: "xmark.octagon.fill").foregroundStyle(.red)
                    }
                    if total >= ReconcileModel.largeBytes {
                        Label("Das sind \(Activity.bytes(total)) – das kann lange dauern. Prüfe, ob wirklich alles nach NEU soll.",
                              systemImage: "exclamationmark.triangle.fill")
                    }
                    if model.oldInCloud {
                        Label(total == 0 ? "Dateien aus \(model.cloudName) werden für die Prüfung erst heruntergeladen."
                              : "Dateien aus \(model.cloudName) werden dabei erst heruntergeladen.", systemImage: "icloud.and.arrow.down")
                    }
                }
                .font(.callout)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.orange.opacity(0.12)))
            }
            VStack(alignment: .leading, spacing: 6) {
                Text(rows.count > 5 ? "Die 5 größten" : "Aktionen").font(.headline)
                ForEach(rows.prefix(5)) { PlanLine(row: $0, cloud: model.probe?.perNode[$0.id] ?? 0) }
                if rows.count > 5 {
                    Text("… und \(rows.count - 5) weitere (\(Activity.bytes(rows.dropFirst(5).reduce(0) { $0 + $1.item.bytes })))")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            VStack(alignment: .leading, spacing: 2) {
                ForEach(model.planEffects, id: \.self) { Text("• " + $0) }
            }
            .font(.callout)
            VStack(alignment: .leading, spacing: 2) {
                Text("In den Papierkorb kommt:").fontWeight(.medium)
                ForEach(model.planCounts, id: \.0) { op, _ in
                    Text("\(op.short): \(op.trashes ?? "nichts")").foregroundStyle(op == .trashOld ? .red : .secondary)
                }
            }
            .font(.callout)
            Text("Nichts wird überschrieben oder endgültig gelöscht. Was sich seit dem Vergleich geändert hat, wird übersprungen. Abbrechen geht jederzeit, fertige Aktionen bleiben dann bestehen.")
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            HStack {
                Button("Plan anzeigen") { model.reviewPlan() }
                Spacer()
                Button("Abbrechen", role: .cancel) { model.cancelConfirm() }.keyboardShortcut(.cancelAction)
                if model.probe?.ok != false {
                    Button("Ausführen") { model.start() }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(model.probing)
                }
            }
        }
        .padding(20)
        .frame(width: 600)
    }
}

/// Replaces the plan bar while the plan runs.
private struct ReconcileProgress: View {
    @ObservedObject var model: ReconcileModel
    let run: ApplyRun

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Text(run.cancelling ? "Wird abgebrochen …" : "Plan wird ausgeführt").font(.headline)
                TimelineView(.periodic(from: .now, by: 1)) { ctx in
                    Text(stats(ctx.date)).monospacedDigit().foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer(minLength: 8)
                Elapsed(since: run.started)
                if run.cancelling {
                    ProgressView().controlSize(.small)
                    Button("Wird abgebrochen …") {}.disabled(true).controlSize(.large)
                } else {
                    Button(role: .destructive) { model.cancelRun() } label: {
                        Label("Abbrechen", systemImage: "stop.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(.red)
                    .controlSize(.large)
                    .help("Hält die laufende Aktion an und entfernt ihre halbe Kopie; fertige Aktionen bleiben")
                }
            }
            ProgressView(value: run.fraction)
            if let i = run.current, run.paths.indices.contains(i) {
                HStack(spacing: 6) {
                    OpBadge(op: run.ops[i])
                    Text(run.paths[i]).fontWeight(.medium).lineLimit(1).truncationMode(.middle)
                    if run.byBytes {
                        Text(current(i)).monospacedDigit().foregroundStyle(.secondary)
                    }
                    if !run.file.isEmpty, run.file != run.paths[i] {
                        Text("· \(run.file)").foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                    }
                    Spacer(minLength: 0)
                }
                .font(.callout)
            }
        }
    }

    /// „1,2 GB von 75,1 GB · 3 von 10 · 12 MB/s · noch ca. 1 Std. 40 Min.“
    private func stats(_ now: Date) -> String {
        var parts: [String] = []
        if run.byBytes { parts.append("\(Activity.bytes(run.processed)) von \(Activity.bytes(run.bytesTotal))") }
        parts.append("\(run.finished) von \(run.total)" + (run.byBytes ? "" : " Aktionen"))
        if run.byBytes && now.timeIntervalSince(run.lastData) > 5 {
            // E.g. a file still downloading from Google Drive: the last rate says nothing.
            parts.append("wartet auf Daten …")
        } else {
            if let r = run.rate, run.byBytes { parts.append(Activity.bytes(Int64(max(r, 0))) + "/s") }
            if let t = run.remaining, !run.cancelling { parts.append("noch ca. " + duration(t)) }
        }
        return parts.joined(separator: " · ")
    }

    private func current(_ i: Int) -> String {
        let pct = run.fraction(i).map { " · \(Int($0 * 100)) %" } ?? ""
        return "\(Activity.bytes(run.currentDone)) von \(Activity.bytes(run.bytes[i]))" + pct
    }
}

// MARK: - result of a plan

private struct ReconcileReportView: View {
    @ObservedObject var model: ReconcileModel
    let report: ApplyReport
    let close: () -> Void
    /// Copies chosen for „Ausgewählte in ALT löschen …“.
    @State private var chosen = Set<Int>()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(report.error != nil ? "Plan nicht ausgeführt" : report.wasCancelled ? "Plan abgebrochen"
                 : report.results.allSatisfy({ $0.op == .trashOld }) ? (report.done == 0 ? "Nichts in ALT gelöscht" : "In ALT gelöscht")
                 : "Plan ausgeführt")
                .font(.title3.weight(.semibold))
            if let e = report.error {
                Label(e, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
            } else {
                Text(counts).foregroundStyle(.secondary)
                if report.wasCancelled {
                    Label(report.done == 0
                          ? "Abgebrochen, bevor etwas fertig war – NEU ist unverändert."
                          : "Fertige Aktionen bleiben bestehen, sonst ist NEU unverändert."
                            + (report.cleanStop ? " Die abgebrochene Aktion hat keine halbe Kopie hinterlassen." : ""),
                          systemImage: "stop.circle")
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if let r = report.results.first(where: \.cloudUnavailable) {
                CloudHint(model: model, error: r.error, retry: false)
            }
            if !report.results.isEmpty {
                List(report.results.sorted { (rank($0), $0.id) < (rank($1), $1.id) }) { r in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        if let o = offer(r) {
                            Toggle("", isOn: Binding(get: { chosen.contains(o.id) },
                                                     set: { if $0 { chosen.insert(o.id) } else { chosen.remove(o.id) } }))
                                .labelsHidden()
                        }
                        Image(systemName: symbol(r)).foregroundStyle(color(r))
                        VStack(alignment: .leading, spacing: 1) {
                            Text(r.to.isEmpty || r.to == r.path || r.op == .trashOld ? r.path : "\(r.path) → \(r.to)")
                                .lineLimit(1).truncationMode(.middle)
                            if let n = note(r) {
                                Text(n).font(.caption).foregroundStyle(r.result == "done" ? .green : .secondary)
                                    .help(r.error ?? "")
                            }
                        }
                        Spacer(minLength: 8)
                        if let o = offer(r) {
                            Text(Activity.bytes(o.bytes)).font(.caption).foregroundStyle(.secondary).monospacedDigit()
                            Button("In ALT löschen …") { trash([o]) }.controlSize(.small)
                        }
                    }
                }
                .frame(minHeight: 160, maxHeight: 320)
            }
            if !report.offers.isEmpty {
                HStack(spacing: 8) {
                    Button(chosen.count == report.offers.count ? "Keine auswählen" : "Alle auswählen") {
                        chosen = chosen.count == report.offers.count ? [] : Set(report.offers.map(\.id))
                    }
                    Text("ALT kann nach Prüfung in den Papierkorb")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Ausgewählte in ALT löschen …") { trash(report.offers.filter { chosen.contains($0.id) }) }
                        .disabled(chosen.isEmpty || model.applying)
                }
            }
            if report.error == nil && report.results.contains(where: { $0.result != "cancelled" }) {
                Text("Was in den Papierkorb kam, lässt sich dort wiederherstellen. Der Vergleich wird neu berechnet.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Spacer()
                Button("OK", action: close).keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 680)
    }

    private func offer(_ r: ApplyResult) -> TrashOffer? { report.offers.first { $0.id == r.id } }

    private func trash(_ offers: [TrashOffer]) {
        guard !offers.isEmpty else { return }
        model.trashOld(offers)
    }

    /// Below the path: reason, or what the done action means.
    private func note(_ r: ApplyResult) -> String? {
        if r.op == .trashOld {
            switch r.result {
            case "done": return "✓ Prüfsumme identisch – im Papierkorb"
            case "skipped": return "übersprungen: " + ReconcileModel.reason(r.error ?? "")
            default: return r.error
            }
        }
        if r.result == "done", offer(r) != nil { return "in ALT und NEU vorhanden" }
        if r.cloudUnavailable { return "\(model.cloudName) liefert nicht" }
        return r.error
    }

    /// „3 erledigt (12,7 MB kopiert) · 1 übersprungen · 6 abgebrochen“, zeros left out.
    private var counts: String {
        var parts = ["\(report.done) erledigt" + (report.bytes > 0 ? " (\(Activity.bytes(report.bytes)) kopiert)" : "")]
        if report.skipped > 0 { parts.append("\(report.skipped) übersprungen") }
        if report.failed > 0 { parts.append("\(report.failed) fehlgeschlagen") }
        if report.cancelled > 0 { parts.append("\(report.cancelled) abgebrochen") }
        return parts.joined(separator: " · ")
    }

    private func rank(_ r: ApplyResult) -> Int {
        switch r.result {
        case "failed": return 0
        case "skipped": return 1
        case "cancelled": return 2
        default: return 3
        }
    }

    private func symbol(_ r: ApplyResult) -> String {
        switch r.result {
        case "done": return "checkmark.circle.fill"
        case "skipped": return "forward.circle.fill"
        case "cancelled": return "stop.circle.fill"
        default: return "xmark.octagon.fill"
        }
    }

    private func color(_ r: ApplyResult) -> Color {
        switch r.result {
        case "done": return .green
        case "skipped": return .orange
        case "cancelled": return .secondary
        default: return .red
        }
    }
}
