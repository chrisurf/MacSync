import AppKit
import CoreText
import ImageIO
import PDFKit
import SwiftUI
import UniformTypeIdentifiers

// File comparison window of the folder reconcile (docs/reconcile.md): text
// as a line diff in the style of VS Code's diff editor (side by side or
// inline), images and PDFs side by side, everything else as info panels.

/// What a file can be compared as.
enum FileCompare {
    enum Kind { case text, image, pdf, other }

    static func kind(of url: URL) -> Kind {
        var isDir: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), !isDir.boolValue else { return .other }
        let type = (try? url.resourceValues(forKeys: [.contentTypeKey]))?.contentType
            ?? UTType(filenameExtension: url.pathExtension)
        if let type {
            if type.conforms(to: .pdf) { return .pdf }
            // SVG is XML: a line diff says more than two renderings.
            if type.conforms(to: .image), !type.conforms(to: .svg) {
                return NSImage.imageTypes.contains(type.identifier) ? .image : .other
            }
        }
        // Text types are verified too: a "text" file with NUL bytes is binary.
        return looksLikeText(url) ? .text : .other
    }

    /// Number of lines of a text file, `nil` for other kinds. Blocking.
    static func lineCount(_ url: URL) -> Int? {
        guard kind(of: url) == .text, let data = try? Data(contentsOf: url, options: .mappedIfSafe) else { return nil }
        var n = 0
        data.withUnsafeBytes { raw in
            for b in raw.bindMemory(to: UInt8.self) where b == 10 { n += 1 }
        }
        if let last = data.last, last != 10 { n += 1 }
        return n
    }

    /// Lines added/removed from `old` to `new`, `nil` unless both are text. Blocking.
    static func lineDelta(old: URL, new: URL) -> (added: Int, removed: Int)? {
        guard kind(of: old) == .text, kind(of: new) == .text,
              let a = readLines(old), let b = readLines(new) else { return nil }
        let d = TextDiff(old: a, new: b, ignoreWhitespace: false)
        return (d.added, d.removed)
    }

    /// Files above this size are not diffed line by line.
    static let maxTextSize: Int64 = 40 << 20

    /// No NUL in the first 8 KB and valid UTF-8, or few control characters (Latin-1).
    static func looksLikeText(_ url: URL) -> Bool {
        guard let h = try? FileHandle(forReadingFrom: url) else { return false }
        defer { try? h.close() }
        guard let data = try? h.read(upToCount: 8192), !data.isEmpty else { return true }
        if data.contains(0) { return false }
        // The sample may end inside a multi-byte character.
        for cut in 0...min(3, data.count - 1) where String(data: data.dropLast(cut), encoding: .utf8) != nil {
            if cut == 0 || data.count == 8192 { return true }
        }
        let control = data.reduce(0) { $0 + (($1 < 0x20 && $1 != 9 && $1 != 10 && $1 != 12 && $1 != 13) || $1 == 0x7F ? 1 : 0) }
        return control * 100 < data.count
    }

    /// Lines without terminators (LF or CRLF); UTF-8, else Windows-1252/Latin-1.
    static func readLines(_ url: URL) -> [String]? {
        guard let data = try? Data(contentsOf: url, options: .mappedIfSafe) else { return nil }
        guard var text = String(data: data, encoding: .utf8)
            ?? String(data: data, encoding: .windowsCP1252)
            ?? String(data: data, encoding: .isoLatin1) else { return nil }
        if text.hasPrefix("\u{FEFF}") { text.removeFirst() }
        if text.isEmpty { return [] }
        var lines = text.utf8.split(separator: 10, omittingEmptySubsequences: false).map { slice -> String in
            var s = Substring(slice)
            if s.utf8.last == 13 { s = Substring(s.utf8.dropLast()) }
            return String(s)
        }
        if lines.count > 1, lines.last?.isEmpty == true { lines.removeLast() }
        return lines
    }

    /// Test and screenshot hook, called once at launch:
    /// MACSYNC_COMPARE="OLD|NEW" (one side may be empty) opens the window,
    /// MACSYNC_SNAPSHOT=/path.png renders it. For screenshots also:
    /// MACSYNC_COMPARE_MODE=inline, MACSYNC_COMPARE_NEXT=n (jump n changes),
    /// MACSYNC_COMPARE_FOLD=0, MACSYNC_COMPARE_WS=1, MACSYNC_COMPARE_DARK=1/0.
    @MainActor
    static func launchHookIfRequested() {
        let env = ProcessInfo.processInfo.environment
        guard let spec = env["MACSYNC_COMPARE"] else { return }
        let parts = spec.split(separator: "|", omittingEmptySubsequences: false).map(String.init)
        let url = { (s: String?) -> URL? in s.flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0) } }
        let old = url(parts.first), new = url(parts.count > 1 ? parts[1] : nil)
        if let dark = env["MACSYNC_COMPARE_DARK"] { NSApp.appearance = NSAppearance(named: dark == "1" ? .darkAqua : .aqua) }
        if let m = env["MACSYNC_COMPARE_MODE"] { UserDefaults.standard.set(m == "inline" ? "inline" : "side", forKey: TextCompareModel.modeKey) }
        let win = FileCompareWindow.shared
        win.show(old: old, new: new, title: (new ?? old)?.lastPathComponent ?? "Vergleich")
        if let m = win.textModel {
            if env["MACSYNC_COMPARE_FOLD"] == "0" { m.foldUnchanged = false }
            if env["MACSYNC_COMPARE_WS"] == "1" { m.ignoreWhitespace = true }
        }
        let jumps = Int(env["MACSYNC_COMPARE_NEXT"] ?? "") ?? 0
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
            for _ in 0..<jumps { win.textModel?.next() }
        }
        if let path = env["MACSYNC_SNAPSHOT"] {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.2) {
                guard let frame = win.window?.contentView?.superview else { return }
                ManagerWindow.snapshot(frame, to: URL(fileURLWithPath: path))
            }
        }
    }
}

// MARK: - Diff engine

/// Line diff: common prefix/suffix, Myers (bounded), patience anchors as
/// fallback for large distant regions; unanchored rest counts as replaced.
enum DiffEngine {
    struct Match { var a: Int, b: Int, count: Int }
    struct Segment { var a: Int, aCount: Int, b: Int, bCount: Int, equal: Bool }

    static func segments(_ a: [Int32], _ b: [Int32]) -> [Segment] {
        var ms: [Match] = []
        solve(a, 0, a.count, b, 0, b.count, &ms)
        return segments(matches: ms, n: a.count, m: b.count)
    }

    static func segments(matches: [Match], n: Int, m: Int) -> [Segment] {
        var out: [Segment] = []
        var x = 0, y = 0
        for mt in matches where mt.count > 0 {
            if mt.a > x || mt.b > y { out.append(Segment(a: x, aCount: mt.a - x, b: y, bCount: mt.b - y, equal: false)) }
            if let last = out.last, last.equal, last.a + last.aCount == mt.a, last.b + last.bCount == mt.b {
                out[out.count - 1].aCount += mt.count
                out[out.count - 1].bCount += mt.count
            } else {
                out.append(Segment(a: mt.a, aCount: mt.count, b: mt.b, bCount: mt.count, equal: true))
            }
            x = mt.a + mt.count
            y = mt.b + mt.count
        }
        if x < n || y < m { out.append(Segment(a: x, aCount: n - x, b: y, bCount: m - y, equal: false)) }
        return out
    }

