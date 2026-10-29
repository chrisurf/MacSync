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
        Text(statusLine)
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
        if let w = model.daemonOutdated {
            Text("⚠︎ \(w)")
            Button("Neu starten") { model.restartOutdatedDaemon() }
        }
        if let w = model.peerOutdated {
            Text("ⓘ \(w)")
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
        Divider()
        Button("MacSync beenden") { model.quit() }
            .keyboardShortcut("q")
    }

    private var statusLine: String {
        var s = model.state.title
        if model.state == .inSync, let d = model.status?.lastSync {
            s += " · " + RelativeDateTimeFormatter().localizedString(for: d, relativeTo: Date())
        }
        return s
    }
}
