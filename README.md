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
Turning emergency charging on needs `--yes`, or y in the TUI.

The unit takes one BLE connection, so one jackerymon holds it at a time
(a lock in `/run/lock`), and while one does the phone app cannot
connect.  If a daemon is running, `tui` and `set` find it and go
through it; `--direct` connects to the unit instead, and `--daemon`
names a daemon by URL or socket.

Commands other than `tui` log BLE recoveries to stderr; `RUST_LOG`
sets the filter.

## Daemon

`jackerymon daemon` holds the connection and serves the full API on a
Unix socket, `$XDG_RUNTIME_DIR/jackerymon/<serial>.sock` (packaged:
`/run/jackerymon/`), and the read-only routes over TCP for Prometheus:

| Route | Gives | TCP |
|---|---|---|
| `GET /status` | link state, counters, latest sample | yes |
| `GET /metrics` | Prometheus `jackery_*` metrics | yes |
| `GET /events` | server-sent events: current state, then changes | yes |
| `GET /history?since=…&until=…` | logged samples, JSON lines; needs `--log-dir` | yes |
| `POST /settings` | one setting, e.g. `{"lm":1}`; answers once applied | only with `--tcp-control` |

```sh
curl -s --unix-socket $XDG_RUNTIME_DIR/jackerymon/<serial>.sock \
    -X POST -H 'content-type: application/json' -d '{"oac":1}' \
    http://localhost/settings
```

Setting keys and values are in `docs/protocol.md`; other options are in
`jackerymon daemon --help`. A custom `--socket` belongs in a directory
only its users can write. TCP has no authentication, so with
`--tcp-control` anyone who reaches it, including a web page through DNS
rebinding, can switch outputs.

## Install

Releases carry `.deb` packages for amd64 and arm64.  The package adds a
`jackerymon` user and group and a `jackerymon` service, installed
disabled.  To run the daemon:

```sh
sudo apt install ./jackerymon_*.deb
jackerymon key                       # if you have no key yet
sudo install -m 0640 -g jackerymon ~/.config/jackerymon/<serial>.json /etc/jackerymon/
sudo usermod -aG jackerymon $USER    # control through the socket; log in again
sudo systemctl enable --now jackerymon
```

Daemon options are in `/etc/default/jackerymon`.  Samples go to
`/var/lib/jackerymon/`, one file per day; a daily timer gzips finished
days, and nothing is deleted.

To build the package: `cargo deb -p jackerymon-cli`; to release, see
[RELEASING.md](RELEASING.md).
