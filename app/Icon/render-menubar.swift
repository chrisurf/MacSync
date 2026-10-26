// Renders the menu bar states for the README (docs/images/menubar.png).
// scripts/screenshots.sh prepends `enum SyncState` and `enum MenuIcon` from
// the app sources, so the picture always shows the real icons.

let states: [(SyncState, String)] = [
    (.inSync, "Synchron"), (.syncing, "Überträgt"), (.paused, "Pausiert"),
    (.offline, "Offline"), (.conflict(1), "Konflikt"), (.stopped(""), "Gestoppt"),
]
let scale: CGFloat = 3
let cell: CGFloat = 96
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(cell * scale) * states.count,
                           pixelsHigh: Int(64 * scale), bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                           isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSAppearance(named: .darkAqua)!.performAsCurrentDrawingAppearance {
    NSColor(white: 0.13, alpha: 1).setFill()
    NSRect(x: 0, y: 0, width: rep.pixelsWide, height: rep.pixelsHigh).fill()
    for (i, (state, label)) in states.enumerated() {
        let icon = MenuIcon.image(for: state)
        let x = CGFloat(i) * cell * scale
        let w = icon.size.width * scale * 1.5, h = icon.size.height * scale * 1.5
        icon.draw(in: NSRect(x: x + (cell * scale - w) / 2, y: 28 * scale, width: w, height: h))
        let text = NSAttributedString(string: label, attributes: [
            .font: NSFont.systemFont(ofSize: 11 * scale), .foregroundColor: NSColor.white,
        ])
        text.draw(at: NSPoint(x: x + (cell * scale - text.size().width) / 2, y: 8 * scale))
    }
}
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