    private static func solve(_ a: [Int32], _ aLo0: Int, _ aHi0: Int, _ b: [Int32], _ bLo0: Int, _ bHi0: Int, _ out: inout [Match]) {
        var aLo = aLo0, aHi = aHi0, bLo = bLo0, bHi = bHi0
        while aLo < aHi, bLo < bHi, a[aLo] == b[bLo] { aLo += 1; bLo += 1 }
        if aLo > aLo0 { out.append(Match(a: aLo0, b: bLo0, count: aLo - aLo0)) }
        while aLo < aHi, bLo < bHi, a[aHi - 1] == b[bHi - 1] { aHi -= 1; bHi -= 1 }
        if aLo < aHi, bLo < bHi {
            let size = aHi - aLo + bHi - bLo
            // Time ~ size·D, memory ~ D²: bounded, the rest goes via anchors.
            let maxD = max(64, min(2000, 40_000_000 / size))
            if let ms = myers(a, aLo, aHi, b, bLo, bHi, maxD: maxD) {
                out += ms
            } else {
                patience(a, aLo, aHi, b, bLo, bHi, &out)
            }
        }
        if aHi < aHi0 { out.append(Match(a: aHi, b: bHi, count: aHi0 - aHi)) }
    }

    /// Splits at lines occurring exactly once on both sides (longest increasing run).
    private static func patience(_ a: [Int32], _ aLo: Int, _ aHi: Int, _ b: [Int32], _ bLo: Int, _ bHi: Int, _ out: inout [Match]) {
        var countA: [Int32: Int] = [:], countB: [Int32: Int] = [:], posB: [Int32: Int] = [:]
        for i in aLo..<aHi { countA[a[i], default: 0] += 1 }
        for j in bLo..<bHi { countB[b[j], default: 0] += 1; posB[b[j]] = j }
        var cand: [(a: Int, b: Int)] = []
        for i in aLo..<aHi where countA[a[i]] == 1 && countB[a[i]] == 1 { cand.append((i, posB[a[i]]!)) }
        guard !cand.isEmpty else { return }
        // Longest increasing subsequence on b (patience sorting).
        var tails: [Int] = [], prev = [Int](repeating: -1, count: cand.count)
        for (i, c) in cand.enumerated() {
            var lo = 0, hi = tails.count
            while lo < hi { let mid = (lo + hi) / 2; if cand[tails[mid]].b < c.b { lo = mid + 1 } else { hi = mid } }
            if lo > 0 { prev[i] = tails[lo - 1] }
            if lo == tails.count { tails.append(i) } else { tails[lo] = i }
        }
        var chain: [Int] = []
        var k = tails.last ?? -1
        while k >= 0 { chain.append(k); k = prev[k] }
        var x = aLo, y = bLo
        for idx in chain.reversed() {
            let c = cand[idx]
            solve(a, x, c.a, b, y, c.b, &out)
            out.append(Match(a: c.a, b: c.b, count: 1))
            x = c.a + 1
            y = c.b + 1
        }
        solve(a, x, aHi, b, y, bHi, &out)
    }

    /// Myers' O((N+M)·D) diff; `nil` if the distance exceeds `maxD`.
    static func myers(_ a: [Int32], _ aLo: Int, _ aHi: Int, _ b: [Int32], _ bLo: Int, _ bHi: Int, maxD: Int) -> [Match]? {
        let n = aHi - aLo, m = bHi - bLo
        let limit = min(n + m, maxD)
        let off = limit + 1
        var v = [Int](repeating: 0, count: 2 * limit + 3)
        // trace[d*d + k + d] = furthest x on diagonal k after step d.
        var trace: [Int] = []
        var found = -1
        a.withUnsafeBufferPointer { pa in
            b.withUnsafeBufferPointer { pb in
                outer: for d in 0...limit {
                    var k = -d
                    while k <= d {
                        var x = (k == -d || (k != d && v[off + k - 1] < v[off + k + 1])) ? v[off + k + 1] : v[off + k - 1] + 1
                        var y = x - k
                        while x < n, y < m, pa[aLo + x] == pb[bLo + y] { x += 1; y += 1 }
                        v[off + k] = x
                        if x >= n, y >= m {
                            trace.append(contentsOf: v[(off - d)...(off + d)])
                            found = d
                            break outer
                        }
                        k += 2
                    }
                    trace.append(contentsOf: v[(off - d)...(off + d)])
                }
            }
        }
        guard found >= 0 else { return nil }
        var out: [Match] = []
        var x = n, y = m
        var d = found
        while d > 0 {
            let k = x - y
            let pv = { (kk: Int) in trace[(d - 1) * (d - 1) + kk + d - 1] }
            let prevK = (k == -d || (k != d && pv(k - 1) < pv(k + 1))) ? k + 1 : k - 1
            let prevX = pv(prevK), prevY = prevX - prevK
            let midX = prevK == k + 1 ? prevX : prevX + 1
            if x > midX { out.append(Match(a: aLo + midX, b: bLo + midX - k, count: x - midX)) }
            x = prevX
            y = prevY
            d -= 1
        }
        if x > 0 { out.append(Match(a: aLo, b: bLo, count: x)) }
        return out.reversed()
    }

    /// Changed UTF-16 ranges within a pair of modified lines; none if the lines
    /// have too little in common (the line background says enough then).
    static func charRanges(_ s: String, _ t: String) -> (old: [NSRange], new: [NSRange]) {
        let a = s.utf16.map { Int32($0) }, b = t.utf16.map { Int32($0) }
        let whole: ([NSRange], [NSRange]) = ([], [])
        guard a.count + b.count < 6000, let ms = myers(a, 0, a.count, b, 0, b.count, maxD: 300) else { return whole }
        var segs = segments(matches: ms, n: a.count, m: b.count)
        // Short equal islands between changes make the highlight noisy; merge them
        // unless the changes are pure insertions or deletions (`x+y` → `x + y`).
        for i in segs.indices where segs[i].equal && segs[i].aCount <= 2 && i > 0 && i < segs.count - 1 {
            let p = segs[i - 1], n = segs[i + 1]
            if p.aCount + n.aCount > 0, p.bCount + n.bCount > 0 { segs[i].equal = false }
        }
        let common = segs.reduce(0) { $0 + ($1.equal ? $1.aCount : 0) }
        let longest = max(a.count, b.count)
        if longest > 8, common * 10 < longest * 3 { return whole }
        var old: [NSRange] = [], new: [NSRange] = []
        for sg in segs where !sg.equal {
            if sg.aCount > 0 {
                if let l = old.last, l.location + l.length == sg.a { old[old.count - 1].length += sg.aCount }
                else { old.append(NSRange(location: sg.a, length: sg.aCount)) }
            }
            if sg.bCount > 0 {
                if let l = new.last, l.location + l.length == sg.b { new[new.count - 1].length += sg.bCount }
                else { new.append(NSRange(location: sg.b, length: sg.bCount)) }
            }
        }
        return (old, new)
    }
}

/// Result of diffing two text files.
struct TextDiff {
    /// Identity of this result (caches depend on the line pairing).
    let id = UUID()
    let old: [String]
    let new: [String]
    let segments: [DiffEngine.Segment]
    let added: Int
    let removed: Int
    /// Paired line on the other side within a change (for intra-line highlights), or -1.
    let pairOfOld: [Int32]
    let pairOfNew: [Int32]
    let maxColumnsOld: Int
    let maxColumnsNew: Int

    var hunkCount: Int { segments.reduce(0) { $0 + ($1.equal ? 0 : 1) } }

