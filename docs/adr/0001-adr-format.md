# ADR-0001: Architekturentscheidungen dokumentieren

- Status: angenommen · Datum: 2026-10-20

## Kontext
RepoSync weicht im MVP bewusst an einigen Stellen vom [Konzept](../concept.md) ab.
Diese Abweichungen müssen nachvollziehbar bleiben.

## Entscheidung
Jede Entscheidung mit Auswirkung auf Architektur, Sicherheit oder
Datenintegrität wird als ADR (Kontext · Entscheidung · Konsequenzen) unter
`docs/adr/` abgelegt und fortlaufend nummeriert. ADRs werden nicht gelöscht,
sondern durch neue ersetzt („ersetzt durch ADR-xxxx“).

## Konsequenzen
Reviewer finden zu jeder Abweichung die Begründung; spätere Phasen können
Entscheidungen gezielt revidieren.
