# Betrieb

## Installation

```sh
cargo build --release
install -m 755 target/release/reposync /usr/local/bin/reposync
```

Voraussetzungen: macOS (APFS) oder Linux, `git` im `PATH`. Auf macOS beim
ersten Start die Berechtigung „Lokales Netzwerk“ erlauben und bei Bedarf
dem Binary *Festplattenvollzugriff* geben (Dokumente-Ordner).

## Einrichtung

```sh
# beide Macs
reposync init --root ~/Code --name "MacBook Pro"

# Mac A: Code anzeigen               Mac B: Code eingeben
reposync pair listen                 reposync pair connect 192.168.1.20:47124 123456
```

Beim Pairing wird gewarnt, wenn CPU-Architektur oder Root-Pfad abweichen
(native Module in `node_modules`, absolute Pfade in virtualenvs).
Ein neues Pairing ersetzt das alte; `reposync unpair <device>` widerruft den
Zugriff sofort.

Dienst:

```sh
reposync service install     # LaunchAgent bzw. systemd-User-Unit
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.reposync.daemon.plist
```

Erst-Sync grosser Datenmengen: beide Macs per Thunderbolt-Kabel
(Thunderbolt Bridge) oder Ethernet verbinden und `[peer] address` auf die
Bridge-Adresse setzen.

## Konfiguration (`~/.config/reposync/config.toml`)

```toml
device_name = "MacBook Pro"
root = "/Users/chris/Code"
# state_dir = "~/Library/Application Support/RepoSync"
listen = "0.0.0.0:47123"      # Verbindungen werden nur aus dem LAN angenommen
pair_port = 47124
mode = "two-way"              # two-way | mirror-send | mirror-receive

[peer]
# address = "mac-b.local:47123"   # sonst Bonjour bzw. Adresse vom Pairing
discovery = true

[sync]
quiet_ms = 400                # Ruhezeit nach dem letzten Ereignis
max_quiet_ms = 5000           # Ruhezeit bei Bursts (npm install)
max_wait_s = 30               # spätestens dann erfassen
anti_entropy_s = 60           # Root-Vergleich mit dem Peer
verify_interval_h = 24        # vollständiges Neu-Hashen
fsck_after_apply = true
strip_quarantine = false      # com.apple.quarantine beim Empfang entfernen
default_excludes = true       # .DS_Store, .Spotlight-V100, .Trashes, .fseventsd …
excludes = []                 # z. B. ["**/target", "Scratch/*.tmp"]
tombstone_ttl_days = 30
trash_retention_days = 14
journal_retention_days = 90
```

Konfigurationsänderungen werden beim Neustart des Daemons übernommen.

## Befehle

| Befehl | Zweck |
| --- | --- |
| `reposync status [unit] [--json]` | Zustand, Root-Hashes beider Seiten, Konflikte |
| `reposync diff [unit]` | Trockenlauf: abweichende Pfade und geplante Aktion mit Grund |
| `reposync sync [units…] [--dry-run]` | sofort synchronisieren |
| `reposync verify [unit]` | vollständiges Neu-Hashen, meldet „stille“ Änderungen |
| `reposync conflicts [--all]` | Konflikte inkl. Speicherort der fremden Version |
| `reposync resolve <id> --keep local\|remote\|both` | Konflikt lösen |
| `reposync log [--unit u] [--since 1h] [--json]` | Journal |
| `reposync pause [unit]` / `resume [unit]` | anhalten / fortsetzen |
| `reposync prefer <unit> local\|remote\|none` | „dieser Mac gewinnt“ |
| `reposync trash list` / `restore <pfad> [--to ziel]` | Papierkorb |
| `reposync peers` / `unpair <device>` | Peers verwalten |

Läuft ein Daemon, gehen ändernde Befehle über seinen Steuer-Socket; sonst
werden sie direkt ausgeführt.

## Zustände einer Einheit

| Zustand | Bedeutung |
| --- | --- |
| `in sync` | Root-Hashes beider Seiten gleich |
| `waiting for quiet` | lokale Änderungen, Ruhe-Gate noch zu |
| `differs` | Roots unterschiedlich, Abgleich folgt |
| `conflict` | mindestens ein offener Konflikt |
| `paused` | pausiert |
| `waiting: …` | Anwendung verschoben (Git arbeitet, nicht erfasste lokale Änderung) |

## Fehlerbehebung

| Symptom | Ursache / Abhilfe |
| --- | --- |
| `peer … not reachable` | Peer-Daemon läuft nicht, Firewall, Gäste-WLAN mit Client-Isolation → `[peer] address` setzen |
| Einheit bleibt `waiting: index.lock exists` | ein Git-Befehl hängt oder ist abgestürzt; `.git/index.lock` prüfen |
| Konflikt `git-state` | auf beiden Macs wurde im selben Repo committet: Objekte sind vereinigt, `git log --all`/`git merge <hash>` nutzen, dann `resolve --keep local` oder `remote` |
| Datei versehentlich überschrieben | `reposync trash list`, `reposync restore <tx>/<pfad>` |
| Datenbank beschädigt | Daemon stoppen, `reposync.db*` löschen, starten: Neuaufbau per Scan; abweichende Stände werden als Konflikte gemeldet, nie überschrieben |
| Ausführliche Logs | `REPOSYNC_LOG=reposync=debug reposync daemon` |
