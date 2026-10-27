# Synchronisations-Manager, Fortschritt und Status-Symbol

Stand: umgesetzt in Version 0.3.0.

## 1. Anforderungen

1. Sehen, **welche Dateien synchron sind und welche nicht** – pro Datei und
   Ordner, getrennt nach **diesem Mac** und dem **anderen Mac**.
2. **Fortschritt** einer laufenden Synchronisation: Prozent, Menge, aktuelle
   Datei, **verbleibende Zeit**.
3. Das **Menüleisten-Symbol dreht sich**, solange synchronisiert wird.
4. Ein einfacher **Dateimanager** (unter „Pausieren“ im Menü), mit dem man
   einzelne Dateien und Ordner **synchronisieren, pausieren und löschen** kann.

## 2. Wie es andere machen (Recherche)

| Werkzeug | Dateistatus | Fortschritt | Bewertung für MacSync |
| --- | --- | --- | --- |
| Dropbox, iCloud Drive, Google Drive | Abzeichen direkt im Finder (File-Provider- bzw. Finder-Sync-Erweiterung) | Menüleiste: „x Dateien, noch y Min.“ | Finder-Abzeichen sind am natürlichsten, brauchen aber eine signierte App-Erweiterung (Developer-ID) – später |
| Syncthing | Pro Ordner „Synchron / x % / n Elemente nicht synchron“, Liste der ausstehenden Dateien | Prozent und Restgröße pro Ordner | Vorbild für Ordner-Zusammenfassung und „Nicht synchron“-Filter |
| Resilio Sync | Transfers-Liste mit Datei, Richtung, Geschwindigkeit | Pro Datei | Vorbild für Richtung („wird gesendet/geholt“) |

**Entscheidung:** eigenes Fenster in der App (funktioniert ohne Developer-ID),
aufgebaut wie eine Finder-Listenansicht, mit Spalten für beide Macs. Finder-
Abzeichen bleiben als Ausbaustufe (Abschnitt 7).

## 3. Woher der Status kommt (ohne Mehraufwand im Normalfall)

MacSync hält pro Einheit einen **Merkle-Baum** (Hash pro Datei, Hash pro
Ordner über seinen Inhalt) und pro Eintrag einen **Versionsvektor**.

`macsync browse` / IPC `browse` listet **ein Verzeichnis** zur Zeit:

1. Kinder des Verzeichnisses lokal (eigener Baum) und beim anderen Mac
   (eine `tree`-Anfrage).
2. **Gleicher Hash ⇒ synchron** – auch für ganze Ordner, ohne hineinzusehen.
3. Nur für abweichende Einträge werden die Einträge des anderen Macs geholt
   (eine `entries`-Anfrage); der **Versionsvektor** entscheidet die Richtung:

| Vergleich | Status | Anzeige |
| --- | --- | --- |
| Hash gleich | `synced` | Synchron |
| dieser Mac neuer | `to-peer` | Wird an *Mac B* gesendet |
| anderer Mac neuer | `from-peer` | Wird von *Mac B* geholt |
| hier gelöscht (neuer) | `delete-on-peer` | Wird auf *Mac B* gelöscht |
| dort gelöscht (neuer) | `delete-here` | Wird hier gelöscht |
| beide geändert / offener Konflikt | `conflict` | Konflikt |
| Ordner mit Abweichungen | `differs` | Änderungen |
| anderer Mac offline | `unknown` | Unbekannt |

Damit ist auch bei 100 000 Dateien jede Ansicht schnell: es wird nur das
geöffnete Verzeichnis verglichen.

## 4. Fortschritt und Restzeit

- Der empfangende Mac kennt vor dem Übertragen die vollständige Liste der
  benötigten Inhalte (Anzahl, Bytes). Die Engine führt einen
  **Transfer-Zustand** (`files_done/total`, `bytes_done/total`, aktuelle Datei),
  der während der Übertragung nach jedem Paket bzw. Datenblock (4 MB)
  aktualisiert und im `status` geliefert wird.
- Die App fragt den Status jede Sekunde ab, bildet daraus eine **geglättete
  Rate** (gleitender Mittelwert) und die **Restzeit** = offene Bytes / Rate.
- Der sendende Mac zählt die ausgelieferten Bytes (`served_bytes`) und zeigt
  „Sende an *Mac A* · 12 MB/s“.
