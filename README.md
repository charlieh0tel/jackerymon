# jackerymon

Monitor and control a Jackery Explorer power station over Bluetooth LE,
without Jackery's cloud.  Linux only; needs BlueZ, and the D-Bus
headers to build.  Background: [PLAN.md](PLAN.md),
[FINDINGS.md](FINDINGS.md), [docs/protocol.md](docs/protocol.md).

```sh
cargo build --release      # builds target/release/jackerymon
cargo test
```

## Hardware

Tested: one Explorer 2000 (app model code 12, advertises as `HT`), from
a laptop with an Intel Bluetooth controller, Ubuntu 25.04, Linux 6.14,
and BlueZ 5.79.

Expected: any Linux machine whose adapter BlueZ supports.  Other
Explorers in the app's `HT` line probably share the protocol, but some
use a codec variant with an IV; jackerymon refuses any model but 12
until one is tested.  The Explorer 2000 Ultra (`HL` line) uses a
different protocol and will not work.  HomePower units (app model codes
3001-3004) encrypt BLE frames with AES-CBC instead of RC4 and likely use
other status keys; support awaits a unit to test.

## Wi-Fi and Bluetooth

The unit turns its radios off if nothing connects within five minutes of
power-on.  To turn them on or off, hold DC/USB and AC together until the
Wi-Fi and Bluetooth icons light or go out.  Flashing icons mean the unit
is waiting for a connection.  Do not hold POWER and DC/USB: that resets
the radios and unbinds the unit from your account.  (From the Explorer
2000 Plus manual.)

## Key

Each unit has its own BLE key, which only Jackery's cloud hands out.
Fetch it once, in a terminal:

```sh
jackerymon scan            # units in range
jackerymon key             # prompts for your Jackery email and password
```

Add `--address` when several units are in range, and `--bind` if the
unit is not yet bound to your account.  The key goes to
`~/.config/jackerymon/<serial>.json`, readable only by you; the password
and session token are never saved.  From then on, everything uses BLE
alone.

Every command finds keys there, except `daemon`, which reads
`/etc/jackerymon/<serial>.json`.  With several keys, choose one with
`--unit SERIAL`; `--key-file` names a file directly.

## Commands

```sh
jackerymon poll            # JSON line per sample until Ctrl-C; --count N
jackerymon tui             # live status screen; m opens settings
jackerymon set light low   # change one setting; see set --help
jackerymon daemon          # HTTP status, metrics, events, settings
```

`set` and the TUI report a change only once a status read shows it.
Turning emergency charging on needs `--yes`, or y in the TUI.  The unit
takes one BLE connection, so while any of these runs, the phone app
cannot connect.

Commands other than `tui` log BLE recoveries to stderr; `RUST_LOG`
sets the filter.

## Daemon

`jackerymon daemon` holds the connection and serves HTTP on
`127.0.0.1:9888` (`--listen`):

| Route | Gives |
|---|---|
| `GET /status` | link state, counters, latest sample |
| `GET /metrics` | Prometheus `jackery_*` metrics |
| `GET /events` | server-sent events: current state, then changes |
| `POST /settings` | one setting, e.g. `{"lm":1}`; answers once applied |

```sh
curl -s -X POST -H 'content-type: application/json' -d '{"oac":1}' \
    localhost:9888/settings
jackerymon tui --daemon http://127.0.0.1:9888
```

Setting keys and values are in `docs/protocol.md`.  Emergency charging
needs `?confirm=true`.  A setting not sent within 60 s is dropped.
`/metrics` omits readings older than 60 s.  `--log-dir` writes a
JSON-lines file per UTC day, about 10 MB.

The API has no authentication and can switch outputs.  Expose it only on
a trusted network; even on localhost, a web page can reach it through
DNS rebinding.

`contrib/systemd/` has an untested service unit and a rule that deletes
logs after 30 days.
