# Auftrag: Sicherheits-Härtung für MacSync 0.5.0

Grundlage: [security-review.md](security-review.md). Branch `feature/0.5.0`.

## Bedrohungsmodell (bitte so umsetzen)

- **Beide Macs gehören derselben Person.** Der gekoppelte Mac ist vertrauenswürdig.
  Schutz gegen ihn ist nur Schadensbegrenzung, falls ein Mac kompromittiert ist
  oder ein Bug falsche Daten liefert.
- **Hauptgegner ist ein Fremder im selben Netz:** Café, Coworking, Hotel,
  Firmen-WLAN. Er kann mitlesen, Verbindungen aufbauen, Bonjour fälschen und
  den Mac mit Anfragen fluten.
- Inhalte sind schon heute durch Noise verschlüsselt. Diese Punkte zielen auf
  **Sichtbarkeit, Angriffsfläche, Verfügbarkeit und Fehlertoleranz**.

Jeder Punkt gibt an, was in 0.5.0 umgesetzt wird, wie es ideal aussähe und woran
er als erledigt gilt. Reihenfolge = Priorität.

---

## 1. Fremde im Netz stumm abweisen (Noise KK statt XX)

**Problem:** Mit `Noise_XX` kann jeder im WLAN einen Handshake beginnen.
Der Mac antwortet mit seinem verschlüsselten, aber identifizierbaren statischen
Schlüssel und arbeitet den Handshake ab, bevor er prüft, ob der andere
gekoppelt ist (`net/src/secure.rs`, `server.rs:109`). Fremde können den Mac so
wiedererkennen und verfolgen.

**0.5.0:**
- Auf `Noise_KK_25519_ChaChaPoly_BLAKE2s` umstellen. Beide kennen den Schlüssel
  des anderen aus der Kopplung. Wer den Schlüssel des Servers nicht kennt, kann
  keine gültige erste Nachricht bauen. Der Server bricht dann ohne Antwort ab.
- `Server::start` bekommt den gekoppelten Peer-Key (oder ein Lookup) statt des
  nachträglichen `Authorizer`. Kein Peer gekoppelt → Port nicht öffnen.
- `PROTOCOL_VERSION` 2 → 3, Prolog entsprechend. Im CHANGELOG vermerken:
  „Beide Macs auf 0.5.0 aktualisieren“.

**Ideal:** zusätzlich ein PSK aus der Kopplung (`Noise_KKpsk0`). Dann wäre die
Sitzung auch dann noch geschützt, wenn ein statischer Schlüssel geleakt ist.

**Fertig, wenn:**
- Ein Test zeigt, dass ein Client mit fremdem Schlüssel oder ohne Kenntnis des
  Server-Keys keine einzige Antwort-Nachricht erhält.
- Die bestehenden Netzwerk- und e2e-Tests laufen grün.

## 2. Verbindungsflut begrenzen

**Problem:** `Server::start` startet für jede TCP-Verbindung einen Thread,
noch vor jeder Authentisierung (10 s Timeout, je 2 × 64 KiB). Jeder im Café
kann so Threads und Speicher erschöpfen.

**0.5.0:**
- Höchstens 4 gleichzeitige Verbindungen, die sich noch nicht ausgewiesen haben,
  und höchstens 4 gekoppelte. Alles darüber sofort schließen.
- Handshake-Timeout 3 s.
- Pro Quell-IP nach 5 fehlgeschlagenen Handshakes 60 s Sperre (einfache
  `HashMap<IpAddr, (count, until)>`).
- Für den Pairing-Listener gilt dasselbe.

**Ideal:** Der Listener ist nur offen, wenn der Peer im selben Netz gefunden
wurde oder eine eigene Verbindung aussteht.

**Fertig, wenn:**
- Ein Test mit 200 parallelen Rohverbindungen den Daemon nicht stört.
- Ein gekoppelter Client sich währenddessen verbinden kann.

## 3. In fremden Netzen unsichtbar (Bonjour)

