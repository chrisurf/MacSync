# Plan v0.2.0: RepoSync als Mac-App mit Menüleiste und Release-Pipeline

Status: umgesetzt (M1–M5). Branch `feature/0.2.0`. Merge nach `main` erzeugt automatisch das Release
`v0.2.0` mit einem `.pkg`-Installer für Apple Silicon (M1–M4).

## 1. Ziel in einem Satz

Einmal installieren, einmal koppeln, danach läuft die Synchronisation unsichtbar
im Hintergrund – Bedienung und Zustand über ein Symbol in der Menüleiste,
kein Terminal mehr nötig.

## 2. Anforderungen (aus dem Auftrag)

| # | Anforderung | Umsetzung |
| --- | --- | --- |
| A1 | Normale App in `/Programme`, per Doppelklick startbar | `MacSync.app`, installiert per `.pkg` |
| A2 | Symbol in der Menüleiste | SwiftUI `MenuBarExtra`, kein Dock-Symbol (`LSUIElement`) |
| A3 | Einstellungen über die Oberfläche | Einstellungsfenster (Allgemein, Synchronisation, Gerät) |
| A4 | Start bei Anmeldung an/aus | Schalter, `SMAppService.mainApp` (Anmeldeobjekte) |
| A5 | Sichtbar: Verzeichnis, Name des anderen Macs, Zustand | Menü + Einstellungen lesen `status` vom Daemon |
| A6 | Release-Pipeline nach Merge | GitHub Actions `release.yml` auf `main` |
| A7 | Nur Apple Silicon | Build `aarch64-apple-darwin` auf `macos-15`-Runner |
| A8 | Installierbares Paket | `.pkg` (ad-hoc signiert, Developer-ID später per Secrets) |

## 3. Analyse und Verbesserungsvorschläge

1. **Version 0.2.0 statt 0.1.0.** `0.1.0` ist das CLI-MVP (CHANGELOG). Die App
   ist die nächste Minor-Version. *(entschieden)*
2. **Release nur bei neuer Version, nicht bei jedem Merge.** Die Pipeline läuft
   bei jedem Push auf `main`, erstellt aber nur dann ein Release, wenn es den Tag
   `v<version>` aus `Cargo.toml` noch nicht gibt. Kleine Fixes ohne
   Versionssprung erzeugen kein neues Release; ein Release = bewusster
   Versionssprung im PR.
3. **Die App startet den Daemon selbst (statt LaunchAgent).** macOS prüft den
   Zugriff aufs lokale Netz pro App. Ein unsignierter LaunchAgent bekommt die
   Freigabe oft gar nicht („No route to host“, siehe Getting Started). Als
   Kindprozess der App gilt die Freigabe der App: einmal „Erlauben“, fertig.
   Autostart = App als Anmeldeobjekt.
4. **Logik bleibt in Rust, die App ist dünn.** Bonjour-Suche und Pairing
   wandern aus dem Shell-Skript in den Rust-Kern (`reposync discover`,
   `reposync pair … --json`). App und `reposync-connect` nutzen dieselben
   Befehle – eine Implementierung, zwei Oberflächen.
5. **Ersteinrichtung als Assistent** statt Einstellungsmaske: Verzeichnis
   wählen → anderen Mac suchen → Code anzeigen/eingeben → fertig. Die
   Verwechslung „Code statt Verzeichnis“ ist damit konstruktiv ausgeschlossen.
6. **Bestehende Einrichtung wird übernommen.** Die App liest dieselbe
   Konfiguration (`~/.config/reposync/config.toml`) und denselben Zustand –
   die beiden gekoppelten Macs müssen nicht neu gekoppelt werden.
7. **Ohne Apple-Developer-Account** *(entschieden)*: ad-hoc signiert. Folgen:
   einmal pro Mac „Trotzdem öffnen“ (Systemeinstellungen → Datenschutz &
   Sicherheit) und nach Updates evtl. erneut „Lokales Netzwerk erlauben“.
   Die Pipeline enthält die Schritte für Developer-ID-Signatur und
   Notarisierung bereits; sie werden aktiv, sobald die Secrets gesetzt sind.
8. **Namen vereinheitlichen (offen):** Repository heisst `MacSync`, Produkt
   und Binary `RepoSync`. Entschieden (0.3.0): die App heisst **MacSync**
   (Bundle-ID `com.alpineperspective.macsync`, Entwickler Alpine Perspective
   GmbH); CLI `reposync` und Konfiguration bleiben.

## 4. Bedienkonzept

### Menüleiste (Klick auf das Symbol)

```
●  Synchron · vor 12 s                 ← Zustand (grün/blau/gelb/rot)
   ~/RepoSyncTest                       ← Verzeichnis (Klick: im Finder öffnen)
   ↔ MacBook Pro M4 CH · verbunden      ← anderer Mac
   2 Konflikte …                        ← nur wenn vorhanden
───────────────────────────────
   Jetzt synchronisieren
   Pausieren / Fortsetzen
───────────────────────────────
   Einstellungen …          ⌘,
   Protokoll anzeigen
   RepoSync beenden         ⌘Q
```

Symbolzustände: synchron · überträgt · wartet auf Ruhe · pausiert ·
anderer Mac nicht erreichbar · Konflikt.

### Ersteinrichtung (Assistent, erscheint ohne Konfiguration)

1. **Verzeichnis** – Ordnerauswahl (Vorschlag `~/RepoSyncTest`), Warnung bei
   Git-Repository, iCloud, Schreibtisch/Dokumente.
