import AppKit
import SwiftUI
import UserNotifications

@main
struct MacSyncApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = AppModel.shared

    var body: some Scene {
        MenuBarExtra {
            MenuContent().environmentObject(model)
        } label: {
            Image(nsImage: MenuIcon.image(for: model.state, frame: model.spinFrame)).renderingMode(.original)
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView().environmentObject(model)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    private var sigterm: DispatchSourceSignal?

    func applicationDidFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
        // `pkill MacSync` (e.g. by the installer): stop the daemon cleanly.
        signal(SIGTERM, SIG_IGN)
        let src = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
        src.setEventHandler { MainActor.assumeIsolated { AppModel.shared.quit() } }
        src.resume()
        sigterm = src
        // Another MacSync runs, or the installed copy should be used instead.
        guard MainActor.assumeIsolated({ Version.launchChecks() }) else { return }
        MainActor.assumeIsolated { AppModel.shared.launch() }
        // Test hook: MACSYNC_COMPARE="OLD|NEW" opens the file comparison.
        MainActor.assumeIsolated { FileCompare.launchHookIfRequested() }
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { AppModel.shared.daemon.stop() }
    }

    /// Clicking the conflict notification opens the conflict view.
    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        if response.notification.request.identifier == "conflicts" {
            DispatchQueue.main.async { MainActor.assumeIsolated { ConflictsWindow.shared.show() } }
        }
        completionHandler()
    }
}

struct MenuContent: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Text(model.state.runLine)
        if let line = statusLine {
            Text(line)
        }
        if let n = pausedUnits, n > 0 {
            Text(n == 1 ? "1 Ordner einzeln pausiert" : "\(n) Ordner einzeln pausiert")
        }
        ForEach(oldCommits, id: \.self) { line in
            Text(line)
        }
        if let a = model.activity {
            Text(a.summary(peer: model.status?.peer?.name ?? "anderer Mac"))
        }
        if let root = model.status?.root {
            Button(Paths.display(root)) { model.openRoot() }
        }
        if let peer = model.status?.peer?.name {
            Text("↔ \(peer) · \(model.status?.peerOnline == true ? "verbunden" : "nicht erreichbar")")
        }
        if model.status?.network?.hidden == true {
            Text("Fremdes Netz – MacSync unsichtbar")
            Button("Diesem Netz vertrauen") { model.trustNetwork() }
        }
        if let e = model.lastError {
            Text("⚠︎ \(e)")
        }
        Divider()
        switch model.state {
        case .notConfigured:
            Button("Einrichten …") { SetupWindow.shared.show() }
        case .stopped:
            Button("Synchronisation starten") { model.startSync() }
        case .paused:
            Button("Fortsetzen") { model.resume() }
        default:
            Button("Jetzt synchronisieren") { model.syncNow() }
            Button("Pausieren") { model.pause() }
        }
        if model.isConfigured {
            Button("Synchronisations-Manager …") { ManagerWindow.shared.show() }
                .keyboardShortcut("m")
            let n = model.status?.conflicts ?? 0
            Button(n > 0 ? "Konflikte (\(n)) …" : "Konflikte …") { ConflictsWindow.shared.show() }
                .keyboardShortcut("k")
        }
        Button("Ordnerabgleich …") { ReconcileWindow.shared.show() }
            .keyboardShortcut("o")
        Divider()
        Text(model.versionLine)
        Button("Über MacSync") { About.show() }
        SettingsLink { Text("Einstellungen …") }
            .keyboardShortcut(",")
        Button("Protokoll anzeigen") { model.openLog() }
        Button("Diagnose beider Macs sichern") { model.saveDiagnosis() }
        Divider()
        Button("MacSync beenden") { model.quit() }
            .keyboardShortcut("q")
    }

    /// What is happening while it runs (the first line says whether it runs).
    private var statusLine: String? {
        switch model.state {
        case .notConfigured, .starting, .paused:
            return nil
        case .stopped(let why):
            return why.isEmpty ? "Starten, um wieder abzugleichen" : why
        case .inSync:
            var s = "Synchron"
            if let d = model.status?.lastSync {
                s += " · zuletzt " + RelativeDateTimeFormatter().localizedString(for: d, relativeTo: Date())
            }
            return s
        default:
            return model.state.title
        }
    }

    private var pausedUnits: Int? {
        guard model.state != .paused else { return nil }
        return model.status?.units.filter(\.paused).count
    }

    /// Repositories where an older commit is checked out: held back, not
    /// distributed until a branch is checked out again.
    private var oldCommits: [String] {
        (model.status?.units ?? []).compactMap { u in
            guard let c = u.oldCommit else { return nil }
            let name = u.name.split(separator: "/").last.map(String.init) ?? u.name
            return "⏸ \(name): alter Stand \(c.prefix(7)) ausgecheckt – wird nicht verteilt"
        }
    }
}
