# Analyse: Robustheit und Leistung der Synchronisation (Stand 0.5.0, Oktober 2026)

Grundlage: Logs und Stichproben (`sample`) der laufenden Daemons auf beiden
Macs, eine Kopie der echten Datenbank (1,13 Mio. Tombstones, 549 000 lebende
Einträge, 107 Units, 800 MB) und nachgestellte Lasttests
(`crates/engine/tests/tombstones.rs`, `resolve_load.rs`). Dazu eine Recherche
zu Syncthing (BEP, v2 mit SQLite), Mutagen, Unison, rsync und Dropbox Nucleus.

## Beobachtete Fehlerbilder und Ursachen

| Fehlerbild | Ursache | Stand |
|---|---|---|
| „Daemon antwortet nicht“, Status hängt | Status zählte alle Einträge unter der Store-Sperre; App fragte nach, ohne auf Antwort zu warten | behoben (ac1b8bf) |
| Konflikt lösen dauert Minuten | Zwei volle Scans der Unit pro Lösung | behoben (91e3075) |
| Jede Änderung in großen Repos träge | Scan, Prüfbaum und Planung luden alle Tombstones mit | behoben (6702c68), Faktor ~3 |
| Sender blockiert minutenlang, „too many connections“, Empfänger bekommt nichts | Ordner nur auf einer Seite → ganzer Teilbaum samt Tombstones ausgetauscht, Store minutenlang gesperrt | behoben (4562503), 2,1 s → 0,11 s |
| Nach Update läuft alter Daemon minutenlang weiter | Beenden wartete auf laufende Übernahme | behoben (6702c68), max. 2 s |
| Versionshinweise ohne Grund | Hinweis bei jedem anderen Commit, obwohl Protokoll gleich | entfernt (6702c68) |

## Strukturelle Schwächen (noch offen)

1. **Eine Store-Sperre für alles.** Scheduler, Netz-Handler des Peers und
   App-Status teilen eine SQLite-Verbindung hinter einem Mutex. Jeder langsame
   Schritt blockiert alles andere. Syncthing v2 und SQLite-Praxis: WAL, eine
   schreibende Verbindung, mehrere lesende, kurze Transaktionen.
2. **Sequentieller Scheduler.** Ein großer Ordner (macmailbox: 514 000
   Einträge, ständig aktive Agenten) hält alle anderen Ordner auf. Syncthing:
   eigener Dienst je Ordner unter einem Supervisor, begrenzte Parallelität.
3. **Vergleich über den ganzen Baum.** Jede Änderung baut den Prüfbaum der
   Unit neu (O(Einträge) statt O(Änderungen)). Syncthing tauscht nur Deltas
   über Sequenznummern aus.
4. **Tombstones wachsen ungebremst.** 30 Tage Frist, Aufräumen nur bei
   synchronen Units; aktive Repos sind selten ganz synchron.
5. **Kein Abbruch mitten in der Arbeit.** Scan und Übernahme prüfen kein
   Stopp-Signal; Beenden kappt sie nach 2 s hart (sicher dank Journal, aber
   unschön).
6. **Wiederholungen ohne Backoff.** Hängt der Peer, öffnet der andere Mac
   neue Verbindungen bis zum Limit.
7. **Keine Sicht auf beide Seiten.** Jeder Mac hat nur sein Text-Log ohne
   Dauern; Ursachen auf dem anderen Mac sind unsichtbar.

## Plan

**0.5.x (jetzt): Telemetrie, um die nächsten Schritte an Messwerten
auszurichten.**

- Ereignis-Log je Mac (`events.jsonl` im Zustandsordner, rotiert, 2 × 8 MB):
  Scan, Übernahme mit Phasen, langsame Anfragen von Peer und App, Wartezeit
  und Haltezeit der Store-Sperre mit Codestelle, Verbindungen und
  Abweisungen, Start/Stopp, Zustand alle 60 s.
- Neuer Peer-Befehl `Diag`: holt die Ereignisse des anderen Macs über die
  gekoppelte Verbindung, ohne Store-Zugriff (funktioniert auch, wenn der
  andere Mac hängt); Uhrversatz wird aus Hin- und Rückweg geschätzt.
  Ältere Builds antworten mit Fehler, sonst bleibt alles kompatibel.
- `macsync diag`: gemeinsame Zeitachse beider Macs; App-Menü „Diagnose
  sichern …“ legt sie als Datei auf den Schreibtisch.

**Danach, nach Messwerten priorisiert:**

| # | Maßnahme | Nutzen | Aufwand |
|---|---|---|---|
| 1 | Lesende Verbindungen getrennt von der schreibenden (WAL) | Status und Peer-Anfragen nie hinter einer Übernahme | M |
| 2 | Kooperativer Abbruch in Scan und Übernahme | sauberes Beenden, Vorziehen ohne Warten | S |
| 3 | Backoff und „busy, später“ statt neuer Verbindungen | kein Verbindungssturm | S |
| 4 | Ein Worker je Unit, begrenzte Parallelität | großer Ordner hält andere nicht auf | M |
| 5 | Tombstones gesammelt aufräumen (bestätigt vom Peer + Frist) | kleinere DB, schnellere Scans | S–M |
| 6 | Prüfbaum inkrementell statt Neuaufbau | O(Änderungen) je Scan | M |
| 7 | Änderungs-Feed mit Sequenznummern (wie Syncthing) | Abgleich ohne Baumvergleich | L |
| 8 | Blockweise Übertragung mit Fortsetzen | große Dateien abbruchfest | L |
| 9 | Deterministischer Simulationstest (wie Dropbox Trinity) | Races reproduzierbar | L |

Alles wird weiterhin synchronisiert, auch Build-Ordner und große Mengen.

## Quellen

- Syncthing BEP: https://docs.syncthing.net/specs/bep-v1
- Syncthing v2.0.0 (SQLite, DB je Ordner, Löschfristen): https://github.com/syncthing/syncthing/releases/tag/v2.0.0
- Syncthing Debug-Endpunkte: https://docs.syncthing.net/rest/debug.html
- Syncthing Tuning (Ordner-Parallelität): https://docs.syncthing.net/users/tuning.html
- Dropbox Nucleus: https://dropbox.tech/infrastructure/rewriting-the-heart-of-our-sync-engine
- Dropbox Tests (CanopyCheck, Trinity): https://dropbox.tech/infrastructure/-testing-our-new-sync-engine
- Mutagen: https://mutagen.io/documentation/synchronization/
- SQLite WAL: https://www.sqlite.org/wal.html
- NTP-Versatz (RFC 5905): https://www.rfc-editor.org/rfc/rfc5905
