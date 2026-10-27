# Duplikate finden

`macsync dupes` zeigt, welche Ordner und Dateien unter **gleichem Namen**
mehrfach vorkommen und **welche Kopie die aktuelle ist**. Es liest nur –
nichts wird verschoben, geändert oder gelöscht (Ausnahme: `--apply`, siehe
unten). Eine Konfiguration oder ein
gekoppelter Mac ist nicht nötig.

```sh
macsync dupes ~/Dokumente                  # Duplikate innerhalb eines Ordners
macsync dupes /Volumes/Backup ~/Dokumente  # ALT gegen NEU: was aus ALT kann weg?
macsync dupes ~/Dokumente --all            # alle Treffer statt der grössten
macsync dupes ~/Dokumente --json           # maschinenlesbar (für die App)
make dupes DIR=~/Dokumente [DIR2=…] [ARGS=-v|-vv|--all]
```

## Ältere mit neuerer Kopie vergleichen

```sh
macsync dupes ALT NEU        # pro Hauptordner eine Zeile
macsync dupes ALT NEU -v     # dazu die Unterordner, die nicht weg können
macsync dupes ALT NEU -vv    # dazu die einzelnen Dateien
```

Mit zwei Ordnern beantwortet `dupes` eine Frage: **Was aus ALT kann weg?**
Der erste Ordner gilt als die ältere Kopie, der zweite als die neuere.
Dateien werden über ihren relativen Pfad gepaart (Gross-/Kleinschreibung
zählt nicht), Inhalte per Hash verglichen, wo die Grösse gleich ist.

| Datei | Bedeutung | geht beim Löschen von ALT verloren? |
| --- | --- | --- |
| ✅ | identisch in NEU | nein |
| 🔼 | in NEU neuer | nein – die Version in ALT ist veraltet |
| 🔀 | in NEU an anderer Stelle (gleicher Inhalt) | nein |
| ❗ | in ALT neuer | **ja** |
| ❓ | anderer Inhalt, gleiches Datum | unklar – prüfen |
| 🟥 | nur in ALT | **ja** – oder in NEU bewusst gelöscht |
| ➕ | nur in NEU | – (neu hinzugekommen) |
| ❌ | nicht lesbar | unklar – nie löschbar |

Ein Ordner ist **🚮 kann weg**, wenn darunter nichts verloren ginge (✅
identisch, wenn zusätzlich nichts neuer oder neu ist), sonst **🟧 prüfen**;
🟥 und ➕ stehen für Ordner, die es nur auf einer Seite gibt.

**Reihenfolge prüfen:** Unter den Dateien, die sich unterscheiden, wird
gezählt, auf welcher Seite sie neuer sind. Sind die meisten in ALT neuer,
gibt es eine Warnung – der Bericht bleibt aber bei den angegebenen Rollen.

Ein **unlesbarer Startordner** bricht den Vergleich ab (macOS: dem Terminal
Festplattenvollzugriff geben). Die frühere Suche nach gleichnamigen Ordnern
über beide Ordner hinweg gibt es weiter mit `--within`.

## Übernehmen: `--files` und `--apply`

```sh
macsync dupes ALT NEU --json --files            # auch die identischen Dateien
macsync dupes ALT NEU --apply PLAN.json [--json] # einen Plan ausführen
```

`--files` listet in `entries` auch die gleichen Dateien (`state: "same"`),
damit die App den ganzen Baum zeigen kann. Fehlt eine Datei in NEU, während
die meisten Dateien ihres Ordners dort unter einem anderen Ordner liegen,
nennt `suggested` den passenden Zielpfad; `path_new` steht da, wo NEU den
Pfad anders schreibt.

`--apply` führt einen Plan aus (`copy`, `move`, `replace`, `keep_both`; `-`
liest ihn von stdin). Es wird nie überschrieben oder endgültig gelöscht:
Kopien gehen über eine temporäre Datei, werden per BLAKE3 bestätigt und
behalten Datum, Rechte und erweiterte Attribute; ersetzte NEU-Dateien und
verschobene ALT-Quellen landen im macOS-Papierkorb (`/usr/bin/trash`, sonst
`~/.Trash`; `MACSYNC_TRASH_DIR` lenkt das für Tests um). Hat sich eine Datei
seit dem Vergleich geändert, wird die Aktion übersprungen. Format und Ausgabe:
[reconcile.md](reconcile.md).

## Der typische Fall

Ein Arbeitsordner wurde irgendwann komplett kopiert – als Backup, auf einen
anderen Mac, an einen neuen Ort. Danach wurde in einer Kopie (oder in beiden)
weitergearbeitet. Die Frage ist: Welche Kopie ist aktuell, und kann die andere
weg?

## Wie es entscheidet

