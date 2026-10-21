# ADR-0007: Staging und Papierkorb unter `<root>/.reposync/`

- Status: angenommen · Datum: 2026-10-20

## Kontext
Atomares `rename()` und kostenloses Verschieben in den Papierkorb funktionieren
nur innerhalb eines Dateisystems. Das State-Verzeichnis kann auf einem anderen
Volume liegen als das Wurzelverzeichnis.

## Entscheidung
Staging (`.reposync/staging/<tx>/`) und Papierkorb (`.reposync/trash/`) liegen
im Wurzelverzeichnis. `.reposync` auf oberster Ebene ist immer vom Scan
ausgeschlossen und enthält `.metadata_never_index`. Datenbank, Identität und
Konfliktspeicher liegen im State-Verzeichnis
(`~/Library/Application Support/RepoSync` bzw. `$XDG_DATA_HOME/reposync`).

## Konsequenzen
Atomare Umbenennungen sind garantiert; der Konfliktspeicher wird kopiert
(nicht verschoben), da er nicht zeitkritisch ist.