    init(old: [String], new: [String], ignoreWhitespace: Bool) {
        self.old = old
        self.new = new
        var ids: [String: Int32] = [:]
        ids.reserveCapacity(old.count + new.count)
        func intern(_ s: String) -> Int32 {
            // Like `git diff -w`: whitespace does not count at all.
            let key = ignoreWhitespace ? String(s.unicodeScalars.filter { !CharacterSet.whitespaces.contains($0) }) : s
            if let id = ids[key] { return id }
            let id = Int32(ids.count)
            ids[key] = id
            return id
        }
        let segs = DiffEngine.segments(old.map(intern), new.map(intern))
        segments = segs
        var po = [Int32](repeating: -1, count: old.count), pn = [Int32](repeating: -1, count: new.count)
        var add = 0, rem = 0
        for s in segs where !s.equal {
            add += s.bCount
            rem += s.aCount
            for i in 0..<min(s.aCount, s.bCount) {
                po[s.a + i] = Int32(s.b + i)
                pn[s.b + i] = Int32(s.a + i)
            }
        }
        added = add
        removed = rem
        pairOfOld = po
        pairOfNew = pn
        let columns = { (ls: [String]) in ls.reduce(0) { m, l in max(m, min(l.utf16.count, TextCanvas.maxDisplay) + l.utf8.reduce(0) { $0 + ($1 == 9 ? 3 : 0) }) } }
        maxColumnsOld = columns(old)
        maxColumnsNew = columns(new)
    }
}

/// One visual row of the diff.
struct DiffRow {
    enum Kind: UInt8 { case context, change, fold }
    var kind: Kind
    /// Line index in old/new, -1 for none (filler in side-by-side).
    var left: Int32
    var right: Int32
    /// Change: hunk number. Fold: segment index.
    var aux: Int32
}

/// Mark in the overview ruler: rows `start..<start+count`, lane 0 removed, 1 added.
struct RulerMark { var start: Int, count: Int, lane: Int }

// MARK: - Model

@MainActor
final class TextCompareModel: ObservableObject {
    enum Mode: String { case side, inline }
    static let modeKey = "FileCompareMode"

    let oldURL: URL?
    let newURL: URL?
    /// The file exists on one side only: 0 = missing in ALT, 1 = missing in NEU, -1 = both there.
    var missing: Int { oldURL == nil ? 0 : (newURL == nil ? 1 : -1) }
    @Published var mode: Mode {
        didSet { UserDefaults.standard.set(mode.rawValue, forKey: Self.modeKey); rebuild() }
    }
    @Published var ignoreWhitespace = false { didSet { if ignoreWhitespace != oldValue { compute() } } }
    @Published var foldUnchanged = true { didSet { expanded = []; rebuild() } }
    @Published private(set) var diff: TextDiff?
    @Published private(set) var loading = true
    @Published private(set) var failure: String?
    @Published private(set) var current = -1

    private(set) var rows: [DiffRow] = []
    private(set) var hunkRows: [Int] = []
    private(set) var marks: [RulerMark] = []
    private var expanded: Set<Int> = []
    private var generation = 0
    weak var view: DiffContainerView? { didSet { push(scrollTop: true) } }

    init(old: URL?, new: URL?) {
        oldURL = old
        newURL = new
        mode = Mode(rawValue: UserDefaults.standard.string(forKey: Self.modeKey) ?? "") ?? .side
        compute()
    }

    func compute() {
        generation += 1
        let gen = generation
        loading = true
        let old = oldURL, new = newURL, ws = ignoreWhitespace
        Task {
            let result = await Task.detached(priority: .userInitiated) { () -> (TextDiff?, Double) in
                let t0 = Date()
                let a = old.map { FileCompare.readLines($0) } ?? [], b = new.map { FileCompare.readLines($0) } ?? []
                guard let a, let b else { return (nil, 0) }
                let d = TextDiff(old: a, new: b, ignoreWhitespace: ws)
                return (d, Date().timeIntervalSince(t0))
            }.value
            guard gen == generation else { return }
            if ProcessInfo.processInfo.environment["MACSYNC_COMPARE"] != nil, let d = result.0 {
                FileHandle.standardError.write(Data(String(format: "FileCompare: %d/%d lines, %d hunks, diff %.0f ms\n",
                                                           d.old.count, d.new.count, d.hunkCount, result.1 * 1000).utf8))
            }
            loading = false
            if let d = result.0 { diff = d } else { failure = "Datei konnte nicht gelesen werden." }
            current = -1
            expanded = []
            rebuild(scrollTop: true)
        }
    }

    private func rebuild(scrollTop: Bool = false) {
        guard let d = diff else { rows = []; hunkRows = []; marks = []; push(scrollTop: scrollTop); return }
        let side = mode == .side
        var rows: [DiffRow] = [], hunks: [Int] = [], marks: [RulerMark] = []
        rows.reserveCapacity(max(d.old.count, d.new.count) + 16)
        let fold = foldUnchanged
        if missing >= 0 {
            // Nothing to compare: the existing file as it is, the other pane hatched.
            for i in 0..<(missing == 0 ? d.new.count : d.old.count) {
                rows.append(DiffRow(kind: .context, left: missing == 0 ? -1 : Int32(i), right: missing == 0 ? Int32(i) : -1, aux: -1))
            }
            self.rows = rows
            hunkRows = []
            self.marks = []
            push(scrollTop: scrollTop)
            return
        }
        let context = 3
        for (si, s) in d.segments.enumerated() {
            if s.equal {
                let head = si == 0 ? 0 : context, tail = si == d.segments.count - 1 ? 0 : context
                let emit = { (r: Range<Int>) in
                    for i in r { rows.append(DiffRow(kind: .context, left: Int32(s.a + i), right: Int32(s.b + i), aux: -1)) }
                }
                if fold, !expanded.contains(si), s.aCount > head + tail + 3 {
                    emit(0..<head)
                    rows.append(DiffRow(kind: .fold, left: -1, right: -1, aux: Int32(si)))
                    emit(s.aCount - tail..<s.aCount)
                } else {
                    emit(0..<s.aCount)
                }
            } else {
                let h = Int32(hunks.count)
                hunks.append(rows.count)
                if side {
                    if s.aCount > 0 { marks.append(RulerMark(start: rows.count, count: s.aCount, lane: 0)) }
                    if s.bCount > 0 { marks.append(RulerMark(start: rows.count, count: s.bCount, lane: 1)) }
                    for i in 0..<max(s.aCount, s.bCount) {
                        rows.append(DiffRow(kind: .change, left: i < s.aCount ? Int32(s.a + i) : -1,
                                            right: i < s.bCount ? Int32(s.b + i) : -1, aux: h))
                    }
                } else {
                    if s.aCount > 0 { marks.append(RulerMark(start: rows.count, count: s.aCount, lane: 0)) }
                    for i in 0..<s.aCount { rows.append(DiffRow(kind: .change, left: Int32(s.a + i), right: -1, aux: h)) }
                    if s.bCount > 0 { marks.append(RulerMark(start: rows.count, count: s.bCount, lane: 1)) }
                    for i in 0..<s.bCount { rows.append(DiffRow(kind: .change, left: -1, right: Int32(s.b + i), aux: h)) }
                }
            }
        }
        self.rows = rows
        hunkRows = hunks
        self.marks = marks
        if current >= hunks.count { current = hunks.count - 1 }
        push(scrollTop: scrollTop)
        if !scrollTop, current >= 0 { view?.scroll(toRow: hunkRows[current]) }
    }

    private func push(scrollTop: Bool) {
        view?.apply(model: self)
        if scrollTop { view?.scrollToTop() }
    }

    func expand(segment: Int) {
        expanded.insert(segment)
        let top = view?.topRow ?? 0
        let foldRow = rows.firstIndex { $0.kind == .fold && $0.aux == Int32(segment) } ?? 0
        rebuild()
        // Keep the rows above the fold where they were.
        if foldRow >= top { view?.scrollTo(row: top) }
    }

    func next() { go(current + 1) }
    func previous() { go(current < 0 ? hunkRows.count - 1 : current - 1) }

