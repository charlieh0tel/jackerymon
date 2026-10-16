# Findings

What is known about talking to Jackery units directly, as of
2026-10-04, from public sources and tests against one Explorer 2000.
Cloud bootstrap is now in scope for the Python key-retrieval PoC;
monitoring is intended to remain direct.

## Test unit

| Item | Value |
|---|---|
| Model | Explorer 2000 v2, 2042 Wh LiFePO4 (app model code 12) |
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
battery / emergency-charge (super-charge) / UPS settings, time zone, charge/discharge and
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
`jackerymon key` (Rust, 2026-10-12) fetched the key by lookup, without
binding, and wrote a file whose key and fields equal the PoC's; a poll
with it succeeded. `jackerymon scan` printed the same line as the PoC's
`--scan-only`.
Which path produced the 2026-10-04 key, `device/bluetoothKey` lookup or
`--bind`, was not recorded and the user does not remember.

The Python PoC in `tools/jackery_key.py` uses Bleak to decode advertisements,
then reproduces login and either key lookup or explicit Explorer binding.
See docs/protocol.md for wire details. (The PoC was removed on
2026-10-12 after the Rust port matched it; git history keeps it.)
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

On 2026-10-11 the user asked to keep the unit polled so its radios stay
on. A restarting Python poller first failed twice in a row: each run
saw three connections drop before the first status request (06:08-06:09
UTC). Its third run then produced samples. The Rust `jackerymon poll`,
run with unlimited retries, then connected on the first attempt and
returned samples with about 220 ms query latency and the same fields
as the PoC. Whether continuous polling keeps the radios on is not yet
known.

That Rust keep-alive ran 608 seconds: 113 samples, 4 connections, 7
transport failures, all around 06:18 UTC; the longest gap between
samples was about 15 seconds. Failures included `le-connection-abort-
by-local` from BlueZ (btleplug passes that reason through) and one
connect timeout. The radios stayed on throughout. `jackerymon tui`
then connected and updated live.

## Control (2026-10-11)

Light control works over BLE with the saved key. Sending `{"lm":1}` and
then `{"lm":0}` on action 17, message type 4, turned the light on and
off. Each was acknowledged by an echo on the same action with result 0
within about 90 ms, followed by an unsolicited action-0 property push
(`{"op":1}`, then `{"op":0}`). Status reads confirmed `lm`. A first
attempt was cut short by a logging bug after the light came on; the
rerun turned it off. Details are in docs/protocol.md.

`jackerymon set light low` and `set light off` then each succeeded,
confirmed by a status read, in about 7.5 seconds including the scan.

The TUI settings menu set the light low, confirmed in about a second.
Setting it off, the unit disconnected before confirmation, so the TUI
reported a connection failure; after reconnecting about 6 seconds later,
status showed the light off. A reported transport failure therefore
does not mean the setting was not applied; the next status is the
truth. During that run the unit was charging at 774-929 W input.

Custom charging is silently refused on the test unit: `cs=2` and
`csc=500` were each acknowledged with result 0 and a body of `{"cs":0}`,
and status stayed `cs=0`. Restoring `cs=0` changed nothing. Pushes
showed `acip` tracking `ip` while charging from AC (264-477 W) until the
battery reached 100%.

AC control works: `jackerymon set ac on` was confirmed in 6.0 seconds
(`oac=1`, `acov=1223`, 59 Hz) and `set --yes ac off` in 4.0 seconds
(`oac=0`, `acov=0`, `acohz=0`), with nothing plugged into AC. DC on and
the light on high, seen in the same status reads, were set by the user
from the TUI menu.

The daemon (2026-10-11) served `/status`, `/metrics`, and `/events`
from a live connection; `/events` replayed the current state, then
streamed samples every 5.3 seconds. `POST /settings` set the light low
and off in about 0.35 seconds each over the held connection, and
refused an unconfirmed emergency charge (409), `{"lm":9}`, and an
empty body (400) without sending anything. `tui --daemon` showed live
status and set the light low and off through the daemon.

## Radio controls (Explorer 2000 Plus manual)

