// Renders the MacSync app icon into app/Resources/AppIcon.icns.
//
// Usage (from the repository root): swift app/Icon/make-icon.swift
// The result is committed; run again only to change the design.
import AppKit

let out = URL(fileURLWithPath: "app/Resources")
let iconset = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

func render(_ px: Int) -> Data {
    let s = CGFloat(px)
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let ctx = NSGraphicsContext.current!.cgContext

    // macOS icon grid: 824/1024 body, centred, continuous-ish corners.
    let inset = s * 100 / 1024
    let body = CGRect(x: inset, y: inset, width: s - 2 * inset, height: s - 2 * inset)
    let radius = body.width * 0.225
    let path = CGPath(roundedRect: body, cornerWidth: radius, cornerHeight: radius, transform: nil)

    // Soft drop shadow.
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -s * 0.01), blur: s * 0.025,
                  color: NSColor.black.withAlphaComponent(0.3).cgColor)
    ctx.addPath(path)
    ctx.setFillColor(NSColor.black.cgColor)
    ctx.fillPath()
    ctx.restoreGState()

    // Background: deep blue to teal, top to bottom.
    ctx.saveGState()
    ctx.addPath(path)
    ctx.clip()
    let colors = [NSColor(srgbRed: 0.10, green: 0.42, blue: 0.95, alpha: 1).cgColor,
                  NSColor(srgbRed: 0.02, green: 0.20, blue: 0.55, alpha: 1).cgColor] as CFArray
    let gradient = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB), colors: colors, locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: 0, y: body.maxY), end: CGPoint(x: 0, y: body.minY), options: [])
    // Subtle highlight on the upper half.
    ctx.setFillColor(NSColor.white.withAlphaComponent(0.06).cgColor)
    ctx.fillEllipse(in: CGRect(x: body.minX - body.width * 0.25, y: body.midY,
                               width: body.width * 1.5, height: body.height))
    ctx.restoreGState()

    // Two laptops facing each other …
    let laptop = NSImage.SymbolConfiguration(pointSize: s * 0.16, weight: .medium)
        .applying(.init(paletteColors: [NSColor.white.withAlphaComponent(0.85)]))
    if let l = NSImage(systemSymbolName: "laptopcomputer", accessibilityDescription: nil)?.withSymbolConfiguration(laptop) {
        let w = l.size.width, h = l.size.height
        l.draw(in: CGRect(x: body.minX + body.width * 0.07, y: body.minY + body.height * 0.10, width: w, height: h))
        l.draw(in: CGRect(x: body.maxX - body.width * 0.07 - w, y: body.minY + body.height * 0.10, width: w, height: h))
    }
    // … and the sync arrows above them.
    let arrows = NSImage.SymbolConfiguration(pointSize: s * 0.36, weight: .semibold)
        .applying(.init(paletteColors: [.white]))
    if let a = NSImage(systemSymbolName: "arrow.triangle.2.circlepath", accessibilityDescription: nil)?.withSymbolConfiguration(arrows) {
        let w = a.size.width, h = a.size.height
        a.draw(in: CGRect(x: body.midX - w / 2, y: body.midY - h / 2 + body.height * 0.08, width: w, height: h))
    }

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for base in [16, 32, 128, 256, 512] {
    try render(base).write(to: iconset.appendingPathComponent("icon_\(base)x\(base).png"))
    try render(base * 2).write(to: iconset.appendingPathComponent("icon_\(base)x\(base)@2x.png"))
}
try render(1024).write(to: out.appendingPathComponent("AppIcon.png"))

let p = Process()
p.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
p.arguments = ["-c", "icns", iconset.path, "-o", out.appendingPathComponent("AppIcon.icns").path]
try p.run()
p.waitUntilExit()
print(p.terminationStatus == 0 ? "wrote app/Resources/AppIcon.icns" : "iconutil failed")