1. **Ordner gleichen Namens** werden über ihre relativen Dateipfade gepaart
   (`Projekt/docs/plan.md` ↔ `Backup/Projekt/docs/plan.md`). Ein Paar zählt,
   wenn mindestens die Hälfte der Pfade des kleineren Ordners im anderen
   vorkommt. Gross-/Kleinschreibung zählt nicht (wie bei APFS).
2. **Inhalte** werden per Hash verglichen – aber nur, wo Name *und* Grösse
   übereinstimmen. Unterschiedliche Grösse heisst sicher „anders“, ohne die
   Datei zu lesen.
3. **Neuer** heisst: späteres Änderungsdatum. Unterschiedlicher Inhalt bei
   gleichem Datum (±2 s) wird als **unklar** gemeldet, nicht geraten.
4. Jedes Paar bekommt ein Urteil:

| Urteil | Bedeutung | Empfehlung |
| --- | --- | --- |
| `IDENTICAL` | Gleiche Dateien, gleicher Inhalt | eine Kopie kann weg |
| `NEWER` | Die erste Kopie enthält alles der zweiten, plus Neues oder Neueres | die zweite (ältere) kann weg |
| `DIVERGED` | Beide haben etwas, das der anderen fehlt | **erst zusammenführen** – die Liste zeigt genau, was wo fehlt |

5. Das **äusserste** Paar gewinnt: Ist `Arbeit` ↔ `Backup/Arbeit` gemeldet,
   erscheinen die Unterordner darin nicht noch einmal einzeln.
6. **Kopier-Zeitpunkt:** Wurden die meisten Dateien eines Ordners innerhalb
   weniger Minuten angelegt, wurde er dann dorthin kopiert oder verschoben.
   Der Bericht nennt diesen Zeitpunkt und zählt, wie viele Dateien auf jeder
   Seite *danach* geändert wurden – daran sieht man, in welcher Kopie
   weitergearbeitet wurde. (Der Finder übernimmt beim Kopieren das
   Erstelldatum; dann gibt es keinen erkennbaren Kopier-Zeitpunkt.)
7. **Einzelne Dateien** gleichen Namens, die nicht schon durch ein Ordnerpaar
   erklärt sind, erscheinen unter `FILES`: entweder als identische Kopien
   (`frees …`) oder als Versionen (`v1`, `v2`, … – neueste markiert).

Beispiel:

```
 1. NEWER      Arbeit/Projekt   ← current
    OLDER      Backup 2021/Projekt
    → the current copy contains everything of the older one: it can go (frees 1.2 GB)
    148 identical · 3 newer in Arbeit/Projekt · 12 only in Arbeit/Projekt
    Backup 2021/Projekt: 151 of 151 files created 2021-03-12 14:02 – copied or moved here
    changed after that: 15 in Arbeit/Projekt, 0 in Backup 2021/Projekt
```

## Was nicht angeschaut wird

- Mac-Metadaten (`.DS_Store`, `.Spotlight-V100`, …), `.git`, `node_modules`,
  Build- und Cache-Ordner (`.build`, `DerivedData`, `__pycache__`, `.venv`,
  `.terraform`, …) sowie jeder Ordner mit einer `CACHEDIR.TAG` (z. B. Rusts
  `target`). `--no-default-excludes` schaltet das ab, `--exclude GLOB` nimmt
  weitere Namen aus.
- Symbolische Links (würden Schleifen und Schein-Duplikate erzeugen).
- **iCloud-Platzhalter** werden nie heruntergeladen: Sie werden nur über
  Grösse und Datum verglichen und als solche markiert.
- Leere Dateien (überall gleich, ohne Aussage).
- Harte Links gelten als identisch, geben beim Löschen aber keinen Platz frei
  – das wird bei `frees` berücksichtigt.

## Grenzen (bewusst, vorerst)

- Nur **gleiche Namen**: Ein Ordner `Projekt alt` wird nicht mit `Projekt`
  gepaart; seine Unterordner gleichen Namens aber schon.
- Gibt es einen Ordner mehr als zweimal, erscheint jede Kombination als
  eigenes Paar.
- Mehr als 200 Ordner gleichen Namens (z. B. `src`) werden nicht verglichen;
  der Bericht nennt sie unter `note:`.
- Eine bewusst gelöschte Datei in der neueren Kopie sieht aus wie „nur in der
  älteren“ – das Urteil wird dann `DIVERGED`. Lieber einmal zu viel
  nachschauen als etwas verlieren.

## Nächste Schritte

1. ✅ Erkennung (`macsync dupes`, Text und JSON)
2. Mit echten Ordnern testen, Regeln nachschärfen
3. App: Fenster „Duplikate …“ – Ordner wählen, Ergebnisse, Quick Look,
   „Im Finder zeigen“
4. Handeln: „nur hier“-Dateien in die aktuelle Kopie übernehmen ✅
   (`--apply`, App: Ordnerabgleich); ganze Kopien in den Papierkorb legen
