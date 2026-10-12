# jackerymon

Direct BLE monitoring for Jackery power stations; see [PLAN.md](PLAN.md)
and [FINDINGS.md](FINDINGS.md).

## Getting your key

Talking to the unit needs its per-device BLE key, which only Jackery's
cloud hands out. Getting it is a one-time step done with the Python PoC
(details below and in `docs/protocol.md`):

1. Wake the unit and check it is visible:
   `uv run tools/jackery_key.py --scan-only`.
2. Fetch the key with your Jackery account (email and password are
   prompted): `uv run tools/jackery_key.py --address <unit address>`.
3. If the unit is not bound to your account, add `--bind`. This binds it
   to the account and takes the key from the bind response.

This writes `jackery-key.json` (mode 0600); move it to
`~/.config/jackerymon/<serial>.json`. Everything after that uses only
BLE.

Keys live one file per unit: `~/.config/jackerymon/<serial>.json` for
every command but `daemon`, which reads `/etc/jackerymon/<serial>.json`.
`--unit SERIAL` picks a unit and may be left out when there is one key;
`--key-file` names a file directly. One daemon serves one unit; run one
per unit for several.

## Rust

`crates/jackerymon` is the library: advertisement decoding, the
model-12 GATT frame codec, key-file loading, typed status, BLE
transport over BlueZ (`bluer`), and polling. `crates/jackerymon-cli`
builds `jackerymon`.

```sh
cargo test
cargo run --release -- scan                  # units in range, no cloud
cargo run --release -- poll                  # until Ctrl-C
cargo run --release -- poll --count 12       # finite run
cargo run --release -- poll --max-failures 0 # retry forever
cargo run --release -- tui                   # live status screen
cargo run --release -- set light low         # change one setting
```

`set` changes AC or DC output, light, screen, auto-off, energy saving,
charge speed (fast or quiet), battery protection, emergency charge, or
UPS mode; see `jackerymon set --help`. It reports success only after a
status read shows the change. Turning emergency charging on needs
`--yes`.

`tui` shows battery, power, outputs, settings, faults, link state, data
age, and a ten-minute input/output power chart. It retries forever while
the unit is unreachable and keeps the last reading. `m` opens a settings
menu covering the same settings as `set`: ↑/↓ selects a setting, ←/→
cycles its value, Enter applies it, and Esc discards or closes. Turning
emergency charging on asks y/n in the same box. The footer
shows each change's progress and result. `q`, Esc, or Ctrl-C quits and
closes the connection. It holds the unit's only BLE
connection, so the phone app cannot connect while it runs.

`poll` finds its key as above and takes the PoC's `--address`,
`--timeout`, `--interval`, and `--count`. Output matches the PoC's
`--poll` JSON lines, except that keys inside `status` are sorted.

Every command but `tui` logs notable BLE events to stderr, such as
reusing a connection a killed process left open, or removing the stale
BlueZ object the unit's duplicate advertisement can leave
(`FINDINGS.md`). `RUST_LOG` changes the filter, which defaults to
`jackerymon=info`.

Linux needs BlueZ and the D-Bus development headers to build.

## Daemon

`jackerymon daemon` owns the unit's BLE connection, polls it, and serves
HTTP on `127.0.0.1:9888` (`--listen` to change):

| Route | Gives |
|---|---|
| `GET /status` | link state, counters, and the latest sample (JSON) |
| `GET /metrics` | Prometheus metrics, `jackery_*`, labeled by serial |
| `GET /events` | server-sent events: current state, then every change |
| `POST /settings` | apply one setting, e.g. `{"lm":1}`; answers once status confirms it |

```sh
cargo run --release -- daemon --log-dir /var/log/jackerymon
curl -s localhost:9888/metrics
curl -s -X POST -H 'content-type: application/json' -d '{"oac":1}' \
    localhost:9888/settings
cargo run --release -- tui --daemon http://127.0.0.1:9888
```

`POST /settings` takes the unit's own one-key body (keys and values in
`docs/protocol.md`, Control). Emergency charging needs `?confirm=true`.
A request waits up to 60 s in all, including for room in the queue and
for the unit to come back; one still queued then is never sent. The API
has no authentication and can switch outputs, so bind it beyond
localhost only on a trusted network. Even on localhost, a web page open
in a browser on the same machine can reach it through DNS rebinding;
this risk is accepted (`PLAN.md`).