    private func go(_ i: Int) {
        guard !hunkRows.isEmpty else { return }
        current = min(max(i, 0), hunkRows.count - 1)
        view?.apply(model: self)
        view?.scroll(toRow: hunkRows[current])
    }
}

// MARK: - Text canvas (draws only the visible rows)

private extension NSColor {
    static func dynamic(light: NSColor, dark: NSColor) -> NSColor {
        NSColor(name: nil) { $0.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light }
    }
    static func rgba(_ r: Int, _ g: Int, _ b: Int, _ a: CGFloat) -> NSColor {
        NSColor(srgbRed: CGFloat(r) / 255, green: CGFloat(g) / 255, blue: CGFloat(b) / 255, alpha: a)
    }
}

/// Colors after VS Code's diff editor (light and dark).
private enum DiffColors {
    static let background = NSColor.dynamic(light: .white, dark: .rgba(30, 30, 30, 1))
    static let text = NSColor.dynamic(light: .rgba(36, 36, 36, 1), dark: .rgba(212, 212, 212, 1))
    static let lineNumber = NSColor.dynamic(light: .rgba(35, 120, 147, 0.75), dark: .rgba(133, 133, 133, 1))
    static let removedLine = NSColor.dynamic(light: .rgba(255, 0, 0, 0.10), dark: .rgba(255, 0, 0, 0.13))
    static let removedText = NSColor.dynamic(light: .rgba(255, 0, 0, 0.20), dark: .rgba(255, 0, 0, 0.28))
    static let removedGutter = NSColor.dynamic(light: .rgba(255, 0, 0, 0.14), dark: .rgba(255, 0, 0, 0.18))
    static let addedLine = NSColor.dynamic(light: .rgba(155, 185, 85, 0.22), dark: .rgba(155, 185, 85, 0.16))
    static let addedText = NSColor.dynamic(light: .rgba(156, 204, 44, 0.35), dark: .rgba(156, 204, 44, 0.27))
    static let addedGutter = NSColor.dynamic(light: .rgba(155, 185, 85, 0.30), dark: .rgba(155, 185, 85, 0.22))
    static let removedMark = NSColor.dynamic(light: .rgba(220, 40, 40, 1), dark: .rgba(240, 90, 90, 1))
    static let addedMark = NSColor.dynamic(light: .rgba(70, 140, 30, 1), dark: .rgba(140, 200, 70, 1))
    static let hatch = NSColor.dynamic(light: .rgba(0, 0, 0, 0.09), dark: .rgba(255, 255, 255, 0.09))
    static let fold = NSColor.dynamic(light: .rgba(0, 110, 200, 0.06), dark: .rgba(80, 160, 255, 0.08))
    static let foldText = NSColor.dynamic(light: .rgba(0, 95, 184, 1), dark: .rgba(110, 170, 240, 1))
    static let divider = NSColor.dynamic(light: .rgba(0, 0, 0, 0.12), dark: .rgba(255, 255, 255, 0.10))
    static let ruler = NSColor.dynamic(light: .rgba(0, 0, 0, 0.03), dark: .rgba(255, 255, 255, 0.03))
    static let rulerView = NSColor.dynamic(light: .rgba(0, 0, 0, 0.10), dark: .rgba(255, 255, 255, 0.12))
}

final class TextCanvas: NSView {
    static let maxDisplay = 4000
    static let lineHeight: CGFloat = 19

    private(set) var rows: [DiffRow] = []
    private var diff: TextDiff?
    private var sideBySide = true
    private var currentHunk: Int32 = -1
    private var hOffset: [CGFloat] = [0, 0]
    var onExpand: ((Int) -> Void)?

