# Changelog

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
