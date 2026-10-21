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
