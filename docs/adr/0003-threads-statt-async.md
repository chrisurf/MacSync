# ADR-0003: Threads und blockierendes I/O statt `tokio`

- Status: angenommen · Datum: 2026-10-20

## Kontext
Die Arbeit des Daemons ist überwiegend Dateisystem- und SQLite-I/O, beides
blockierend. Es gibt genau einen Peer und damit wenige gleichzeitige
Verbindungen.

## Entscheidung
Der Daemon verwendet wenige langlebige Threads (Watcher, Scheduler, Server mit
einem Thread je Verbindung, IPC) und Kanäle (`std::sync::mpsc`). Die Engine ist
synchron und dadurch ohne Laufzeit testbar.

## Konsequenzen
- Einfachere Fehlersuche, deterministische Tests, kein `spawn_blocking`.
- Parallele Transfers mehrerer Ströme sind im MVP nicht vorgesehen; sie lassen
  sich über mehrere Verbindungen nachrüsten.
