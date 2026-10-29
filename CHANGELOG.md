# Changelog

## 0.5.0 (in Arbeit)

**Beide Macs auf 0.5.0 aktualisieren:** Das Netzwerkprotokoll ist neu
(Version 3). Eine bestehende Kopplung bleibt gültig; ein Mac mit 0.4 und einer
mit 0.5 verbinden sich nicht miteinander.

### Sicherheit
- Fremde im Netz (Café, Hotel, Firmen-WLAN) bekommen keine Antwort mehr:
  Noise KK statt XX, beide Macs kennen den Schlüssel des anderen aus der
  Kopplung. Der Port ist nur offen, solange ein Mac gekoppelt ist
- Verbindungsflut begrenzt: höchstens 4 Verbindungen im Handshake (die
  älteste weicht, damit der eigene Mac durchkommt), 3 s Handshake, nach
  5 Fehlschlägen 60 s Sperre je Adresse, höchstens 8 gekoppelte Verbindungen
- Bonjour zeigt keine feste Geräte-ID mehr, sondern eine stündlich
  wechselnde, die nur der gekoppelte Mac zuordnen kann; Hostname neutral
- Kopplung: Daten (Name, Pfad, Version) verschlüsselt statt nur
  signiert; beide Macs zeigen danach vier gleiche Wörter, die App speichert
  erst nach „Ja, stimmt überein“. Ein Fremder mit falschen Codes beendet das
  Kopplungsfenster nicht mehr (3 Versuche je Adresse, 10 insgesamt). Beim
  Koppeln wird nur ein kurzer Name angekündigt („MacBook Pro“, einstellbar
  unter Einstellungen → Gerät). `pair connect` liest den Code aus
  `MACSYNC_PAIR_CODE` oder stdin statt aus der Befehlszeile
- Neu (Standard aus): „In unbekannten Netzen unsichtbar“ – in Netzen, in
  denen der andere Mac nie erreicht wurde, kein offener Port und keine
  Ankündigung; „Diesem Netz vertrauen“ im Menü
- MacSync schreibt und löscht nur innerhalb des Sync-Ordners: nichts unter
  einem Symlink, keine Einträge, die nicht angefragt wurden
- Git-Aufrufe von MacSync führen kein `core.fsmonitor` und keine Hooks
  aus einer synchronisierten `.git/config` aus. Ändert der andere Mac
  `.git/config` oder einen Hook, zeigt der Manager ein Schild am Repository
- Geräteschlüssel: wird nicht geladen, wenn andere ihn lesen können oder
  er manipuliert ist; nicht mehr in Time Machine; State-Verzeichnis 0700;
  Schlüssel werden im Speicher genullt

### Geändert
- „Über MacSync“ und Einstellungen → Allgemein zeigen den Commit des Builds
  (wie `macsync --version`), damit klar ist, was installiert ist
- Synchronisations-Manager arbeitet wie ein Dateimanager: Liste und Baum
  kommen direkt von der Platte und stehen sofort – auch ohne laufende
  Synchronisation oder Verbindung, mit allen Ordnern und Dateien (auch
  neuen, noch nicht erfassten). Der Status folgt in zwei Schritten (erst
  dieser Mac, dann der andere) mit „Status wird geladen …“; nur dort
  Vorhandenes erscheint ausgegraut. Hinweise und Fehler in einer schmalen
  Leiste statt anstelle der Liste. Doppelklick/↩/⌘↓ öffnet Dateien mit dem
  Standardprogramm, ⌘↑ geht nach oben, neu: „Pfad kopieren“
- IPC `browse` mit `path` (relativ zum Sync-Ordner) und `local`; wartet nie
  auf den anderen Mac
- Nur noch eine MacSync gleichzeitig: eine zweite Kopie holt die laufende
  nach vorn und beendet sich. Liegt die gestartete App nicht in „Programme“,
  fragt sie, ob die installierte geöffnet werden soll (mit beiden Versionen)
- Versionen sichtbar: Menü und Manager-Fußzeile zeigen „MacSync 0.5.0
  (0309498)“. Läuft die Synchronisation noch mit einer anderen Version als
  die App, warnen Menü und Manager („Synchronisation läuft noch mit 0.3.0
  (App 0.5.0)“) mit „Neu starten“; hat der andere Mac eine andere Version,
  gibt es einen Hinweis
