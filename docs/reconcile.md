# Ordnerabgleich (App)

Fenster der Menüleisten-App, das eine ältere Kopie (ALT, links) mit einer
neueren (NEU, rechts) vergleicht und fehlende oder neuere Dateien aus ALT nach
NEU übernimmt. Die Logik liegt in Rust (`macsync dupes`), die App zeigt nur an
und ruft das mitgelieferte `macsync` auf.

## Ablauf

1. **Auswahl:** ALT und NEU über „Auswählen …“ oder Drag & Drop, ⇄ tauscht.
   „Vergleichen“ startet `macsync dupes ALT NEU --json --files`.
2. **Reihenfolge:** Banner, wenn `order.status` `contradicted` oder `unknown`
   bei Gleichstand ist; „Seiten tauschen“ / „So lassen“.
3. **Ansicht:** ein gemeinsamer Baum, links ALT, Mitte Zustand, rechts NEU,
   dieselbe Datei in derselben Zeile, fehlende Seite als „—“. Standardfilter
   „Nur Offenes“; Filterchips je Zustand mit Anzahl.
4. **Details:** Größe, Datum, Zeilen (Text), „Unterschiede zeigen“ öffnet den
   Vergleich: Text zeilenweise (untereinander oder nebeneinander), PDF und
   Bilder nebeneinander.
5. **Plan:** Aktionen sammeln sich unten, „Vorschläge“ übernimmt die
   vorgeschlagenen, „Ausführen“ fragt einmal nach und ruft
   `macsync dupes ALT NEU --apply PLAN --json` auf; danach neue Prüfung.

## Aktionen

| Zustand | Aktionen | Vorschlag |
| --- | --- | --- |
| `only_old` | `copy`, `move` (Ziel: `suggested` oder derselbe Pfad) | `copy` |
| Ordner `only_old` (nicht in NEU) | `copy`/`move` mit `kind: "dir"` | `copy` |
| `old_newer` | `replace`, `keep_both`, nichts | `replace` |
| `unclear` | `replace`, `keep_both`, nichts | keiner |
| sonst | – | – |

Sicherheit: nie überschreiben oder endgültig löschen. Ersetzte NEU-Dateien
und verschobene ALT-Dateien kommen in den macOS-Papierkorb. Kopien behalten
Änderungsdatum, Rechte und erweiterte Attribute, werden über eine temporäre
Datei geschrieben und per BLAKE3 bestätigt. Vor jeder Aktion wird geprüft, ob
die Dateien noch so sind wie beim Vergleich (`expect`); sonst wird sie
übersprungen.

## Schnittstelle zur App

### Vergleich: `macsync dupes ALT NEU --json --files`

JSON wie bisher (`Comparison`, siehe `crates/dupes/src/compare.rs`). Mit
`--files` enthält `entries` auch die gleichen Dateien (`state: "same"`), damit
die App den ganzen Baum zeigen kann. Zusätzlich je Eintrag:

- `suggested` (nur `only_old`): vorgeschlagener Zielpfad in NEU, relativ.
  Liegen die meisten Dateien des ALT-Ordners in NEU verschoben unter einem
  anderen Ordner, zeigt er dorthin; sonst fehlt das Feld (= gleicher Pfad).
- `path_new` (wenn in NEU vorhanden und anders geschrieben): Pfad in NEU.
- `size_new` (wenn in NEU vorhanden, bei `moved` die Zieldatei): Größe in
  NEU; `size` ist die Größe in ALT (bei `only_new` die in NEU).

Pfade sind relativ zu ALT bzw. NEU, mit `/`. Zeiten in ms seit 1970.

### Ausführen: `macsync dupes ALT NEU --apply PLAN.json --json`

```json
{
  "actions": [
    { "op": "copy", "kind": "file", "path": "a/b.txt", "to": "a/b.txt",
      "expect_old": { "size": 12, "mtime_ms": 1700000000000 }, "expect_new": null },
    { "op": "move", "kind": "dir", "path": "altes-experiment", "to": "altes-experiment" },
    { "op": "replace", "kind": "file", "path": "x.py", "to": "x.py",
      "expect_old": { "size": 4, "mtime_ms": 1 }, "expect_new": { "size": 3, "mtime_ms": 0 } },
    { "op": "keep_both", "kind": "file", "path": "x.py", "to": "x.py" }
  ]
}
```

- `op`: `copy` | `move` | `replace` | `keep_both`; `kind`: `file` | `dir`
  (`dir` nur für `copy`/`move`).
- `path`: relativ zu ALT; `to`: relativ zu NEU (fehlt = `path`).
- `copy`/`move`: Ziel darf nicht existieren. `move` legt die Quelle nach
  bestätigter Kopie in den Papierkorb.
- `replace`: NEU-Datei in den Papierkorb, dann ALT-Datei an ihre Stelle.
- `keep_both`: ALT-Datei neben die NEU-Datei als `name (ALT JJJJ-MM-TT).ext`.
- `expect_old`/`expect_new`: optional; weichen Größe oder Änderungszeit ab,
  wird die Aktion übersprungen (`skipped`).

Ausgabe: eine JSON-Zeile je Aktion, sobald sie fertig ist, dann eine
Zusammenfassung:

