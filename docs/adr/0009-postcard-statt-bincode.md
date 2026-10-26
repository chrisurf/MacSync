# ADR-0009: `postcard` statt `bincode` als Binärformat

- Status: angenommen · Datum: 2026-10-20

## Kontext
Wire-Protokoll, Transaktionsjournal und einige Datenbankspalten wurden mit
`bincode` 1.x kodiert. `cargo deny check` meldet `bincode` als nicht mehr
gewartet (RUSTSEC-2025-0141). Es gibt keine bekannte Sicherheitslücke, aber
eine unbetreute Bibliothek im Kern der Datenhaltung widerspricht dem Ziel,
auf neue Versionen und Sicherheitsmeldungen reagieren zu können.

## Entscheidung
Alle Kodierung läuft über `macsync_core::codec::{encode, decode}`, das
intern `postcard` 1.x (serde-kompatibel, aktiv gewartet) verwendet. Ein
späterer Formatwechsel betrifft damit genau eine Datei.

## Konsequenzen
- Das Binärformat ändert sich gegenüber früheren Entwicklungs-Builds:
  Datenbanken und Peers älterer Builds sind inkompatibel. Für den MVP vor dem
  ersten Einsatz akzeptabel; ab v0.1 gilt: Formatänderungen erhöhen
  `PROTOCOL_VERSION` bzw. die Schema-Version.
- `cargo deny check` ist ohne Ausnahmen grün.