    private let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
    private let smallFont = NSFont.systemFont(ofSize: 11)
    private lazy var charWidth = ("0" as NSString).size(withAttributes: [.font: font]).width
    private lazy var baseline: CGFloat = {
        let h = font.ascender - font.descender
        return ((Self.lineHeight - h) / 2 + font.ascender).rounded()
    }()
    private lazy var paragraph: NSParagraphStyle = {
        let p = NSMutableParagraphStyle()
        p.tabStops = []
        p.defaultTabInterval = charWidth * 4
        return p
    }()
    private var lineCache: [Int: CTLine] = [:]
    private var rangeCache: [Int: [NSRange]] = [:]

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }

    func apply(rows: [DiffRow], diff: TextDiff?, sideBySide: Bool, current: Int32) {
        if diff?.id != self.diff?.id {
            if diff?.old != self.diff?.old || diff?.new != self.diff?.new {
                lineCache = [:]
                hOffset = [0, 0]
            }
            rangeCache = [:]
        }
        if sideBySide != self.sideBySide { hOffset = [0, 0] }
        self.rows = rows
        self.diff = diff
        self.sideBySide = sideBySide
        currentHunk = current
        needsDisplay = true
    }

    // MARK: Geometry

    private var digits: Int { String(max(diff?.old.count ?? 0, diff?.new.count ?? 0, 99)).count }
    private var numberWidth: CGFloat { CGFloat(digits) * charWidth + 18 }
    private let indicatorWidth: CGFloat = 18

    /// x ranges of the panes: side by side two, inline one.
    private var panes: [(x: CGFloat, width: CGFloat)] {
        guard sideBySide else { return [(0, bounds.width)] }
        let half = ((bounds.width - 1) / 2).rounded(.down)
        return [(0, half), (half + 1, bounds.width - half - 1)]
    }

    private func textX(pane p: Int) -> CGFloat {
        panes[p].x + (sideBySide ? numberWidth : 2 * numberWidth) + indicatorWidth
    }

    private func maxOffset(pane p: Int) -> CGFloat {
        guard let d = diff else { return 0 }
        let cols = sideBySide ? (p == 0 ? d.maxColumnsOld : d.maxColumnsNew) : max(d.maxColumnsOld, d.maxColumnsNew)
        let visible = panes[p].x + panes[p].width - textX(pane: p)
        return max(0, CGFloat(cols) * charWidth + 40 - visible)
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        guard let ctx = NSGraphicsContext.current?.cgContext else { return }
        DiffColors.background.setFill()
        dirtyRect.fill()
        let lh = Self.lineHeight
        if !rows.isEmpty {
            let first = max(0, Int(dirtyRect.minY / lh)), last = min(rows.count - 1, Int(dirtyRect.maxY / lh))
            if first <= last {
                for r in first...last { drawRow(r, y: CGFloat(r) * lh, ctx: ctx) }
            }
        }
        if sideBySide {
            DiffColors.divider.setFill()
            NSRect(x: panes[0].width, y: dirtyRect.minY, width: 1, height: dirtyRect.height).fill()
        }
    }

    private func drawRow(_ r: Int, y: CGFloat, ctx: CGContext) {
        let row = rows[r]
        let lh = Self.lineHeight
        if row.kind == .fold {
            drawFold(row, y: y)
            return
        }
        guard let d = diff else { return }
        if sideBySide {
            for p in 0..<2 {
                let line = p == 0 ? row.left : row.right
                let pane = panes[p]
                let rect = NSRect(x: pane.x, y: y, width: pane.width, height: lh)
                if line < 0 {
                    hatch(NSRect(x: pane.x, y: y, width: pane.width, height: lh))
                    continue
                }
                let changed = row.kind == .change
                let removed = p == 0
                if changed {
                    (removed ? DiffColors.removedLine : DiffColors.addedLine).setFill()
                    rect.fill()
                    (removed ? DiffColors.removedGutter : DiffColors.addedGutter).setFill()
                    NSRect(x: pane.x, y: y, width: numberWidth, height: lh).fill()
                }
                drawNumber(Int(line) + 1, right: pane.x + numberWidth - 8, y: y)
                if changed { drawIndicator(removed, x: pane.x + numberWidth, y: y) }
                if changed, row.aux == currentHunk { drawCurrentBar(x: pane.x, y: y) }
                let text = removed ? d.old[Int(line)] : d.new[Int(line)]
                let ranges = changed ? highlights(old: removed, line: Int(line), d) : nil
                drawText(text, key: Int(line) * 2 + p, pane: p, y: y, ranges: ranges,
                         color: removed ? DiffColors.removedText : DiffColors.addedText, ctx: ctx)
            }
        } else {
            let pane = panes[0]
            let rect = NSRect(x: 0, y: y, width: pane.width, height: lh)
            let removed = row.right < 0
            if row.kind == .change {
                (removed ? DiffColors.removedLine : DiffColors.addedLine).setFill()
                rect.fill()
                (removed ? DiffColors.removedGutter : DiffColors.addedGutter).setFill()
                NSRect(x: 0, y: y, width: 2 * numberWidth, height: lh).fill()
                if removed { drawNumber(Int(row.left) + 1, right: numberWidth - 8, y: y) }
                else { drawNumber(Int(row.right) + 1, right: 2 * numberWidth - 8, y: y) }
                drawIndicator(removed, x: 2 * numberWidth, y: y)
                if row.aux == currentHunk { drawCurrentBar(x: 0, y: y) }
                let line = Int(removed ? row.left : row.right)
                drawText(removed ? d.old[line] : d.new[line], key: line * 2 + (removed ? 0 : 1), pane: 0, y: y,
                         ranges: highlights(old: removed, line: line, d),
                         color: removed ? DiffColors.removedText : DiffColors.addedText, ctx: ctx)
            } else {
                if row.left >= 0 { drawNumber(Int(row.left) + 1, right: numberWidth - 8, y: y) }
                if row.right >= 0 { drawNumber(Int(row.right) + 1, right: 2 * numberWidth - 8, y: y) }
                let text = row.right >= 0 ? d.new[Int(row.right)] : d.old[Int(row.left)]
                let key = row.right >= 0 ? Int(row.right) * 2 + 1 : Int(row.left) * 2
                drawText(text, key: key, pane: 0, y: y, ranges: nil, color: .clear, ctx: ctx)
            }
        }
    }

    private func drawFold(_ row: DiffRow, y: CGFloat) {
        let lh = Self.lineHeight
        let rect = NSRect(x: 0, y: y, width: bounds.width, height: lh)
        DiffColors.fold.setFill()
        rect.fill()
        DiffColors.divider.setFill()
        NSRect(x: 0, y: y, width: bounds.width, height: 1).fill()
        NSRect(x: 0, y: y + lh - 1, width: bounds.width, height: 1).fill()
        guard let d = diff else { return }
        let seg = d.segments[Int(row.aux)]
        let si = Int(row.aux)
        let hidden = seg.aCount - (si == 0 ? 0 : 3) - (si == d.segments.count - 1 ? 0 : 3)
        let label = "⋯  \(hidden.formatted()) unveränderte Zeilen – zum Einblenden klicken"
        let attrs: [NSAttributedString.Key: Any] = [.font: smallFont, .foregroundColor: DiffColors.foldText]
        let ty = y + (lh - smallFont.ascender + smallFont.descender) / 2 - 1
        for p in panes.indices {
            (label as NSString).draw(at: NSPoint(x: panes[p].x + (sideBySide ? numberWidth : 2 * numberWidth) + 4, y: ty), withAttributes: attrs)
        }
    }

    private func hatch(_ rect: NSRect) {
        NSGraphicsContext.saveGraphicsState()
        rect.clip()
        let path = NSBezierPath()
        let step: CGFloat = 7
        // Lines x + y = c in absolute coordinates, so stripes run on across rows.
        var c = ((rect.minX + rect.minY) / step).rounded(.down) * step
        while c < rect.maxX + rect.maxY {
            path.move(to: NSPoint(x: c - rect.minY, y: rect.minY))
            path.line(to: NSPoint(x: c - rect.maxY, y: rect.maxY))
            c += step
        }
        path.lineWidth = 1
        DiffColors.hatch.setStroke()
        path.stroke()
        NSGraphicsContext.restoreGraphicsState()
    }

    private func drawNumber(_ n: Int, right: CGFloat, y: CGFloat) {
        let s = String(n) as NSString
        let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: DiffColors.lineNumber]
        s.draw(at: NSPoint(x: right - CGFloat(s.length) * charWidth, y: y + baseline - font.ascender), withAttributes: attrs)
    }

    private func drawIndicator(_ removed: Bool, x: CGFloat, y: CGFloat) {
        let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: removed ? DiffColors.removedMark : DiffColors.addedMark]
        ((removed ? "−" : "+") as NSString).draw(at: NSPoint(x: x + 3, y: y + baseline - font.ascender), withAttributes: attrs)
    }

    private func drawCurrentBar(x: CGFloat, y: CGFloat) {
        NSColor.controlAccentColor.setFill()
        NSRect(x: x, y: y, width: 3, height: Self.lineHeight).fill()
    }

    private func highlights(old: Bool, line: Int, _ d: TextDiff) -> [NSRange]? {
        let other = old ? d.pairOfOld[line] : d.pairOfNew[line]
        guard other >= 0 else { return nil }
        let key = line * 2 + (old ? 0 : 1)
        if let r = rangeCache[key] { return r }
        let (a, b) = old ? (d.old[line], d.new[Int(other)]) : (d.old[Int(other)], d.new[line])
        let res = DiffEngine.charRanges(a, b)
        rangeCache[line * 2 + (old ? 0 : 1)] = old ? res.old : res.new
        rangeCache[Int(other) * 2 + (old ? 1 : 0)] = old ? res.new : res.old
        return old ? res.old : res.new
    }

    private func ctLine(_ s: String, key: Int) -> CTLine {
        if let l = lineCache[key] { return l }
        if lineCache.count > 4000 { lineCache = [:] }
        var text = s
        if text.utf16.count > Self.maxDisplay {
            text = String(text.utf16.prefix(Self.maxDisplay)) ?? String(text.prefix(Self.maxDisplay))
            text += " …"
        }
        let attr = NSAttributedString(string: text, attributes: [
            .font: font,
            .paragraphStyle: paragraph,
            NSAttributedString.Key(kCTForegroundColorFromContextAttributeName as String): true,
        ])
        let line = CTLineCreateWithAttributedString(attr)
        lineCache[key] = line
        return line
    }

    private func drawText(_ s: String, key: Int, pane p: Int, y: CGFloat, ranges: [NSRange]?, color: NSColor, ctx: CGContext) {
        guard !s.isEmpty else { return }
        let x0 = textX(pane: p)
        let clip = NSRect(x: x0, y: y, width: panes[p].x + panes[p].width - x0, height: Self.lineHeight)
        let x = x0 - hOffset[p]
        let line = ctLine(s, key: key)
        NSGraphicsContext.saveGraphicsState()
        clip.clip()
        if let ranges {
            color.setFill()
            for r in ranges where r.length > 0 && r.location < Self.maxDisplay {
                let a = CTLineGetOffsetForStringIndex(line, r.location, nil)
                let b = CTLineGetOffsetForStringIndex(line, min(r.location + r.length, Self.maxDisplay), nil)
                NSRect(x: x + a, y: y, width: max(b - a, 2), height: Self.lineHeight).fill()
            }
        }
        DiffColors.text.setFill()
        ctx.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
        ctx.textPosition = CGPoint(x: x, y: y + baseline)
        CTLineDraw(line, ctx)
        NSGraphicsContext.restoreGraphicsState()
    }

    // MARK: Events

    override func scrollWheel(with event: NSEvent) {
        // Horizontal scrolling moves only the pane under the pointer (inline: the one pane).
        if abs(event.scrollingDeltaX) > abs(event.scrollingDeltaY) {
            let pt = convert(event.locationInWindow, from: nil)
            let p = sideBySide && pt.x > panes[0].width ? 1 : 0
            let dx = event.hasPreciseScrollingDeltas ? event.scrollingDeltaX : event.scrollingDeltaX * charWidth * 3
            hOffset[p] = min(max(0, hOffset[p] - dx), maxOffset(pane: p))
            needsDisplay = true
        } else {
            super.scrollWheel(with: event)
        }
    }

    private func row(at event: NSEvent) -> Int? {
        let pt = convert(event.locationInWindow, from: nil)
        let r = Int(pt.y / Self.lineHeight)
        return rows.indices.contains(r) ? r : nil
    }

    override func mouseDown(with event: NSEvent) {
        if let r = row(at: event), rows[r].kind == .fold { onExpand?(Int(rows[r].aux)) }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect], owner: self))
    }

    override func mouseMoved(with event: NSEvent) {
        if let r = row(at: event), rows[r].kind == .fold { NSCursor.pointingHand.set() } else { NSCursor.arrow.set() }
    }
}