The English manual for the Explorer 2000 Plus (JE-2000C; Home Depot's
copy, read 2026-10-12) says Wi-Fi and Bluetooth turn on at power-on and
turn off by themselves if no app connects within five minutes. Holding
DC/USB and AC together turns them on or off; flashing icons mean the
unit awaits a connection. Holding POWER and DC/USB for three seconds
resets them to factory settings, reboots, and unbinds the app account.
The test unit is an Explorer 2000 v2, not this model, but the
five-minute rule fits the radio-off episodes above, and pressing
several buttons restored the radios once. Whether a reset changes the
BLE key is unknown.

## Duplicate advertisement hides the unit (2026-10-12)

The unit sends two kinds of connectable `ADV_IND` from the same six
address bytes. Most carry the public address type with the name,
manufacturer data, and `0xbdee` service data; a scannable response
follows. About one in ten carry the random address type and no data at
all. A 30-second privileged `btmon` capture during an LE-only scan
(user-run, 2026-10-12) counted 574 public and 59 random reports for
`90:E5:B1:21:ED:BA`. The prefix is Espressif's, so the unit is
ESP32-based; the second advertisement looks like a firmware
configuration error, not a known ESP32 bug.

BlueZ 5.79 keys a device by address and type but names its D-Bus object
by address alone. Whichever kind it hears first after the unit's object
is created owns that object; every report of the other kind fails with
`device_new() Unable to register device interface`. Usually the public
kind wins (12 of 12 times after `bluetoothctl remove`), and the empty
random reports only produce that log noise. When the empty random kind
wins, every advertisement carrying the identity is refused, discovery
never matches the saved serial, and polling fails with "saved device
identity not found" while the unit advertises with icons flashing. The
random reports keep that object fresh, so it does not expire; the unit
stayed unreachable from 05:47 UTC until bluetoothd was restarted.

Evidence that the cause is BlueZ and the unit, not our client: a fresh
bluetoothd with only `bluetoothctl scan le` running logged the same
failures for the unit within a minute, and a classic-only scan saw
nothing. The same failures, for many nearby devices, ran at about 3,000
an hour on 2026-10-11 while our pollers scanned. Some earlier "not
found" episodes may have been this; the ones where the user saw the
icons off were not. Fix: when the saved address is present without the
identity, remove the object (BlueZ `RemoveDevice`, no privilege
needed) and scan again. btleplug cannot; `bluer` can (see `PLAN.md`).

After the move to `bluer` (2026-10-12), `jackerymon poll --count 12`
returned 12 samples on one connection with no failures, about 218 ms
per query, both from a connection a killed process left open and from
a fresh scan. bluetoothd logged no registration errors and the device
object count stayed at 6, since the scan's address pattern keeps other
devices out. Connecting right after a connection drops can still take
several attempts (`le-connection-abort-by-local`).

