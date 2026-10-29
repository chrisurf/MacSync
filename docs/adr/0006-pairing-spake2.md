# ADR-0006: Pairing per SPAKE2, Schlüssel als Datei

- Status: angenommen · Datum: 2026-10-20

## Kontext
Das Konzept nutzt Ed25519-Schlüssel im macOS-Schlüsselbund und SPAKE2 mit
6-stelligem Code.

## Entscheidung
- Identität: statisches X25519-Schlüsselpaar (für Noise), gespeichert in
  `<state_dir>/identity.key` mit Modus 0600. Geräte-ID = erste 16 Hex-Zeichen
  von BLAKE3(öffentlicher Schlüssel).
- Pairing: symmetrisches SPAKE2 (Ed25519-Gruppe) mit dem Code als Passwort.
  Aus dem gemeinsamen Schlüssel *K* wird je Rolle ein MAC
  `BLAKE3-keyed(K, rolle ‖ pubkey ‖ name)` gebildet; beide Seiten prüfen den
  MAC der Gegenseite, bevor der Schlüssel gespeichert wird. Ein falscher Code
  ergibt unterschiedliche *K* und damit ungültige MACs. Der Lauscher erlaubt
  höchstens 3 Versuche je Code.

## Konsequenzen
- Offline-Wörterbuchangriffe auf den Code sind durch SPAKE2 ausgeschlossen;
  online maximal 3 Rateversuche von 10⁶.
- Keychain-Speicherung ist eine reine Speicherfrage und kann ohne
  Protokolländerung nachgerüstet werden.

## Nachtrag 0.5.0
- Kontext `macsync-pair-v2`. Statt eines MAC wird der Payload mit
  ChaCha20-Poly1305 verschlüsselt (je Rolle ein Schlüssel aus dem
  SPAKE2-Secret, `blake3::derive_key`); ein Mitschnitt zeigt weder Namen noch
  Pfad. Das Secret wird genullt.
- Danach zeigen beide Seiten einen Fingerprint aus vier Wörtern
  (BLAKE3 über beide Public Keys, 256 Wörter je Byte). Die App speichert die
  Kopplung erst nach Bestätigung (`pair … --confirm`, `pair confirm`).
- Versuche: 3 je Adresse, 10 insgesamt, 3 s je Schritt. Ein Fremder kann das
  Fenster so nicht mehr schließen.
- Schlüssel bleibt eine Datei (0600, Verzeichnis 0700, Time-Machine-Ausnahme,
  Prüfung von Rechten und Schlüsselpaar beim Laden). Der Schlüsselbund folgt
  erst mit Developer-ID-Signatur: Bei ad-hoc signierten Builds hängt die
  Zugriffsfreigabe am Binary-Hash, und macOS würde nach jedem Update fragen.