/// Change markers along the right edge; click jumps there.
final class OverviewRuler: NSView {
    weak var container: DiffContainerView?
    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        DiffColors.background.setFill()
        bounds.fill()
        DiffColors.ruler.setFill()
        bounds.fill()
        DiffColors.divider.setFill()
        NSRect(x: 0, y: 0, width: 1, height: bounds.height).fill()
        guard let c = container, let model = c.model else { return }
        // Same scale as the document, so short files map 1:1 onto the visible text.
        let scale = bounds.height / max(1, c.canvas.frame.height)
        let lh = TextCanvas.lineHeight
        let laneW = (bounds.width - 3) / 2
        for m in model.marks {
            (m.lane == 0 ? DiffColors.removedMark : DiffColors.addedMark).withAlphaComponent(0.8).setFill()
            NSRect(x: 2 + CGFloat(m.lane) * laneW, y: CGFloat(m.start) * lh * scale,
                   width: laneW, height: max(2, CGFloat(m.count) * lh * scale)).fill()
        }
        let vis = c.scrollView.contentView.bounds
        let docH = max(1, c.canvas.frame.height)
        DiffColors.rulerView.setFill()
        NSRect(x: 1, y: vis.minY / docH * bounds.height, width: bounds.width - 1, height: max(8, vis.height / docH * bounds.height)).fill()
    }

    override func mouseDown(with event: NSEvent) { jump(event) }
    override func mouseDragged(with event: NSEvent) { jump(event) }

    private func jump(_ event: NSEvent) {
        guard let c = container else { return }
        let y = convert(event.locationInWindow, from: nil).y
        let docH = c.canvas.frame.height, visH = c.scrollView.contentView.bounds.height
        c.scrollTo(y: y / max(1, bounds.height) * docH - visH / 2)
    }
}

final class DiffContainerView: NSView {
    let scrollView = NSScrollView()
    let canvas = TextCanvas()
    let ruler = OverviewRuler()
    private(set) weak var model: TextCompareModel?
    private var observer: NSObjectProtocol?

    override init(frame: NSRect) {
        super.init(frame: frame)
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.drawsBackground = true
        scrollView.backgroundColor = DiffColors.background
        scrollView.documentView = canvas
        canvas.autoresizingMask = [.width]
        scrollView.contentView.postsBoundsChangedNotifications = true
        ruler.container = self
        addSubview(scrollView)
        addSubview(ruler)
        observer = NotificationCenter.default.addObserver(forName: NSView.boundsDidChangeNotification, object: scrollView.contentView,
                                                          queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.ruler.needsDisplay = true }
        }
    }

    required init?(coder: NSCoder) { fatalError() }

    deinit {
        if let observer { NotificationCenter.default.removeObserver(observer) }
    }

    override func layout() {
        super.layout()
        let rw: CGFloat = 14
        scrollView.frame = NSRect(x: 0, y: 0, width: bounds.width - rw, height: bounds.height)
        ruler.frame = NSRect(x: bounds.width - rw, y: 0, width: rw, height: bounds.height)
        sizeCanvas()
    }

    private func sizeCanvas() {
        let content = scrollView.contentSize
        let h = CGFloat(canvas.rows.count) * TextCanvas.lineHeight + TextCanvas.lineHeight
        canvas.frame = NSRect(x: 0, y: 0, width: content.width, height: max(h, content.height))
    }

    func apply(model: TextCompareModel) {
        self.model = model
        canvas.onExpand = { [weak model] s in model?.expand(segment: s) }
        canvas.apply(rows: model.rows, diff: model.diff, sideBySide: model.mode == .side,
                     current: model.current >= 0 ? Int32(model.current) : -1)
        sizeCanvas()
        ruler.needsDisplay = true
    }

    var topRow: Int { Int(scrollView.contentView.bounds.minY / TextCanvas.lineHeight) }

    func scrollTo(y: CGFloat) {
        let maxY = max(0, canvas.frame.height - scrollView.contentView.bounds.height)
        scrollView.contentView.scroll(to: NSPoint(x: 0, y: min(max(0, y), maxY)))
        scrollView.reflectScrolledClipView(scrollView.contentView)
    }

    func scrollTo(row: Int) { scrollTo(y: CGFloat(row) * TextCanvas.lineHeight) }

    /// Puts the row at a third of the visible height, like VS Code's change navigation.
    func scroll(toRow row: Int) {
        scrollTo(y: CGFloat(row) * TextCanvas.lineHeight - scrollView.contentView.bounds.height / 3)
    }

    func scrollToTop() { scrollTo(y: 0) }
}

struct DiffTextView: NSViewRepresentable {
    let model: TextCompareModel

    func makeNSView(context: Context) -> DiffContainerView {
        let v = DiffContainerView()
        model.view = v
        return v
    }

    func updateNSView(_ v: DiffContainerView, context: Context) {
        if model.view !== v { model.view = v }
    }
}

// MARK: - SwiftUI views

/// Metadata of one side; `nil` URL means the file does not exist there.
struct FileMeta {
    let url: URL?
    let size: Int64?
    let modified: Date?

    init(_ url: URL?) {
        let values = url.flatMap { try? $0.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey, .isRegularFileKey]) }
        self.url = values == nil ? nil : url
        size = values?.fileSize.map(Int64.init)
        modified = values?.contentModificationDate
    }

    var exists: Bool { url != nil }
    var name: String { url?.lastPathComponent ?? "Nicht vorhanden" }
    var sizeText: String { size.map { ByteCountFormatter.string(fromByteCount: $0, countStyle: .file) } ?? "–" }
    var dateText: String { modified?.formatted(date: .abbreviated, time: .shortened) ?? "–" }
    var kindText: String {
        guard let url else { return "–" }
        return (try? url.resourceValues(forKeys: [.localizedTypeDescriptionKey]))?.localizedTypeDescription ?? "Datei"
    }
}

