# Changelog

## 0.4.0 – Duplikate finden (in Arbeit)

### Hinzugefügt
- `macsync dupes <Ordner> [<Ordner2>]` (`make dupes DIR=…`): findet Ordner und
  Dateien gleichen Namens und sagt, welche Kopie aktuell ist – `IDENTICAL`,
  `NEWER` (die ältere kann weg) oder `DIVERGED` (erst zusammenführen), mit
  Kopier-Zeitpunkt und Änderungen danach. Nur lesend; Inhalte per Hash, nur
  wo Name und Grösse passen; iCloud-Platzhalter werden nicht geladen.
  Ausgabe als Text oder `--json`. Siehe [docs/duplicates.md](docs/duplicates.md)
- **Konfliktansicht** (Menü → Konflikte …, ⌘K; im Manager über die rote
  Konflikt-Schaltfläche oder „Konflikt ansehen …“; Klick auf die Mitteilung;
  `open -a MacSync --args --conflicts`): alle offenen Konflikte über alle
  Ordner in einer Liste, nach Ordner und Art filterbar, mit Erklärung in
  Klartext und beiden Fassungen nebeneinander (Größe, Änderungsdatum, „neuer“,
  Öffnen, Im Finder zeigen, Vergleichen mit FileMerge). Lösen pro Konflikt
  oder gesammelt für eine Auswahl (⌘A); „Erledigt“ zeigt die Historie.
  Siehe [docs/sync-manager.md](docs/sync-manager.md#7-konflikte)
- `macsync resolve` nimmt mehrere IDs oder `--all [--unit <Ordner>]`; jede
  Einheit wird dabei nur einmal erfasst statt einmal pro Konflikt. IPC
  `conflicts` und `resolve-many`

### Behoben
- **Umbenennen nur der Gross-/Kleinschreibung** (`agent` → `Agent`) auf
  APFS: kam beim anderen Mac als `case-collision`-Konflikt plus Löschen in
  den Papierkorb an und wurde erst in einer weiteren Runde neu übertragen.
  Jetzt wird die Datei bzw. der Ordner dort in einem Schritt umbenannt –
  ohne Konflikt, ohne Löschen, ohne erneute Übertragung; Inhaltsänderungen
  im selben Zug werden übernommen
- **Hauptordner bzw. Repository umbenannt** (`obsidian` → `Obsidian`): Der
  alte Name blieb als zweiter Ordner bestehen, der dieselben Dateien mit
  eigenen Versionsvektoren erfasste – Hunderte Konflikte, im Manager
  `obsidian` und `Obsidian` nebeneinander. Jetzt wird der Ordner
  umbenannt (Historie bleibt), der andere Mac zieht die Schreibweise nach.
  Solche Doppel aus älteren Versionen werden beim Start entfernt, ihre
  Konflikte geschlossen; Dateien bleiben unberührt. Ist ein Ordner auf
  beiden Macs unterschiedlich geschrieben, wartet MacSync, statt hin und
  her umzubenennen – auf einem Mac angleichen genügt
- Echte Kollision der Schreibweise (`todo.md` und `TODO.md` unabhängig
  angelegt): „Version des anderen Macs übernehmen“ übernahm den Inhalt, aber
  nicht dessen Schreibweise. Jetzt wird auch umbenannt

## 0.3.0 – MacSync, Synchronisations-Manager (2026-10-26)

### Umbenennung RepoSync → MacSync (ADR-0011)
- Alles heisst jetzt MacSync: Werkzeug `macsync` / `macsync-connect`, Crates
  `macsync-*`, Konfiguration `~/.config/macsync/`, Zustand
  `~/Library/Application Support/MacSync/` mit `macsync.db`, Privatordner
  `.macsync/`, Umgebungsvariablen `MACSYNC_*`, Bonjour `_macsync._tcp`,
  LaunchAgent `com.alpineperspective.macsync.daemon`
- Bestehende Installationen werden automatisch übernommen (`macsync migrate`,
  läuft vor jedem Befehl und beim App-Start); Kopplung bleibt erhalten
- Protokollversion 2: **beide Macs aktualisieren** – 0.3.0 verbindet sich
  nicht mit älteren Versionen
- README neu mit Screenshots; `make screenshots` erzeugt sie mit Demo-Daten
- `open -a MacSync --args --manager [Ordner]` öffnet den Manager direkt

### Geändert
- Die App heisst **MacSync** (`MacSync.app`, Installer
  `MacSync-<version>-arm64.pkg`, Bundle-ID `com.alpineperspective.macsync`).
  Der Installer entfernt `RepoSync.app` aus 0.2.0; Konfiguration, Kopplung und
  das CLI `reposync` bleiben unverändert
- Menüleisten-Symbol: Synchronisationspfeile plus farbiger Zustandspunkt –
  grün ✓ synchron, blau … überträgt, orange ❙❙ pausiert, orange ! anderer Mac
  nicht erreichbar, rot ! Konflikt, rot ✕ gestoppt
- Bereits eingerichtete Macs starten MacSync beim ersten Start künftig bei der
  Anmeldung (abschaltbar in den Einstellungen)

### Hinzugefügt
- **Synchronisations-Manager** (Menü → Synchronisations-Manager …, ⌘M):
  Verzeichnisansicht mit dem Zustand jeder Datei auf beiden Macs (synchron,
  wird gesendet/geholt, wird gelöscht, Konflikt), Filter „Nicht synchron“,
  Suche; Aktionen Jetzt synchronisieren, Pausieren (Hauptordner), Konflikt
  lösen, Im Finder zeigen, In den Papierkorb; links ein ein-/ausblendbarer,
  in der Breite verstellbarer Verzeichnisbaum mit Markierung geänderter
  Ordner (docs/sync-manager.md)
- **Fortschritt**: Prozent, Dateien, Bytes, aktuelle Datei, Geschwindigkeit
  und Restzeit im Menü und im Manager; der sendende Mac zeigt „Sende an …“
- Das Menüleisten-Symbol dreht sich während der Synchronisation
- Gelöschte Hauptordner verschwinden aus dem Manager, sobald beide Macs die
  Löschung kennen (vorher erschienen sie wegen der 30 Tage aufbewahrten
  Löschvermerke weiter als „vorhanden · synchron“); bis dahin „Wird auf …
  gelöscht“ bzw. „Wird hier gelöscht“
- `reposync browse [VERZEICHNIS] [--json]`, IPC `browse`; `status` liefert
  `transfer`, `served_bytes`, `served_ms`
- App-Icon (`app/Resources/AppIcon.icns`, erzeugt von `app/Icon/make-icon.swift`)
- Entwickler-Angabe: Alpine Perspective GmbH, alpineperspective.com – im
  Fenster „Über MacSync“, in den Einstellungen und im Copyright der App

## 0.2.0 – Mac-App (2026-10-26)

### Hinzugefügt
- **RepoSync.app**: Menüleisten-App für Apple Silicon (macOS 14+) – Zustand,
  Verzeichnis und gekoppelter Mac auf einen Blick; Jetzt synchronisieren,
  Pausieren; Einstellungen (Bei Anmeldung starten, Mitteilungen bei
  Konflikten, Verzeichnis, Richtung, Gerätename, Kopplung, CLI installieren);
  Einrichtungsassistent ohne Terminal (ADR-0010)
- Installer `RepoSync-<version>-arm64.pkg`; `make app`, `make pkg`,
  `make install-app`
- Release-Pipeline: Merge auf `main` mit neuer Version erzeugt ein
  GitHub-Release mit dem Installer; jeder PR baut ihn als Artefakt
- `reposync discover`, `reposync pair … --json`; der wartende Mac meldet sich
  per Bonjour an, der andere findet ihn ohne Rechnernamen
- `reposync-connect` (`scripts/connect.sh`, `make connect`): verbindet zwei
  Macs in einem Schritt – Konfiguration, Bonjour-Suche, Pairing, Start;
  erkennt selbst, welcher Mac wartet und welcher sich verbindet
- `reposync-connect` fragt nach dem Verzeichnis, `--stop` / `make stop`,
  `make connect-help`, Kurzanleitung `MAKE-HELP.md`
- `make doctor` prüft, ob der C-Linker mit dem Standard-SDK funktioniert
- `Makefile` (doctor, install, update, version, sandbox, init, pair-*, run,
  status, lint, test, e2e, bench, audit, outdated, update-deps, update-toolchain)
- `scripts/doctor.sh` (Voraussetzungen, Versionen, Netzwerk, Firewall,
  Terminal.app) und `scripts/check-updates.sh`
- Gepinnte Toolchain (`rust-toolchain.toml`, Rust 1.99.1)
- Build-Version mit Git-Commit in `reposync --version`, im Handshake und in
  `status` (Warnung bei unterschiedlichen Builds)
- Dependabot (Crates, GitHub Actions), Workflow „Dependencies“ mit
  `cargo deny` (RustSec, Lizenzen, Quellen), `deny.toml`
- Hinweis im Log, wenn macOS den Zugriff aufs lokale Netz blockiert
- Doku: Getting Started auf zwei Macs, Wartung

### Geändert
- Hostnamen werden bevorzugt per IPv4 aufgelöst (vorher scheiterte
  `pair connect` an IPv6-Link-Local-Adressen)
- Peer-Adresse ohne Host (`:47123`) wird abgewiesen
- Standard-Gerätename auf macOS ist der Computername
- Das macOS-Attribut `com.apple.provenance` wird nicht mehr synchronisiert
  (rechnerlokal; liess Dateien sonst dauerhaft als geändert erscheinen)
- `bincode` (nicht mehr gewartet, RUSTSEC-2025-0141) durch `postcard`
  ersetzt (ADR-0009); Binärformat inkompatibel zu früheren Dev-Builds
- CI nutzt die Make-Targets

## 0.1.0 – MVP (2026-10-20)

Erste funktionsfähige Version gemäss [Implementierungsplan](docs/implementation-plan.md).

### Hinzugefügt
- Datenmodell mit BLAKE3, Merkle-Baum je Einheit, Versionsvektoren, Tombstones
- Git-bewusste Erfassung: Ruhe-Gate, semantischer Index-Hash, Hardlink-Gruppen,
  xattrs, BSD-Flags (macOS), Symlinks, leere Ordner, verschachtelte Repos
- Empfängergesteuertes Protokoll mit Merkle-Abstieg, Bündelung, zstd,
  Bereichs-Transfer, lokaler Deduplizierung (Reflink/`clonefile`)
- Atomare Anwendung in Git-Reihenfolge mit eigenem `index.lock`,
  Transaktionsjournal, Crash-Recovery, Papierkorb, `fsck`-Sicherheitsnetz
- Konfliktklassen inkl. Git-Kohärenzgruppe, Case-Kollision, Ändern-vor-Löschen,
  Reflog-Merge, Konfliktspeicher, Auflösung `local | remote | both`
- Zweiweg-, Spiegel-Sender-, Spiegel-Empfänger-Modus, Richtungsvorgabe je Einheit
- Pairing per SPAKE2 (6-stelliger Code), Noise-XX-Transport, LAN-only, mDNS
- Daemon mit Watcher, adaptiver Ruhezeit, Anti-Entropie, Steuer-Socket
- CLI `reposync` mit allen im Konzept genannten Befehlen
- Tests: Unit, Property (VV, decide, Konvergenz), Integration mit echten
  Git-Repos, Netzwerk, E2E mit zwei Daemons, Benchmark; CI für Linux und macOS
