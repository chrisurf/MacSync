import AppKit

/// Developer credits: shown in the About panel and in Settings → General.
enum About {
    static let company = "Alpine Perspective GmbH"
    static let website = URL(string: "https://alpineperspective.com")!

    static func show() {
        let credits = NSMutableAttributedString(
            string: "\(Bundle.main.build)\n\n",
            attributes: [.font: NSFont.monospacedSystemFont(ofSize: 11, weight: .regular),
                         .foregroundColor: NSColor.secondaryLabelColor])
        credits.append(NSAttributedString(
            string: "Entwickelt von \(company)\n",
            attributes: [.font: NSFont.systemFont(ofSize: 11), .foregroundColor: NSColor.labelColor]))
        credits.append(NSAttributedString(
            string: "alpineperspective.com",
            attributes: [.font: NSFont.systemFont(ofSize: 11), .link: website]))
        let centred = NSMutableParagraphStyle()
        centred.alignment = .center
        credits.addAttribute(.paragraphStyle, value: centred, range: NSRange(location: 0, length: credits.length))
        NSApp.activate(ignoringOtherApps: true)
        // Without .version the panel repeats the version in brackets.
        NSApp.orderFrontStandardAboutPanel(options: [.credits: credits, .version: ""])
    }
}
