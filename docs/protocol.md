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
| `ec` | error code; app shows n as fault F(n-1) | 0 none |
| `cip` | car (12 V) input power | W |
| `acpsp` | solar (MPPT) input power; app adds it to `cip` as DC input | /10 -> W |
| `acpss` | solar input present; app shows MPPT icon when nonzero | |
| `acip` | AC input present; app shows AC icon when nonzero | |
| `ast` | auto power-off under light load on battery | min, 0 never |
| `pm` | energy saving: outputs off at no/light load | min, 0 never |
| `pmb` | energy saving active; app shows low-power icon when nonzero | |
| `sltb` | screen setting | 1 off, 2 on 2 min, 3 on 2 h |
| `sfc` | super charge (one-shot fast charge) | 0/1 |
| `pal` | parallel/expansion battery; app shows merge icon at 2 | |
| `ta` | temperature alarm | 0 none, 1 high, 2 low |

Keys from `ec` down were decoded from the app 2.4.2
(`PortableBody.java`, `PortablePanelActivity.java`,
`PortableSettingActivity.java`, `lc/a.java`, `ab/o6.java`, layout
`panel_activity_portable.xml`); `it` is shown /10 as hours there too.
App option lists for model 12 (`rb/k.java`, "E2000Pro2"): `ast` 120,
480, 720, 1440, never; `pm` 120, 480, 720, 1440, never. The test unit
reported `ast=120`, `pm=720`, `sltb` 1 or 2, all others 0.

The app has a "Bluetooth/Wi-Fi auto-off" setting (key `tmt`, command 44,
`SETTING_BLUETOOTH_MODULE_SLEEP_TIME`), but hides it for model 12
(`w0()` is false) and the unit does not report `tmt`. Whether this
firmware has a fixed radio timeout is unknown.

Some models combine DC output into `odc`; others split `odcu`/`odcc`.
On the test Explorer 2000, enabling USB, AC, and the light manually
changed the reported state to `odc=1`, `oac=1`, `lm=1`. Its response did
not include separate `odcu`/`odcc` fields. AC voltage was `acov=1209`
(120.9 V); output power was `op=1` W. This is a state check, not an
independent calibration of the voltage or power readings.

No explicit battery-current, battery-voltage, or battery-power measurement
has appeared in this Explorer's status responses. `ip`/`op` are input/output
power; their difference must not be labeled measured battery power because
internal consumption and conversion losses may be excluded. The APK's
"Battery power" chart references found in this investigation are in the
home-system UI, not the Explorer portable UI. Other commands or firmware
may expose additional telemetry; this has not been established.

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
- Manufacturer data: reconstruct the two-byte company ID in little-endian
  order. Byte 0 is category (2 = Jackery device), byte 1 is the first
  ASCII serial character; append the remaining manufacturer bytes.
- For 14-byte `0xbdee` service data, RC4-decrypt using UTF-8 bytes of
  `serial[:3] + serial[-5:] + "LYx*G!6u9#"`. This advertisement key is
  distinct from the private GATT key.
- Check CRC-16/MODBUS over all but the last two bytes; trailer is
  little-endian. The preceding byte is the XOR mask for the body.
- Decoded body: model (2 bytes, big-endian), GUID (6 bytes), battery
  (1 byte), bind/reset counter (2 bytes, big-endian). The older 13-byte
  variant omits RC4 and the battery byte; CRC/XOR processing is the same.
- Source: `fb/e.java`, `fb/a.java`, `yb/d.java`, `yb/b.java`, `yb/w.java`.
  Synthetic fixtures and a live test-unit scan validate decoding. The live
  result was model 12, battery 100, bind/reset counter 3, and a six-byte GUID.

### Cloud key bootstrap (Android 2.4.2)

Base URL: `https://iot.jackeryapp.com/v1/`. Successful envelopes have
`code: 0`, `data`, and optionally a replacement `token`. Subsequent
requests carry the token in header `token`, not Authorization.

- `POST auth/login`, form fields `aesEncryptData` and `rsaForAesKey`.
  Generate 16 random bytes, base64-encode them, and use the resulting
  24 ASCII bytes as the AES-192 key. Encrypt compact login JSON using
  AES/ECB/PKCS7; base64-encode ciphertext. RSA-wrap the same ASCII key
  with the APK's public key using PKCS#1 v1.5; base64-encode it.
  Login JSON includes account, password, loginType=2, regionCode,
  registerAppId=com.hbxn.jackery, and macId (client identifier).
