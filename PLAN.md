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

No Rust code is written yet. The Python PoC provides a reference for
the eventual Rust BLE backend.

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
- **Property model follows the BLE wire keys.** Use a typed model for
  battery percentage, temperature, input/output power, and output states.
  Preserve unknown fields during research; do not invent battery-current
  or battery-power measurements from input/output totals.

## Next steps and open questions

1. Measure polling reliability and diagnose intermittent disconnects,
   including multi-day advertising loss.
2. Compare telemetry with the display under a known load.
3. Reset/unbinding effect on the key remains unknown.
4. Port the validated BLE session, codec, and property model to Rust.

## Housekeeping

- `scratch/` holds a downloaded APK, its decompiled source, and the
  `jadx`/`apkeep` tools used for protocol research.  It is not ours and
  must not be committed; keep it out of git (or delete it).
