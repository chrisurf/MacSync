import AppKit
import SwiftUI

@main
struct MacSyncApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = AppModel.shared

    var body: some Scene {
        MenuBarExtra {
            MenuContent().environmentObject(model)
        } label: {
            Image(nsImage: MenuIcon.image(for: model.state)).renderingMode(.original)
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView().environmentObject(model)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var sigterm: DispatchSourceSignal?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // `pkill MacSync` (e.g. by the installer): stop the daemon cleanly.
        signal(SIGTERM, SIG_IGN)
        let src = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
        src.setEventHandler { MainActor.assumeIsolated { AppModel.shared.quit() } }
        src.resume()
        sigterm = src
        MainActor.assumeIsolated { AppModel.shared.launch() }
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { AppModel.shared.daemon.stop() }
    }
}

struct MenuContent: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Text(statusLine)
        if let root = model.status?.root {
            Button(Paths.display(root)) { model.openRoot() }
        }
        if let peer = model.status?.peer?.name {
            Text("↔ \(peer) · \(model.status?.peerOnline == true ? "verbunden" : "nicht erreichbar")")
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
        Divider()
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
