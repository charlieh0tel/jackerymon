# Plan

Records why things are the way they are.  Read before proposing
architectural changes; update when a decision changes.  Observations
and measurements belong in [FINDINGS.md](FINDINGS.md), not here.

## Goal

A Rust workspace for monitoring Jackery units over BLE: a library, a
logging daemon, a TUI, and a Prometheus exporter.  The Explorer protocol
was proven with a small Python PoC (`tools/jackery_key.py`), then ported.

## Decisions

### Scope and transport

- **BLE is the monitoring transport.**  IP and MQTT were ruled out for
  the Explorer (`FINDINGS.md`).  The library is built around the
  validated BLE session; no multi-transport abstraction.
- **Cloud only for the key.**  The user authorized reproducing the app's
  cloud key retrieval as a one-time bootstrap.  Monitoring and control
  never touch the cloud.
- **Key bootstrap stays in Python.**  A one-time step; porting it buys
  nothing.  Python tooling uses uv inline script dependencies.
- **Keys: one file per unit, by serial.**  `~/.config/jackerymon/` for
  interactive commands, `/etc/jackerymon/` for the daemon; `--unit
  SERIAL` chooses, and is optional when one key is present.  The file
  format is the PoC's, so either writes keys the other reads.
- **Several units: one daemon per unit.**  Each instance owns one unit's
  connection and port; one adapter can hold several connections.  A
  systemd template (`jackerymon@<serial>`) comes with the always-on
  host.
- **BLE library: `bluer`.**  btleplug came first, but the unit's empty
  random-type advertisement can take over its BlueZ object and hide it
  (`FINDINGS.md`, Duplicate advertisement).  Recovery needs BlueZ
  `RemoveDevice`, which `bluer` has and btleplug lacks; `bluer` also
  avoids a bluez-async panic when bluetoothd restarts.  Linux only.
- **How a connect finds the unit.**  Reuse a connection a dead process
  left open (a connected unit does not advertise); remove a random-type
  object holding the unit's address; scan LE only with a discovery
  pattern of that address, so BlueZ makes no objects for other devices;
  accept only a unit with an RSSI, meaning heard during this scan.
- **Library logging through `log`.**  The library reports recoveries it
  makes on its own, such as reusing a connection or removing the stale
  object.  The CLI logs to stderr with `env_logger`, except in `tui`,
  whose screen stderr would corrupt.

### Library

- **Property model follows the BLE wire keys.**  `Status` types the
  documented keys and keeps every field in `raw`.  Absent keys are
  `None`; wrong types fail; unknown enum values are kept as `Unknown`.
  Battery current or power is never derived from input/output totals.
- **Polling lives in the library.**  It is generic over a small
  connector/link pair so its retry policy is tested offline.  Events go
  to a callback that can stop polling; a stop future also closes the
  connection.  Five seconds between a response and the next request.
  A sample resets the failure count; invalid protocol data is fatal; a
  timeout closes the connection to discard late responses.  The failure
  limit is optional: `poll` defaults to three, monitors retry forever.
- **A setting counts as applied only when a status read shows it.**  The
  acknowledgment carries the resulting state, and the firmware
  acknowledges some settings it then ignores.  If the link drops
  mid-change, the first status after reconnecting decides.  Settings
  share the poller's connection and never overlap a status query.  A
  request whose requester has gone is not sent.
- **Control scope.**  AC/DC outputs, light, screen, auto-off and
  energy-saving timers, charge speed, battery protection, emergency
  charge, and UPS mode.  Reboot and power-off cannot be sent.  No custom
  charge power: model 12 acknowledges it without applying it.
- **Confirmation only for turning emergency charging on.**  Jackery
  reserves it for emergencies.  Outputs need no confirmation.

### Programs

- **One program, subcommands.**  `crates/jackerymon` is the library
  (typed errors, no `anyhow`); `crates/jackerymon-cli` builds the single
  `jackerymon` binary: `poll`, `set`, `tui`, `daemon`.
- **The unit takes one BLE connection.**  Whatever holds it, a direct
  TUI or the daemon, locks out the phone app and other clients.
- **TUI.**  It consumes updates, not BLE, so it connects directly or
  follows a daemon (`--daemon URL`); it never starts or stops one.  `m`
  opens one inline settings list: ←/→ cycles a value, Enter applies it,
  the menu stays open.  The requested value stays on screen while it
  applies.  Each sample is aged by its own timestamp, so an old one
  replayed by a daemon does not look fresh.
- **Daemon.**  Owns the connection and retries forever.  HTTP on
  `127.0.0.1:9888` by default: `GET /metrics` (Prometheus), `GET
  /status`, `GET /events` (server-sent events, current state first),
  `POST /settings` (the unit's one-key body; answers once status confirms
  it, within 60 s including queueing).  Optional `--log-dir` writes daily
  JSON-lines files; pruning is left to tmpfiles or logrotate.  Readings
  older than 60 s are left out of `/metrics`.  When polling ends, by
  signal or fatal error, HTTP stops too and the daemon exits with the
  polling result, so a supervisor sees failures.
- **Daemon access is unauthenticated, by the user's choice.**  Anyone who
  can reach it can change settings: no token, peer, or `Host` check.
  Accepted risks: DNS rebinding lets a web page in a browser on the
  daemon's host reach it even on localhost, and binding beyond
  localhost for a remote Prometheus exposes control too.  Browsers
  cannot post JSON cross-origin without a preflight the daemon never
  answers.

## Next steps and open questions

1. Run the daemon for days: dropouts, reconnects, stale-object removals,
   and whether polling keeps the radios on.  Why the radios sometimes
   turn off is unknown.
2. On the always-on host: rework the systemd unit (`DynamicUser=`, the
   key through `LoadCredential=`, no `bluetooth` group, which BlueZ's
   default D-Bus policy does not need) and test whether the daemon
   survives a bluetoothd restart and host sleep.
3. Where Prometheus runs, and so whether the daemon binds beyond
   localhost.
4. Compare telemetry with the display under a known load.
5. Reset/unbinding effect on the key.

## Housekeeping

- `scratch/` holds a downloaded APK, its decompiled source, and the
  `jadx`/`apkeep` tools used for protocol research.  It is not ours and
  must not be committed.
