import AppKit
import SwiftUI

struct SettingsView: View {
    var body: some View {
        TabView {
            GeneralTab().tabItem { Label("Allgemein", systemImage: "gearshape") }
            SyncTab().tabItem { Label("Synchronisation", systemImage: "folder") }
            FoldersTab().tabItem { Label("Ordner", systemImage: "shippingbox") }
            DeviceTab().tabItem { Label("Gerät", systemImage: "laptopcomputer") }
        }
        .frame(width: 480)
        .padding()
    }
}

private struct GeneralTab: View {
    @EnvironmentObject var model: AppModel
    @AppStorage("notifyConflicts") private var notifyConflicts = true
    @State private var cliMessage: String?

    var body: some View {
        Form {
            Toggle("Bei Anmeldung starten", isOn: Binding(
                get: { model.launchAtLogin },
                set: { model.setLaunchAtLogin($0) }))
            Toggle("Mitteilung bei Konflikten", isOn: $notifyConflicts)
                .onChange(of: notifyConflicts) { _, on in if on { model.requestNotificationPermission() } }
            LabeledContent("Befehlszeile") {
                VStack(alignment: .trailing) {
                    Button("Werkzeug „macsync“ installieren") { cliMessage = model.installCommandLineTool() }
                    if let m = cliMessage { Text(m).font(.caption).foregroundStyle(.secondary) }
                }
            }
            LabeledContent("Version", value: Bundle.main.shortVersion)
            LabeledContent("Entwickelt von") {
                VStack(alignment: .trailing) {
                    Text(About.company)
                    Link("alpineperspective.com", destination: About.website)
                }
            }
            if let e = model.lastError {
                Text(e).foregroundStyle(.red).font(.caption)
            }
        }
        .formStyle(.grouped)
    }
}

private struct SyncTab: View {
    @EnvironmentObject var model: AppModel

    private var root: String { model.config?.string("root") ?? "–" }
    private var mode: String { model.config?.string("mode") ?? "two-way" }
    /// Choices for the journal retention; a value set by hand is added.
    private static let standardDays = [1, 3, 7, 14, 30, 90]
    private static var journalDays: [Int] {
        let current = ConfigFile.load()?.int("journal_retention_days", in: "sync") ?? 7
        return Array(Set(standardDays + [current])).sorted()
    }

    var body: some View {
        Form {
            LabeledContent("Verzeichnis") {
                VStack(alignment: .trailing) {
                    Text(Paths.display(root)).textSelection(.enabled)
                    HStack {
                        Button("Im Finder zeigen") { model.openRoot() }
                        Button("Ändern …") { chooseRoot() }
                    }
                }
            }
            Picker("Richtung", selection: Binding(get: { mode }, set: { model.setConfig("mode", $0) })) {
                Text("In beide Richtungen").tag("two-way")
                Text("Nur senden (dieser Mac gibt vor)").tag("mirror-send")
                Text("Nur empfangen (anderer Mac gibt vor)").tag("mirror-receive")
            }
            Picker("Protokoll aufbewahren", selection: Binding(
                get: { model.config?.int("journal_retention_days", in: "sync") ?? 7 },
                set: { model.setConfig("journal_retention_days", $0, in: "sync") })) {
                ForEach(Self.journalDays, id: \.self) { d in
                    Text(d == 1 ? "1 Tag" : "\(d) Tage").tag(d)
                }
            }
            .help("Wie lange „Protokoll anzeigen“ zurückreicht. Ältere Einträge werden gelöscht, sobald die Änderung gilt.")
            LabeledContent("Zustand", value: model.state.title)
            if let units = model.status?.units, !units.isEmpty {
                ForEach(units, id: \.name) { u in
                    LabeledContent(u.name == "." ? "(Wurzel)" : u.name, value: u.state)
                }
            }
        }
        .formStyle(.grouped)
    }

