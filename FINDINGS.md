# Findings

What is known about talking to Jackery units directly, as of
2026-10-04, from public sources and tests against one Explorer 2000.
Cloud bootstrap is now in scope for the Python key-retrieval PoC;
monitoring is intended to remain direct.

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
message as binary bytes beginning `DFEC` (represented as hex in the app).
It XORs the bytes, appends the XOR mask, then appends CRC-16/MODBUS
(poly reversed `0xA001`, little-endian output) before RC4. The
RC4 key is **per device** and is not derived on the handset: the app
fetches it from the cloud endpoint `device/bluetoothKey` (keyed by
serial/GUID), or the Explorer binding response, and base64-decodes it
before supplying it to RC4. Model code 12 selects this codec, with no IV.

Command set (BLE message id -> meaning, with the matching cloud message
name where one exists): 6 = read device info (`QueryDeviceProperty`),
plus output toggles (AC, DC, DC car, DC USB), light, screen, charge /
battery / super-charge / UPS settings, time zone, charge/discharge and
TOU schedules, reboot (`DeviceReboot`), power off, Wi-Fi read/write, and
MQTT-connect sync.  Device property keys match the cloud set (`rb`
battery %, `ip`/`op` in/out W, `bt` temp x10, `acov` AC volts x10,
`it`/`ot` hours x10, output-state flags, etc.).

### Status

Direct BLE status now works with the per-device key. The app fetches it
from the cloud and saves
device-list JSON, including `bluetoothKey`, in MMKV store `user_cache_id`,
entry `user_cache_id_bind_devices` (`ce/d.java`, `ce/h.java`). Successful
Explorer unbinding removes the device and rewrites that cache
(`PortableSettingActivity.java`). The user's phone is unrooted and the
unit was unbound before bootstrap; cache extraction was not needed.

The Python PoC in `tools/jackery_key.py` uses Bleak to decode advertisements,
then reproduces login and either key lookup or explicit Explorer binding.
See README.md for uv commands and docs/protocol.md for wire details.
Twenty-one offline tests pass. A live Bleak scan decoded the test unit as model
12, battery 100, bind/reset counter 3, with a six-byte GUID. After the
user completed login and a local key file appeared, the PoC successfully
used that key for an encrypted BLE status query. No cloud requests were
made by the status process. Selected returned values:

| Key | Raw value | Interpretation |
|---|---|---|
| `rb` | 100 | 100% battery |
| `ip`, `op` | 0 | No input/output power |
| `bt` | 333 | 33.3 C battery temperature |
| `oac`, `odc` | 0 | AC/DC outputs off |
| `ec` | 0 | No reported error |

Correction: the user removed Wi-Fi credentials before testing began, so
the initial and repeat BLE tests all used a unit without Wi-Fi connectivity.
Three repeat status reads succeeded with the same saved key, with no retries. Durations
(discovery, connection, query, cleanup) were 4.90, 3.08, and 3.26 seconds.
All three returned `rb=100`, `bt=332`, `ip=0`, `op=0`, and `ec=0`.
The client made no cloud requests. Network isolation is based on the
user's credential removal, not a router-side packet capture. These tests
do not establish whether credential removal changes a key acquired earlier.
Key lifetime and behavior
after power-cycle/reset/unbinding are still unknown.

Subsequent reads exposed intermittent disconnects during service discovery
and before status completion. The original unconditional `stop_notify`
could replace an underlying failure with "Service Discovery has not been
performed yet" after Bleak cleared its services. The PoC now relies on
disconnect cleanup and retries transport failures up to three times. A live
test recovered from a first-attempt disconnect and returned status on the
second attempt. The cause of the peripheral disconnect remains unknown.

The PoC now supports persistent polling, keeping one connection and
subscription between queries. Each query uses a fresh decoder. JSON-line
records expose successful connections, transport failures, and query
latency so repeated polling can measure reliability.

A live `--interval 5 --count 12` run completed in 62.4 seconds: 12 samples,
one connection, zero transport failures. Query latency ranged from 214.4
to 306.4 ms. All samples reported `rb=100`, `bt=332`, `ip=0`, `op=0`, and
`ec=0`. Wi-Fi credentials were absent throughout, as in the earlier tests.
This validates a short persistent session, not long-duration reliability.

A subsequent 60-sample run stopped after 24.1 seconds with zero samples:
one discovery miss, followed by two disconnects before a status request.
The configured three-consecutive-failure limit stopped it cleanly. Two
subscriptions were established but produced no samples. Sleep or another
BLE client has not been established as the cause.