Readings older than 60 s are left out of `/metrics`, so a unit that has
gone quiet never looks live; `jackery_connected` and
`jackery_last_sample_timestamp_seconds` show what happened. `--log-dir`
writes one `YYYY-MM-DD.jsonl` file per UTC day in the `poll` line format
(a directory it cannot write to stops the daemon at startup), about 10
MB a day at the default 5 s interval.

`tui --daemon` follows the daemon instead of connecting to the unit, so
the TUI and the daemon can run together; the TUI never starts or stops a
daemon. A replayed sample shows its real age, and one older than the
chart's ten minutes is not charted. While the daemon runs, the phone app
cannot connect.

`contrib/systemd/` has a service unit and a tmpfiles rule that prunes
logs after 30 days. The daemon exits when polling fails for good, with
the error, so systemd can restart it; on SIGINT or SIGTERM it closes
`/events` streams rather than waiting for their clients. Neither has been tested; check the unit's user,
group, and paths against your system before installing it.

## Python BLE PoC

Requires Python 3.11+, uv, and a working Bluetooth adapter. On Linux,
BlueZ must be running. Dependencies are declared in the script; uv
resolves them automatically.

Scan and decode advertisements without cloud access:

```sh
uv run tools/jackery_key.py --scan-only
```

Retrieve the key for the test unit (email/password prompted locally):

```sh
uv run tools/jackery_key.py --address 90:E5:B1:21:ED:BA
```

For an unbound Explorer, explicitly bind it to your account and obtain
the key from the binding response:

```sh
uv run tools/jackery_key.py --address 90:E5:B1:21:ED:BA --bind
```

`--bind` changes the account/device association. There is no automatic
binding fallback. `--region` sets the login regionCode if your account
requires it (default empty, matching the APK's initial value).

The key is saved to `jackery-key.json` with mode 0600; existing files
are never overwritten. Use `--output jackery-key-other.json` to choose
another path. Keep custom output paths out of version control. Passwords,
tokens, and keys are not printed; passwords and tokens are not saved.

Read status directly over BLE with the saved key (no cloud requests):

```sh
uv run tools/jackery_key.py --status jackery-key.json
```

Poll on one connection, every five seconds, until Ctrl-C:

```sh
uv run tools/jackery_key.py --poll jackery-key.json
```

For a finite run:

```sh
uv run tools/jackery_key.py --poll jackery-key.json --interval 5 --count 12
```

Polling emits flushed JSON lines on stdout: UTC `timestamp`, `sample`,
`connection` (successful subscriptions), `transport_failures`, `query_ms`,
and raw `status`. Diagnostics and a final summary go to stderr.
`--interval` is the delay after each response; queries never overlap.
`--count` counts successful samples, with 0 meaning unlimited.
Transport failures reconnect with fresh identity discovery; three
consecutive failures stop polling, and each successful sample resets that
failure streak. Key/CRC/device errors stop immediately. A timed-out query
always closes its connection before retrying.

Live validation: 60 polls on one connection in 313.4 seconds, zero transport
failures, median query latency 216.2 ms (p95 303.4 ms). USB, AC, and light
states matched the user's manual changes. Wi-Fi credentials were absent.
The saved key still worked after a power-cycle. Longer runs and independent
load measurements remain to be tested.

The status path verifies the advertised serial/address, subscribes to
notifications, sends one read-device-info command, and prints the returned
wire-key JSON. It currently supports model 12 only. It handles fragmented
notifications and paginated responses, checking CRC and device error codes.
It does not change output states or settings.

Status reads retry transport failures up to three times, rescanning and
reconnecting each time. Disconnects are reported immediately; notification
cleanup cannot replace the original error. Invalid keys, CRCs, and device
error responses fail without retry. If all attempts fail, wake the unit
and close competing BLE clients. `--timeout` applies per scan/connection/
response wait, so the full retry sequence can take longer.

Live status succeeded on the Explorer 2000 with the user-acquired key:
100% battery, 0 W input/output, 33.3 C battery temperature, outputs off,
and error code 0. The user removed Wi-Fi credentials before testing began;
all these BLE tests used a unit without Wi-Fi connectivity. Three repeat
status reads succeeded with the existing key and no retries.
The status process made no cloud requests. The key survived a power-cycle;
long-term validity and reset/unbinding behavior remain unverified. Cloud
access is needed for the implemented key acquisition.

Internally, `StatusSession` owns connection-loss notification and one
pending request, with a fresh decoder for each query. This separates
request state from connection setup so polling can reuse its subscription.

Offline tests:

```sh
uv run tools/test_jackery_key.py
```