**Problem:** `_macsync._tcp` kündigt dauerhaft die feste Geräte-ID und den
Hostnamen `macsync-<device-id>.local` an (`net/src/discovery.rs`). Im Café sieht
jeder: „Dieser MacSync-Mac ist da“, und kann ihn über Tage wiedererkennen.
Im Kopplungsmodus kommt der Gerätename im Klartext dazu.

**0.5.0:**
- Statt der Geräte-ID eine **rotierende, geblendete ID** ankündigen:
  `blake3_keyed(K, "macsync-mdns-v1" ‖ stunde)[..8]`. Dabei ist `K` ein
  X25519-DH aus eigenem privatem und Peer-öffentlichem Schlüssel
  (`x25519-dalek`), abgeleitet per `blake3::derive_key`.
  - `find()` prüft die aktuelle und die vorige Stunde.
  - Hostname neutral (`macsync-<geblendete-id>.local`).
- Pairing-Ankündigung nur während des Kopplungsfensters (wie heute). Statt des
  vollen Gerätenamens nur den Modell- oder Vornamen senden; in der App
  einstellbar.
- Einstellung „In unbekannten Netzen pausieren“ (Standard: aus). Ist sie
  aktiv, gibt es keine Ankündigung und keinen offenen Port, solange der Peer
  nicht erreichbar ist.

**Ideal:**
- Vertrauenswürdige Netze merken, erkannt am Default-Gateway (MAC-Adresse des
  Routers; braucht keine Ortungsfreigabe wie die SSID).
- In fremden Netzen: Port zu, nur ausgehend zum Peer verbinden.
- Menüleiste zeigt „Fremdes Netz – MacSync unsichtbar“.

**Fertig, wenn:**
- Ein Mitschnitt (`dns-sd -B _macsync._tcp`) keine stabile ID mehr zeigt.
- Beide Macs sich weiterhin automatisch finden.

## 4. Kopplung absichern (gerade im offenen WLAN)

**Problem:**
- Der Kopplungs-Payload ist nur per MAC geschützt, nicht verschlüsselt
  (`net/src/pairing.rs:162/187`). Gerätename, Architektur, Version und der
  absolute Sync-Pfad mit Benutzername sind mitlesbar.
- Drei Fehlversuche eines Fremden beenden das Kopplungsfenster.
- Der Code steht als CLI-Argument in `ps`.

**0.5.0:**
- Payload mit ChaCha20-Poly1305 (Schlüssel per `derive_key` aus dem
  SPAKE2-Secret, getrennte Schlüssel je Rolle) verschlüsseln, statt nur einen
  MAC anzuhängen.
- Kontext-Strings auf `macsync-pair-v2` umstellen.
- Nach erfolgreicher Kopplung zeigen beide Seiten einen **Fingerprint aus
  4 Wörtern** (aus beiden Public Keys).
  - Die App zeigt „Stimmt mit dem anderen Mac überein?“; erst danach wird
    gespeichert.
  - Die CLI gibt den Fingerprint aus.
- Fehlversuche von fremden IPs zählen pro IP. Das Fenster bleibt für den
  richtigen Mac offen (Gesamtlimit 10, pro IP 3).
- `pair connect` liest den Code von stdin oder aus `MACSYNC_PAIR_CODE`.
  Die App übergibt ihn so.

**Ideal:** Kopplung per QR-Code oder Bluetooth-Nähe statt 6 Ziffern. In
fremden Netzen vor dem Koppeln warnen („Am besten zu Hause koppeln“).

**Fertig, wenn:**
- Tests für den verschlüsselten Payload und das Limit pro IP laufen.
- Ein Paketmitschnitt der Kopplung weder Pfad noch Gerätenamen zeigt.

## 5. Nur innerhalb des Sync-Ordners schreiben

**Problem:**
- `apply_op` (`engine/src/apply.rs`) schreibt per `rename`/`create_dir_all`,
  ohne zu prüfen, ob ein übergeordnetes Verzeichnis ein Symlink ist.
- `fetch_entries` (`engine/src/pull.rs`) übernimmt bei Exakt-Pfad-Anfragen auch
  nicht angefragte Einträge.
- Ein fehlerhafter oder kompromittierter Mac könnte so außerhalb des
  Sync-Ordners schreiben, etwa in `~/.zshrc`.

