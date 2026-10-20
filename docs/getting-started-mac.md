# Getting Started: Test auf zwei Macs im eigenen WLAN

Diese Anleitung bringt RepoSync auf zwei Macs zum Laufen, die im selben
Netz hinter deinem Router hängen. Zeitaufwand: ca. 20 Minuten pro Mac
(überwiegend der erste Build).

> **Erst mit Testdaten.** Der erste Test läuft in einem eigenen Ordner
> `~/RepoSyncTest` mit Beispieldaten – nicht mit deinem echten Code-Ordner.
> Erst wenn alles wie erwartet läuft, auf echte Daten wechseln (Abschnitt 8).

## 1. Überblick: welcher Netzweg?

| Weg | Eignung | Bemerkung |
| --- | --- | --- |
| **Gleiches WLAN / LAN hinter deinem Router** | ✅ **empfohlen** | Nichts zu konfigurieren ausser der macOS-Firewall-Abfrage. Die Macs finden sich über ihren Bonjour-Namen `<Name>.local`. |
| Thunderbolt-/USB-C-Kabel (Thunderbolt Bridge) | ✅ optional für den Erst-Sync grosser Datenmengen | Kabel einstecken → macOS legt die „Thunderbolt Bridge“ automatisch an (Adressen `169.254.x.x`). Schneller und unabhängig vom WLAN. |
| Bluetooth | ❌ | Dass sich die Macs „über Bluetooth finden“, ist Apples Continuity/AirDrop (Bluetooth LE zum Entdecken, ein separates Apple-WLAN-Protokoll für Daten). Das steht normalen Programmen nicht als Netzwerkverbindung zur Verfügung und wäre für Datenmengen viel zu langsam. |
| Gäste-WLAN / Firmen-WLAN mit „Client-Isolation“ | ❌ | Geräte dürfen dort nicht miteinander sprechen. |

RepoSync spricht ausschliesslich mit privaten Adressen (192.168.x.x, 10.x.x.x,
172.16–31.x.x, 169.254.x.x, fdxx::) und nur mit dem einmal gepairten Mac.
Der Router muss nicht angepasst werden; keine Portfreigabe ins Internet.

## 2. Voraussetzungen (auf beiden Macs)

```sh
# 1. Xcode Command Line Tools (C-Compiler/Linker, git)
xcode-select --install

# 2. Rust (rustup) – danach ein neues Terminal-Fenster öffnen
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 3. Repository auschecken
git clone https://github.com/chrisurf/RepoSync.git ~/Developer/RepoSync
cd ~/Developer/RepoSync
git checkout claude/mvp-implementation-docs-dzm5uj   # bis der Branch gemergt ist

# 4. Alles prüfen
make doctor
```

Die Rust-Version ist in `rust-toolchain.toml` festgelegt; der erste Build
lädt genau diese Version automatisch. Beide Macs bauen dadurch mit
identischem Compiler und identischen Bibliotheken (`Cargo.lock`).

## 3. Bauen und installieren (auf beiden Macs)

```sh
make install          # baut target/release/reposync und kopiert es nach ~/.local/bin
```

Falls `make doctor` meldet, dass `~/.local/bin` nicht im `PATH` ist:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc && exec zsh
reposync --version    # z. B. reposync 0.1.0 (a1b2c3d4e5, 2026-10-20)
```

Die Version enthält den Git-Commit. Beide Macs zeigen sich gegenseitig im
`make status` an, ob sie denselben Build fahren.

## 4. Bonjour-Namen herausfinden

Auf jedem Mac:

```sh
scutil --get LocalHostName     # z. B. MacBook-Pro-Chris  →  MacBook-Pro-Chris.local
```

(oder Systemeinstellungen → Allgemein → Freigaben → „Lokaler Hostname“).
Im Folgenden heissen die Macs **Mac A** (`mac-a.local`) und **Mac B**
(`mac-b.local`) – ersetze das durch deine Namen.

Erreichbarkeit testen (auf Mac A): `ping -c 3 mac-b.local`.

## 5. Einrichten

```sh
# Mac A
make sandbox                                   # ~/RepoSyncTest mit Beispiel-Repo
make init PEER=mac-b.local:47123

# Mac B – leere Testwurzel, Daten kommen von A
mkdir -p ~/RepoSyncTest
make init PEER=mac-a.local:47123
```

`PEER=<name>.local:47123` sorgt dafür, dass die Macs sich auch nach einem
neuen DHCP-Lease des Routers wiederfinden. Ohne `PEER` wird Bonjour bzw.
die Adresse vom Pairing verwendet. Die Konfiguration liegt in
`~/.config/reposync/config.toml` (Optionen: [operations.md](operations.md)).

## 6. Pairing (einmalig)

```sh
# Mac B
make pair-listen
#   pairing code: 123 456
#   on the other Mac run: reposync pair connect 192.168.1.23:47124 123456

