# ADR-0002: Noise über TCP statt QUIC im MVP

- Status: angenommen · Datum: 2026-10-20

## Kontext
Das Konzept sieht QUIC (`quinn` + `rustls` mit Raw Public Keys) vor. Für genau
zwei Peers im LAN sind die QUIC-Vorteile (Multiplexing, Connection-Migration)
nachrangig; Raw-Public-Key-Unterstützung in `rustls` erfordert zudem eigene
Verifier und erhöht die Angriffsfläche durch eigenen Krypto-Klebecode.

## Entscheidung
Transport ist TCP mit dem Noise-Protokoll `Noise_XX_25519_ChaChaPoly_BLAKE2s`
(Crate `snow`). Jeder Daemon besitzt ein statisches X25519-Schlüsselpaar. Nach
dem Handshake wird der statische Schlüssel der Gegenseite gegen den beim
Pairing gespeicherten Schlüssel geprüft (Pinning, beidseitig). Nachrichten
werden in Noise-Frames ≤ 65 535 Byte zerlegt; logische Nachrichten werden ab
4 KiB mit zstd komprimiert, wenn das kleiner ist.

## Konsequenzen
- Gegenseitige Authentifizierung und Vorwärtssicherheit wie bei TLS 1.3.
- Keine Connection-Migration: Bei Schnittstellenwechsel wird neu verbunden;
  Transaktionen sind ohnehin idempotent und werden wiederholt.
- Der `PeerApi`-Trait kapselt den Transport; ein Wechsel auf QUIC/iroh betrifft
  nur `reposync-net`.
