import AppKit
import SwiftUI

/// A Mac that waits for pairing (`macsync discover --json`).
struct Candidate: Decodable, Identifiable, Hashable {
    var device: String
    var name: String
    var addr: String
    var id: String { device }
}

@MainActor
final class SetupModel: ObservableObject {
    enum Step { case folder, pair, done }
    enum Pairing: Equatable { case idle, waiting(code: String), entering(Candidate), connecting, failed(String) }

    @Published var step: Step
    @Published var root: URL
    @Published var name: String
    @Published var candidates: [Candidate] = []
    @Published var pairing: Pairing = .idle
    @Published var code = ""
    @Published var pairedWith: String?
    @Published var error: String?
    @Published var busy = false

    private var listen: Process?
    private var discoverTask: Task<Void, Never>?

    init(step: Step) {
        self.step = step
        let cfg = ConfigFile.load()
        root = URL(fileURLWithPath: Paths.expand(cfg?.string("root") ?? Paths.home.appendingPathComponent("MacSync").path))
        name = cfg?.string("device_name") ?? Host.current().localizedName ?? "Mac"
        if step == .pair { startDiscovery() }
    }

    /// Why `url` is a questionable sync root, or nil.
    static func rootWarning(_ url: URL) -> String? {
        let p = url.standardizedFileURL.path
        let home = Paths.home.path
        if p == home || p == "/" { return "Das ganze Benutzer- oder Systemverzeichnis kann nicht synchronisiert werden." }
        if p.hasPrefix(home + "/Library") { return "Ordner in ~/Library sollten nicht synchronisiert werden." }
        var dir = url.standardizedFileURL
        while dir.path != "/" {
            if FileManager.default.fileExists(atPath: dir.appendingPathComponent(".git").path) {
                return "Der Ordner liegt im Git-Repository \(Paths.display(dir.path)). Meist ist ein eigener Ordner wie ~/MacSync gemeint, der Repositories enthält."
            }
            dir.deleteLastPathComponent()
        }
        for f in ["Desktop", "Documents", "Downloads", "Library/Mobile Documents"] where p.hasPrefix(home + "/" + f) {
            return "Der Ordner ist durch macOS geschützt oder liegt in iCloud; macOS fragt nach Zugriff."
        }
        return nil
    }

