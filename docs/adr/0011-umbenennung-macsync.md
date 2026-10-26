# ADR-0011: Umbenennung RepoSync → MacSync

- Status: angenommen · Datum: 2026-10-26

## Kontext
Repository und App hiessen MacSync, Werkzeug, Konfiguration, Zustand und
Doku noch RepoSync/`reposync`. Zwei Namen für ein Produkt verwirren – etwa
„die Datenbank heisst reposync.db, die App MacSync“.

## Entscheidung
Ab 0.3.0 heisst alles **MacSync**:

| Was | bis 0.2.x | ab 0.3.0 |
| --- | --- | --- |
| Werkzeug | `reposync`, `reposync-connect` | `macsync`, `macsync-connect` |
| Crates | `reposync-*` | `macsync-*` |
| Konfiguration | `~/.config/reposync/` | `~/.config/macsync/` |
| Zustand | `~/Library/Application Support/RepoSync/` | `…/MacSync/` |
| Datenbank | `reposync.db` | `macsync.db` |
| Privatordner im Sync-Ordner | `.reposync/` | `.macsync/` |
| Umgebungsvariablen | `REPOSYNC_*` | `MACSYNC_*` |
| Bonjour | `_reposync._tcp`, `_reposync-pair._tcp` | `_macsync._tcp`, `_macsync-pair._tcp` |
| LaunchAgent | `com.reposync.daemon` | `com.alpineperspective.macsync.daemon` |

**Nicht** umbenannt werden die Kennungen in Hashes und Kryptografie
(`reposync-entry-v1`, `reposync-dir-v1`, `reposync-empty-unit`,
`reposync-git-index-v1`, `reposync-pair-v1`): Sie sind Teil des Datenformats.
Eine Änderung würde jeden gespeicherten Hash ungültig machen und beide Macs
zu einem vollständigen Neu-Einlesen und -Abgleich zwingen.

## Übernahme bestehender Installationen
`macsync migrate` läuft automatisch vor jedem Befehl und beim Start der App –
nur für die Standard-Installation (nicht bei `--config` auf eine andere
Datei oder gesetztem `MACSYNC_CONFIG`):

- verschiebt Konfiguration und Zustand an die neuen Orte, wenn dort noch
  nichts liegt (nie überschreibend);
- benennt `reposync.db` (mit WAL-Dateien) in `macsync.db` um;
- der Daemon benennt `.reposync/` im Sync-Ordner in `.macsync/` um;
  `.reposync/` bleibt dauerhaft ausgeschlossen, falls ein Rest existiert;
- weist auf einen alten LaunchAgent hin.

Geräteschlüssel und Kopplung bleiben erhalten – kein neues Koppeln nötig.

## Konsequenzen
- Protokollversion 2 (Noise-Prologue `macsync/2`): 0.3.0 verbindet sich nicht
  mit älteren Versionen. Gewollt – sonst würde ein alter Mac den neuen
  Privatordner `.macsync/` als normale Daten synchronisieren. Beide Macs
  müssen aktualisiert werden.
- Installer und `make install` entfernen `RepoSync.app` bzw. `reposync`.
