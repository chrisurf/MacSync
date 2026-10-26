# Make-Hilfe – zwei Macs verbinden

Alle `make`-Befehle im Repository-Ordner. Übersicht aller Targets: `make help`.

## Mit der App (empfohlen)

```sh
make install-app       # baut MacSync.app + Installer, installiert nach /Programme
make app               # nur bauen: build/MacSync.app
make pkg               # nur bauen: build/MacSync-<version>-arm64.pkg
```

Oder den Installer aus den GitHub-Releases nehmen. Danach alles über das
Symbol in der Menüleiste: Einrichtung, Zustand, Einstellungen, Beenden.
**Synchronisations-Manager …** (⌘M) zeigt jede Datei mit ihrem Zustand auf
beiden Macs; im Terminal: `macsync browse notes`.
Die folgenden Abschnitte betreffen den Betrieb im Terminal.

## Einmalig (beide Macs)

```sh
make doctor            # Voraussetzungen prüfen
make install           # baut macsync, installiert macsync + macsync-connect
```

## Verbinden (beide Macs, in Terminal.app)

```sh
macsync-connect       # oder: make connect
```

1. Das Skript fragt nach dem Verzeichnis, das synchronisiert werden soll
   (Enter = Vorschlag). Direkt angeben: `macsync-connect ~/MacSync`
   bzw. `make connect ROOT=~/MacSync`.
2. **Reihenfolge egal:** Der erste Mac wartet und zeigt einen 6-stelligen Code.
   Dann auf dem **anderen** Mac starten – er findet den ersten und fragt nach dem Code.
3. Danach startet die Synchronisation im Vordergrund.

Nur einmal nötig: Beim nächsten Mal ist die Kopplung bekannt, das Skript startet direkt.

Startreihenfolge egal – beim ersten Koppeln wie bei jedem späteren Start.
Gestartet wird immer nur der Daemon; die Macs finden sich selbst.

## Wo liegt was (je Mac, nichts davon wird synchronisiert)

| Ort | Inhalt | Löschen? |
| --- | --- | --- |
| `~/.config/macsync/config.toml` | Konfiguration (Verzeichnis, Modus, Port) | ja, `macsync-connect` legt sie neu an |
| `~/Library/Application Support/MacSync/` | Geräteschlüssel, Datenbank, Kopplung | nur zum kompletten Zurücksetzen (danach neu koppeln) |
| `<Sync-Verzeichnis>/.macsync/` | Zwischenablage für Übertragungen, Papierkorb | nur bei gestopptem Daemon; entsteht neu |

Komplett zurücksetzen (Daemon vorher stoppen):
`rm -rf ~/.config/macsync "~/Library/Application Support/MacSync" ~/MacSync/.macsync`

## Stoppen

| Situation | Befehl |
| --- | --- |
| Läuft im aktuellen Fenster | `Ctrl-C` |
| Läuft in einem anderen Fenster | `macsync-connect --stop` oder `make stop` |

## Anderes Verzeichnis / neu koppeln

```sh
macsync-connect --force ~/AnderesVerzeichnis   # Konfiguration ersetzen, neu koppeln
```

## Hilfe und Zustand

```sh
macsync-connect --help    # alle Optionen (oder: make connect-help)
make status                # Zustand beider Seiten
make log                   # was in der letzten Stunde passiert ist
```

## Häufige Optionen

| Option | Wirkung |
| --- | --- |
| `--peer NAME.local` / `--peer IP` | anderen Mac direkt angeben (ohne Bonjour) |
| `--send` / `--receive` | nur senden (Mac mit Daten) / nur empfangen |
| `--port N` | anderer Port (Standard 47123, Pairing N+1) |
| `--no-run` | nur einrichten und koppeln, nicht starten |

Mit make: `make connect ARGS="--send"`.

## Probleme

| Meldung | Abhilfe |
| --- | --- |
| `cargo: No such file or directory` | Rust fehlt/nicht im PATH → neues Terminal oder `brew install rustup` |
| `No route to host` | in Terminal.app starten; anderer Mac wach und im selben WLAN? |
| Bonjour findet nichts | `--peer NAME.local` oder IP angeben (`ipconfig getifaddr en0` auf dem anderen Mac) |
| Beide Macs warten | auf einem `Ctrl-C`, dort erneut starten |
| Falsches Verzeichnis vorgeschlagen | Verzeichnis direkt angeben: `macsync-connect ~/MacSync` |