    func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.directoryURL = root.deletingLastPathComponent()
        panel.prompt = "Auswählen"
        if panel.runModal() == .OK, let url = panel.url { root = url }
    }

    /// Step 1 → 2: write the configuration (keeps identity and pairing).
    func saveFolder() async {
        busy = true
        defer { busy = false }
        do {
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            if var cfg = ConfigFile.load() {
                cfg.set("root", root.path)
                cfg.set("device_name", name)
                try cfg.save()
            } else {
                _ = try await CLI.run(["init", "--root", root.path, "--name", name])
            }
            error = nil
            let peers = (try? await CLI.run(["peers"])) ?? ""
            if !peers.isEmpty && !peers.hasPrefix("not paired") {
                pairedWith = peers.split(separator: "  ").dropFirst().first.map(String.init)
            }
            step = .pair
            startDiscovery()
        } catch {
            self.error = error.localizedDescription
        }
    }

    func startDiscovery() {
        discoverTask?.cancel()
        discoverTask = Task {
            while !Task.isCancelled && step == .pair {
                if case .idle = pairing,
                   let out = try? await CLI.run(["discover", "--json", "--timeout-s", "3"]),
                   let list = try? JSONDecoder().decode([Candidate].self, from: Data(out.utf8)) {
                    candidates = list
                }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }

    /// This Mac waits and shows the code.
    func waitForOther() {
        AppModel.shared.daemon.stop() // frees the ports, avoids two writers
        pairing = .connecting
        listen = CLI.stream(["pair", "listen", "--json"], line: { line in
            Task { @MainActor in self.handle(line) }
        }, done: { status, err in
            Task { @MainActor in
                self.listen = nil
                if status != 0, self.step == .pair, self.pairing != .idle {
                    self.pairing = .failed(err.isEmpty ? "Kopplung abgebrochen" : err)
                }
            }
        })
    }

    func enterCode(for c: Candidate) {
        code = ""
        pairing = .entering(c)
    }

    func connect(to c: Candidate) async {
        let digits = code.filter(\.isNumber)
        guard digits.count == 6 else { error = "Der Code hat 6 Ziffern"; return }
        pairing = .connecting
        do {
            let out = try await CLI.run(["pair", "connect", c.addr, digits, "--json"])
            handle(out.trimmingCharacters(in: .whitespacesAndNewlines))
        } catch {
            pairing = .failed(error.localizedDescription)
        }
    }

    func cancelPairing() {
        listen?.interrupt()
        listen = nil
        pairing = .idle
        error = nil
    }

    private func handle(_ line: String) {
        guard let obj = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any] else { return }
        switch obj["event"] as? String {
        case "code":
            pairing = .waiting(code: obj["code"] as? String ?? "")
        case "paired":
            pairedWith = obj["name"] as? String
            pairing = .idle
            finishPairing()
        default:
            break
        }
    }

    func finishPairing() {
        discoverTask?.cancel()
        step = .done
    }

    func finish(launchAtLogin: Bool) {
        UserDefaults.standard.set(true, forKey: "offeredLaunchAtLogin")
        AppModel.shared.setLaunchAtLogin(launchAtLogin)
        AppModel.shared.restartDaemon()
        SetupWindow.shared.close()
    }

    func shutdown() {
        discoverTask?.cancel()
        listen?.interrupt()
        if AppModel.shared.isConfigured { AppModel.shared.startSync() }
    }
}