```json
{"index":0,"path":"a/b.txt","to":"a/b.txt","result":"done","error":null,"trashed":null}
{"index":1,"path":"x.py","to":"x.py","result":"skipped","error":"changed since the comparison","trashed":null}
{"summary":true,"done":3,"skipped":1,"failed":0,"bytes":1234}
```

`to` ist das tatsächliche Ziel (bei `keep_both` der gewählte Name), `trashed`
der Ort im Papierkorb. `skipped`: geändert seit dem Vergleich, Quelle fehlt
oder Ziel existiert schon; `failed`: ungültiger Pfad (absolut, `..`, außerhalb
von NEU) oder Fehler beim Schreiben. `expect_*` gilt nur für `kind: "file"`.

`result`: `done` | `skipped` | `failed`. Exit-Code 0, wenn nichts `failed`
ist, sonst 1. Ohne `--json` dieselben Ergebnisse als lesbarer Text.

#### Fortschritt und Abbrechen

Vor der ersten Aktion kommt eine Zeile mit der Größe jeder Aktion (Ordner:
alle Dateien darin, nur per `stat`, nichts wird gelesen), während des
Kopierens höchstens fünfmal pro Sekunde eine Fortschrittszeile:

```json
{"start":true,"bytes_total":75120000000,"actions":[{"index":0,"bytes":12700000,"files":181}]}
{"progress":true,"index":0,"bytes_done":884000,"bytes":12700000,"file":"claude-code/.devcontainer/Dockerfile"}
```

`bytes_done` zählt innerhalb der Aktion `index`, `file` ist der Pfad in ALT,
der gerade kopiert wird. Große Dateien melden auch zwischendurch Fortschritt.

SIGTERM oder SIGINT bricht ab: die laufende Aktion hält nach spätestens 1 MB
an, ihre temporäre Kopie wird entfernt, NEU bleibt wie vorher. Sie und alle
folgenden Aktionen kommen mit `"result":"cancelled"`, die Zusammenfassung mit
`"cancelled":true`. Fertige Aktionen bleiben bestehen. Exit-Code wie oben
(Abbrechen allein ist kein Fehler).

#### ALT löschen: `trash_old`

```json
{ "op": "trash_old", "kind": "dir", "path": "photo-organizer-cli", "to": "photo-organizer-cli" }
```

Legt eine ALT-Datei oder einen ALT-Ordner in den macOS-Papierkorb, aber nur,
wenn NEU unter `to` (fehlt = `path`) von **jeder** Datei darin eine
identische Kopie hat: gleiche Größe und gleicher BLAKE3-Inhalt, Symlinks mit
gleichem Ziel. Zusätzliche Dateien in NEU stören nicht. Sonst `skipped` mit
der ersten abweichenden Datei (`"x/a.txt differs in NEU"`, `"… is missing in
NEU"`, `"not in NEU"`). Fortschritt zählt die geprüften Bytes; liegt ALT in
Google Drive, werden die Dateien dafür gelesen (heruntergeladen). NEU wird nie
verändert, `trashed` nennt den Ort im Papierkorb.

#### Dateien aus der Cloud (Google Drive, iCloud)

Platzhalter-Dateien (`dataless`: Inhalt noch in der Cloud) muss die
Cloud-App beim Lesen erst herunterladen. Läuft sie nicht oder ist sie
offline, schlägt das Lesen fehl (meist `os error 60`, Zeitüberschreitung).

- `start` und jede Größe in `actions` haben zusätzlich `cloud_files` und
  `cloud_bytes`: was davon erst geladen werden muss.
- Scheitert das Lesen einer Platzhalter-Datei, endet die Aktion `failed` mit
  `"code":"cloud_unavailable"` und einem `error`, der die Datei nennt. Alle
  folgenden Aktionen mit Platzhaltern werden dann ohne Versuch ebenso
  gemeldet (die Cloud liefert ja nicht).
- Vorab-Prüfung ohne Änderung: `macsync dupes ALT NEU --apply PLAN --probe
  --json` gibt eine Zeile aus und liest dafür nur die kleinste
  Platzhalter-Datei des Plans an (sie wird dabei heruntergeladen):

```json
{"probe":true,"bytes_total":12700000,"files":181,"cloud_files":120,"cloud_bytes":11900000,"cloud_ok":false,"error":"Google Drive/iCloud did not deliver claude-code/LICENSE.md: Operation timed out (os error 60)","actions":[{"index":0,"bytes":12700000,"files":181,"cloud_files":120,"cloud_bytes":11900000}]}
```

`cloud_ok`: `true` (liefert), `false` (liefert nicht, `error` sagt warum),
`null` (keine Platzhalter im Plan).

#### Was Ordner-Aktionen auslassen

Ordnerkopien, ihre Größen und die Prüfung von `trash_old` lassen aus, was
der Vergleich ignoriert und Werkzeuge neu erzeugen (`DEFAULT_EXCLUDES`:
`node_modules`, `.venv`, `__pycache__`, Caches, `.DS_Store` …, dazu Ordner
mit `CACHEDIR.TAG` wie `target`). **`.git` wird immer mitkopiert und geprüft**,
denn die Historie lässt sich nicht neu erzeugen. Weicht `.git` ab, bleibt
ALT stehen: `"repo/.git differs in NEU: ALT may have commits, branches or
stashes that NEU lacks – compare with git"`.
