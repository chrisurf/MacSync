import Foundation

/// File locations shared with the Rust side (see crates/daemon/src/config.rs).
enum Paths {
    static let home = FileManager.default.homeDirectoryForCurrentUser

    static var config: URL {
        if let p = ProcessInfo.processInfo.environment["REPOSYNC_CONFIG"] { return URL(fileURLWithPath: p) }
        return home.appendingPathComponent(".config/reposync/config.toml")
    }

    static var stateDir: URL {
        if let s = ConfigFile.load()?.string("state_dir") { return URL(fileURLWithPath: expand(s)) }
        return home.appendingPathComponent("Library/Application Support/RepoSync")
    }

    static var socket: URL { stateDir.appendingPathComponent("daemon.sock") }

    static var logFile: URL { home.appendingPathComponent("Library/Logs/RepoSync/daemon.log") }

    /// `reposync` inside the app bundle (Contents/Helpers – Contents/MacOS
    /// already holds `RepoSync`, and APFS is case-insensitive); falls back to
    /// ~/.local/bin for development.
    static var binary: URL? {
        let bundled = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/reposync")
        if FileManager.default.isExecutableFile(atPath: bundled.path) { return bundled }
        let dev = home.appendingPathComponent(".local/bin/reposync")
        return FileManager.default.isExecutableFile(atPath: dev.path) ? dev : nil
    }

    static func expand(_ p: String) -> String {
        p.hasPrefix("~") ? home.path + p.dropFirst() : p
    }

    static func display(_ p: String) -> String {
        p.hasPrefix(home.path) ? "~" + p.dropFirst(home.path.count) : p
    }
}

/// Minimal reader/writer for the top-level string keys of config.toml.
/// Edits single lines so all other settings and comments stay untouched.
struct ConfigFile {
    var text: String

    static func load() -> ConfigFile? {
        (try? String(contentsOf: Paths.config, encoding: .utf8)).map(ConfigFile.init)
    }

    func string(_ key: String) -> String? {
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let l = line.trimmingCharacters(in: .whitespaces)
            if l.hasPrefix("[") { break } // top-level keys only
            guard l.hasPrefix(key), let eq = l.firstIndex(of: "=") else { continue }
            guard l[..<eq].trimmingCharacters(in: .whitespaces) == key else { continue }
            let v = l[l.index(after: eq)...].trimmingCharacters(in: .whitespaces)
            guard v.count >= 2, v.first == "\"", v.last == "\"" else { return nil }
            return String(v.dropFirst().dropLast())
                .replacingOccurrences(of: "\\\"", with: "\"")
                .replacingOccurrences(of: "\\\\", with: "\\")
        }
        return nil
    }

    var port: Int {
        guard let l = string("listen"), let p = l.split(separator: ":").last.flatMap({ Int($0) }) else { return 47123 }
        return p
    }

    mutating func set(_ key: String, _ value: String) {
        let escaped = value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        let newLine = "\(key) = \"\(escaped)\""
        var lines = text.components(separatedBy: "\n")
        if let i = lines.firstIndex(where: {
            let l = $0.trimmingCharacters(in: .whitespaces)
            return l.hasPrefix(key) && l.dropFirst(key.count).trimmingCharacters(in: .whitespaces).hasPrefix("=")
        }) {
            lines[i] = newLine
        } else if let i = lines.firstIndex(where: { $0.hasPrefix("[") }) {
            lines.insert(newLine, at: i)
        } else {
            lines.append(newLine)
        }
        text = lines.joined(separator: "\n")
    }

    func save() throws {
        try text.write(to: Paths.config, atomically: true, encoding: .utf8)
    }
}