2. **Anderen Mac finden** – Liste per Bonjour; „Auf anderen Mac warten“ zeigt
   den Code groß an, „Verbinden“ fragt nach dem Code (6 Ziffern-Feld).
3. **Fertig** – Schalter „Bei Anmeldung starten“ (an), Synchronisation startet.

### Einstellungen

| Reiter | Inhalt |
| --- | --- |
| Allgemein | Bei Anmeldung starten · Mitteilungen bei Konflikten · Version |
| Synchronisation | Verzeichnis (ändern) · Modus (zwei Richtungen / nur senden / nur empfangen) · Ausschlüsse |
| Gerät | Name dieses Macs · gekoppelter Mac (Name, Adresse, Version, zuletzt gesehen) · Entkoppeln · Neu koppeln · Port (Erweitert) |

## 5. Architektur

```
MacSync.app/Contents/
  MacOS/MacSync           SwiftUI-App (Menüleiste, Einstellungen, Assistent)
  Helpers/reposync        Rust-Binary (Daemon + CLI), unverändert nutzbar
                          (nicht in MacOS/: APFS unterscheidet RepoSync/reposync nicht)
  Info.plist              LSUIElement, NSLocalNetworkUsageDescription,
                          NSBonjourServices (_reposync._tcp, _reposync-pair._tcp)
```

- **Prozess:** App startet `reposync daemon` als Kindprozess, überwacht ihn,
  startet ihn nach Absturz neu, beendet ihn beim Beenden der App.
- **Zustand:** App fragt den Daemon über den vorhandenen Unix-Socket
  (`daemon.sock`, JSON) alle 2 s nach `status`; Aktionen (`sync`, `pause`,
  `resume`, `resolve`) über denselben Socket.
- **Einrichtung/Pairing:** App ruft `reposync init`, `reposync discover --json`,
  `reposync pair listen|connect --json` auf und liest JSON-Zeilen.
- **Autostart:** `SMAppService.mainApp.register()/unregister()`.
- **CLI weiterhin verfügbar:** Menüpunkt „Befehlszeilenwerkzeug installieren“
  legt `~/.local/bin/reposync` als Symlink in die App.
- **Mindestversion:** macOS 14 (Sonoma); Apple Silicon only.
- **Build ohne Xcode-Projekt:** Swift Package unter `app/`, Bundle wird von
  `scripts/build-app.sh` zusammengesetzt (läuft lokal mit den Command Line
  Tools und in CI identisch).

### Erweiterungen im Rust-Kern

| Befehl | Zweck |
| --- | --- |
| `reposync discover [--json] [--timeout-s N]` | Bonjour-Suche nach wartenden Macs (`_reposync-pair._tcp`) |
| `reposync pair listen --json` | JSON-Ereignisse: `code`, `paired`, `error`; meldet sich selbst per Bonjour an |
| `reposync pair connect … --json` | JSON-Ergebnis statt Text |
| `reposync status --json` | vorhanden; ergänzt um Name/Adresse des Peers, falls nötig |

`reposync-connect` wird danach auf diese Befehle umgestellt (kein `dns-sd` mehr).

## 6. Release-Pipeline

`.github/workflows/release.yml`, Auslöser: Push auf `main` (= Merge).

1. Version aus `Cargo.toml` lesen; existiert Tag `v<version>` → Ende.
2. `macos-15` (arm64): `make lint test`.
3. `cargo build --release --target aarch64-apple-darwin`, `scripts/build-app.sh`.
4. Signieren: Developer-ID, falls Secrets vorhanden; sonst ad-hoc (`codesign -s -`).
5. `pkgbuild` + `productbuild` → `MacSync-<version>-arm64.pkg`
   (installiert nach `/Applications`). Notarisierung nur mit Secrets.
6. Tag `v<version>`, GitHub-Release mit `.pkg`, SHA-256 und dem
   CHANGELOG-Abschnitt als Text.

Zusätzlich baut `ci.yml` in jedem PR das `.pkg` als Artefakt (zum Testen vor dem Merge).

## 7. Meilensteine (auf `feature/0.2.0`)

| # | Inhalt | Abnahme |
| --- | --- | --- |
| M1 | Rust: `discover`, `pair --json`, Pairing-Anmeldung per Bonjour; Tests | `reposync discover` findet wartenden Mac; Skript ohne `dns-sd` |
| M2 | App-Gerüst: Swift Package, Bundle-Skript, Menüleiste mit Zustand, Daemon als Kindprozess | App startet, Symbol zeigt „synchron“ mit bestehender Konfiguration |
| M3 | Assistent + Einstellungen + Autostart | Einrichtung auf frischem Mac ohne Terminal |
| M4 | Pipeline: `release.yml`, PR-Artefakt, `.pkg` | `.pkg` aus PR installiert sich auf beiden Macs |
| M5 | Doku (README, Getting Started, MAKE-HELP), CHANGELOG 0.2.0, Version 0.2.0 | Merge → Release `v0.2.0` erscheint |

## 8. Offene Punkte

- App-Name/Bundle-ID (Vorschlag oben, Abschnitt 3.8).
- Mitteilungen bei Konflikten standardmässig an?
- Soll `make install` weiter nur das CLI installieren oder auch die App bauen
  (`make app`)? Vorschlag: neues Target `make app` / `make pkg`, `install` bleibt.
