import AppKit
import SwiftUI

struct SettingsView: View {
    var body: some View {
        TabView {
            GeneralTab().tabItem { Label("Allgemein", systemImage: "gearshape") }
            SyncTab().tabItem { Label("Synchronisation", systemImage: "folder") }
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
                    Button("Werkzeug „reposync“ installieren") { cliMessage = model.installCommandLineTool() }
                    if let m = cliMessage { Text(m).font(.caption).foregroundStyle(.secondary) }
                }
            }
            LabeledContent("Version", value: model.status?.version ?? Bundle.main.shortVersion)
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
    @State private var confirmUnpair = false

    var body: some View {
        Form {
            Section("Dieser Mac") {
                HStack {
                    TextField("Name", text: $name)
                    Button("Sichern") { model.setConfig("device_name", name) }
                        .disabled(name.isEmpty || name == model.config?.string("device_name"))
                }
                LabeledContent("Port", value: String(model.config?.port ?? 47123))
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
        .onAppear { name = model.config?.string("device_name") ?? "" }
        .confirmationDialog("Kopplung aufheben?", isPresented: $confirmUnpair) {
            Button("Entkoppeln", role: .destructive) { Task { await model.unpair() } }
        } message: {
            Text("Der andere Mac kann sich danach nicht mehr verbinden. Dateien bleiben unverändert.")
        }
    }
}

extension Bundle {
    var shortVersion: String {
        object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"
    }
}