- Anzeige: Menü („Empfange 42 % · noch ca. 2 Min.“) und Kopf des Managers
  (Fortschrittsbalken, „3 von 120 Dateien · 120 MB von 290 MB · 25 MB/s ·
  notes/video.mov“).

## 5. Menüleisten-Symbol

- Pfeile + farbiger Zustandspunkt (grün ✓, blau …, orange ❙❙ / !, rot ! / ✕).
- **Drehung:** solange synchronisiert wird, dreht die App die Pfeile in
  12 Schritten pro 1,2 s (Bildfolge, kein Dauer-Rendering; ruht sonst).

## 6. Bedienung des Managers

Öffnen: Menü → **Synchronisations-Manager …** (⌘M) oder
`open -a MacSync --args --manager`.

```
┌ Empfange 42 % · noch ca. 2 Min. ───────────────────────────────────────┐
│ ████████████░░░░░░░  3 von 120 Dateien · 120 MB von 290 MB · notes/a.mov│
├────────────────────────────────────────────────────────────────────────┤
│ ‹  MacSync › notes          [Alle | Nicht synchron]  [Suchen    ] │
├──────────────┬────────────┬──────────────┬─────────────────────┬──────┤
│ Name         │ Dieser Mac │ MacBook M4   │ Status              │ Größe│
│ 📁 sub       │ ✓ vorhanden│ ✓ vorhanden  │ ✓ Synchron          │  –   │
│ 📄 a.txt     │ ● neuer    │ ✓ vorhanden  │ → Wird gesendet     │ 2 KB │
│ 📄 neu.txt   │ – fehlt    │ ● neuer      │ ← Wird geholt       │ 4 KB │
│ 📄 alt.txt   │ 🗑 gelöscht │ ✓ vorhanden  │ Wird dort gelöscht  │  –   │
├────────────────────────────────────────────────────────────────────────┤
│ 24 Einträge · 3 nicht synchron     [Im Finder] [Jetzt synchronisieren] │
└────────────────────────────────────────────────────────────────────────┘
```

- **Verzeichnisbaum links** (wie im Finder): Ordner aufklappen, anklicken →
  rechts der Inhalt. Ein-/ausblenden mit dem Seitenleisten-Knopf oder ⌃⌘S,
  Breite per Trennlinie verstellbar (die Einstellung bleibt erhalten).
  Ordner mit offenen Änderungen tragen einen Punkt (blau = Änderungen,
  orange = Löschung, rot = Konflikt). Der Baum lädt jeden Ordner erst beim
  Aufklappen – auch große Verzeichnisse bleiben schnell.
- Doppelklick: Ordner öffnen bzw. Datei öffnen; Pfadleiste oben zum Zurückspringen.
- Filter **Nicht synchron** zeigt nur, was noch offen ist.
- Kontextmenü / Fußleiste pro Auswahl:
  - **Jetzt synchronisieren** – erfasst sofort und holt die betroffenen Einheiten.
  - **Pausieren / Fortsetzen** – für Hauptordner (Einheiten).
  - **Konflikt ansehen …** – öffnet die Konfliktansicht (Abschnitt 7) mit
    diesem Konflikt.
  - **Konflikt lösen** – Meine Version / Version des anderen Macs / Beide behalten.
  - **Im Finder zeigen**, **Öffnen**.
  - **In den Papierkorb …** – mit Rückfrage; die Löschung wird auf dem anderen
    Mac nachgezogen (dort 14 Tage im MacSync-Papierkorb).

## 7. Konflikte

Ein Konflikt entsteht, wenn beide Macs denselben Pfad geändert haben, bevor
abgeglichen werden konnte. MacSync überschreibt dann nichts: die eigene
Fassung bleibt an ihrem Platz, die des anderen Macs liegt in der
Konfliktablage (`<state_dir>/conflicts/…`), bis entschieden ist.

**Öffnen:** Menü → **Konflikte …** (⌘K, mit Anzahl), im Manager die rote
Schaltfläche „n Konflikte“ bzw. Kontextmenü **Konflikt ansehen …**, Klick
auf die Mitteilung „MacSync: Konflikt“, oder
`open -a MacSync --args --conflicts [id]`.