# Mac A
make pair-connect HOST=mac-b.local CODE=123456
#   paired with Mac B (…) at 192.168.1.23:47123
```

Bei einer Firewall-Abfrage „reposync … eingehende Verbindungen akzeptieren?“
auf **Erlauben** klicken. Ein falscher Code wird abgelehnt (maximal 3 Versuche).
Warnungen zu unterschiedlicher CPU-Architektur oder unterschiedlichem
Root-Pfad sind für den Test unkritisch.

## 7. Starten und beobachten

**Wichtig: den Daemon für den Test in *Terminal.app* starten.**

```sh
# auf beiden Macs, jeweils in einem eigenen Terminal.app-Fenster
make run
```

Warum Terminal.app? Seit macOS 15 prüft macOS jeden Zugriff aufs lokale
Netz („Lokales Netzwerk“ unter Datenschutz & Sicherheit). Programme, die aus
Terminal.app oder per SSH gestartet werden, sind davon laut Apple ausgenommen.
Nicht ausgenommen sind andere Terminal-Apps (VS Code, iTerm …), die eine
eigene Freigabe brauchen, und – wichtig – **per launchd gestartete,
unsignierte Hintergrundprogramme**: Für diese wird der Zugriff unter
macOS 26/27 vielfach ohne Nachfrage verweigert (Fehler „No route to host“).
RepoSync weist im Log ausdrücklich darauf hin, wenn das passiert.

Dann ausprobieren (in einem zweiten Terminal-Fenster):

```sh
make status                                    # beide Seiten, Roots, Konflikte, Version des Peers

# Mac A: Änderung → nach ca. 0,5 s auf Mac B
echo "Hallo von A" > ~/RepoSyncTest/notes/hallo.txt

# Mac B: im Repo committen → auf Mac A sichtbar
cd ~/RepoSyncTest/demo-repo && echo "// B" >> index.js && git commit -qam "von B" && git log --oneline

# Konflikt provozieren: auf beiden Macs gleichzeitig dieselbe Datei ändern
# (z. B. beide Daemons mit Ctrl-C stoppen, auf A und B unterschiedlich bearbeiten, neu starten)
reposync conflicts
reposync resolve <id> --keep remote            # oder local / both

make log                                       # Journal: jede Aktion mit Begründung
reposync verify                                # Vollprüfung gegen das Dateisystem
```

Stoppen: `Ctrl-C` im Terminal-Fenster des Daemons.

## 8. Danach: echte Daten, Dauerbetrieb

1. **Spiegel-Modus zuerst** (Empfehlung des Konzepts): auf dem führenden Mac
   `mode = "mirror-send"`, auf dem anderen `mode = "mirror-receive"` in
   `~/.config/reposync/config.toml`; einige Wochen laufen lassen, täglich
   `reposync verify`. Dann auf `two-way` umstellen.
2. **Wurzel wechseln:** `root = "/Users/<du>/Code"` setzen. Ordner in
   Schreibtisch/Dokumente/iCloud vermeiden (macOS fragt dann zusätzlich nach
   Dateizugriff).
3. **Hintergrunddienst:** `make service-install` richtet einen LaunchAgent
   ein. Wegen der oben beschriebenen Netzwerkprüfung von macOS für
   unsignierte Programme ist das im MVP **experimentell**: Prüfe nach dem
   Start mit `make status` und `make log`, ob der Peer erreichbar ist. Wenn
   nicht, den Daemon weiter in Terminal.app laufen lassen (das Fenster darf
   minimiert sein). Ein signiertes App-Bundle als zuverlässiger
   Hintergrunddienst ist für v0.2 vorgesehen.

## 9. Updates

```sh
make update        # git pull --ff-only, neu bauen, installieren
make version       # Repo-Stand, installierte Version, gepinnte Rust-Version
```

Danach den Daemon neu starten (`Ctrl-C`, `make run`) – **auf beiden Macs**,
damit beide denselben Build fahren (`make status` zeigt es an).

Wie die Bibliotheken aktuell gehalten werden: [maintenance.md](maintenance.md).

## 10. Fehlerbehebung

| Symptom | Abhilfe |
| --- | --- |
| `No route to host` im Log | Daemon in Terminal.app statt in einer anderen App/als LaunchAgent starten; ggf. Systemeinstellungen → Datenschutz & Sicherheit → Lokales Netzwerk prüfen |
| `ping mac-b.local` geht nicht | beide im selben WLAN? Gäste-WLAN/Client-Isolation? Notfalls IP-Adresse statt Name verwenden (`ipconfig getifaddr en0`) |
| `Connection refused` | Daemon auf dem anderen Mac läuft nicht (`make run`) |
| Pairing „wrong pairing code“ | Code neu erzeugen (`make pair-listen`), innerhalb von 5 Minuten eingeben |
| Firewall blockiert | Systemeinstellungen → Netzwerk → Firewall → Optionen → `reposync` erlauben (nach jedem neuen Build kann macOS erneut fragen) |
| Unterschiedliche Builds | `make update` auf beiden Macs, Daemons neu starten |
| Datei versehentlich überschrieben | `reposync trash list`, `reposync restore <eintrag>` |
| Ausführliche Logs | `REPOSYNC_LOG=reposync=debug make run` |
| Alles zurücksetzen (nur Testumgebung!) | Daemons stoppen, `rm -rf ~/.config/reposync "~/Library/Application Support/RepoSync" ~/RepoSyncTest/.reposync` |

## Quellen zur macOS-Netzwerkprüfung

- Apple TN3179 „Understanding local network privacy“ (Ausnahmen für Terminal/SSH, launchd-Daemons, root): <https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy>
- Apple Developer Forums, Launch Agent mit Kommandozeilen-Tool blockiert: <https://developer.apple.com/forums/thread/778457>
- Erfahrungsbericht macOS 26/27, launchd-Prozess ohne App-Identität blockiert: <https://github.com/NousResearch/hermes-agent/issues/71206>