private struct SideHeader: View {
    let label: String
    let meta: FileMeta
    let newer: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(label)
                    .font(.caption.weight(.bold))
                    .padding(.horizontal, 5).padding(.vertical, 1)
                    .background(RoundedRectangle(cornerRadius: 4).fill(Color.secondary.opacity(0.18)))
                Text(meta.name)
                    .font(.headline)
                    .foregroundStyle(meta.exists ? .primary : .secondary)
                    .lineLimit(1).truncationMode(.middle)
                if newer {
                    Text("neuer")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 5).padding(.vertical, 1)
                        .background(RoundedRectangle(cornerRadius: 4).fill(Color.green.opacity(0.8)))
                }
                Spacer(minLength: 0)
                if let url = meta.url {
                    Button { NSWorkspace.shared.activateFileViewerSelecting([url]) } label: { Image(systemName: "folder") }
                        .buttonStyle(.borderless)
                        .help("Im Finder zeigen")
                }
            }
            if let url = meta.url {
                Text(url.deletingLastPathComponent().path)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1).truncationMode(.middle)
                    .textSelection(.enabled)
                Text("Geändert \(meta.dateText) · \(meta.sizeText)")
                    .font(.caption)
                    .foregroundStyle(newer ? .primary : .secondary)
            } else {
                Text("Diese Seite hat die Datei nicht.").font(.caption).foregroundStyle(.secondary)
                Text(" ").font(.caption)
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct CompareHeader: View {
    let old: FileMeta
    let new: FileMeta

    var body: some View {
        let o = old.modified ?? .distantPast, n = new.modified ?? .distantPast
        HStack(spacing: 0) {
            SideHeader(label: "ALT", meta: old, newer: old.exists && new.exists && o > n)
            Divider()
            SideHeader(label: "NEU", meta: new, newer: old.exists && new.exists && n > o)
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

private struct TextCompareView: View {
    @ObservedObject var model: TextCompareModel

    var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider()
            ZStack {
                DiffTextView(model: model)
                if model.loading {
                    ProgressView("Vergleiche …").padding(20)
                        .background(RoundedRectangle(cornerRadius: 8).fill(.regularMaterial))
                } else if let f = model.failure {
                    Text(f).foregroundStyle(.secondary)
                } else {
                    hint
                }
            }
        }
        .background(shortcuts)
    }

    /// Badge over the hatched pane (missing side) or the text (empty files).
    @ViewBuilder private var hint: some View {
        let d = model.diff
        // Inline, the toolbar already says which side has the file.
        let text: String? = model.missing >= 0
            ? (model.mode == .side ? "Nicht vorhanden" : nil)
            : (d.map { $0.old.isEmpty && $0.new.isEmpty } == true ? "Beide Dateien sind leer." : nil)
        if let text {
            let badge = Text(text)
                .font(.callout.weight(.medium))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12).padding(.vertical, 5)
                .background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: DiffColors.background)))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color(nsColor: DiffColors.divider)))
                .padding(.top, 24)
            HStack(spacing: 0) {
                if model.mode == .side && model.missing == 1 { Color.clear }
                badge.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                if model.mode == .side && model.missing == 0 { Color.clear }
            }
            .padding(.trailing, 14)
            .frame(maxHeight: .infinity, alignment: .top)
            .allowsHitTesting(false)
        }
    }

    private var toolbar: some View {
        HStack(spacing: 14) {
            Picker("", selection: $model.mode) {
                Text("Nebeneinander").tag(TextCompareModel.Mode.side)
                Text("Untereinander").tag(TextCompareModel.Mode.inline)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .fixedSize()
            if model.missing < 0 {
                Toggle("Leerzeichen ignorieren", isOn: $model.ignoreWhitespace)
                Toggle("Unveränderte einklappen", isOn: $model.foldUnchanged)
            }
            Spacer()
            if let d = model.diff {
                if model.missing >= 0 {
                    let n = max(d.old.count, d.new.count)
                    Text("\(model.missing == 0 ? "Nur in NEU" : "Nur in ALT") · \(n.formatted()) \(n == 1 ? "Zeile" : "Zeilen")")
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(.secondary)
                } else if d.hunkCount == 0 {
                    Label(model.ignoreWhitespace ? "Keine Unterschiede (Leerzeichen ignoriert)" : "Keine Unterschiede",
                          systemImage: "checkmark.circle")
                        .foregroundStyle(.secondary)
                } else {
                    HStack(spacing: 4) {
                        Text("+\(d.added.formatted())").foregroundStyle(Color(nsColor: DiffColors.addedMark))
                        Text("−\(d.removed.formatted())").foregroundStyle(Color(nsColor: DiffColors.removedMark))
                        Text("Zeilen").foregroundStyle(.secondary)
                    }
                    .font(.callout.monospacedDigit().weight(.medium))
                    Text(model.current >= 0 ? "Änderung \(model.current + 1) von \(d.hunkCount)"
                                            : "\(d.hunkCount) \(d.hunkCount == 1 ? "Änderung" : "Änderungen")")
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            HStack(spacing: 2) {
                Button { model.previous() } label: { Image(systemName: "arrow.up") }
                    .help("Vorige Änderung (⌥↑ oder ⇧F7)")
                    .keyboardShortcut(.upArrow, modifiers: .option)
                Button { model.next() } label: { Image(systemName: "arrow.down") }
                    .help("Nächste Änderung (⌥↓ oder F7)")
                    .keyboardShortcut(.downArrow, modifiers: .option)
            }
            .disabled(model.hunkRows.isEmpty)
        }
        .toggleStyle(.checkbox)
        .controlSize(.small)
        .padding(.horizontal, 12).padding(.vertical, 6)
    }

    /// F7 / ⇧F7 as in VS Code; invisible buttons carry the shortcuts.
    private var shortcuts: some View {
        let f7 = KeyEquivalent(Character(UnicodeScalar(UInt16(NSF7FunctionKey))!))
        return ZStack {
            Button("") { model.next() }.keyboardShortcut(f7, modifiers: [])
            Button("") { model.previous() }.keyboardShortcut(f7, modifiers: .shift)
        }
        .opacity(0)
        .frame(width: 0, height: 0)
        .accessibilityHidden(true)
    }
}

private struct MissingPane: View {
    var body: some View {
        VStack(spacing: 6) {
            Image(systemName: "doc.badge.ellipsis").font(.system(size: 34)).foregroundStyle(.tertiary)
            Text("Nicht vorhanden").font(.title3).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct ImagePane: View {
    let meta: FileMeta
    let image: NSImage?
    let pixels: CGSize?

    init(meta: FileMeta) {
        self.meta = meta
        image = meta.url.flatMap { NSImage(contentsOf: $0) }
        pixels = meta.url.flatMap { url in
            guard let src = CGImageSourceCreateWithURL(url as CFURL, nil),
                  let p = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any],
                  let w = p[kCGImagePropertyPixelWidth] as? Int, let h = p[kCGImagePropertyPixelHeight] as? Int else { return nil }
            return CGSize(width: w, height: h)
        }
    }

    var body: some View {
        if !meta.exists {
            MissingPane()
        } else {
            VStack(spacing: 0) {
                ZStack {
                    Checkerboard()
                    if let image {
                        Image(nsImage: image).resizable().interpolation(.high).scaledToFit().padding(16)
                    } else {
                        Text("Bild kann nicht angezeigt werden.").foregroundStyle(.secondary)
                    }
                }
                Divider()
                Text([pixels.map { "\(Int($0.width)) × \(Int($0.height)) Pixel" }, meta.sizeText, meta.kindText]
                    .compactMap { $0 }.joined(separator: " · "))
                    .font(.caption).foregroundStyle(.secondary)
                    .padding(6)
            }
        }
    }
}

private struct Checkerboard: View {
    var body: some View {
        Canvas { ctx, size in
            let s: CGFloat = 10
            for r in 0..<Int(size.height / s + 1) {
                for c in 0..<Int(size.width / s + 1) where (r + c) % 2 == 0 {
                    ctx.fill(Path(CGRect(x: CGFloat(c) * s, y: CGFloat(r) * s, width: s, height: s)), with: .color(.secondary.opacity(0.08)))
                }
            }
        }
        .background(Color(nsColor: .textBackgroundColor))
    }
}

/// Keeps two PDF views at the same relative scroll position.
@MainActor
final class PDFScrollSync {
    private var views: [PDFView] = []
    private var observers: [NSObjectProtocol] = []
    private var syncing = false

    func register(_ v: PDFView) {
        views.append(v)
        DispatchQueue.main.async { [weak self] in self?.observe(v) }
    }

    private func observe(_ v: PDFView) {
        // Only user scrolling: PDFView also moves its clip view while laying out (autoscale).
        guard let scroll = Self.scrollView(in: v) else { return }
        observers.append(NotificationCenter.default.addObserver(forName: NSScrollView.didLiveScrollNotification, object: scroll,
                                                                queue: .main) { [weak self, weak v] _ in
            MainActor.assumeIsolated { if let v { self?.follow(v) } }
        })
    }

    private func follow(_ source: PDFView) {
        guard !syncing, let s = Self.scrollView(in: source), let doc = s.documentView else { return }
        let range = doc.frame.height - s.contentView.bounds.height
        guard range > 0 else { return }
        let flipped = doc.isFlipped
        let pos = s.contentView.bounds.minY / range
        let rel = flipped ? pos : 1 - pos
        syncing = true
        for v in views where v !== source {
            guard let t = Self.scrollView(in: v), let tdoc = t.documentView else { continue }
            let tr = tdoc.frame.height - t.contentView.bounds.height
            guard tr > 0 else { continue }
            let y = (tdoc.isFlipped ? rel : 1 - rel) * tr
            t.contentView.scroll(to: NSPoint(x: t.contentView.bounds.minX, y: y))
            t.reflectScrolledClipView(t.contentView)
        }
        syncing = false
    }

    static func scrollView(in v: NSView) -> NSScrollView? {
        for sub in v.subviews {
            if let s = sub as? NSScrollView { return s }
            if let s = scrollView(in: sub) { return s }
        }
        return nil
    }

    deinit {
        for o in observers { NotificationCenter.default.removeObserver(o) }
    }
}

private struct PDFKitView: NSViewRepresentable {
    let document: PDFDocument
    let sync: PDFScrollSync

    func makeNSView(context: Context) -> PDFView {
        let v = PDFView()
        v.document = document
        v.displayMode = .singlePageContinuous
        v.autoScales = true
        v.backgroundColor = .underPageBackgroundColor
        sync.register(v)
        return v
    }

    func updateNSView(_ v: PDFView, context: Context) {}
}

private struct PDFPane: View {
    let meta: FileMeta
    let sync: PDFScrollSync
    let document: PDFDocument?

    init(meta: FileMeta, sync: PDFScrollSync) {
        self.meta = meta
        self.sync = sync
        document = meta.url.flatMap { PDFDocument(url: $0) }
    }

    var body: some View {
        if !meta.exists {
            MissingPane()
        } else {
            VStack(spacing: 0) {
                if let document {
                    PDFKitView(document: document, sync: sync)
                } else {
                    Text("PDF kann nicht geöffnet werden.").foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                Divider()
                Text([document.map { "\($0.pageCount) \($0.pageCount == 1 ? "Seite" : "Seiten")" }, meta.sizeText]
                    .compactMap { $0 }.joined(separator: " · "))
                    .font(.caption).foregroundStyle(.secondary)
                    .padding(6)
            }
        }
    }
}

private struct InfoPane: View {
    let meta: FileMeta
    let note: String?

    var body: some View {
        if let url = meta.url {
            VStack(spacing: 10) {
                Image(nsImage: NSWorkspace.shared.icon(forFile: url.path))
                    .resizable().frame(width: 96, height: 96)
                Text(meta.name).font(.headline).lineLimit(2).multilineTextAlignment(.center)
                Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 4) {
                    GridRow { Text("Art").foregroundStyle(.secondary); Text(meta.kindText) }
                    GridRow { Text("Größe").foregroundStyle(.secondary); Text(meta.sizeText) }
                    GridRow { Text("Geändert").foregroundStyle(.secondary); Text(meta.dateText) }
                }
                .font(.callout)
                if let note { Text(note).font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center) }
                HStack {
                    Button("Mit Quick Look öffnen") { FileCompareWindow.quickLook(url) }
                    Button("Im Finder zeigen") { NSWorkspace.shared.activateFileViewerSelecting([url]) }
                }
                .padding(.top, 6)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            MissingPane()
        }
    }
}

private struct SideBySide<L: View, R: View>: View {
    @ViewBuilder let left: L
    @ViewBuilder let right: R

    var body: some View {
        HStack(spacing: 0) {
            left.frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            right.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

struct FileCompareRoot: View {
    enum Content {
        case text(TextCompareModel)
        case image
        case pdf(PDFScrollSync)
        case other(note: String?)
    }

    let old: FileMeta
    let new: FileMeta
    let content: Content

    var body: some View {
        VStack(spacing: 0) {
            CompareHeader(old: old, new: new)
            Divider()
            switch content {
            case .text(let m):
                TextCompareView(model: m)
            case .image:
                SideBySide { ImagePane(meta: old) } right: { ImagePane(meta: new) }
            case .pdf(let sync):
                SideBySide { PDFPane(meta: old, sync: sync) } right: { PDFPane(meta: new, sync: sync) }
            case .other(let note):
                SideBySide { InfoPane(meta: old, note: note) } right: { InfoPane(meta: new, note: note) }
            }
        }
        .frame(minWidth: 760, minHeight: 420)
    }
}

// MARK: - Window

/// Window comparing two versions of a file; either side may be missing.
@MainActor
final class FileCompareWindow: NSObject, NSWindowDelegate {
    static let shared = FileCompareWindow()
    private(set) var window: NSWindow?
    private(set) var textModel: TextCompareModel?

    func show(old: URL?, new: URL?, title: String) {
        let oldMeta = FileMeta(old), newMeta = FileMeta(new)
        let urls = [oldMeta.url, newMeta.url].compactMap { $0 }
        let kinds = urls.map(FileCompare.kind)
        var content: FileCompareRoot.Content
        switch Set(kinds).count == 1 ? kinds[0] : .other {
        case .text where [oldMeta, newMeta].contains(where: { ($0.size ?? 0) > FileCompare.maxTextSize }):
            content = .other(note: "Zu groß für den zeilenweisen Vergleich.")
        case .text:
            let m = TextCompareModel(old: oldMeta.url, new: newMeta.url)
            content = .text(m)
        case .image: content = .image
        case .pdf: content = .pdf(PDFScrollSync())
        case .other: content = .other(note: kinds.count == 2 && kinds[0] != kinds[1] ? "Die beiden Seiten haben verschiedene Dateiarten." : nil)
        }
        if case .text(let m) = content { textModel = m } else { textModel = nil }
        let root = FileCompareRoot(old: oldMeta, new: newMeta, content: content)
        if let w = window, let host = w.contentViewController as? NSHostingController<FileCompareRoot> {
            host.rootView = root
            w.title = "MacSync – Vergleich: \(title)"
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let w = NSWindow(contentViewController: NSHostingController(rootView: root))
        w.title = "MacSync – Vergleich: \(title)"
        w.styleMask = [.titled, .closable, .resizable, .miniaturizable]
        w.setContentSize(NSSize(width: 1200, height: 720))
        w.isReleasedWhenClosed = false
        w.delegate = self
        if ProcessInfo.processInfo.environment["MACSYNC_SNAPSHOT"] == nil { w.setFrameAutosaveName("MacSyncFileCompare") }
        w.center()
        window = w
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    static func quickLook(_ url: URL) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/qlmanage")
        p.arguments = ["-p", url.path]
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run()
    }

    func windowWillClose(_ notification: Notification) {
        window = nil
        textModel = nil
    }
}
