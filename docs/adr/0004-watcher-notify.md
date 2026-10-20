# ADR-0004: Watcher über `notify` statt direkter FSEvents-Anbindung

- Status: angenommen · Datum: 2026-10-20

## Kontext
Das Konzept nutzt `fsevent-sys` direkt, um Event-IDs zu persistieren und nach
einem Neustart verpasste Ereignisse nachzuliefern. Das ist macOS-spezifisch
und auf Linux (CI, Entwicklung) nicht testbar.

## Entscheidung
`notify` (FSEvents-Backend auf macOS, inotify auf Linux). Verpasste Ereignisse
werden nicht über Event-IDs, sondern über einen **Start-Scan** gefunden, der
dank Stat-Cache (Inode, Grösse, `mtime`, `ctime`) nur geänderte Dateien hasht.
Zusätzlich vergleicht die Anti-Entropie alle 60 s die Roots, und einmal pro Tag
(`verify_interval_h`) wird vollständig neu gehasht.

## Konsequenzen
- Start kostet einen Stat-Durchlauf (bei 1 Mio. Dateien wenige Sekunden),
  aber kein Hashing.
- Event-ID-Replay kann in v0.2 als Optimierung ergänzt werden, ohne das
  Datenmodell zu ändern.