**0.5.0:**
- In `fetch_entries` bei Exakt-Pfaden nur Einträge akzeptieren, deren Pfad
  angefragt wurde.
- Neue Funktion `macsync_fs::ensure_no_symlink_ancestor(unit_dir, rel)`.
  Aufruf vor jedem Write, Mkdir, Symlink, Rename, Delete und Meta. Verletzung
  → Op überspringen und als Konflikt bzw. Warnung protokollieren.
- Beim Planen einen Eintrag verwerfen, dessen Vorfahre in derselben Lieferung
  ein Symlink ist.

**Ideal:** Dateioperationen über `openat`/`O_NOFOLLOW` relativ zu einem
Verzeichnis-Handle des Sync-Ordners (TOCTOU-frei).

**Fertig, wenn:**
- Ein Regressionstest mit manipuliertem `PeerApi` (liefert `x → $TMP/outside`
  und `x/datei`) beweist, dass außerhalb nichts entsteht.
- Ein Test zeigt, dass nicht angefragte Einträge ignoriert werden.

## 6. Git-Aufrufe von MacSync härten

**Problem:** Der Daemon ruft `git update-index --refresh` und `git fsck` auf
(`gitaware/src/cmd.rs`). Ein synchronisiertes `.git/config` mit
`core.fsmonitor` wird dabei ausgeführt. Der Kommentar „Never run hooks“
stimmt so nicht.

**0.5.0:**
- In `git()` immer `-c core.fsmonitor=false -c core.hooksPath=/dev/null`
  setzen, außerdem `GIT_CONFIG_NOSYSTEM=1`.
- Änderungen an `.git/config` und `.git/hooks/*` wie heute die Hooks
  protokollieren und im Manager als Hinweis zeigen.

**Ideal:** Optional `.git/hooks` und `.git/config` vom Sync ausnehmen
(Einstellung). Standard bleibt Mitsynchronisieren, weil beide Macs der Person
gehören.

**Fertig, wenn:** Ein Test mit `core.fsmonitor = "touch $TMP/pwned"` nach
`refresh_index` keine Datei erzeugt.

## 7. Geräteschlüssel in den Schlüsselbund

**Problem:**
- `identity.key` liegt als Hex-Klartext im State-Verzeichnis
  (`net/src/identity.rs`) und landet in Time-Machine-Backups.
- Schlüssel werden im Speicher nicht gelöscht.
- Beim Laden wird weder das Schlüsselpaar noch die Dateiberechtigung geprüft.

**0.5.0:**
- Privaten Schlüssel in die macOS-Keychain legen
  (`security-framework`, `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`,
  nicht synchronisierbar).
- Vorhandene `identity.key` beim ersten Start migrieren und danach löschen.
  Fallback auf die Datei nur unter Linux/CI.
- Beim Laden: Public Key aus dem Private Key neu berechnen und vergleichen.
- Hat die Datei nicht `0600`, abbrechen.
- `zeroize` für Identity, SPAKE-Secret und abgeleitete Schlüssel.
- State-Verzeichnis und Socket-Verzeichnis explizit auf `0700` setzen.

**Ideal:** Schlüssel in der Secure Enclave. Für X25519 nicht direkt möglich,
daher Keychain mit Gerätebindung als realistisches Ziel.

**Fertig, wenn:**
- Nach dem Update keine `identity.key` mehr existiert.
- Die Kopplung bleibt erhalten (gleiche Geräte-ID).
- Der Test für eine manipulierte Datei schlägt fehl.

---

## Nicht Teil dieses Auftrags (später)

- Noise-Rekeying in langen Sitzungen
- Developer-ID-Signatur und Notarisierung verbindlich machen (Release-Prozess)

## Abschluss

- `make` / `cargo test --workspace` / `tests/e2e/two_daemons.sh` grün.
- CHANGELOG 0.5.0, Abschnitt „Sicherheit“, mit Hinweis auf den
  Protokollwechsel (beide Macs aktualisieren).
- ADR-0002 (Transport) und ADR-0006 (Pairing) fortschreiben bzw. neue ADR
  „Noise KK und geblendete Ankündigung“.
