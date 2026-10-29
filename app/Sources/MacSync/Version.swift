import AppKit

/// Versions of the app, its helper and the daemon, and the launch checks
/// (single instance, installed copy in /Applications).
enum Version {
    static let installed = URL(fileURLWithPath: "/Applications/MacSync.app")

    /// CFBundleShortVersionString of a bundle (this app: `nil` → main bundle).
    static func bundle(_ url: URL? = nil) -> String? {
        (url.flatMap(Bundle.init(url:)) ?? Bundle.main).infoDictionary?["CFBundleShortVersionString"] as? String
    }

    /// `macsync --version` without the leading "macsync",
    /// e.g. "0.5.0 (0309498b5b, 2026-10-28)". Blocking.
    static func helper(_ bin: URL?) -> String? {
        guard let bin, FileManager.default.isExecutableFile(atPath: bin.path) else { return nil }
        let p = Process()
        p.executableURL = bin
        p.arguments = ["--version"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        let s = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard p.terminationStatus == 0, !s.isEmpty else { return nil }
        return s.hasPrefix("macsync ") ? String(s.dropFirst("macsync ".count)) : s
    }

    /// "0.5.0 (0309498b5b, 2026-10-28)" → "0.5.0 (0309498)".
    static func short(_ v: String) -> String {
        let parts = v.split(separator: " ", maxSplits: 1).map(String.init)
        guard parts.count == 2 else { return v }
        let commit = parts[1].trimmingCharacters(in: CharacterSet(charactersIn: "()")).split(separator: ",").first.map(String.init) ?? ""
        return commit.isEmpty ? parts[0] : "\(parts[0]) (\(commit.prefix(7)))"
    }

    static func semver(_ v: String) -> String { String(v.split(separator: " ").first ?? Substring(v)) }

    /// Test runs (screenshots, own configuration) skip the launch checks.
    static var isTestRun: Bool {
        let env = ProcessInfo.processInfo.environment
        return env["MACSYNC_SNAPSHOT"] != nil || env["MACSYNC_CONFIG"] != nil || env["MACSYNC_DEV"] != nil
    }

    /// Only one MacSync at a time, preferably the one in /Applications.
    /// `false`: this instance quits (another one takes over).
    @MainActor
    static func launchChecks() -> Bool {
        if isTestRun { return true }
        let me = NSRunningApplication.current
        // `--takeover`: started by an older copy that is quitting right now.
        if !CommandLine.arguments.contains("--takeover"), let id = Bundle.main.bundleIdentifier,
           let other = NSRunningApplication.runningApplications(withBundleIdentifier: id)
               .first(where: { $0.processIdentifier != me.processIdentifier }) {
            other.activate()
            NSApp.terminate(nil)
            return false
        }
        let here = Bundle.main.bundleURL.standardizedFileURL
        guard here.path != installed.path, FileManager.default.fileExists(atPath: installed.path) else { return true }
        let mine = describe(bundle: Bundle.main.bundleURL, helper: Paths.binary)
        let theirs = describe(bundle: installed, helper: installed.appendingPathComponent("Contents/Helpers/macsync"))
        let alert = NSAlert()
        alert.messageText = "Diese MacSync liegt nicht in „Programme“"
        alert.informativeText = "Diese MacSync liegt nicht in „Programme“ (Version \(mine), \(Paths.display(here.path))). "
            + "Die installierte Version \(theirs) öffnen?"
        alert.addButton(withTitle: "Installierte öffnen")
        alert.addButton(withTitle: "Diese verwenden")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return true }
        let config = NSWorkspace.OpenConfiguration()
        // Same bundle id as this instance: without this, Launch Services
        // would just bring this one to the front.
        config.createsNewApplicationInstance = true
        config.arguments = Array(CommandLine.arguments.dropFirst()) + ["--takeover"]
        NSWorkspace.shared.openApplication(at: installed, configuration: config) { _, _ in
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
        return false
    }

    private static func describe(bundle url: URL, helper: URL?) -> String {
        let b = bundle(url) ?? "?"
        guard let h = self.helper(helper) else { return b }
        return semver(h) == b ? short(h) : b
    }
}
