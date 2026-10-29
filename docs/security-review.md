# Sicherheits-Review: Austausch zwischen zwei Macs

Stand: 29.10.2026, Branch `feature/0.5.0` (Commit `a0e50fa`).
Geprüft: `crates/net` vollständig, `crates/engine/src/{serve,pull,apply}.rs`,
`crates/core/src/{path,proto}.rs`, `crates/gitaware/src/cmd.rs`,
`crates/daemon/src/{ipc,config,runtime}.rs`, Kopplung in `crates/cli/src/main.rs`.
`cargo test -p macsync-net`: grün.

## Ergebnis in einem Satz

Die Übertragung zwischen den Macs ist **vollständig Ende-zu-Ende verschlüsselt
und gegenseitig authentisiert**; Kryptografie und Protokollaufbau sind solide.
Die Schwachstellen liegen nicht in der Verschlüsselung, sondern darin, **wie viel
ein gekoppelter Peer auf dem anderen Mac anrichten kann**, und bei kleineren
Härtungen.

## Was geschützt ist

| Baustein | Umsetzung | Bewertung |
|---|---|---|
| Transport | `Noise_XX_25519_ChaChaPoly_BLAKE2s` über TCP (`snow`), Prolog bindet Protokollversion | Stark: Vertraulichkeit, Integrität, Forward Secrecy durch Ephemeral-Keys |
| Authentisierung | Statischer X25519-Schlüssel je Mac; Server prüft den Peer-Key gegen die Kopplung **vor** dem ersten Anwendungs-Request (`server.rs:110`), Client pinnt den erwarteten Key (`client.rs:109`) | Korrekt, gegenseitig |
| Kopplung | Symmetrisches SPAKE2 (Ed25519) mit 6-stelligem Code; daraus MAC-Schlüssel, der die öffentlichen Schlüssel beider Seiten mit Rollen-Label (`C`/`L`) bindet | Korrekt: kein Offline-Brute-Force, MITM braucht den Code, Reflexion verhindert |
| Rate-Limit | Max. 3 Versuche pro Kopplungsfenster → Ratechance ≈ 3 : 1 000 000 | Ausreichend |
| Nachrichten | Längen-Limits (64 MiB), zstd-Dekompression begrenzt (Bomb-Schutz) | Gut |
| Pfade vom Peer | `path::validate` verwirft absolute Pfade, `..`, `.`, `\`, NUL – auf Server- und Empfangsseite | Gut |
| Lokale Steuerung | Unix-Socket `0600`, Identitätsdatei `0600` | Gut |
| Abhängigkeiten | `cargo-deny` mit RustSec-Advisories in CI | Gut |

Verschlüsselt sind damit **alle Inhalte, Dateinamen, Metadaten und Git-Daten**
während der Synchronisation. Im Klartext sichtbar bleiben nur: Bonjour-Ankündigung
(Geräte-ID, im Kopplungsmodus auch der Gerätename), Ports, Datenmenge und Zeitpunkte.

## Befunde und Verbesserungsvorschläge

### Hoch

**H1 – Schreiben durch Symlink-Vorfahren möglich (aus Code-Lektüre, nicht per Test bestätigt).**
`apply_op` schreibt mit `fs::rename(staged, target)` und `create_dir_all(parent)`
(`engine/src/apply.rs`), ohne zu prüfen, ob ein Vorfahre von `target` ein Symlink
ist. Zusätzlich übernimmt `fetch_entries` (`engine/src/pull.rs`) bei
Exakt-Pfad-Anfragen **jeden** gültigen Pfad aus der Antwort, nicht nur die
angefragten. Ein bösartiger oder kompromittierter Peer könnte so `x → /Users/<user>`
als Symlink plus `x/.zshrc` als Datei liefern und außerhalb des Sync-Ordners schreiben.
*Vorschlag:* (a) in `fetch_entries` nur angefragte Pfade akzeptieren; (b) vor jedem
Schreiben/Löschen prüfen, dass kein Vorfahre unterhalb von `unit_dir` ein Symlink
ist (oder `openat`/`O_NOFOLLOW`-basiert arbeiten); (c) Regressionstest mit einem
manipulierten `PeerApi`.

**H2 – Gekoppelter Peer ≙ Codeausführung (Designfolge).**
Da `.git` komplett synchronisiert wird, kann ein Peer `.git/hooks/*` oder
`.git/config` (`core.fsmonitor`, `core.sshCommand`, Aliase …) setzen. Besonders:
MacSync selbst ruft `git update-index --refresh` auf (`gitaware/src/cmd.rs:28`) –
das führt einen in `.git/config` gesetzten `core.fsmonitor` **aus**. Der Kommentar
„Never run hooks“ stimmt nur teilweise; gesetzt wird lediglich `GIT_PAGER`.
*Vorschlag:* Git-Aufrufe mit `-c core.fsmonitor=false -c core.hooksPath=/dev/null`
härten; Änderungen an `.git/config` wie bei Hooks protokollieren und in der App
sichtbar warnen. In der Doku klar sagen: Koppeln heißt, dem anderen Mac voll zu vertrauen.

### Mittel

**M1 – Kopplungsdaten unverschlüsselt.** Der Pairing-Payload (Public Key,
Gerätename, Architektur, **absoluter Root-Pfad inkl. Benutzername**, Version) wird
nur per MAC geschützt, nicht verschlüsselt (`pairing.rs:162/187`). Passiv
mitlesbar im WLAN. *Vorschlag:* mit dem SPAKE2-Schlüssel per ChaCha20-Poly1305
verschlüsseln (AEAD statt MAC).

**M2 – Unbegrenzte Verbindungs-Threads.** `Server::start` startet pro TCP-Verbindung
einen Thread, die Handshake-Phase ist unauthentisiert (10 s Timeout, je 2 × 64 KiB
Puffer). Jeder Rechner im LAN kann so Threads/Speicher erschöpfen.
*Vorschlag:* Semaphore (z. B. max. 8 gleichzeitige unauthentisierte Handshakes),
kürzerer Handshake-Timeout.

**M3 – Kopplungsfenster per DoS blockierbar.** Drei falsche Versuche eines
Fremden beenden das Kopplungsfenster; mDNS-Spoofing kann in der Mac-Auswahl
einen fremden „Mac“ anzeigen. Sicherheit bleibt erhalten (Code nötig), aber die
Bedienung leidet. *Vorschlag:* nach erfolgreicher Kopplung auf beiden Seiten
einen kurzen Fingerprint (z. B. 4 Wörter aus dem Public Key) anzeigen.

**M4 – „Nur LAN“ ist keine Vertrauensgrenze.** `lan::is_local` erlaubt alle
RFC-1918-Adressen – also auch Café-, Hotel- und Firmen-WLANs. Der echte Schutz
ist Noise; der Daemon lauscht auf `0.0.0.0:47123`. *Vorschlag:* in der Doku so
benennen; optional auf die Schnittstelle des Heimnetzes einschränken.

### Niedrig

- **N1** Identitätsschlüssel liegt als Hex-Klartext auf der Platte (`identity.key`);
  landet in Time-Machine-Backups. Besser: Keychain (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`).
  Beim Laden wird außerdem nicht geprüft, ob Public Key zum Private Key passt
  und ob die Datei noch `0600` ist.
- **N2** Private Keys und SPAKE-Secret werden nicht genullt (`zeroize`).
- **N3** Kopplungscode als CLI-Argument (`pair connect <addr> <code>`) ist kurz in
  `ps` sichtbar; via stdin/Umgebung übergeben.
- **N4** Kein Rekeying in langen Noise-Sessions (Nonce-Raum unkritisch, aber
  `rekey` nach z. B. 1 GiB ist gute Praxis).
- **N5** Socket wird erst nach `bind` auf `0600` gesetzt (kurzes Rennfenster);
  unkritisch, solange das State-Verzeichnis `0700` ist – das sollte explizit
  gesetzt werden.
- **N6** Kontext-Strings heißen noch `reposync-pair-v1` – kosmetisch, aber beim
  nächsten Protokollbruch auf `macsync-pair-v2` umstellen.
- **N7** Releases ohne Secrets nur ad-hoc signiert; Nutzer müssen Gatekeeper
  umgehen. Developer-ID-Signatur + Notarisierung verbindlich machen.

## Empfohlene Reihenfolge

1. H1 (Symlink-Vorfahren + nur angefragte Einträge) mit Test.
2. H2-Härtung der Git-Aufrufe (zwei `-c`-Optionen, sehr geringer Aufwand).
3. M2 Verbindungslimit, M1 Pairing-Payload verschlüsseln.
4. N1 Keychain, Fingerprint-Anzeige (M3), Rest nach Gelegenheit.
