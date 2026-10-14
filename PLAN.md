# Plan

Records why things are the way they are.  Read before proposing
architectural changes; update when a decision changes.  Observations
and measurements belong in [FINDINGS.md](FINDINGS.md), not here.

## Goal

A Rust workspace for monitoring Jackery units over BLE: a library, a
logging daemon, a TUI, a web view, and a Prometheus exporter.

## Decisions

### Scope and transport

- **BLE is the monitoring transport.**  IP and MQTT were ruled out for
  the Explorer (`FINDINGS.md`).  The library is built around the
  validated BLE session; no multi-transport abstraction.
- **Cloud only for the key.**  The user authorized reproducing the app's
  cloud key retrieval as a one-time bootstrap.  Monitoring and control
  never touch the cloud.
- **Key bootstrap is `jackerymon key`.**  It is always built in; the
  one-time cloud step lives in one CLI module, and the library stays
  cloud-free.  It never binds unless asked (`--bind` changes which
  account owns the unit).
- **Keys: one file per unit, by serial.**  `~/.config/jackerymon/` for
  interactive commands, `/etc/jackerymon/` for the daemon; `--unit
  SERIAL` chooses, and is optional when one key is present.
- **Several units: one daemon per unit.**  Each instance owns one unit's
  connection and port; one adapter can hold several connections.  The
  package's unit serves one unit; a template (`jackerymon@<serial>`)
  waits for a second unit.
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
  The unit reports no battery current or power, and the library does
  not derive one; the web view draws input minus output, marked as an
  estimate, since losses and the unit's own draw are hidden in it.
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
  `jackerymon` binary: `poll`, `scan`, `key`, `set`, `tui`, `daemon`,
  `compress-logs`.
- **The unit takes one BLE connection, so one process holds it.**  Every
  command that uses BLE takes `/run/lock/jackerymon-<serial>.lock`, one
  file shared by all users and the system daemon (sticky directory,
  opened without following symlinks; another user's lock is opened
  read-only).  While the lock is free, a connection BlueZ still shows is a
  dead process's leftover and safe to reuse and close.  The holder also
  locks out the phone app.
- **Clients find a running daemon.**  `tui` and `set` look for the named
  unit's socket (from `--unit` or `--key-file`), or the only one running,
  in the user's runtime directory and then `/run/jackerymon`, and check
  the daemon's serial before sending anything.  A directory or socket
  they may not open is an error, not a reason to go direct.  `--direct` skips the
  daemon; `--daemon` names one by URL or socket.  They never start or
  stop a daemon.
- **TUI.** It consumes updates, not BLE, so it connects directly or
  follows a daemon. `m` opens one inline settings list: ←/→ cycles a
  value, Enter applies it, the menu stays open. The requested value
  stays on screen while it applies. Each sample is aged by its own
  timestamp, so an old one replayed by a daemon does not look fresh.
- **Daemon.**  Owns the connection and retries forever.  The full API is
  on a Unix socket per unit, `<runtime directory>/jackerymon/<serial>.sock`:
  `GET /metrics` (Prometheus), `GET /status`, `GET /events` (server-sent
  events, current state first), `GET /history` (logged samples between
  two RFC 3339 times, streamed as JSON lines from the daily files,
  gzipped or not; the TUI fetches its chart window from it), `GET
  /series` and the web view (below), and `POST /settings` (the unit's
  one-key body; answers once status confirms it, within 60 s including
  queueing).  TCP, `127.0.0.1:9888` by default, serves the read-only
  routes unless `--tcp-control`.  A daemon refuses to start while
  another answers on its socket, and replaces a dead one's leftover.
  Optional `--log-dir` writes daily JSON-lines files.  Readings
  older than 60 s are left out of `/metrics`.  When polling ends, by
  signal or fatal error, HTTP stops too and the daemon exits with the
  polling result, so a supervisor sees failures.