- `GET device/bluetoothKey`, query parameters `deviceSn` and uppercase
  hex `guid`. The app uses this endpoint on its home-system path;
  acceptance for an unbound Explorer is unverified.
- Explorer scan-to-bind uses `POST device/bind`, form fields `devId`
  (advertised serial), `guid`, `bindKey` (advertised counter), and
  `timezoneOffset` (local UTC offset in minutes). Its response model
  includes `bluetoothKey`; the PoC requires explicit `--bind`.
- App headers include platform=2, app_version=2.4.2,
  app_version_code=101, Accept-Language, sys_version, model, network.

Sources: `LoginApi.java`, `vc/b.java`, `ce/d.java`, `ce/h.java`,
`DeviceBluetoothApi.java`, `DeviceBindApi.java`, `AppApplication.java`,
`RequestServer.java`, `BluetoothScanDeviceActivity.java` in scratch sources.
The user completed login and a key file was produced; that key was then
validated with a live GATT status query. The specific bootstrap endpoint
used was not captured in this session. The PoC saves only selected device
metadata/key, not the password or account token.

### Frame format

The wire carries binary bytes; the APK represents them as hex strings.
For model 12 (`hb/g.java`), encode in this order:

1. Inner payload: `DFEC`, flags/code byte, action ID, message type,
   one-byte body length, then body. Empty read-status request:
   `DFEC00060300` (action 6, message type 3).
2. Choose a nonzero random byte, XOR every inner byte by it, append mask.
3. Append CRC-16/MODBUS over the XORed bytes plus mask (poly reversed
   `0xA001`, init `0xFFFF`, two-byte little-endian result).
4. RC4-encrypt the complete packet with the base64-decoded cloud key.
   RC4 restarts for each packet. Write to `0xee01` without response;
   subscribe to `0xee02` before writing.

On receive, reverse RC4, check CRC, strip CRC/mask, undo XOR, verify
`DFEC`. The first post-marker byte's high bit indicates pagination;
its lower seven bits are a result code. For paginated responses, the
one-byte body length is followed by two-byte big-endian page index and
page count, then body (`ac/e.java`). Concatenate pages in index order
before JSON decoding. Notification fragmentation and protocol pagination
are separate layers. The PoC bounds total received bytes and rejects
conflicting pages, invalid CRCs, and nonzero device result codes.

BLE connection loss clears Bleak's service table. Notifications stop on
disconnect, so the PoC does not call `stop_notify` during teardown. It
detects disconnects while awaiting status and retries transport failures
with fresh discovery, connection, and page assembly, up to three attempts.
CRC/key/device-response errors are not retried. This recovered successfully
from an observed disconnect; it does not establish the reason for the drop.

Send templates (hex): `DFEC00%s%s%s%s`, paginated `DFEC80%s%s%s%s%s%s`;
a newer variant uses a `DFED0001...` header.

The RC4 key is **per device**.  The handset does not derive it; it is
fetched from the cloud endpoint `device/bluetoothKey` (by serial) and
handed to the codec.  Model code 12 (Explorer 2000) selects the codec
variant with no IV. The saved key now successfully decodes a live status
response. The user removed Wi-Fi credentials before testing began; all
BLE tests used the unit in that state. Three repeat queries succeeded with
the same saved key and no retries or client cloud requests. These tests do
not measure key changes on Wi-Fi credential removal. Power-cycle/reset/
unbinding behavior is untested.
Obtaining the key without cloud bootstrap remains unresolved.

The PoC's `StatusSession` owns one pending query and its decoder. It
ignores notifications outside a pending request and rejects overlapping
queries; disconnects fail the current query immediately.

`--poll` keeps one GATT connection and notification subscription across
queries. It discards notifications outside pending requests and starts a
fresh decoder per request. Queries run sequentially, with a configurable
delay after each completed response. Timeouts close the connection before
retrying, preventing reuse of that connection's partial response. The wire
format has no per-query sequence number; an unsolicited/delayed action-6
message arriving during a query cannot be conclusively distinguished from
its response. This remains a limitation of the experimental poller.
Live validation produced 12 successive decoded responses over one
connection (five-second response-to-request delay), with no transport
failures. The run lasted 62.4 seconds including setup and teardown.
A later run with USB, AC, and light enabled returned 60 responses on one
connection in 313.4 seconds, again without transport failures. Median
query latency was 216.2 ms; p95 was 303.4 ms.

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
