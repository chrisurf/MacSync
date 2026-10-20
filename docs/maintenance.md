# Wartung: Abhängigkeiten, Toolchain, Sicherheitsmeldungen

Ziel: jederzeit wissen, *womit* RepoSync gebaut wird, und Updates sowie
Sicherheitsmeldungen schnell und kontrolliert einspielen.

## Was festgelegt ist

| Was | Wo | Wirkung |
| --- | --- | --- |
| Rust-Compiler | `rust-toolchain.toml` (`channel = "1.99.1"`) | beide Macs und CI bauen mit identischem Compiler; rustup lädt ihn automatisch |
| Bibliotheken (Crates) | `Cargo.lock` | exakte Versionen aller direkten und transitiven Abhängigkeiten; `make build` verwendet `--locked` |
| Erlaubte Lizenzen, Quellen, Sicherheitsregeln | `deny.toml` | `make audit` / CI schlagen fehl bei RustSec-Meldungen, gezogenen (yanked) Crates, fremden Registries, unzulässigen Lizenzen |
| Build-Version | im Binary (`reposync --version`) | Paketversion + Git-Commit; beide Macs zeigen sich im Handshake ihre Version (`make status`) |

## Automatisch (GitHub)

1. **Dependabot** (`.github/dependabot.yml`): jeden Montag
   * ein gesammelter PR mit allen Minor-/Patch-Updates der Crates,
   * je ein eigener PR pro Major-Update (Breaking Changes bleiben sichtbar),
   * ein gesammelter PR für die GitHub Actions.
   Jeder PR durchläuft die CI (Lint, Tests, E2E auf Linux und macOS) – grün
   heisst: kann gemergt werden.
2. **Workflow „Dependencies“** (`.github/workflows/deps.yml`): montags und bei
   jeder Änderung an `Cargo.toml`/`Cargo.lock`/`deny.toml`/`rust-toolchain.toml`
   * `cargo deny check` (RustSec-Datenbank, Lizenzen, Quellen) – **schlägt
     fehl** bei bekannten Schwachstellen,
   * Bericht über neue Rust-Version und verfügbare Crate-Updates in der
     Job-Zusammenfassung.
3. **Einmalig im Repository aktivieren** (Settings → Code security):
   *Dependency graph*, *Dependabot alerts* und *Dependabot security updates*.
   Dann öffnet GitHub für Sicherheitsmeldungen sofort einen eigenen PR,
   unabhängig vom Wochenrhythmus.

Neue Rust-Versionen aktualisiert Dependabot nicht. Der Workflow meldet sie;
das Update ist ein Einzeiler (`make update-toolchain`, siehe unten).

## Manuell (lokal)

```sh
make outdated          # neue Rust-Version? Crate-Updates? Sicherheitsmeldungen?
make update-deps       # Cargo.lock auf neueste kompatible Versionen, danach lint+test
make update-toolchain  # neueste stabile Rust-Version pinnen, danach lint+test
make audit             # cargo deny check (vorher einmal: make tools)
make check             # alles, was die CI prüft
```

Danach committen, pushen, CI abwarten, auf beiden Macs `make update`.

## Reaktion auf eine Sicherheitsmeldung

1. Dependabot-Sicherheits-PR bzw. roter „Dependencies“-Workflow zeigt die
   RUSTSEC-ID und den betroffenen Crate.
2. Gibt es eine gepatchte Version: PR mergen (oder `make update-deps`).
3. Gibt es keine: prüfen, ob RepoSync den betroffenen Code nutzt. Wenn nicht,
   die ID mit Begründung in `deny.toml` unter `[advisories] ignore` eintragen;
   wenn doch, Crate ersetzen (Beispiel: [ADR-0009](adr/0009-postcard-statt-bincode.md)).
4. Auf beiden Macs `make update` und Daemons neu starten.

## Versionsstand prüfen

```sh
make version   # Repo-Commit, installierte Version, gepinnte Rust-Version
make doctor    # zusätzlich: ist das Repo hinter origin? ist das installierte Binary aktuell?
make status    # zeigt die Version des Peers und warnt bei unterschiedlichen Builds
```
