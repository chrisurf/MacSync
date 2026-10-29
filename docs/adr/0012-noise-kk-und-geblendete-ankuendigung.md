# ADR-0012: Noise KK und geblendete Ankündigung

- Status: angenommen · Datum: 2026-10-29

## Kontext
Der Hauptgegner ist ein Fremder im selben Netz (Café, Hotel, Firmen-WLAN). Mit
`Noise_XX` konnte er einen Handshake beginnen und bekam den (verschlüsselten,
aber wiedererkennbaren) statischen Schlüssel zu sehen; Bonjour kündigte
dauerhaft die feste Geräte-ID an. Beides machte einen MacSync-Mac über Tage
verfolgbar. Jede TCP-Verbindung kostete zudem einen Thread.

## Entscheidung
- **Noise KK** (`Noise_KK_25519_ChaChaPoly_BLAKE2s`, Protokoll 3): Beide Seiten
  kennen den statischen Schlüssel des anderen aus der Kopplung. Eine erste
  Nachricht, die nicht für genau dieses Schlüsselpaar gebaut ist, scheitert
  beim Entschlüsseln; der Server schließt ohne ein einziges Byte Antwort.
  `Server::start` bekommt den Peer-Key; ohne Kopplung ist kein Port offen.
  Kein PSK (`KKpsk0`): bestehende Kopplungen haben keinen, ein PSK hätte eine
  Neu-Kopplung erzwungen.
- **Limits:** höchstens 4 Verbindungen im Handshake. Ist das Kontingent voll,
  wird die *älteste* geschlossen, nicht die neue: Ein echter Handshake dauert
  Millisekunden, so verdrängt eine Flut stummer Verbindungen sich selbst und
  nicht den gekoppelten Mac. 3 s Handshake, 5 Fehlschläge je Adresse → 60 s
  Sperre, höchstens 8 gekoppelte Verbindungen (Synchronisation, Status des
  Managers, bis zu 3 zusätzliche für große Übertragungen, Reserve).
- **Geblendete Ankündigung:** `K = derive_key("macsync-mdns-v1 key",
  X25519(eigener privater, Peer-öffentlicher Schlüssel))`; angekündigt wird
  `BLAKE3-keyed(K, "macsync-mdns-v1" ‖ eigener Public Key ‖ Stunde)[..8]` als
  Instanz- und Hostname. Der Public Key des Ankündigenden trennt die beiden
  Richtungen. `find()` akzeptiert vorige, aktuelle und nächste Stunde
  (Uhrabweichung).
- **Unbekannte Netze (optional):** Netze werden am Hash der MAC-Adresse des
  Default-Gateways erkannt. Bekannt wird ein Netz bei der Kopplung, beim
  ersten Kontakt zum Peer oder per „Diesem Netz vertrauen“. Mit
  `hide_in_unknown_networks` bleiben Port und Ankündigung dort aus; der
  Daemon verbindet nur ausgehend. Die Definition „solange der Peer nicht
  erreichbar ist“ allein hätte zwei Macs mit der Einstellung gegenseitig
  unsichtbar gemacht.

## Konsequenzen
- Beide Macs müssen gleichzeitig auf 0.5.0 (Protokoll 3).
- Ein Fremder sieht nur noch, dass im Netz ein `_macsync._tcp`-Dienst mit
  wechselnder ID existiert, und bekommt auf Verbindungen keine Antwort.
- Rekeying langer Sitzungen und der Schlüsselbund bleiben offen (siehe
  ADR-0006, Nachtrag).
