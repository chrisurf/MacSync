# ADR-0010: Menüleisten-App startet den Daemon als Kindprozess

- Status: angenommen · Datum: 2026-10-26

## Kontext
v0.2.0 bringt eine SwiftUI-Menüleisten-App. Der Daemon lief bisher im
Terminal oder als LaunchAgent. Seit macOS 15 braucht jeder Zugriff aufs
lokale Netz eine Freigabe pro App; unsignierte, von launchd gestartete
Programme bekommen sie unter macOS 26/27 häufig nicht („No route to host“).
Die App wird vorerst nur ad-hoc signiert (kein Developer-Account).

## Entscheidung
Die App enthält das Rust-Binary `macsync` im Bundle und startet
`macsync daemon` als Kindprozess. Autostart erfolgt über
`SMAppService.mainApp` (die App ist Anmeldeobjekt), nicht über einen
LaunchAgent. Die App spricht mit dem Daemon über den vorhandenen
JSON-Unix-Socket; Einrichtung und Pairing laufen über CLI-Befehle mit
`--json`-Ausgabe. Die Logik bleibt im Rust-Kern.

## Konsequenzen
- Die Netzwerkfreigabe gilt für die App (`MacSync.app`); ein Dialog, eine Freigabe.
- Ohne laufende App keine Synchronisation – gewollt, der Zustand ist sichtbar.
- Ad-hoc-Signatur: nach Updates kann macOS die Freigabe erneut abfragen.
  Mit Developer-ID entfällt das; die Pipeline ist darauf vorbereitet.
- `macsync service install` (LaunchAgent) bleibt für Headless-Betrieb
  erhalten, wird in der Doku aber nicht mehr empfohlen.
