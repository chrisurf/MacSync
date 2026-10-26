# ADR-0007: Staging und Papierkorb unter `<root>/.macsync/`

- Status: angenommen · Datum: 2026-10-20

## Kontext
Atomares `rename()` und kostenloses Verschieben in den Papierkorb funktionieren
nur innerhalb eines Dateisystems. Das State-Verzeichnis kann auf einem anderen
Volume liegen als das Wurzelverzeichnis.

## Entscheidung
Staging (`.macsync/staging/<tx>/`) und Papierkorb (`.macsync/trash/`) liegen
im Wurzelverzeichnis. `.macsync` auf oberster Ebene ist immer vom Scan
ausgeschlossen und enthält `.metadata_never_index`. Datenbank, Identität und
Konfliktspeicher liegen im State-Verzeichnis
(`~/Library/Application Support/MacSync` bzw. `$XDG_DATA_HOME/macsync`).

## Konsequenzen
Atomare Umbenennungen sind garantiert; der Konfliktspeicher wird kopiert
(nicht verschoben), da er nicht zeitkritisch ist.
