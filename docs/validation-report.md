# Validierungsbericht MVP v0.1

Stand: 20.10.2026 · Commit-Basis: Branch `claude/mvp-implementation-docs-dzm5uj`

Dieser Bericht belegt die Gates aus dem
[Implementierungsplan](implementation-plan.md#4-meilensteine-und-gates) mit
den tatsächlich ausgeführten Tests und Messwerten. Alles hier ist
reproduzierbar (Befehle jeweils angegeben).

## 1. Testumgebungen

| Umgebung | Details |
| --- | --- |
| Entwicklungs-Sandbox | Linux 6.18, ext4 auf virtio-Disk, 4 vCPUs, Rust 1.97, git 2.43 – I/O stark schwankend |
| GitHub Actions `ubuntu-latest` | Lint, Tests, Release-Build, E2E, Benchmark |
| GitHub Actions `macos-latest` | Tests (APFS, Case-insensitiv, FSEvents, macOS-Codepfade), Release-Build, E2E, Benchmark |

## 2. Ergebnisse je Gate

| Gate | Nachweis | Ergebnis |
| --- | --- | --- |
| M0 Gerüst | `cargo build`, CI `lint` (fmt, clippy `-D warnings`) | ✅ |
| M1 Core | 31 Tests, u. a. proptest: VV ist Halbordnung, `merge` kommutativ/assoziativ/idempotent, Merkle reihenfolgeunabhängig, `decide` symmetrisch und ohne stillen Verlust | ✅ |
| M2 Dateisystem & Store | 12 + 4 Tests: Typen, Symlinks (auch tote), FIFO übersprungen, verschachtelte Repos, Ausschlüsse, Papierkorb, xattrs, Konflikt-Deduplizierung | ✅ |
| M3 Git-bewusst | 7 Tests gegen echtes git: Index v2/v3/v4 semantisch stabil bei Stat-Änderungen, Locks inkl. `refs/**/*.lock`, Rebase-Erkennung, `.git`-Datei, Reflog-Merge, `fsck`-Erkennung | ✅ |
| M4 Engine | 17 Ordner- + 10 Git-Integrationstests + Konvergenz-Property-Test (siehe 3.) | ✅ |
| M5 Netzwerk | 7 + 3 Tests: Noise-Roundtrip (1 MB, komprimierbar/zufällig), Pairing richtig/falsch/Limit 3, Sync über TCP (9-MB-Datei + 500 Dateien), fremder und gefälschter Schlüssel abgelehnt, öffentliche Adresse abgelehnt | ✅ |
| M6 Daemon & CLI | E2E mit zwei Daemon-Prozessen (siehe 4.), Latenz < 3 s | ✅ Linux und macOS |
| M7 Validierung | dieser Bericht | ✅ |

Befehl: `cargo test --workspace` → **104 Tests, 0 Fehler** (Linux und macOS; zusätzlich 1 Benchmark mit `--ignored`).

## 3. Wichtige Integrationsszenarien (Engine)

| Szenario | Prüfung |
| --- | --- |
| Erst-Sync aller Typen | Inhalte, Modus 0755, Symlinks (relativ/tot), leere Ordner, Unicode-Namen, 0-Byte-Dateien, Root-Dateien, `mtime` erhalten, `.DS_Store` ausgeschlossen |
| Zweiweg-Änderungen und Löschungen | Löschungen inkl. Unterordner, Tombstones verhindern Wiederbelebung, alte Version im Papierkorb |
| Nebenläufige Bearbeitung | Konflikt auf beiden Seiten, beide Versionen bleiben, fremde Version im Konfliktspeicher, keine Duplikate, Auflösung `remote`/`local`/`both`, veraltete Konflikte schliessen sich selbst |
| Ändern vs. Löschen | Änderung gewinnt |
| Richtungsvorgabe / Spiegel | `prefer local` löst automatisch; Spiegel-Empfänger überschreibt lokale Änderungen, Sender übernimmt nie |
| Nicht erfasste lokale Änderung | Anwendung wird verschoben, nie überschrieben; danach regulärer Konflikt |
| Grosse Dateien / Dedup | 6-MB-Datei per Bereichs-Transfer; identischer Inhalt in anderer Einheit: 0 Bytes übertragen |
| Hardlinks, xattrs | Hardlink-Gruppe neu verlinkt (gleiche Inode); xattr-Änderung ohne Inhaltsübertragung |
| Git: Spiegelung inkl. nicht committeter Arbeit | `git status`, `git log`, gestagte/ignorierte/ungetrackte Dateien identisch; **Refs vor = nach (keine Commits/Refs/Stashes durch RepoSync)**; `git fsck --full` sauber |
| Git: Commits, Branches, Checkout, Rückweg | HEAD, Branch, sauberer `git status` auf beiden Seiten |
| Git: `git status` auf dem Ziel | 0 geänderte Einträge, kein Verkehr (semantischer Index-Hash) |
| Git: Ruhe-Gate | `index.lock` verschiebt Erfassung; laufender Rebase verschiebt Anwendung |
| Git: gleichzeitige Commits | genau ein `git-state`-Konflikt; HEAD/Index jeder Seite bleiben; Objekte beider Seiten vorhanden; Auflösung stellt fremden HEAD her, `fsck` sauber |
| Git: `gc --prune=now` | konvergiert, `fsck` sauber, lose Objekte auch auf B entfernt (nach Ankunft des Packs) |
| Absturz während Anwendung | Fehlerinjektion nach 4 Dateioperationen: Journal-Eintrag vorhanden, `index.lock` blockiert Git; Neustart vollendet die Transaktion, Lock entfernt, `fsck` sauber, HEAD identisch |
| Verschachteltes Repo, Typwechsel Ordner → Repo, Hooks | eigene Einheit; Versionsvektoren bleiben; Hook übertragen und im Journal gewarnt |

**Konvergenz-Property-Test** (`tests/convergence.rs`): zufällige Schreib-/
Löschfolgen (bis 24 Schritte) auf beiden Seiten mit zufälligen Teil-Syncs.
Invarianten: nach Konfliktauflösung identische Roots und Bäume; die letzte
vom Nutzer geschriebene Version jeder Seite existiert danach noch (Baum,
Papierkorb oder Konfliktspeicher). 40 Fälle je CI-Lauf, lokal zusätzlich mit
400 Fällen ausgeführt – **keine Verletzung**. Der Test fand während der
Entwicklung eine echte Regression (fehlendes Anlegen des Konfliktordners nach
einer Optimierung), die vor dem Commit behoben wurde.

### Zuverlässigkeit (`tests/robustness.rs`)

| Szenario | Ergebnis |
| --- | --- |
| Absturz nach **jeder** einzelnen Dateioperation einer Git-Transaktion (Commit mit Änderung, neuer und gelöschter Datei; 18 Absturzpunkte) | an jedem Punkt: HEAD alt oder neu, `git fsck --full` sauber, Git durch `index.lock` blockiert; nach Neustart vollständig, Lock entfernt, Arbeitsbaum identisch zur Quelle |
| Netzabbruch beim 1., 2. und 3. Blob-Aufruf (6 000 Dateien + 9-MB-Datei) | Zielbaum bitgenau unverändert, kein offenes Journal, Staging bereinigt; nach Wiederverbindung vollständiger Sync |
| Quelle ändert eine Datei während der Übertragung | Transaktion verworfen (kein Mix aus alt/neu), danach Konvergenz auf den neuen Stand |
| Datenbankverlust auf einer Seite + lokale Bearbeitung | Neuaufbau per Scan; abweichender Stand wird Konflikt, **nie** still überschrieben |

Zusätzlich im Daemon: Stat-Rescan aller Einheiten alle 10 Minuten
(`rescan_interval_s`), damit verpasste Dateisystem-Ereignisse nicht erst bei
der täglichen Vollprüfung auffallen; Funde werden als `drift` journalisiert.

## 4. End-to-End mit zwei Daemons (`tests/e2e/two_daemons.sh`)

Ablauf: Init beider Seiten → Pairing per Code (falscher Code wird abgelehnt)
→ Start zweier Daemon-Prozesse → Erst-Sync (Git-Repo + 3 000 Dateien
`node_modules` + Ordner) → 5 × Latenzmessung → Git-Torture auf A (5 Commits,
Branch, Checkout, `stash`, `rebase`, `stash pop`, `gc --prune=now`, gestagte
Datei) → Commits auf B → Ordnerlöschung → nebenläufige Bearbeitung →
Konflikt → Auflösung → `verify`.

| Messgrösse | Linux (Sandbox) | Ziel (Konzept) |
| --- | --- | --- |
| Erst-Sync 3 003 Dateien | 1,6–2,1 s | – |
| Latenz kleine Änderung (Speichern → Ziel) | 426–442 ms (Median 436 ms) | < 3 s ✅ |
| Torture-Szenario | konvergiert, `fsck --full` sauber, HEAD/Status/Refs identisch | `fsck`-gültig ✅ |
| Leerlauf-CPU beider Daemons (CPU-Zeit über 10 s ohne Änderungen, `/proc`-Ticks) | 0,1 % eines Kerns | ≈ 0 % ✅ |
| `verify` nach dem Lauf | 0 stille Abweichungen | – |

Korrektur: Eine frühere Fassung meldete „0 % CPU (ps-Mittel)“. Das war ein
Messfehler – gemessen wurde die Subshell statt des Daemon-Prozesses (der
deshalb auf macOS beim Aufräumen auch nicht beendet wurde). Skript und
Messung sind korrigiert; der Wert oben ist die echte Leerlauf-Messung.

Die Latenz setzt sich aus der Ruhezeit (400 ms, konfigurierbar) und < 50 ms
Übertragung/Anwendung zusammen.

## 5. Benchmark (`tests/bench.rs`, 100 000 Dateien, in-process)

`cargo test --release -p reposync-engine --test bench -- --ignored --nocapture`

| Messgrösse | Sandbox | Ziel |
| --- | --- | --- |
| Leerlauf-Prüfung (Root-Vergleich aller Einheiten) | **0,12 ms** | < 1 s ✅ |
| Kalter Scan (alles hashen) | 4,7 s | – |
| Warmer Re-Scan ohne Änderung (nur Stat) | 1,1 s | – |
| Erst-Sync 100 000 Dateien | 17,9 s (≈ 5 600 Dateien/s) | Bandbreite ausschöpfen ⚠️ |
| Eine geänderte Datei: Scan der Einheit | 1,2 s | – |
| Eine geänderte Datei: Pull (Merkle-Abstieg) | 1,7 s, genau 1 Schreibvorgang | – |

Optimierungen während der Validierung (gemessen, 30 000 Dateien): gebündeltes
`fsync` pro Transaktion (14,6 s → 9,5 s) und paralleles Schreiben in den
Staging-Ordner (→ 5,4 s). Parallele Lesezugriffe auf der Quellseite waren in
der Sandbox messbar langsamer und wurden verworfen. 100 000 Dateien: 44 s →
17,9 s. Die Sandbox-I/O (`openat` ≈ 67 µs) schwankt um Faktor 2.

macOS-CI-Runner (`macos-latest`, virtualisiertes APFS, gleicher Benchmark):

| Messgrösse | macOS-CI |
| --- | --- |
| Leerlauf-Prüfung | **0,08 ms** ✅ |
| Erzeugen der 100 000 Testdateien (Vergleichswert für die I/O des Runners) | 19,4 s |
| Kalter Scan / warmer Re-Scan | 28,5 s / 3,5 s |
| Erst-Sync 100 000 Dateien | 86 s (≈ 1 160 Dateien/s; fetch 54 s, apply 29 s) |
| Eine geänderte Datei: Scan / Pull | 6,0 s / 5,8 s |

Der CI-Runner ist beim Dateierzeugen selbst ≈ 4× langsamer als die Sandbox;
die Werte sind daher kein Mass für echte Macs, zeigen aber, dass Erst-Sync
und Einheiten-Scan bei sehr vielen kleinen Dateien I/O-gebunden sind. Echte
Messungen auf den Ziel-Macs sind Teil des Dogfoodings (Konzept: 4 Wochen
Spiegel-Modus).

## 6. macOS (GitHub Actions)

* `cargo test --workspace` auf `macos-latest`: **alle Tests bestanden**
  (inkl. macOS-spezifischer Codepfade: BSD-Flags, `lchflags`,
  Case-insensitive-Probe auf APFS, FSEvents-Backend, `clonefile` via
  `reflink-copy`).
* E2E: erster Lauf scheiterte an einer zu streng formulierten Prüfung des
  Skripts (leere Stash-Liste statt Gleichheit mit A); nach Korrektur
  **bestanden**: Pairing, Erst-Sync 3 003 Dateien ≈ 1,7 s, Latenz
  638–860 ms (< 3 s ✅), Git-Torture mit sauberem `fsck`, Konflikt und
  Auflösung, `verify`.
* Benchmark: siehe Abschnitt 5.

## 7. Bekannte Grenzen (bewusst, siehe Roadmap)

| Thema | Stand |
| --- | --- |
| Scan einer Einheit nach Ereignis | Stat-Walk der ganzen Einheit (1,1 s Sandbox / 3,5–6 s macOS-CI bei 100 000 Dateien in *einer* Einheit); Scan nur der geänderten Teilbäume (FSEvents-Pfade) ist die wichtigste Optimierung für v0.2 |
| Erst-Sync-Durchsatz | eine Verbindung, sequentielles Anwenden; mehrere Ströme sind v0.2 |
| Grosse Dateien | Ganzdatei-Transfer (4-MB-Bereiche, fortsetzbar durch Dedup), kein FastCDC-Delta |
| Atomare Bundles/SQLite-Gruppen, Sparse Files, `birthtime`, ACLs | nicht im MVP |
| `node_modules`-Konflikte | pro Datei statt ein Lockfile-Konflikt |
| Menüleiste, Übergabe-Modus, Keychain | nicht im MVP; IPC-Socket als Schnittstelle vorhanden |

## 8. Fazit

Das Kernprinzip des Konzepts – Repository als Transaktion, Git-geordnete
atomare Anwendung, nie stille Datenverluste – ist umgesetzt und durch
automatisierte Tests gegen echte Git-Repositories, Absturz-Injektion,
Property-Tests und einen Zwei-Prozess-E2E-Test belegt. Die nicht-funktionalen
Ziele für Latenz, Leerlauf-Prüfung, CPU im Leerlauf, Integrität und
Absturzsicherheit sind erfüllt; der Erst-Sync-Durchsatz für sehr viele kleine
Dateien ist der wichtigste Optimierungspunkt für v0.2. Empfehlung gemäss
Konzept: zunächst 4 Wochen Spiegel-Modus mit täglichem `reposync verify`,
dann Zweiweg.
