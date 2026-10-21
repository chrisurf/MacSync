# ADR-0005: Optimistischer Snapshot statt `clonefile()`-Einfrieren

- Status: angenommen · Datum: 2026-10-20

## Kontext
Das Konzept friert geänderte Dateien per APFS-Klon in einen Staging-Ordner ein,
damit die Quelle während der Übertragung einen konsistenten Stand liefert.
Das setzt APFS voraus und verdoppelt die Verwaltung (Klon-Lebenszyklus).

## Entscheidung
Der Snapshot einer Einheit ist ihr **Manifest**, das nur bei offenem
Ruhe-Gate erzeugt wird; ändert sich die Einheit während des Scans
(Ereigniszähler) oder ist das Gate danach geschlossen, wird der Scan verworfen.
Beim Ausliefern liest die Quelle jede Datei und prüft ihren BLAKE3-Hash gegen
das Manifest; bei Abweichung liefert sie „nicht verfügbar“. Der Empfänger
prüft jeden Inhalt erneut und **verwirft die gesamte Transaktion**, sobald ein
Inhalt fehlt.

## Konsequenzen
- Gleiche Garantie wie das Konzept: Angewendet wird nur ein Zustand, der auf
  der Quelle zu einem Ruhezeitpunkt vollständig existierte.
- Unter Dauerlast kann eine Einheit mehrere Anläufe brauchen; die
  Höchstwartezeit verhindert Aushungern.
- `clonefile()`-Freeze bleibt als Optimierung für sehr grosse Dateien möglich.
