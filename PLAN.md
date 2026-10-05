# Plan

Records why things are the way they are.  Read before proposing
architectural changes; update when a decision changes.

## Goal

A Rust workspace for monitoring Jackery units that can do MQTT:
a library, a logging daemon, a TUI, and a Prometheus exporter.
Investigate Explorer BLE first with a small Python bootstrap PoC.

## Current state (2026-10-04): direct BLE status works

The Python PoC reads status directly from the Explorer 2000 over BLE
using a user-acquired key. See [FINDINGS.md](FINDINGS.md) for the evidence.
Summary:

- **Local MQTT** is real and documented, but only for the SolarVault3
  line.  No Explorer is known to support it, and the test unit's app
  showed no MQTT configuration page.
- **IP** is a dead end for the Explorer 2000: it runs no services and
  only dials Jackery's cloud over TLS pinned to a private CA.
- **BLE** status has been validated with a per-device key that
  is fetched from Jackery's cloud and cached by the Android app.
  `tools/jackery_key.py` reproduces advertisement decoding and cloud key
  acquisition, plus a validated model-12 GATT status query. The user
  completed login and supplied the resulting local key file.
  Wi-Fi credentials were removed before testing began. Three repeat
  reads succeeded using the same saved key with no reconnect retries.

No Rust code is written yet. The immediate transport blocker is resolved;
the Python PoC provides a reference for the eventual Rust BLE backend.

## Decisions

- **Direct monitoring, cloud bootstrap PoC.** The user authorized a Python
  PoC reproducing the app's cloud key retrieval. This is an exception to
  the earlier no-cloud research scope. Monitoring should still use BLE
  directly. Status queries succeeded without client cloud requests and
  with Wi-Fi credentials already absent before testing. Sustained polling,
  key lifetime, and power-cycle behavior remain to be tested.
- **Python tooling uses uv.** Inline script dependencies keep the PoC
  runnable without a manually managed virtual environment.
- **BLE request state belongs to a session.** `StatusSession` separates
  notification/disconnect handling and per-query decoder state from the
  connection setup, preparing to reuse a connection for polling.
- **Bounded BLE reconnects.** The unit sometimes disconnects during service
  discovery or before status arrives. The read-only query retries transport
  failures three times with fresh discovery and decoder state. Protocol/key
  errors fail immediately. Notification cleanup is left to disconnect.
- **Transport is abstract.**  Whatever lands first (SolarVault local
  MQTT, or BLE on some unit), the library models a unit's state and
  commands behind a transport trait, so MQTT and BLE are
  interchangeable backends.
- **Property model follows the wire keys.**  Both the cloud/MQTT and
  BLE paths use the same short property keys (`rb`, `ip`, `op`, ...),
  so one typed model serves all transports.

## Open decisions (need user input)

1. Whether the bootstrap key permits sustained offline BLE monitoring,
   and whether reset or unbinding invalidates it.
2. Scope of "units that can do MQTT" in AGENTS.md vs. adding BLE as a
   first-class transport now rather than "future work".

## Housekeeping

- `scratch/` holds a downloaded APK, its decompiled source, and the
  `jadx`/`apkeep` tools used for protocol research.  It is not ours and
  must not be committed; keep it out of git (or delete it).
