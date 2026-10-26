# Changelog

## Unveröffentlicht

### Hinzugefügt
- `reposync-connect` (`scripts/connect.sh`, `make connect`): verbindet zwei
  Macs in einem Schritt – Konfiguration, Bonjour-Suche, Pairing, Start;
  erkennt selbst, welcher Mac wartet und welcher sich verbindet
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