- Manager: läuft die Synchronisation nicht, startet „Synchronisation
  starten“ sie direkt aus der Hinweisleiste
- Ordnerabgleich: statt Aktions-Markierung je Zeile zwei Spalten mit
  Häkchen „Kopieren“ und „Verschieben“ (bei „in ALT neuer“/„unklar“ heißt
  „Kopieren“: ALT-Fassung übernehmen). Das Häkchen im Spaltenkopf wählt alle
  sichtbaren Zeilen (keine/einige/alle), Leertaste setzt „Kopieren“ für die
  Auswahl. Dateien in einem gewählten Ordner zeigen „mit Ordner“, gewählte
  Einträge ab 1 GB sind orange markiert

- Protokoll (`macsync log`, „Protokoll anzeigen“) wird standardmässig 7
  statt 90 Tage aufbewahrt; einstellbar unter Einstellungen →
  Synchronisation → „Protokoll aufbewahren“ (gilt sofort, ältere Einträge
  werden gelöscht). Bestehende Konfigurationen behalten ihren Wert
  (`journal_retention_days` in `[sync]`)
- Manager: der Status des anderen Mac kommt über eine eigene Verbindung und
  wartet nicht mehr hinter einer laufenden Übertragung („Status wird
  geladen …“ hing im WLAN oft lange)
- Neu: Rechtsklick → „Als Nächstes synchronisieren“ zieht Ordner oder
  Dateien vor; ihr Repository kommt direkt nach dem, das gerade übertragen
  wird, auf beiden Macs. Ein blauer Pfeil markiert, was vorgezogen ist.
  Auf der Befehlszeile: `macsync first <pfad>`
- Große Übertragungen laufen über bis zu 4 Verbindungen gleichzeitig (je
  32 MB ausstehend eine weitere); kleine bleiben bei einer. Bricht eine
  zusätzliche Verbindung ab, übernehmen die anderen ihren Teil
- Manager: In einem unbekannten Netz erklärt die Hinweisleiste, warum die
  Macs sich nicht finden, mit Knopf „Diesem Netz vertrauen“

### Behoben
- Daemon antwortete nach dem Start mit großer Datenbank minutenlang nicht
  („Der Daemon antwortet nicht“, „Startet …“): Jede Statusabfrage zählte
  alle Einträge neu und hielt dabei die Datenbank fest; die App fragte jede
  Sekunde nach und startete bei Zeitüberschreitung zusätzlich
  `macsync status`, sodass sich Hunderte Anfragen stauten. Jetzt: Zählung
  zwischengespeichert und im Hintergrund auf eigener Verbindung, höchstens
  16 gleichzeitige Anfragen am Steuer-Socket (darüber sofort „busy“), die
  App fragt erst nach, wenn die vorige Antwort da ist
- Ersetzte der andere Mac einen Ordner durch einen Symlink, löschte MacSync
  die alten Dateien am Ziel des Symlinks, also außerhalb des Sync-Ordners
- Git-Beispiel-Hooks (`*.sample`) erzeugen keine Protokollwarnungen mehr
- Synchronisations-Manager „hing“ und zeigte „0 Einträge · alles synchron“
  mit „Die Daten konnten nicht geöffnet werden, da sie nicht das korrekte
  Format haben“: eine Zeitüberschreitung der Daemon-Anfrage wurde als
  kaputtes JSON gemeldet und ersetzte die Liste. Jetzt: „Der Daemon
  antwortet nicht (Zeitüberschreitung)“, die Liste bleibt
