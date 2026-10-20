# RepoSync

Git-bewusste, bitgenaue LAN-Synchronisierung zwischen zwei Macs – inklusive
`.git`, `node_modules`, ignorierter Dateien und nicht committeter Arbeit,
**ohne je einen Commit, Branch, Stash oder Ref zu erzeugen**.

RepoSync behandelt jedes Repository als **Transaktionseinheit**: erfasst wird
nur, wenn Git ruht; angewendet wird in Git-Reihenfolge (Objekte → Refs →
`HEAD` → Arbeitsbaum → `index` zuletzt über das eigene `index.lock`), mit
Staging, Hash-Verifikation, Transaktionsjournal und Papierkorb. Konflikte
werden nie still aufgelöst.

| Dokument | Inhalt |
| --- | --- |
| [docs/concept.md](docs/concept.md) | Originalkonzept |
| [docs/implementation-plan.md](docs/implementation-plan.md) | Bewertung, MVP-Umfang, Meilensteine, Gates |
| [docs/architecture.md](docs/architecture.md) | Umsetzung: Crates, Datenmodell, Protokoll, Abläufe |
| [docs/operations.md](docs/operations.md) | Installation, Konfiguration, Bedienung, Fehlerbehebung |
| [docs/validation-report.md](docs/validation-report.md) | Testergebnisse und Messwerte |
| [docs/adr/](docs/adr/) | Architekturentscheidungen |

## Schnellstart

```sh
cargo build --release            # Binary: target/release/reposync

# auf beiden Macs
reposync init --root ~/Code --name "MacBook"

# Mac A                                  # Mac B
reposync pair listen                     reposync pair connect <mac-a>.local <code>

# auf beiden Macs: Dienst installieren (LaunchAgent) oder im Vordergrund starten
reposync service install                 # oder: reposync daemon

reposync status                          # Zustand aller Repos/Ordner
reposync diff web                        # was unterscheidet sich, was würde passieren
reposync conflicts                       # offene Konflikte
reposync resolve 3 --keep remote         # Konflikt lösen
reposync log --since 1h                  # Journal: jede Aktion mit Grund
```

## Status (MVP v0.1)

Funktionsfähig: Zweiweg-, Spiegel-Sender- und Spiegel-Empfänger-Modus,
Pairing mit 6-stelligem Code (SPAKE2), Noise-verschlüsselter Transport nur im
LAN, Watcher mit Ruhe-Gate, Anti-Entropie, Konfliktspeicher, Crash-Recovery,
CLI. Nicht im MVP: Menüleisten-App, Übergabe-Modus, Delta-Transfer grosser
Dateien (siehe [Roadmap](docs/implementation-plan.md#22-nicht-enthalten-roadmap-v02)).

## Entwicklung

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                     # Unit-, Property-, Integrations-, Netzwerktests
tests/e2e/two_daemons.sh target/release/reposync   # zwei echte Daemons
cargo test --release -p reposync-engine --test bench -- --ignored --nocapture
```

Rust stable, keine Laufzeitabhängigkeiten ausser `git` (für
`update-index --refresh` und `fsck`).