```
┌ [Offen | Erledigt]  Art: [Alle Arten ▾]                       [Suchen   ] ┐
├─────────────────┬──────────────────────────────────┬───────────────────────┤
│ Alle Ordner   4 │ Datei          Art          Seit │ Ideen.md              │
│ Ordner          │ index.html     ⑂ Auf beiden …    │ ⑂ Auf beiden Macs     │
│  Notizen      3 │ todo.md        Aa Groß-/Klein…   │   geändert            │
│  Projekte     1 │ Ideen.md       ⑂ Auf beiden …    │ Erklärung …           │
│                 │ Einkauf.md     ⑂ Auf beiden …    │ ┌ Dieser Mac ───────┐ │
│                 │                                  │ │ 37 Byte · 17:14   │ │
│                 │                                  │ │ Öffnen 🔍 [Diese behalten]│
│                 │                                  │ ┌ MacBook Air ──────┐ │
│                 │                                  │ │ Öffnen 🔍 [Diese übernehmen]│
│                 │                                  │ [Vergleichen] [Beide behalten]│
├─────────────────┴──────────────────────────────────┴───────────────────────┤
│ 4 Konflikte                [Alle auswählen] [Im Manager zeigen] [↻]        │
└────────────────────────────────────────────────────────────────────────────┘
```

- **Links** die Ordner mit offenen Konflikten (Anzahl), oben Filter nach
  **Art** und Suche.
- **Rechts** der gewählte Konflikt: Art und Erklärung in Klartext, beide
  Fassungen mit Größe, Änderungsdatum und Markierung **neuer**; jede lässt
  sich öffnen oder im Finder zeigen (die des anderen Macs als Kopie in der
  Konfliktablage). **Vergleichen** öffnet FileMerge, wenn Xcode installiert
  ist, sonst beide Dateien.
- **Lösen:** „Diese behalten“ / „Diese übernehmen“ / „Beide behalten“ (die
  andere Fassung kommt als `name.conflict-<gerät>-<zeit>.ext` daneben).
  Ersetzte Fassungen bleiben 14 Tage im MacSync-Papierkorb.
- **Viele Konflikte auf einmal:** Ordner (und ggf. Art) wählen, **Alle
  auswählen** (⌘A), rechts „Alle: Version dieses Macs behalten“ bzw. „Alle:
  Version von … übernehmen“. Jede Einheit wird dafür nur einmal erfasst.
- **Erledigt** zeigt gelöste Konflikte mit Entscheidung und Zeitpunkt
  („überholt“: hat sich durch eine neuere Fassung selbst erledigt).

| Art | Bedeutung | Empfehlung |
| --- | --- | --- |
| Auf beiden Macs geändert (`concurrent-modify`) | Inhalt auf beiden Seiten geändert, oder hier geändert und dort gelöscht | Vergleichen; die richtige Fassung behalten oder beide |
| Datei / Ordner (`kind-change`) | Hier Datei, dort Ordner/Link | Die gewünschte Seite wählen |
| Git-Zustand (`git-state`) | HEAD/Branches/Index beider Macs geändert; nur als Ganzes übernehmbar | Auf einem Mac committen, dann dessen Stand übernehmen |
| Groß-/Kleinschreibung (`case-collision`) | `todo.md` hier, `TODO.md` dort – für macOS derselbe Name | Die Seite mit der gewünschten Schreibweise übernehmen (Name wird mit übernommen) |

Im Terminal: `macsync conflicts`, `macsync resolve <id>… --keep
local|remote|both`, `macsync resolve --all [--unit <Ordner>] --keep …`.

## 8. Grenzen und nächste Schritte

- **Pausieren einzelner Dateien/Unterordner** ist bewusst noch nicht enthalten:
  Ein Ausschluss gilt heute nur auf einem Mac; der andere würde die Datei
  weiter anbieten, der Zustand bliebe dauerhaft „abweichend“. Richtig wäre eine
  zwischen beiden Macs abgeglichene Ausschlussliste (Protokollerweiterung).
  Bis dahin: Pausieren pro Hauptordner.
- **Ordner-Zusammenfassung** („n Dateien ausstehend“, Prozent pro Ordner) –
  erfordert Zählung im Merkle-Abstieg; heute zeigt ein Ordner „Änderungen“.
- **Finder-Abzeichen** über eine Finder-Sync-Erweiterung, sobald eine
  Developer-ID vorhanden ist.
- **Warteschlange** aller ausstehenden Übertragungen und Bandbreitenlimit.
