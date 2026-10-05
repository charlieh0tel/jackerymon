# Jackery protocol notes

Working notes on Jackery's direct (non-cloud) protocols, for building
jackerymon.  Reverse-engineering sourced from public projects and from
a decompile of the Android app `com.hbxn.jackery` 2.4.2.  See
[../FINDINGS.md](../FINDINGS.md) for what was tested and what is
unverified.  Property keys and command semantics are shared across
transports.

## Property keys

Integers; some scaled.  Shared by the MQTT and BLE paths.

| Key | Meaning | Scale |
|---|---|---|
| `rb` | remaining battery | % |
| `bt` | battery temperature | /10 -> C |
| `bs` | battery status | 0 idle, 1 charging, 2 discharging |
| `ip` / `op` | input / output power | W |
| `it` / `ot` | input / output time remaining | /10 -> h |
| `oac` / `odc` / `odcu` / `odcc` | AC / DC / USB / car output state | 0/1 |
| `iac` / `idc` | AC / DC input state | 0/1 |
| `acov` | AC output voltage | /10 -> V |
| `acohz` | AC output frequency | Hz |
| `lm` | light mode | 0 off, 1 low, 2 high, 3 SOS |
| `cs` | charge speed | 0 fast, 1 mute |
| `ups` | UPS mode | 0/1 |
| `lps` | battery protection | 0 full, 1 eco |
| `ec` | error code | |

Some models combine DC output into `odc`; others split `odcu`/`odcc`.

## Local MQTT (official; SolarVault3 line)

Device is an MQTT client of a broker the user runs; configured in the
Jackery app (2.0.0+).  Commands carry a token the app generates.

- Topics (prefix default `hb`):
  - `hb/device/{sn}/status` — device publishes state
  - `hb/device/{sn}/event` — device publishes incremental pushes
  - `hb/device/{sn}/action` — client publishes queries/commands
- Envelope: `{type, eventId, messageId, ts, token, body}`
- Message types:

  | type | direction | meaning |
  |---|---|---|
  | 25 | query -> | host status |
  | 105 -> 106 | query/reply | full system attributes |
  | 100 -> 101 | query/reply | sub-device list (CT, meter, plugs) |
  | 102 | <- | sub-device incremental update |
  | 103 | query -> | sub-device switch |
  | 107 | <- | incremental push (soc, workMode) |
  | 123 | <- | error/auth; `errorCode` 401 = bad token |

- Poll cadence ~5 s; device treated offline after 60 s silence.
- A bad token gets no reply (123/401 at most).

Not known to apply to any Explorer.

## BLE (Explorer 2000, app line "HT")

Direct GATT connection succeeds with no pairing/bonding.

### GATT

| Service | Characteristic | Properties | Role |
|---|---|---|---|
| `0000bdee-0000-1000-8000-00805f9b34fb` | `0000ee01-...` | write-without-response | command in |
| `0000bdee-...` | `0000ee02-...` | notify | response out |
| `0000bdff-...` | `0000ff01-...` | write-without-response | unknown (OTA/Wi-Fi?) |

MTU negotiates to 23 by default (20-byte payloads); large responses are
fragmented and must be reassembled.

### Advertisement

- Local name `HT`.
- Manufacturer data: ASCII decimal digits encoding a device identifier.
- Service data under `0xbdee`: 14 bytes that change between
  advertisements (not yet decoded; may be a rolling token).

### Frame format

Messages are ASCII-hex structures, not plaintext JSON.  Layers,
outermost last:

1. Inner payload begins with marker `DFEC`, then a type byte
   (`00` single, `80` paginated), then the body.
2. CRC-16/MODBUS appended (poly `0x8005` / reversed `0xA001`, init
   `0xFFFF`, output byte-swapped as two hex pairs).
3. A single random byte is chosen; the hex string is XORed byte-wise by
   it; the XOR byte is carried in the frame.
4. The whole thing is run through RC4 to produce the bytes written to
   `0xee01`.  Notifications on `0xee02` reverse this; a valid decrypt
   starts with `DFEC`.

Send templates (hex): `DFEC00%s%s%s%s`, paginated `DFEC80%s%s%s%s%s%s`;
a newer variant uses a `DFED0001...` header.

The RC4 key is **per device**.  The handset does not derive it; it is
fetched from the cloud endpoint `device/bluetoothKey` (by serial) and
handed to the codec.  Model code 12 (Explorer 2000) selects the codec
variant with no fixed IV.  Without this key the BLE link cannot be
exercised; obtaining it offline is unresolved (see FINDINGS.md).

### Commands

BLE message id -> meaning (cloud message name where one exists):

- 6 read device info (`QueryDeviceProperty`)
- output toggles: AC, DC, DC car, DC USB; AC 240; output priority
- 18 screen, light, 19 auto-shutdown
- 21 charge, 22 battery, 23 super-charge, 24 UPS, 25 time zone
- 26-30 charge/discharge plans (`*ElectricityStrategy`)
- 42/43 TOU peaks/troughs (`TOUSchedule`, `QueryTOUSchedule`)
- 45 reboot (`DeviceReboot`), 46 power off
- 50 sync MQTT connect info, 51 read sub-device CT, 52 Wi-Fi config
- Wi-Fi read/write, OTA page/version
