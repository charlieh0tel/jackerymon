# Plan

Records why things are the way they are.  Read before proposing
architectural changes; update when a decision changes.

## Goal

A Rust workspace for monitoring Jackery units that can do MQTT:
a library, a logging daemon, a TUI, and a Prometheus exporter.
Start with IP; BLE may come later.

## Current state (2026-10-04): blocked on transport

We have no working direct, no-cloud transport to the test unit (an
Explorer 2000).  See [FINDINGS.md](FINDINGS.md) for the full evidence.
Summary:

- **Local MQTT** is real and documented, but only for the SolarVault3
  line.  No Explorer is known to support it, and the test unit's app
  showed no MQTT configuration page.
- **IP** is a dead end for the Explorer 2000: it runs no services and
  only dials Jackery's cloud over TLS pinned to a private CA.
- **BLE** is physically reachable and its frame format is understood,
  but each unit's BLE traffic is obfuscated with a per-device key that
  presently lives only behind Jackery's cloud account API.  A no-cloud
  BLE path is blocked on obtaining that key offline.

No Rust code is written yet; blocking that on a decision about
transport (below) avoids building against an interface we cannot reach.

## Decisions

- **No cloud.**  The project talks to units directly only.  Jackery
  cloud protocol facts are reference material (for property keys and
  command semantics), not a runtime dependency.
- **Transport is abstract.**  Whatever lands first (SolarVault local
  MQTT, or BLE on some unit), the library models a unit's state and
  commands behind a transport trait, so MQTT and BLE are
  interchangeable backends.  This keeps the Explorer blocker from
  dictating the whole design.
- **Property model follows the wire keys.**  Both the cloud/MQTT and
  BLE paths use the same short property keys (`rb`, `ip`, `op`, ...),
  so one typed model serves all transports.

## Open decisions (need user input)

1. Which transport to build against first, given the Explorer is
   blocked:
   - target SolarVault3-style local MQTT (needs such a unit), or
   - pursue a no-cloud BLE key path for the Explorer (currently no
     known method), or
   - park until Jackery documents local Explorer access.
2. Scope of "units that can do MQTT" in AGENTS.md vs. adding BLE as a
   first-class transport now rather than "future work".

## Housekeeping

- `scratch/` holds a downloaded APK, its decompiled source, and the
  `jadx`/`apkeep` tools used for protocol research.  It is not ours and
  must not be committed; keep it out of git (or delete it).
