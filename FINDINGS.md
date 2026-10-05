# Findings

What is known about talking to Jackery units directly, as of
2026-10-04, from public sources and tests against one Explorer 2000.
Cloud access is out of scope for this project; cloud facts appear only
where they limit a direct path.

## Test unit

| Item | Value |
|---|---|
| Model | Explorer 2000 (app model code 12) |
| Wi-Fi | ESP32, DHCP name `espressif`, MAC `90:e5:b1:21:ed:b8` |
| BLE | advertises as `HT`, MAC `90:E5:B1:21:ED:BA` |
| App | `com.hbxn.jackery` (Android "Jackery" app) |

## Public sources

| Source | Covers | Use to us |
|---|---|---|
| [Jackery-Official/jackery](https://github.com/Jackery-Official/jackery) | Official Home Assistant integration; local MQTT for SolarVault3 only | Only official local protocol; no Explorer support |
| [csoscd/ha-solarvault](https://github.com/csoscd/ha-solarvault) | Third-party captures of SolarVault local MQTT | Write commands for SolarVault |
| [jlopez/socketry](https://github.com/jlopez/socketry) | Cloud protocol for Explorer line | Property keys and action IDs only |
| [theak/jackery-homeassistant](https://github.com/theak/jackery-homeassistant) | Cloud polling for Explorer line | Confirms property keys |
| [iLLixM/jackery_home_cloud-ha](https://github.com/iLLixM/jackery_home_cloud-ha) | Cloud, "Jackery Home" app (HomePower) | None for Explorer |
| [Wlad2288/Ultra_Jack](https://github.com/Wlad2288/Ultra_Jack), [bouldinnathan/home-assistant-jackery](https://github.com/bouldinnathan/home-assistant-jackery) | BLE, plain JSON, "HL" line (2000 Ultra) | Different protocol from the Explorer 2000 |

## Local MQTT (official, SolarVault3)

The unit is an MQTT client of a broker the user runs.  The broker
address and credentials are set in the Jackery app (2.0.0+, Device
Details -> Settings -> MQTT, or reportedly a long press on the logo on
the About page); the app generates a token that commands must carry.

- Topics (prefix default `hb`): device publishes on
  `hb/device/{sn}/status` and `hb/device/{sn}/event`; clients publish on
  `hb/device/{sn}/action`.
- Command envelope: `{type, eventId, messageId, ts, token, body}`.
- Types seen: 25 status query, 105/106 system attributes, 100/101
  sub-devices, 102 sub-device update, 103 sub-device switch, 107
  incremental push, 123 error (401 = bad token).  A bad token gets no
  reply.

**No evidence any Explorer supports this.**  No Explorer appears in the
official repo or its issues.  Jackery support told one user the
HomePower 2000 does not support MQTT.  The MQTT page was not found in
the app for the test unit.

## IP / Wi-Fi behavior of the Explorer 2000

- No listening services.  All 65535 TCP ports closed; no mDNS; UDP not
  fully probed.  The unit drops off Wi-Fi (stops answering ping) when
  idle, consistent with a power-save sleep.
- It is an outbound client only.  On boot it looks up
  `emqx.jackeryapp.com` (cloud MQTT) and `time.apple.com` (NTP), nothing
  else.  It does not query `iot.jackeryapp.com` (the REST API is
  app-only).
- It uses the DHCP-provided resolver, so a per-host DNS override reaches
  it.  But the cloud MQTT connection is TLS to a server whose cert is
  issued by a private CA (`CN=ca.jackery.com`); the unit rejects any
  other cert with `unknown_ca`.  A DNS-redirect to a local broker
  therefore fails at the TLS handshake.

Conclusion: there is no local IP interface on the Explorer 2000.  Over
IP it only speaks to Jackery's cloud, pinned to Jackery's CA.

## BLE (Explorer 2000)

A direct BLE connection from a Linux host works with no pairing or
bonding.  GATT layout:

| Service | Characteristic | Properties | Role |
|---|---|---|---|
| `0000bdee-...` | `0000ee01-...` | write-without-response | commands in |
| `0000bdee-...` | `0000ee02-...` | notify | responses out |
| `0000bdff-...` | `0000ff01-...` | write-without-response | unknown (OTA/Wi-Fi?) |

On first connect the unit pushed one unsolicited ~19-byte notify frame,
then went quiet; a later session got nothing unprompted.

The advertisement carries manufacturer data (ASCII digits, a device
identifier) and 14 bytes of changing service data under UUID `0xbdee`.

### Payload format (from app decompile)

Frames on `0xee01`/`0xee02` are not plaintext.  The app wraps each
message as an ASCII-hex structure beginning `DFEC`, protected by a
CRC-16/MODBUS (poly reversed `0xA001`, byte-swapped output) and a
single-byte XOR, then obfuscated with RC4 over the whole frame.  The
RC4 key is **per device** and is not derived on the handset: the app
fetches it from the cloud endpoint `device/bluetoothKey` (keyed by
serial) and supplies it to the codec.  Model code 12 selects the codec
with no fixed IV.

Command set (BLE message id -> meaning, with the matching cloud message
name where one exists): 6 = read device info (`QueryDeviceProperty`),
plus output toggles (AC, DC, DC car, DC USB), light, screen, charge /
battery / super-charge / UPS settings, time zone, charge/discharge and
TOU schedules, reboot (`DeviceReboot`), power off, Wi-Fi read/write, and
MQTT-connect sync.  Device property keys match the cloud set (`rb`
battery %, `ip`/`op` in/out W, `bt` temp x10, `acov` AC volts x10,
`it`/`ot` hours x10, output-state flags, etc.).

### Status

The frame layout and command set are understood, but exercising BLE
needs the per-device key, which currently exists only behind the cloud
account API.  Obtaining that key without the cloud is unresolved and is
the blocker for a no-cloud BLE path.

## Open questions

- Is there any way to get the per-device BLE key without the cloud
  (e.g. printed/labeled on the unit, derivable from serial, or readable
  from the device itself)?  Unknown.
- Does any Explorer firmware expose local MQTT, as the SolarVault3 does?
  Not seen; worth asking Jackery.
- Does the BLE advertisement's changing service data carry any
  telemetry in the clear?  Not yet decoded.
- What is on service `0xbdff` / char `0xff01`?  Untested.