import AppKit

/// Menu bar icon: the sync arrows (in the menu bar's text colour) plus a
/// coloured status dot with a glyph, so the state is clear at a glance and
/// without relying on colour alone:
///
/// green ✓ in sync · blue … syncing · orange ❙❙ paused · orange ! other Mac
/// unreachable · red ! conflict · red ✕ stopped / not set up
enum MenuIcon {
    private struct Badge {
        let color: NSColor
        let glyph: String
    }

    private static var cache: [String: NSImage] = [:]

    static func image(for state: SyncState) -> NSImage {
        let badge: Badge = switch state {
        case .inSync: Badge(color: .systemGreen, glyph: "checkmark")
        case .syncing, .starting: Badge(color: .systemBlue, glyph: "ellipsis")
        case .paused: Badge(color: .systemOrange, glyph: "pause.fill")
        case .offline: Badge(color: .systemOrange, glyph: "exclamationmark")
        case .conflict: Badge(color: .systemRed, glyph: "exclamationmark")
        case .stopped, .notConfigured: Badge(color: .systemRed, glyph: "xmark")
        }
        let key = "\(badge.color)-\(badge.glyph)"
        if let img = cache[key] { return img }
        let img = render(badge)
        cache[key] = img
        return img
    }

    private static func render(_ badge: Badge) -> NSImage {
        let probe = NSImage(systemSymbolName: "arrow.triangle.2.circlepath", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 14, weight: .semibold))
        let a = probe?.size ?? NSSize(width: 18, height: 15)
        let d: CGFloat = 10 // dot diameter
        let size = NSSize(width: ceil(a.width + 1 + d), height: max(16, ceil(a.height)))
        // Drawn on demand, so the arrows follow light/dark menu bars.
        let img = NSImage(size: size, flipped: false) { rect in
            let arrowsCfg = NSImage.SymbolConfiguration(pointSize: 14, weight: .semibold)
                .applying(.init(paletteColors: [.headerTextColor]))
            NSImage(systemSymbolName: "arrow.triangle.2.circlepath", accessibilityDescription: "MacSync")?
                .withSymbolConfiguration(arrowsCfg)?
                .draw(in: NSRect(x: 0, y: (rect.height - a.height) / 2, width: a.width, height: a.height))
            let dot = NSRect(x: rect.maxX - d, y: (rect.height - d) / 2 - 2, width: d, height: d)
            badge.color.setFill()
            NSBezierPath(ovalIn: dot).fill()
            let glyphCfg = NSImage.SymbolConfiguration(pointSize: 6, weight: .black)
                .applying(.init(paletteColors: [.white]))
            if let g = NSImage(systemSymbolName: badge.glyph, accessibilityDescription: nil)?.withSymbolConfiguration(glyphCfg) {
                let gs = g.size
                g.draw(in: NSRect(x: dot.midX - gs.width / 2, y: dot.midY - gs.height / 2, width: gs.width, height: gs.height))
            }
            return true
        }
        img.isTemplate = false // keep the colours
        return img
    }
}