    private func chooseRoot() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.directoryURL = URL(fileURLWithPath: Paths.expand(root))
        panel.prompt = "Auswählen"
        NSApp.activate(ignoringOtherApps: true)
        guard panel.runModal() == .OK, let url = panel.url else { return }
        if let warning = SetupModel.rootWarning(url) {
            let alert = NSAlert()
            alert.messageText = "Dieses Verzeichnis synchronisieren?"
            alert.informativeText = warning
            alert.addButton(withTitle: "Abbrechen")
            alert.addButton(withTitle: "Trotzdem verwenden")
            if alert.runModal() == .alertFirstButtonReturn { return }
        }
        model.setConfig("root", url.path)
    }
}

private struct DeviceTab: View {
    @EnvironmentObject var model: AppModel
    @State private var name = ""
    @State private var pairName = ""
    @State private var confirmUnpair = false

    var body: some View {
        Form {
            Section("Dieser Mac") {
                HStack {
                    TextField("Name", text: $name)
                    Button("Sichern") { model.setConfig("device_name", name) }
                        .disabled(name.isEmpty || name == model.config?.string("device_name"))
                }
                HStack {
                    TextField("Name beim Koppeln", text: $pairName, prompt: Text(Self.shortName(name)))
                    Button("Sichern") { model.setConfig("pair_name", pairName) }
                        .disabled(pairName == (model.config?.string("pair_name") ?? ""))
                }
                .help("Diesen Namen sehen Macs im selben Netz nur, während dieser Mac auf eine Kopplung wartet. Leer: der Gerätename ohne Besitzer.")
                LabeledContent("Port", value: String(model.config?.port ?? 47123))
                Toggle("In unbekannten Netzen unsichtbar", isOn: Binding(
                    get: { model.config?.bool("hide_in_unknown_networks", in: "peer") ?? false },
                    set: { model.setConfig("hide_in_unknown_networks", $0, in: "peer") }))
                    .help("In Netzen, in denen MacSync den anderen Mac noch nie erreicht hat (Café, Hotel), öffnet es keinen Port und kündigt sich nicht an; es verbindet sich nur selbst zum anderen Mac. Bekannt ist ein Netz nach der Kopplung, nach dem ersten Kontakt oder über „Diesem Netz vertrauen“ im Menü.")
            }
            Section("Gekoppelter Mac") {
                if let peer = model.status?.peer {
                    LabeledContent("Name", value: peer.name ?? "–")
                    LabeledContent("Adresse", value: peer.address ?? "–")
                    LabeledContent("Verbindung", value: model.status?.peerOnline == true ? "verbunden" : "nicht erreichbar")
                    if let v = model.status?.peerVersion { LabeledContent("Version", value: v) }
                    if let p = peer.paired, let d = Status.parseDate(p) {
                        LabeledContent("Gekoppelt", value: d.formatted(date: .abbreviated, time: .shortened))
                    }
                    HStack {
                        Button("Neu koppeln …") { SetupWindow.shared.show(step: .pair) }
                        Button("Entkoppeln", role: .destructive) { confirmUnpair = true }
                    }
                } else {
                    Text("Noch nicht gekoppelt")
                    Button("Koppeln …") { SetupWindow.shared.show(step: .pair) }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            name = model.config?.string("device_name") ?? ""
            pairName = model.config?.string("pair_name") ?? ""
        }
        .confirmationDialog("Kopplung aufheben?", isPresented: $confirmUnpair) {
            Button("Entkoppeln", role: .destructive) { Task { await model.unpair() } }
        } message: {
            Text("Der andere Mac kann sich danach nicht mehr verbinden. Dateien bleiben unverändert.")
        }
    }
}

extension DeviceTab {
    /// Mirrors `discovery::short_name`: the device name without its owner.
    static func shortName(_ n: String) -> String {
        let n = n.trimmingCharacters(in: .whitespaces)
        for sep in [" von ", " de ", " di "] {
            if let r = n.range(of: sep) { return String(n[..<r.lowerBound]) }
        }
        for sep in ["’s ", "'s ", "’ ", "' "] {
            if let r = n.range(of: sep) { return String(n[r.upperBound...]) }
        }
        return n
    }
}

extension Bundle {
    var shortVersion: String {
        object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"
    }

}

/// Build and package folders per language: off by default (they can be
/// installed or built again on each Mac), Git's own folders on. Matches
/// `macsync_fs::ECOSYSTEMS` and `[sync] sync_build_*` / `sync_git`.
private struct FoldersTab: View {
    @EnvironmentObject var model: AppModel

    struct Ecosystem: Identifiable {
        let key: String
        let title: String
        let folders: String
        let again: String
        var id: String { key }
        var configKey: String { "sync_build_\(key)" }
    }

    static let ecosystems = [
        Ecosystem(key: "node", title: "Node.js / JavaScript",
                  folders: "node_modules, .next, .nuxt, .svelte-kit, .turbo, .parcel-cache",
                  again: "npm install"),
        Ecosystem(key: "python", title: "Python",
                  folders: ".venv, venv (mit pyvenv.cfg), __pycache__, .mypy_cache, .pytest_cache, .ruff_cache, .tox",
                  again: "pip install / uv sync"),
        Ecosystem(key: "rust", title: "Rust (Cargo)", folders: "target (neben Cargo.toml)", again: "cargo build"),
        Ecosystem(key: "swift", title: "Swift / Xcode",
                  folders: ".build (neben Package.swift), DerivedData, Pods (neben Podfile)",
                  again: "swift build / pod install"),
        Ecosystem(key: "go", title: "Go", folders: "vendor (neben go.mod)", again: "go mod vendor"),
        Ecosystem(key: "terraform", title: "Terraform",
                  folders: ".terraform, .terragrunt-cache", again: "terraform init"),
    ]

    private func syncs(_ e: Ecosystem) -> Bool {
        model.config?.bool(e.configKey, in: "sync") ?? false
    }

    private var syncsGit: Bool { model.config?.bool("sync_git", in: "sync") ?? true }

    var body: some View {
        Form {
            Section {
                Toggle(isOn: Binding(get: { syncsGit }, set: { on in confirmGit(on) })) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Git-Repositorys (.git)")
                        Text("Verlauf, Branches und Stash wandern mit. Aus: nur die Dateien des Arbeitsbereichs.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            } header: {
                Text("Versionsverwaltung")
            }
            Section {
                Toggle(isOn: Binding(
                    get: { Self.ecosystems.allSatisfy(syncs) },
                    set: { on in model.setSyncFlags(Dictionary(uniqueKeysWithValues: Self.ecosystems.map { ($0.configKey, on) })) })) {
                    Text("Alle Build- und Paketordner synchronisieren").bold()
                }
                ForEach(Self.ecosystems) { e in
                    Toggle(isOn: Binding(get: { syncs(e) }, set: { model.setConfig(e.configKey, $0, in: "sync") })) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(e.title)
                            Text(e.folders).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .help("Wiederherstellen auf dem anderen Mac: \(e.again)")
                }
            } header: {
                Text("Build- und Paketordner")
            } footer: {
                Text("Aus (Standard): diese Ordner werden nicht abgeglichen – sie lassen sich auf jedem Mac neu installieren oder bauen. Vorhandene Kopien bleiben auf beiden Macs erhalten, es wird nichts gelöscht. Ein Ordner namens „target“ ohne Cargo.toml daneben zählt nicht dazu. Gilt für diesen Mac: stelle den anderen Mac genauso ein.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }

    private func confirmGit(_ on: Bool) {
        if !on {
            let alert = NSAlert()
            alert.messageText = "Git-Ordner nicht mehr synchronisieren?"
            alert.informativeText = "Commits, Branches und Stash wandern dann nicht mehr zum anderen Mac; nur die Dateien im Arbeitsbereich. Die vorhandenen .git-Ordner bleiben auf beiden Macs erhalten."
            alert.addButton(withTitle: "Nicht synchronisieren")
            alert.addButton(withTitle: "Abbrechen")
            guard alert.runModal() == .alertFirstButtonReturn else { return }
        }
        model.setConfig("sync_git", on, in: "sync")
    }
}
