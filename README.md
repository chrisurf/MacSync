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
| [docs/getting-started-mac.md](docs/getting-started-mac.md) | **Test auf zwei Macs im eigenen WLAN – Schritt für Schritt** |
| [docs/operations.md](docs/operations.md) | Installation, Konfiguration, Bedienung, Fehlerbehebung |
| [docs/maintenance.md](docs/maintenance.md) | Abhängigkeiten, Toolchain, Sicherheitsmeldungen, Updates |
| [docs/validation-report.md](docs/validation-report.md) | Testergebnisse und Messwerte |
| [docs/adr/](docs/adr/) | Architekturentscheidungen |

## Schnellstart (zwei Macs)

```sh
xcode-select --install                                         # einmalig
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh # einmalig
git clone https://github.com/chrisurf/RepoSync.git && cd RepoSync
git checkout claude/mvp-implementation-docs-dzm5uj    # bis der Branch gemergt ist

make doctor          # prüft Voraussetzungen, Version, Netzwerk
make install         # baut und installiert reposync nach ~/.local/bin

# Mac A                                   # Mac B
make sandbox                              mkdir -p ~/RepoSyncTest
make init PEER=mac-b.local:47123          make init PEER=mac-a.local:47123
                                          make pair-listen        # zeigt Code
make pair-connect HOST=mac-b.local CODE=123456
make run                                  make run                # in Terminal.app
```

Details, Netzwerk und macOS-Besonderheiten: [Getting Started](docs/getting-started-mac.md).
Alle Befehle: `make help`.

## Status (MVP v0.1)

Funktionsfähig: Zweiweg-, Spiegel-Sender- und Spiegel-Empfänger-Modus,
Pairing mit 6-stelligem Code (SPAKE2), Noise-verschlüsselter Transport nur im
LAN, Watcher mit Ruhe-Gate, Anti-Entropie, Konfliktspeicher, Crash-Recovery,
CLI. Nicht im MVP: Menüleisten-App, Übergabe-Modus, Delta-Transfer grosser
Dateien (siehe [Roadmap](docs/implementation-plan.md#22-nicht-enthalten-roadmap-v02)).

## Entwicklung

```sh
make check        # lint + test + audit (wie die CI)
make e2e          # zwei echte Daemons auf diesem Rechner
make bench        # 100 000 Dateien
make outdated     # verfügbare Updates und Sicherheitsmeldungen
```

Rust-Version in `rust-toolchain.toml` gepinnt, Abhängigkeiten in `Cargo.lock`;
Dependabot und der Workflow „Dependencies“ halten beides aktuell
([maintenance.md](docs/maintenance.md)). Laufzeitabhängigkeit: `git`.
