# ADR-0008: Semantischer Vergleichs-Hash für `.git/index`

- Status: angenommen · Datum: 2026-10-20

## Kontext
`.git/index` enthält pro Eintrag Inode, Gerät, `ctime`, `mtime`, UID/GID.
Diese Werte sind maschinenspezifisch und ändern sich durch lesende Befehle
(`git status`). Ein Byte-Vergleich erzeugt Schein-Drift und Ping-Pong.

## Entscheidung
Jeder Eintrag trägt `blob` (BLAKE3 der Bytes, für Transfer und Prüfung) und
`cmp` (Vergleichswert für Merkle-Baum und Versionsvektor). Für `.git/index`
berechnet `reposync-gitaware` `cmp` über Version, Einträge (Modus, Objekt-ID,
Flags inkl. Stage, `skip-worktree`, `intent-to-add`, Pfad) und die
`REUC`-Erweiterung. Abgeleitete bzw. stat-artige Erweiterungen (`TREE`,
`UNTR`, `FSMN`, `EOIE`, `IEOT`) werden ignoriert. Ist der Index nicht lesbar
(z. B. Split-Index), fällt `cmp` auf `blob` zurück.

## Konsequenzen
- `git status` auf einer Seite erzeugt keine Übertragung.
- Nach Anwendung auf dem Ziel ändert `update-index --refresh` die Bytes;
  der Scan aktualisiert nur `blob`, ohne den Versionsvektor zu erhöhen.