Restarting bluetoothd made bluez-async 0.9.0, under btleplug, panic in
a worker thread (`messagestream.rs:40`, unwrap on "No match with that
id found"); the poller survived.

The host crash of 2026-10-11 09:50 was unrelated: the `xe` GPU driver
failed to resume, a task hung on it, and suspend then failed.

## Daemon socket and unit lock (2026-10-12)

With the daemon on its Unix socket, `set` found it unprompted and
applied a setting in 0.39 seconds; TCP answered `POST /settings` with
404. A second daemon, a direct `poll`, and `set --direct` were refused
by the unit lock. Naming another unit made `set` refuse the daemon. After
`kill -9`, the leftover socket was ignored and the leftover BLE
connection reused; a clean exit removed the socket. With a daemon from
before the socket running, a direct `set` reused that daemon's live
connection and then closed it, which the unit lock now prevents.

## Reconnects and signal strength (2026-10-12)

Ten minutes of `poll --max-failures 0` with the unit's RSSI at about
-87 dBm gave 98 samples on 35 connections: 35 disconnects and 54
`le-connection-abort-by-local` connect failures. Most connections
carried one or two samples; recovery took 2.5 seconds when the first
reconnect worked and up to 18 seconds after ten failed attempts, with
failures spread through recovery rather than bunched after the
disconnect. The same ten minutes with the unit beside the laptop gave
115 samples on one connection, no failures, median query 218.9 ms
(maximum 310.2 ms). The failures were signal strength, not reconnect
timing, so retry delays stay as they are.

## Battery state never leaves idle (2026-10-13)

In all 15,418 samples the daemon logged from 2026-10-12 to 2026-10-13,
`bs` was 0 (idle): 307 of them with input power (AC charging at up to
786 W, charge 99 %) and 794 with output power.  The charge never fell
below 99 %, so whether `bs` reports charging or discharging further
from full is unknown.  The web view's state strip uses input and
output power above zero instead.

In the same samples, `acip + cip + acpsp/10` equaled `ip` every time,
but `cip` and `acpsp` were always 0, so only the AC path is confirmed.

## Power the unit does not report (2026-10-14)

Mains passes through to the AC outlets without counting as input: with
the unit plugged in and full, AC output on, `ip` and `acip` read 0
while the outlets ran on mains.  The unit sends no `iac`, so mains
presence shows only in the AC output while it is on: on mains it
wandered 121-123 V and read 59 Hz more often than 60; when the user
unplugged it (21:03:36 UTC) it went to 119.8-120.3 V at exactly 60 Hz,
the inverter.  Every AC-on stretch logged before then had the mains
signature.

`op` misses small loads.  Across about 28,000 samples it took only the
values 0, 2, 10, 15 and 17.  The light on high read exactly 2 W every
time.  A laptop on an AC outlet (its battery full, drawing its running
load only) read 0 W apart from 25 readings of 10-17 W.

On battery with AC and DC output on, no loads and the light off, `op`
read 0 W while the charge fell 100 to 99 % at 21:22:31 UTC and 99 to
98 % at 21:46:02, 23.5 minutes for one point.  At 20.4 Wh a point
(2042 Wh nominal) that is about 52 W of draw the unit does not report.
This is one clean interval of a whole-point estimate.

With AC output off and DC output on, still no loads, the charge fell 98
to 97 % at 02:59:59 UTC on 2026-10-15, 5 h 14 min after reaching 98 %,
and 97 to 96 % at 09:04:34, 6 h 5 min later: about 3.5 W.  So the AC
inverter idles at about 48 W, and the rest of the unit at about 3.5 W.
DC output went off at 08:43:05, exactly 12 h after it was turned on,
as the 12 h energy saving setting (`pm`) did once before (02:30:46 to
14:30:50 on 2026-10-14); the last 21 minutes ran with both outputs off.
With both off, 96 to 95 % took 6 h 30 min (to 15:34:33): about 3.1 W
of standby, so DC output on adds little.

Charging from the wall with both outputs off (2026-10-15) confirms the
scale: `ip` summed over 96-97, 97-98 and 98-99 % gave 20.9, 22.4 and
22.7 Wh at about 1030 W, 20.4 Wh a point plus about 6 % charger loss.
The last point is different: 99 to 100 % took 19 minutes and 203 Wh
as `ip` tapered from 786 to 263 W.  The charge reads 99 % through the
taper, so 100 % holds more than a linear scale implies; the discharge
figures above come from 99 % and below.

`ot` says nothing about this draw: through 2026-10-14 it read 99.8 or
99.9 h in all but one sample, whatever outputs were on.  It sits at
its ceiling; 48 W empties a full pack in about 42 h.

So input minus output, the web view's battery-power estimate, read 0 W
while the battery lost about 52 W.  It was dropped in 0.1.10.

On 2026-10-13 the charge fell to 99 % after about 15.5 hours on mains,
AC and DC output on with no reported load, and the unit then drew
304 Wh at up to 786 W to return to 100 %, the same taper.  What drew
it down while on mains is unknown; the outlets were on mains, so not
the inverter.

## Explorer 3000 and HomePower 3000 (2026-10-16)

The Explorer 3000 (model 14, "E3000" in the app) answered the Explorer
2000's status request with the same keys plus `dt` (10), `en` (0) and
`uo` (-25200, the UTC offset in seconds for PDT).  Its `ot` read 96.1
h, below the ceiling the 2000 sits at.

The HomePower 3000 (model 19, "HP3000"; advertised battery 90,
bind counter 1) connects and takes the request write, but sends no
notification: three attempts with 10 s timeouts, three with 30 s.  The
app gives it the same GATT UUIDs, codec (RC4, no IV) and frame version
as models 12 and 14.  The unit may be bound to another account: the
cloud gave this account a key for its serial, and a key the unit does
not use would explain the silence.  Unconfirmed until the unit is
bound to the account that fetched the key.

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
