# jackerymon

Direct monitoring research for Jackery power stations. No Rust workspace
yet; see [PLAN.md](PLAN.md) and [FINDINGS.md](FINDINGS.md).

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
The status process made no cloud requests. Long-term key validity and
behavior after power-cycle/reset/unbinding remain unverified. Cloud
access is needed for the implemented key acquisition.

Internally, `StatusSession` owns connection-loss notification and one
pending request, with a fresh decoder for each query. This separates
request state from connection setup in preparation for polling.

Offline tests:

```sh
uv run --with bleak --with cryptography --with requests python -m unittest discover -s tools -p 'test_*.py'
```