struct SetupView: View {
    @ObservedObject var setup: SetupModel
    @State private var launchAtLogin = true

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 12) {
                Image(systemName: "arrow.triangle.2.circlepath").font(.system(size: 28))
                VStack(alignment: .leading) {
                    Text("MacSync einrichten").font(.title2.bold())
                    Text(subtitle).foregroundStyle(.secondary)
                }
            }
            Divider()
            switch setup.step {
            case .folder: folder
            case .pair: pair
            case .done: done
            }
            if let e = setup.error {
                Text(e).foregroundStyle(.red).font(.callout)
            }
        }
        .padding(24)
        .frame(width: 520)
    }

    private var subtitle: String {
        switch setup.step {
        case .folder: return "Schritt 1 von 3 · Verzeichnis"
        case .pair: return "Schritt 2 von 3 · Anderen Mac koppeln"
        case .done: return "Schritt 3 von 3 · Fertig"
        }
    }

    private var folder: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Welches Verzeichnis soll mit dem anderen Mac synchronisiert werden?")
            HStack {
                Image(systemName: "folder")
                Text(Paths.display(setup.root.path)).textSelection(.enabled)
                Spacer()
                Button("Ändern …") { setup.chooseFolder() }
            }
            .padding(10)
            .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
            if let w = SetupModel.rootWarning(setup.root) {
                Label(w, systemImage: "exclamationmark.triangle").foregroundStyle(.orange).font(.callout)
            }
            TextField("Name dieses Macs", text: $setup.name)
            HStack {
                Spacer()
                Button("Weiter") { Task { await setup.saveFolder() } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(setup.busy || setup.name.isEmpty)
            }
        }
    }

    @ViewBuilder private var pair: some View {
        switch setup.pairing {
        case .waiting(let code):
            VStack(spacing: 12) {
                Text("Öffne MacSync auf dem anderen Mac und gib dort diesen Code ein:")
                Text(code.count == 6 ? "\(code.prefix(3)) \(code.suffix(3))" : code)
                    .font(.system(size: 44, weight: .semibold, design: .monospaced))
                    .textSelection(.enabled)
                Text("Gültig für 5 Minuten").font(.caption).foregroundStyle(.secondary)
                Button("Abbrechen") { setup.cancelPairing() }
            }
            .frame(maxWidth: .infinity)
        case .entering(let c):
            VStack(alignment: .leading, spacing: 12) {
                Text("Code, der auf „\(c.name)“ angezeigt wird:")
                TextField("123 456", text: $setup.code)
                    .font(.system(size: 28, design: .monospaced))
                    .onSubmit { Task { await setup.connect(to: c) } }
                HStack {
                    Button("Zurück") { setup.cancelPairing() }
                    Spacer()
                    Button("Koppeln") { Task { await setup.connect(to: c) } }
                        .keyboardShortcut(.defaultAction)
                        .disabled(setup.code.filter(\.isNumber).count != 6)
                }
            }
        case .connecting:
            HStack { ProgressView().controlSize(.small); Text("Verbinde …") }
        case .failed(let m):
            VStack(alignment: .leading, spacing: 8) {
                Label(m, systemImage: "xmark.octagon").foregroundStyle(.red)
                Text("Beide Macs im selben WLAN? Ist der andere Mac wach?").font(.callout).foregroundStyle(.secondary)
                Button("Erneut versuchen") { setup.cancelPairing() }
            }
        case .idle:
            VStack(alignment: .leading, spacing: 12) {
                if let p = setup.pairedWith {
                    Label("Bereits gekoppelt mit \(p)", systemImage: "checkmark.circle").foregroundStyle(.green)
                }
                Text("Starte MacSync auch auf dem anderen Mac. Ein Mac wartet und zeigt einen Code, der andere verbindet sich.")
                GroupBox("Wartende Macs im Netzwerk") {
                    if setup.candidates.isEmpty {
                        HStack { ProgressView().controlSize(.small); Text("Suche …").foregroundStyle(.secondary) }
                            .frame(maxWidth: .infinity, alignment: .leading)
                    } else {
                        ForEach(setup.candidates) { c in
                            HStack {
                                Image(systemName: "laptopcomputer")
                                Text(c.name)
                                Spacer()
                                Button("Verbinden …") { setup.enterCode(for: c) }
                            }
                        }
                    }
                }
                HStack {
                    Button("Dieser Mac wartet (Code anzeigen)") { setup.waitForOther() }
                    Spacer()
                    if setup.pairedWith != nil {
                        Button("Kopplung behalten") { setup.finishPairing() }
                            .keyboardShortcut(.defaultAction)
                    }
                }
            }
        }
    }

    private var done: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label(setup.pairedWith.map { "Gekoppelt mit \($0)" } ?? "Eingerichtet", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green).font(.title3)
            Text("MacSync synchronisiert \(Paths.display(setup.root.path)) im Hintergrund. Das Symbol in der Menüleiste zeigt den Zustand.")
            Toggle("Bei Anmeldung automatisch starten", isOn: $launchAtLogin)
            Text("macOS fragt beim ersten Verbinden, ob MacSync auf das lokale Netzwerk zugreifen darf – bitte „Erlauben“.")
                .font(.callout).foregroundStyle(.secondary)
            HStack {
                Spacer()
                Button("Fertig") { setup.finish(launchAtLogin: launchAtLogin) }
                    .keyboardShortcut(.defaultAction)
            }
        }
    }
}

/// The setup assistant window (AppKit-managed, so it can open at launch).
@MainActor
final class SetupWindow: NSObject, NSWindowDelegate {
    static let shared = SetupWindow()
    private var window: NSWindow?
    private var model: SetupModel?

    func show(step: SetupModel.Step = .folder) {
        if let w = window {
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let m = SetupModel(step: step)
        let w = NSWindow(contentViewController: NSHostingController(rootView: SetupView(setup: m)))
        w.title = "MacSync"
        w.styleMask = [.titled, .closable]
        w.isReleasedWhenClosed = false
        w.delegate = self
        w.center()
        model = m
        window = w
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func close() {
        window?.close()
    }

    func windowWillClose(_ notification: Notification) {
        model?.shutdown()
        model = nil
        window = nil
    }
}
