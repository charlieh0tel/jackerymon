# Plan

Records why things are the way they are.  Read before proposing
architectural changes; update when a decision changes.

## Goal

A Rust workspace for monitoring Jackery units over BLE:
a library, a logging daemon, a TUI, and a Prometheus exporter.
Prove the Explorer protocol with a small Python PoC, then port it to Rust.

## Current state (2026-10-10): BLE status works; key survives power-cycle

The Python PoC reads status directly from the Explorer 2000 over BLE
using a user-acquired key. See [FINDINGS.md](FINDINGS.md) for the evidence.
Summary:

- **BLE** status has been validated with a per-device key that
  is fetched from Jackery's cloud and cached by the Android app.
  `tools/jackery_key.py` reproduces advertisement decoding and cloud key
  acquisition, plus a validated model-12 GATT status query. The user
  completed login and supplied the resulting local key file.
  Wi-Fi credentials were removed before testing began. Three repeat
  reads succeeded using the same saved key with no reconnect retries.
  Persistent polling then produced 12 samples over one connection in
  62.4 seconds, with zero transport failures.
  A five-minute run with USB, AC, and light enabled produced 60 samples
  on one connection, zero failures, and 216.2 ms median query latency.

After a 2026-10-05 power-cycle the unit stopped advertising for days
(no Jackery advertisement at the HCI layer); by 2026-10-10 it was back,
cause unknown. The saved key then still worked: it survives power-cycle.

The Rust library `crates/jackerymon` ports the codec, advertisement
decoding, key-file loading, and typed status from the PoC, with offline
tests. BLE transport is not ported yet. Key bootstrap stays in Python.

## Decisions

- **Direct monitoring, cloud bootstrap PoC.** The user authorized a Python
  PoC reproducing the app's cloud key retrieval. This is an exception to
  the earlier no-cloud research scope. Monitoring should still use BLE
  directly. Status queries succeeded without client cloud requests and
  with Wi-Fi credentials already absent before testing. Short persistent
  polling is validated and the key survives power-cycle; long-duration
  reliability, key lifetime, and reset/unbinding remain to be tested.
- **Python tooling uses uv.** Inline script dependencies keep the PoC and tests
  runnable without a manually managed virtual environment.
- **BLE request state belongs to a session.** `StatusSession` separates
  notification/disconnect handling and per-query decoder state from the
  connection setup. Polling reuses the connection/subscription and permits
  only one query at a time, with fresh decoder state for each query.
- **Polling is sequential and observable.** Default delay is five seconds
  after each response. JSON lines include timestamps, query latency,
  connection count, and transport failure count. A successful sample resets
  the three-failure reconnect budget; invalid protocol data fails immediately.
  Timeout recovery closes the connection to discard late responses.
- **Bounded BLE reconnects.** The unit sometimes disconnects during service
  discovery or before status arrives. The read-only query retries transport
  failures three times with fresh discovery and decoder state. Protocol/key
  errors fail immediately. Notification cleanup is left to disconnect.
- **BLE is the monitoring transport.** Focus development on the working
  Explorer BLE protocol. Design the Rust library around the validated
  session behavior; a generic multi-transport abstraction is not needed.
- **BLE library: btleplug.** Chosen by the user over `bluer`. It covers
  everything the PoC does (scan with manufacturer data, connect,
  notify, write). Costs: generic errors hide BlueZ disconnect reasons
  (use `btmon`/journal instead), no passive advertisement monitoring,
  no adapter control. Keep BLE in one small module so a switch to
  `bluer` stays cheap if disconnect diagnosis needs it.
- **Property model follows the BLE wire keys.** `Status` types the
  documented keys and keeps every field in `raw`. Absent keys are `None`;
  wrong types fail; unknown enum values are kept as `Unknown`. Do not
  invent battery-current or battery-power measurements from input/output
  totals.
- **TUI first, daemon later.** The unit accepts one BLE connection, so
  eventually one daemon owns it: it polls, logs JSON lines, serves
  Prometheus `/metrics`, and serves HTTP JSON to the TUI.  To start,
  the TUI connects directly. It consumes a stream of samples, not BLE,
  so the daemon can replace the source without TUI changes. The TUI does
  not log. While it runs, the phone app cannot connect.
- **Workspace grows by need.** `crates/jackerymon` (library, typed errors,
  no `anyhow`) now; a JSON-lines poller binary, `jackerymon-tui`, and
  later `jackerymond` are added when started. Monitoring only; control
  commands later.
- **Key bootstrap stays in Python.** The cloud login is a one-time step;
  porting it buys nothing yet.

## Next steps and open questions

1. Measure polling reliability and diagnose intermittent disconnects.
   The unit sometimes turns its BLE/Wi-Fi radios off by itself; the
   daemon must treat long absence as normal. Find what turns them off
   and which button restores them.
2. Compare telemetry with the display under a known load.
3. Reset/unbinding effect on the key remains unknown.
4. Port the BLE session (btleplug) and a JSON-lines poller binary;
   validate against the unit.
5. TUI: battery, input/output power, temperature, output states, error
   code, connection state, data age, power chart.

## Housekeeping

- `scratch/` holds a downloaded APK, its decompiled source, and the
  `jadx`/`apkeep` tools used for protocol research.  It is not ours and
  must not be committed; keep it out of git (or delete it).
