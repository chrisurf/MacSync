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
            ReconcileReportView(report: r) { model.report = nil }
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
            planBar.padding(10)
        }
        .overlay {
            if let a = model.applying {
                ZStack {
                    Color.black.opacity(0.15)
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Plan wird ausgeführt …").font(.headline)
                        ProgressView(value: Double(a.finished), total: Double(max(a.total, 1)))
                        Text("\(a.finished) von \(a.total)" + (a.current.isEmpty ? "" : " · \(a.current)"))
                            .font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                    }
                    .padding(20)
                    .frame(width: 420)
                    .background(RoundedRectangle(cornerRadius: 12).fill(.regularMaterial))
                }
            }
        }
    }

    private var topBar: some View {
        HStack(spacing: 10) {
            Button { model.backToStart() } label: { Label("Andere Ordner …", systemImage: "chevron.left") }
            Spacer(minLength: 8)
            side("ALT", model.comparison?.old ?? model.oldPath, files: model.comparison?.oldFiles, bytes: model.comparison?.oldBytes,
                 last: model.comparison?.order.oldLastMs)
            Button {
                model.swap()
                model.compare()
            } label: { Image(systemName: "arrow.left.arrow.right") }
                .help("Seiten tauschen und neu vergleichen")
                .disabled(model.comparing || model.applying != nil)
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
                .disabled(model.comparing || model.applying != nil)
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
            Button("Vorschläge") { model.suggest() }
                .help("Nur in ALT → kopieren, in ALT neuer → ALT übernehmen; Unklares bleibt deine Entscheidung")
                .disabled(model.comparison == nil)
            Button("Plan leeren") { model.clearPlan() }.disabled(model.plan.isEmpty)
            Button("Ausführen …") { model.execute() }
                .buttonStyle(.borderedProminent)
                .disabled(model.plan.isEmpty || model.applying != nil || model.comparing)
        }
    }
}

// MARK: - tree table

private struct ReconcileTable: View {
    @ObservedObject var model: ReconcileModel

    var body: some View {
        Table(model.rows, selection: $model.selection) {
            TableColumn("ALT") { n in NameCell(model: model, node: n, old: true) }
                .width(min: 160, ideal: 250)
            TableColumn("Größe") { n in size(n.inOld, n.oldBytes) }
                .width(min: 56, ideal: 66)
            TableColumn("Geändert") { n in Text(n.inOld ? date(n.oldMs) : "").foregroundStyle(.secondary) }
                .width(min: 90, ideal: 118)
            TableColumn("") { n in StateCell(model: model, node: n) }
                .width(min: 80, ideal: 100)
            TableColumn("NEU") { n in NameCell(model: model, node: n, old: false) }
                .width(min: 160, ideal: 250)
            TableColumn("Größe") { n in size(n.inNew, n.newBytes) }
                .width(min: 56, ideal: 66)
            TableColumn("Geändert") { n in Text(n.inNew ? date(n.newMs) : "").foregroundStyle(.secondary) }
                .width(min: 90, ideal: 118)
        }
        .contextMenu(forSelectionType: Int.self) { ids in
            ReconcileActions(model: model, targets: ids.sorted().map { model.nodes[$0] }, menu: true)
        } primaryAction: { ids in
            guard ids.count == 1, let id = ids.first else { return }
            let n = model.nodes[id]
            if n.isDir { model.toggle(n) } else { model.showDiff(n) }
        }
    }

    private func size(_ present: Bool, _ b: Int64?) -> some View {
        Text(present ? b.map { Activity.bytes($0) } ?? "" : "").foregroundStyle(.secondary).monospacedDigit()
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
            if let p = model.plan[node.id] {
                // Same place: the name is enough; elsewhere the whole target.
                Text("→ " + (p.to == node.path ? node.name : p.to))
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
            if let p = model.plan[node.id] {
                Text(p.op.short)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Color.accentColor)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 1)
                    .background(Capsule().fill(Color.accentColor.opacity(0.15)))
            } else if model.covered(node) {
                Text("mit Ordner").font(.caption).foregroundStyle(Color.accentColor)
            } else if node.isDir && node.open > 0 && node.folderState != .onlyOld {
                Text("\(node.open) offen").font(.caption).foregroundStyle(.secondary)
            }
        }
        .help(node.stateTitle)
    }
}

/// Plan actions for the selected rows (context menu or detail pane).
private struct ReconcileActions: View {
    @ObservedObject var model: ReconcileModel
    let targets: [RNode]
    let menu: Bool

    var body: some View {
        let ops = [PlanOp.copy, .move, .replace, .keepBoth].compactMap { op -> (PlanOp, [RNode])? in
            let ns = targets.filter { model.ops(for: $0).contains(op) }
            return ns.isEmpty ? nil : (op, ns)
        }
        let planned = targets.filter { model.plan[$0.id] != nil }
        let keepNew = targets.contains { $0.fileState == .oldNewer || $0.fileState == .unclear }
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

// MARK: - result of a plan

private struct ReconcileReportView: View {
    let report: ApplyReport
    let close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(report.error == nil ? "Plan ausgeführt" : "Plan nicht ausgeführt").font(.title3.weight(.semibold))
            if let e = report.error {
                Label(e, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
            } else {
                Text("\(report.done) erledigt · \(report.skipped) übersprungen · \(report.failed) fehlgeschlagen · \(Activity.bytes(report.bytes))")
                    .foregroundStyle(.secondary)
            }
            if !report.results.isEmpty {
                List(report.results.sorted { (rank($0), $0.id) < (rank($1), $1.id) }) { r in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: symbol(r)).foregroundStyle(color(r))
                        VStack(alignment: .leading, spacing: 1) {
                            Text(r.to.isEmpty || r.to == r.path ? r.path : "\(r.path) → \(r.to)").lineLimit(1).truncationMode(.middle)
                            if let e = r.error { Text(e).font(.caption).foregroundStyle(.secondary) }
                        }
                    }
                }
                .frame(minHeight: 160, maxHeight: 320)
            }
            if report.error == nil {
                Text("Ersetzte und verschobene Dateien liegen im Papierkorb. Der Vergleich wird neu berechnet.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Spacer()
                Button("OK", action: close).keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 560)
    }

    private func rank(_ r: ApplyResult) -> Int { r.result == "failed" ? 0 : r.result == "skipped" ? 1 : 2 }

    private func symbol(_ r: ApplyResult) -> String {
        switch r.result {
        case "done": return "checkmark.circle.fill"
        case "skipped": return "forward.circle.fill"
        default: return "xmark.octagon.fill"
        }
    }

    private func color(_ r: ApplyResult) -> Color {
        switch r.result {
        case "done": return .green
        case "skipped": return .orange
        default: return .red
        }
    }
}