- Fortschritt: steht die Übertragung, heisst es „Übertragung angehalten –
  anderer Mac antwortet nicht“ statt einer absurden Restzeit („noch ca.
  29'055h 5min“)
- Synchronisations-Manager: Git-Repositories direkt in einem Ordner (z. B.
  `chrisurf/MacSync`) fehlten in der Liste und im Baum; zu sehen waren nur
  die losen Dateien daneben, eine Ebene tiefer stimmte es. Synchronisiert
  wurden sie trotzdem. Ursache: Präfix `chrisurf//` beim Suchen der Einheiten

## 0.4.0 – Duplikate finden, Ordnerabgleich (2026-10-27)

### Hinzugefügt
- `macsync dupes <Ordner> [<Ordner2>]` (`make dupes DIR=…`): findet Ordner und
  Dateien gleichen Namens und sagt, welche Kopie aktuell ist – `IDENTICAL`,
  `NEWER` (die ältere kann weg) oder `DIVERGED` (erst zusammenführen), mit
  Kopier-Zeitpunkt und Änderungen danach. Nur lesend; Inhalte per Hash, nur
  wo Name und Grösse passen; iCloud-Platzhalter werden nicht geladen.
  Ausgabe als Text oder `--json`. Siehe [docs/duplicates.md](docs/duplicates.md)
- `macsync dupes ALT NEU`: zwei Ordner werden als ältere und neuere Kopie
  verglichen (über den relativen Pfad) – pro Hauptordner eine Zeile: ✅
  identisch, 🚮 kann weg, 🟧 prüfen, 🟥 nur in ALT, ➕ nur in NEU. `-v` geht
  in die Unterordner, die nicht weg können, `-vv` nennt die Dateien.
  Verschobene Dateien werden per Hash erkannt. Die Reihenfolge wird an den
  geänderten Dateien geprüft; passt sie nicht, gibt es eine Warnung.
  Die bisherige Duplikatsuche über zwei Ordner: `--within`
- `macsync dupes ALT NEU --files` listet auch die identischen Dateien, mit
  `suggested` (wohin eine fehlende Datei gehört, wenn ihr Ordner in NEU
  verschoben wurde) und `path_new` (andere Schreibweise in NEU)
- `macsync dupes ALT NEU --apply PLAN.json`: führt einen Plan aus – kopieren,
  verschieben, ersetzen oder beide Fassungen behalten (`name (ALT
  JJJJ-MM-TT).ext`). Nie überschreiben: Kopien über eine temporäre Datei,
  per BLAKE3 bestätigt, mit Datum, Rechten und erweiterten Attributen;
  Ersetztes und Verschobenes kommt in den macOS-Papierkorb. Geänderte Dateien
  werden übersprungen. Eine JSON-Zeile je Aktion für die App. Siehe
  [docs/reconcile.md](docs/reconcile.md)
- **Ordnerabgleich** (Menü → Ordnerabgleich …, ⌘O): ALT links, NEU rechts in
  einem gemeinsamen Baum mit den Zuständen aus `macsync dupes`, Warnung bei
  vertauschter Reihenfolge, Filter „Nur Offenes“. Aktionen sammeln sich in
  einem Plan („Vorschläge“, „Ausführen …“), danach neuer Vergleich
- `--apply` meldet vorab die Größe jeder Aktion und beim Kopieren den
  Fortschritt (auch innerhalb großer Dateien); SIGTERM/Ctrl-C bricht nach
  spätestens 1 MB sauber ab, die halbe Kopie wird entfernt. Neue Aktion
  `trash_old`: ALT-Datei oder -Ordner in den Papierkorb, nur wenn NEU von
  jeder Datei eine identische Kopie hat (BLAKE3). Ordnerkopien lassen
  Wiederherstellbares aus (`node_modules`, `.venv`, Caches), `.git` nie
- Cloud-Platzhalter (Google Drive, iCloud): `--apply … --probe` prüft vorab,
  ob die Cloud liefert; liefert sie nicht, nennt der Fehler die Datei
  (`code: cloud_unavailable`) und weitere Cloud-Aktionen werden nicht
  versucht. Die App führt dann nicht aus und erklärt, was zu tun ist
- Ordner aus der Cloud werden vor dem Kopieren mit 16 parallelen Lesern
  geladen: Google Drive braucht pro Datei 1–2 s unabhängig von der Größe,
  nacheinander dauerte ein Repo mit 2 000 Dateien eine Stunde, jetzt gut
  zwei Minuten
- **Dateivergleich**: Textdateien zeilenweise wie in VS Code (nebeneinander
  oder untereinander, Änderungen innerhalb der Zeile, nächste/vorige
  Änderung), Bilder und PDFs nebeneinander
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
- Ein unlesbarer Startordner (z. B. Google Drive ohne Festplattenvollzugriff)
  galt als leer – und der andere Ordner damit als „enthält alles“. Jetzt
  bricht `dupes` mit Fehlermeldung und Hinweis auf die Berechtigung ab;
  unlesbare Unterordner gelten nie als löschbar
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
- `make app` packte mit neueren Swift-Versionen eine veraltete App ein
  (Ausgabeordner `.build/out/Products` statt `.build/arm64-apple-macosx`);
  der Pfad wird jetzt von `swift build --show-bin-path` erfragt

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