After the user manually enabled USB, AC, and the light, a new 60-sample
run completed in 313.4 seconds on one connection with zero transport
failures. Query latency: minimum 212.4 ms, median 216.2 ms, nearest-rank
p95 303.4 ms, maximum 351.3 ms. All samples reported `oac=1`, `odc=1`,
`lm=1`, `op=1`, `ip=0`, `rb=100`, and `ec=0`. Battery temperature ranged
from 33.1 to 33.2 C; AC voltage from 120.5 to 121.4 V, frequency 59-60 Hz.
This validates five minutes of polling and reports the manually changed
output states. It does not prove enabling outputs prevents disconnects,
and voltage/power measurements were not independently calibrated.

No battery current or battery power field appeared. The app's battery
power chart references found during review belong to its home-system UI.
Do not infer measured battery drain from the reported 1 W output.

On 2026-10-05, the user power-cycled the unit. A saved-key polling run
collected no samples: GATT/service-discovery failures exhausted three
attempts in 18.4 seconds. A second run with a 15-second scan timeout
missed the saved device in all three scans (47.1 seconds total). Neither
run established a notification subscription or decoded a response. No
login/key refresh was performed. Key persistence across power-cycle is
therefore still unverified; these transport failures are not evidence
that the key changed.

Follow-up raw scans saw 35-47 other devices but no Jackery-like advertisement,
even while the user reported blinking Wi-Fi/Bluetooth icons. Host BlueZ
5.79 logged repeated failures to register discovered device interfaces.
Those errors returned after the user restarted Bluetooth; a saved-key
retry then missed the unit in all three scans (47.2 seconds). The logged
registration failures named other devices, not the Jackery, so their
causal relationship to this failure is unproven. A controller-level capture
could show whether advertisements arrive below BlueZ's device registration
layer; a second receiver could independently check discovery. An unprivileged
`btmon` attempt failed with `Operation not permitted`, so no controller capture
was collected. The assistant must not run sudo; the user handles privileged
host maintenance.

The user then collected `/tmp/jackery-radio.btsnoop` with privileged `btmon`.
The capture contains a 20-second active scan accepting all advertisements:
2,248 advertising-report events from 37 distinct addresses, with all 16
command status results successful. No saved Jackery address/prefix, HT/HL
name, serial, bdee UUID, or category-2 manufacturer identifier appeared.
Thus no recognizable Jackery advertisement reached the captured HCI layer
during that scan; Bleak filtering or BlueZ device registration alone cannot
explain this capture. This does not distinguish peripheral advertising state,
interference, or controller reception trouble. The user had nabemono next to
the unit around the middle of the run, making simple distance less likely;
the exact proximity interval was not timestamped. Eight reports used an
all-zero address; their significance is unknown. Key persistence remains
untested because discovery still fails before authentication.

On 2026-10-10 the unit was advertising again; why BLE and Wi-Fi stopped
is unknown. A saved-key `--status` read then succeeded with no login or
cloud request, so the key survived the 2026-10-05 power-cycle. An
immediate second read failed: two disconnects (one before the status
request, one before it completed), then three-attempt exhaustion.
Whether the phone app held a BLE connection at the time is unknown.

Minutes later a 60-sample poll found no advertisement in three scans
(32.1 seconds). The user then saw the display on but the Wi-Fi and
Bluetooth icons off; the unit had turned its radios off by itself,
trigger unknown. The user pressed several buttons (which one restored
the radios was not determined). A subsequent 60-sample poll
completed in 325.8 seconds: two disconnects before the first status
request, then all 60 samples on the third connection. Query latency:
minimum 213.3 ms, median 216.0 ms, maximum 577.4 ms. Outputs off,
`rb=100`, `ec=0`. Radio-off explains this "not found" failure. It may
not explain the btmon case, when the user reported blinking icons.
Either way the daemon must tolerate long absences.

## Open questions

- Is there any way to get the per-device BLE key without the cloud
  (e.g. printed/labeled on the unit, derivable from serial, or readable
  from the device itself)?  Unknown.
- Does any Explorer firmware expose local MQTT, as the SolarVault3 does?
  Not seen; worth asking Jackery.
- Does the decoded advertisement battery byte match the unit? The APK
  parser exposes it and both the live advertisement and GATT status
  reported 100; comparison with the display remains outstanding.
- What is on service `0xbdff` / char `0xff01`?  Untested.
