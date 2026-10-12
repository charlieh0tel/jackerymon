# jackerymon

Direct BLE monitoring for Jackery power stations; see [PLAN.md](PLAN.md)
and [FINDINGS.md](FINDINGS.md).

## Getting your key

Talking to the unit needs its per-device BLE key, which only Jackery's
cloud hands out. Getting it is a one-time step:

1. Wake the unit and check it is in range: `jackerymon scan`.
2. Fetch the key with your Jackery account, in a terminal (email and
   password are prompted): `jackerymon key`. With several units in
   range, add `--address`.
3. If the unit is not bound to your account, add `--bind`. This binds it
   to the account and takes the key from the bind response.

This writes `~/.config/jackerymon/<serial>.json` (mode 0600, directory
0700) and never overwrites an existing file; `--output` writes
elsewhere. The password and the session token are never printed or
saved. Everything after that uses only BLE.

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
cargo run --release -- key                   # fetch the key, once
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

`poll` prints one JSON line per sample: UTC `timestamp`, `sample`,
`connection`, `transport_failures`, `query_ms`, and the raw `status`
with its keys sorted. Diagnostics and a final summary go to stderr.
`--interval` is the delay after each response; queries never overlap.

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
