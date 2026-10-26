import Foundation

enum IPCError: LocalizedError {
    case daemon(String)
    case protocolError(String)

    var errorDescription: String? {
        switch self {
        case .daemon(let m): return m
        case .protocolError(let m): return "Ungültige Antwort: \(m)"
        }
    }
}

/// Client for the daemon's control socket: one JSON request per line
/// (crates/daemon/src/ipc.rs). Blocking – call off the main thread.
enum IPC {
    static func isListening() -> Bool {
        let fd = connectSocket(timeout: 1)
        if fd >= 0 { close(fd) }
        return fd >= 0
    }

    /// Returns `nil` if no daemon is running.
    static func call(_ request: [String: Any], timeout: Int = 10) throws -> Any? {
        let fd = connectSocket(timeout: timeout)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        var data = try JSONSerialization.data(withJSONObject: request)
        data.append(0x0A)
        let sent = data.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        guard sent == data.count else { throw IPCError.protocolError("write failed") }
        var response = Data()
        var buf = [UInt8](repeating: 0, count: 65536)
        while true {
            let n = read(fd, &buf, buf.count)
            if n <= 0 { break }
            response.append(buf, count: n)
            if buf[n - 1] == 0x0A { break }
        }
        guard let obj = try JSONSerialization.jsonObject(with: response) as? [String: Any] else {
            throw IPCError.protocolError(String(decoding: response, as: UTF8.self))
        }
        if obj["ok"] as? Bool == true { return obj["data"] }
        throw IPCError.daemon(obj["error"] as? String ?? "Fehler im Daemon")
    }

    private static func connectSocket(timeout: Int) -> Int32 {
        let path = Paths.socket.path
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return -1 }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8CString)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        guard bytes.count <= capacity else { close(fd); return -1 }
        withUnsafeMutablePointer(to: &addr.sun_path) {
            $0.withMemoryRebound(to: CChar.self, capacity: capacity) { dst in
                for (i, b) in bytes.enumerated() { dst[i] = b }
            }
        }
        var tv = timeval(tv_sec: timeout, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.connect(fd, $0, len) }
        }
        if rc != 0 { close(fd); return -1 }
        return fd
    }
}

/// Runs the bundled `reposync` CLI.
enum CLI {
    struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    /// Runs to completion; returns stdout, throws with stderr on failure.
    static func run(_ args: [String]) async throws -> String {
        guard let bin = Paths.binary else { throw Failure(message: "reposync nicht gefunden") }
        return try await withCheckedThrowingContinuation { cont in
            let p = Process()
            p.executableURL = bin
            p.arguments = ["--config", Paths.config.path] + args
            let out = Pipe(), err = Pipe()
            p.standardOutput = out
            p.standardError = err
            p.terminationHandler = { proc in
                let o = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
                let e = String(decoding: err.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
                if proc.terminationStatus == 0 {
                    cont.resume(returning: o)
                } else {
                    let msg = e.split(separator: "\n").last.map { String($0) } ?? "Fehler \(proc.terminationStatus)"
                    cont.resume(throwing: Failure(message: msg.replacingOccurrences(of: "error: ", with: "")))
                }
            }
            do { try p.run() } catch { cont.resume(throwing: error) }
        }
    }

    /// Starts a long-running command and delivers each stdout line.
    static func stream(_ args: [String], line: @escaping @Sendable (String) -> Void,
                       done: @escaping @Sendable (Int32, String) -> Void) -> Process? {
        guard let bin = Paths.binary else { done(1, "reposync nicht gefunden"); return nil }
        let p = Process()
        p.executableURL = bin
        p.arguments = ["--config", Paths.config.path] + args
        let out = Pipe(), err = Pipe()
        p.standardOutput = out
        p.standardError = err
        let buffer = LineBuffer()
        out.fileHandleForReading.readabilityHandler = { h in
            for l in buffer.feed(h.availableData) { line(l) }
        }
        p.terminationHandler = { proc in
            out.fileHandleForReading.readabilityHandler = nil
            for l in buffer.feed(out.fileHandleForReading.readDataToEndOfFile()) { line(l) }
            let e = String(decoding: err.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            done(proc.terminationStatus, e.split(separator: "\n").last.map { String($0).replacingOccurrences(of: "error: ", with: "") } ?? "")
        }
        do { try p.run() } catch { done(1, error.localizedDescription); return nil }
        return p
    }
}

private final class LineBuffer: @unchecked Sendable {
    private var data = Data()
    private let lock = NSLock()

    func feed(_ chunk: Data) -> [String] {
        lock.lock(); defer { lock.unlock() }
        data.append(chunk)
        var lines: [String] = []
        while let i = data.firstIndex(of: 0x0A) {
            lines.append(String(decoding: data[data.startIndex..<i], as: UTF8.self))
            data.removeSubrange(data.startIndex...i)
        }
        return lines
    }
}