- **Web view, after smartclock-web.**  The daemon serves one page at
  `/` over TCP, read-only: a status strip polled every second; power
  and battery charts over 10 min to 7 d; and the unit's settings.  The
  power chart draws each input source that delivered anything in the
  window, under a thin line for the reported total they should sum to.
  An AC output chart, volts and hertz averaged only while the output
  is on, appears when it was on in the window.  A right axis's range
  is centered on its data, so a steady 60 Hz sits mid-chart.  Under
  the charts, a state strip: AC and DC output on, input and output
  power flowing, fault or temperature alarm, and no data; a band marks
  any interval the state held in at all, fainter for part of it, so a
  brief fault shows at 7 d.  It uses power above zero rather than the
  unit's battery state, which reads idle while charging (FINDINGS.md).
  Y axes have fixed widths, and a chart without a right axis pads the
  same room, so the time axes line up.  Y axes have no rotated titles;
  each number carries its unit, in its line's color.  A right axis takes the left's
  number of divisions, each a round step, so its ticks fall on the
  left's gridlines.  Same
  look, `common.js` lifecycle, and uPlot 1.6.32 from jsdelivr as
  smartclock-web; served by the daemon rather than a separate process,
  since one daemon owns one unit.  `/series` averages samples into
  intervals of at most 5,000 points, and marks a stretch with no
  sample for over a minute (the `/metrics` staleness limit) and at
  least one empty interval with a row of nulls, so the charts break
  there rather than draw across it; `/status` carries the reading
  worded as the TUI words it (`text.rs`), and the
  footer's author, license and repository from Cargo metadata.  Playwright tests fake the API
  per test; because the daemon needs a key and an adapter to start, they
  serve the page's files from the tree, and a Rust test checks the
  daemon serves the same paths.
- **Control goes over the socket; TCP is read-only by default.**  The
  socket is mode 0660 and its directory decides the rest: a user's
  daemon is private; the system daemon's admits the `jackerymon` group.
  TCP has no token, peer, or `Host`
  check, so `--tcp-control` brings back the accepted risks: DNS
  rebinding lets a web page on the daemon's host change settings, and a
  remote listener exposes control.  Without it, TCP exposes telemetry
  only.

### Packaging

- **A `.deb` built by `cargo-deb`,** through the shared
  `charlieh0tel/deb-workflows` (`rust-ci.yml`, `rust-build-deb.yml`):
  amd64 and arm64, glibc of Ubuntu 22.04 so Debian bookworm installs it.
  A `v*` tag releases.  The toolchain is pinned in `rust-toolchain.toml`.
- **License: MIT OR Apache-2.0.**
- **The daemon runs as system user and group `jackerymon`,** from
  `debian/jackerymon.sysusers`, which cargo-deb (3.8 and later) installs
  and applies in `postinst`; the package has no hand-written maintainer
  scripts, so none can duplicate it.  Group members may use
  the socket in `/run/jackerymon` and read keys in `/etc/jackerymon`.
  The unit is installed disabled: it needs a key first.
- **`/etc/default/jackerymon` sets TCP:** `JACKERYMON_LISTEN` and
  `JACKERYMON_TCP_CONTROL`, which the daemon also reads from its
  environment.  A conffile, so edits survive upgrades.
- **Sample logs are kept forever, gzipped once a day ends,** in
  `/var/lib/jackerymon` (data, not disposable logs).  The daemon
  only appends; `jackerymon compress-logs`, run daily by a timer that
  the daemon's unit pulls in, compresses finished days (about 1 MB a day
  compressed, 10 MB plain).  Nothing deletes them.
- **cargo-deb applies one enable/start setting to every unit in a
  package,** so the timer is not a separately enabled unit; the daemon's
  `Wants=` starts it.
- **The package assumes a systemd host.**  Purging it keeps keys in
  `/etc/jackerymon` and history in `/var/lib/jackerymon`.
- **Known limits, accepted:** the unit lock is cooperative: a local user
  who owns a stale lock file can replace it, so it keeps honest
  processes apart rather than hostile users.  A custom `--socket` must
  be in a directory others cannot write, or someone could connect in the
  moment before its mode is set.  A sample written more than fifteen
  minutes after midnight for the day before, or a clock stepping back
  into a compressed day, is not compressed and makes `compress-logs`
  report the leftover file.
- **RUSTSEC-2023-0071 (`rsa`, Marvin timing attack) is accepted.**  It
  concerns private-key operations; `jackerymon key` only encrypts with
  the app's public key.  CI ignores that advisory by ID.

## Next steps and open questions

1. Run the daemon for days: dropouts, reconnects, stale-object removals,
   and whether polling keeps the radios on.  The radios turn off five
   minutes after power-on if nothing connects (`FINDINGS.md`, Radio
   controls); confirm that applies to this unit.
2. Install the package on the always-on host; test whether the daemon
   survives a bluetoothd restart and host sleep.
3. Where Prometheus runs, and so whether the daemon binds beyond
   localhost.
4. Compare telemetry with the display under a known load.
5. Reset/unbinding effect on the key.

## Housekeeping

- `scratch/` holds a downloaded APK, its decompiled source, and the
  `jadx`/`apkeep` tools used for protocol research.  It is not ours and
  must not be committed.
