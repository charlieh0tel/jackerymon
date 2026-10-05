# /// script
# requires-python = ">=3.11"
# dependencies = ["bleak>=0.21,<4", "cryptography>=41", "requests>=2.31,<3"]
# ///
"""Discover an HT Jackery over BLE and retrieve its key using the Android API."""

import argparse
import asyncio
import base64
import getpass
import json
import math
import os
import secrets
import sys
import uuid
from dataclasses import asdict, dataclass
from datetime import datetime
from pathlib import Path

import requests
from bleak import BleakClient, BleakScanner
from bleak.exc import BleakError
from cryptography.hazmat.primitives import padding, serialization
from cryptography.hazmat.primitives.asymmetric import padding as rsa_padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

SERVICE = "0000bdee-0000-1000-8000-00805f9b34fb"
COMMAND = "0000ee01-0000-1000-8000-00805f9b34fb"
NOTIFY = "0000ee02-0000-1000-8000-00805f9b34fb"
READ_DEVICE_INFO = 6
QUERY_PROPERTY = 3
FRAME_HEADER = bytes.fromhex("dfec")
MAX_RESPONSE_BYTES = 65536
STATUS_ATTEMPTS = 3
RECONNECT_DELAY = 1.0
API = "https://iot.jackeryapp.com/v1/"
PUBLIC_KEY = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQCVmzgJy/4XolxPnkfu32YtJqYGFLYqf9/rnVgURJED+8J9J3Pccd6+9L97/+7COZE5OkejsgOkqeLNC9C3r5mhpE4zk/HStss7Q8/5DqkGD1annQ+eoICo3oi0dITZ0Qll56Dowb8lXi6WHViVDdih/oeUwVJY89uJNtTWrz7t7QIDAQAB"


@dataclass(frozen=True)
class Device:
    """Identity and provisioning fields decoded from one BLE advertisement."""

    address: str
    serial: str
    guid: str
    model: int
    bind_counter: int
    battery: int | None


def rc4(data: bytes, key: bytes) -> bytes:
    """Match the APK's per-packet RC4, including its nonstandard key lengths."""
    if not key:
        raise ValueError("Empty RC4 key")
    state = list(range(256))
    j = 0
    for i in range(256):
        j = (j + state[i] + key[i % len(key)]) % 256
        state[i], state[j] = state[j], state[i]
    i = j = 0
    output = bytearray()
    for byte in data:
        i = (i + 1) % 256
        j = (j + state[i]) % 256
        state[i], state[j] = state[j], state[i]
        output.append(byte ^ state[(state[i] + state[j]) % 256])
    return bytes(output)


def crc16(data: bytes) -> bytes:
    crc = 0xFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0xA001 if crc & 1 else 0)
    return crc.to_bytes(2, "little")


def decode_advertisement(address, manufacturer, service_data):
    payload = service_data.get(SERVICE)
    if payload is None or len(payload) not in (13, 14):
        return None
    for company, value in sorted(manufacturer.items()):
        prefix = company.to_bytes(2, "little")
        if prefix[0] != 2:  # JACKERY_DEVICE; accessories use another category.
            continue
        try:
            serial = (prefix[1:] + value).decode("ascii")
        except UnicodeDecodeError:
            continue
        if len(serial) < 8 or not serial.isdecimal():
            continue
        plain = payload
        if len(payload) == 14:
            key = (serial[:3] + serial[-5:] + "LYx*G!6u9#").encode()
            plain = rc4(payload, key)
        if crc16(plain[:-2]) != plain[-2:]:
            continue
        body = bytes(byte ^ plain[-3] for byte in plain[:-3])
        return Device(
            address,
            serial,
            body[2:8].hex().upper(),
            int.from_bytes(body[:2], "big"),
            int.from_bytes(body[-2:], "big"),
            body[8] if len(payload) == 14 else None,
        )
    return None


async def discover(address, timeout):
    found = {}

    def receive(device, advertisement):
        if address and device.address.lower() != address.lower():
            return
        parsed = decode_advertisement(
            device.address,
            advertisement.manufacturer_data,
            advertisement.service_data,
        )
        if parsed:
            found[device.address] = parsed

    async with BleakScanner(detection_callback=receive):
        await asyncio.sleep(timeout)
    return list(found.values())


def login_fields(account, password, region, client_id):
    # The APK base64-encodes 16 random bytes, then uses those 24 ASCII bytes
    # as the AES key. Decoding that string before encryption would be wrong.
    key = base64.b64encode(secrets.token_bytes(16))
    body = json.dumps(
        {
            "account": account,
            "password": password,
            "regionCode": region,
            "loginType": 2,
            "registerAppId": "com.hbxn.jackery",
            "macId": client_id,
        },
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode()
    padder = padding.PKCS7(128).padder()
    padded = padder.update(body) + padder.finalize()
    encryptor = Cipher(algorithms.AES(key), modes.ECB()).encryptor()
    encrypted = encryptor.update(padded) + encryptor.finalize()
    public = serialization.load_der_public_key(base64.b64decode(PUBLIC_KEY))
    wrapped = public.encrypt(key, rsa_padding.PKCS1v15())
    return {
        "aesEncryptData": base64.b64encode(encrypted).decode(),
        "rsaForAesKey": base64.b64encode(wrapped).decode(),
    }


class Cloud:
    """Short-lived API session; credentials and tokens are never saved."""

    def __init__(self):
        self.session = requests.Session()
        self.session.headers.update(
            {
                "Accept-Language": "en",
                "platform": "2",
                "app_version": "2.4.2",
                "app_version_code": "101",
                "sys_version": "Android 14,level 34/[arm64-v8a]",
                "model": "jackerymon/python-poc",
                "network": "wifi",
            }
        )

    def request(self, method, path, fields=None):
        try:
            response = self.session.request(
                method,
                API + path,
                params=fields if method == "GET" else None,
                data=fields if method == "POST" else None,
                timeout=30,
                allow_redirects=False,
            )
        except requests.RequestException:
            raise RuntimeError(f"{path}: network request failed") from None
        if response.status_code != 200:
            raise RuntimeError(f"{path}: HTTP {response.status_code}")
        try:
            envelope = response.json()
        except ValueError:
            raise RuntimeError(f"{path}: invalid JSON response") from None
        if not isinstance(envelope, dict) or envelope.get("code") != 0:
            code = envelope.get("code") if isinstance(envelope, dict) else None
            raise RuntimeError(f"{path}: API error code {code!r}")
        if envelope.get("token"):
            self.session.headers["token"] = envelope["token"]
        return envelope.get("data")

    def login(self, account, password, region, client_id):
        self.request(
            "POST", "auth/login", login_fields(account, password, region, client_id)
        )
        if not self.session.headers.get("token"):
            raise RuntimeError("Login succeeded without a token")

    def retrieve(self, device, bind):
        if bind:
            offset = datetime.now().astimezone().utcoffset()
            data = self.request(
                "POST",
                "device/bind",
                {
                    "devId": device.serial,
                    "guid": device.guid,
                    "bindKey": device.bind_counter,
                    "timezoneOffset": int(offset.total_seconds() / 60),
                },
            )
        else:
            data = self.request(
                "GET",
                "device/bluetoothKey",
                {
                    "deviceSn": device.serial,
                    "guid": device.guid,
                },
            )
        if (
            not isinstance(data, dict)
            or not isinstance(data.get("bluetoothKey"), str)
            or not data["bluetoothKey"]
        ):
            raise RuntimeError("Response contains no BLE key; no file saved")
        return {
            **asdict(device),
            "bluetoothKey": data["bluetoothKey"],
            "modelCode": data.get("modelCode"),
            "region": data.get("region"),
        }


def save_key(path, data):
    # Exclusive creation avoids overwriting a key or following a symlink.
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as output:
        json.dump(data, output, indent=2, ensure_ascii=True)
        output.write("\n")


def status_command(key):
    frame = FRAME_HEADER + bytes([0, READ_DEVICE_INFO, QUERY_PROPERTY, 0])
    mask = secrets.randbelow(255) + 1
    wrapped = bytes(byte ^ mask for byte in frame) + bytes([mask])
    return rc4(wrapped + crc16(wrapped), key)


class StatusDecoder:
    """Bounded DFEC notification stream and paginated response assembly."""

    def __init__(self, key):
        self.key = key
        self.buffer = bytearray()
        self.pages = {}
        self.page_count = None
        self.page_metadata = None
        self.received = 0

    def feed(self, fragment):
        self.received += len(fragment)
        if self.received > MAX_RESPONSE_BYTES:
            raise ValueError("BLE response exceeds PoC size limit")
        self.buffer.extend(fragment)
        while len(self.buffer) >= 6:
            wrapped = rc4(bytes(self.buffer), self.key)
            # DFEC provides the XOR mask before the trailing mask arrives.
            mask = wrapped[0] ^ FRAME_HEADER[0]
            frame = bytes(byte ^ mask for byte in wrapped)
            if frame[:2] != FRAME_HEADER:
                raise ValueError("Invalid BLE header; wrong key or unsupported codec")
            paginated = bool(frame[2] & 0x80)
            header_length = 10 if paginated else 6
            packet_length = header_length + frame[5] + 3
            if len(wrapped) < packet_length:
                return None
            packet = wrapped[:packet_length]
            if packet[-3] != mask or crc16(packet[:-2]) != packet[-2:]:
                raise ValueError("Invalid BLE CRC or XOR mask")
            del self.buffer[:packet_length]
            if frame[3] != READ_DEVICE_INFO:
                continue
            code = frame[2] & 0x7F
            if code:
                raise ValueError(f"Device status query failed with code {code}")
            body = frame[header_length : packet_length - 3]
            if paginated:
                page = int.from_bytes(frame[6:8], "big")
                count = int.from_bytes(frame[8:10], "big")
                if not 1 <= page <= count <= 256:
                    raise ValueError("Invalid BLE pagination")
                metadata = (frame[3], frame[4], code)
                if self.page_count is not None and (
                    count != self.page_count or metadata != self.page_metadata
                ):
                    raise ValueError("Inconsistent BLE pagination")
                if page in self.pages and self.pages[page] != body:
                    raise ValueError("Conflicting duplicate BLE page")
                self.page_count = count
                self.page_metadata = metadata
                self.pages[page] = body
                if len(self.pages) != count:
                    continue
                body = b"".join(self.pages[index] for index in range(1, count + 1))
            result = json.loads(body.decode("utf-8") or "{}")
            if not isinstance(result, dict):
                raise TypeError("Expected a JSON object in BLE status response")
            return result
        return None


async def read_status(path, address, timeout):
    saved = json.loads(path.read_text())
    if not isinstance(saved, dict) or saved.get("model") != 12:
        raise ValueError("Offline status currently supports only model 12")
    try:
        key = base64.b64decode(saved["bluetoothKey"], validate=True)
        serial = saved["serial"]
        target = address or saved["address"]
    except (KeyError, TypeError, ValueError):
        raise ValueError("Key file has invalid identity or base64 key") from None
    if not key:
        raise ValueError("Empty BLE key")

    def matches(device, advertisement):
        if device.address.lower() != target.lower():
            return False
        decoded = decode_advertisement(
            device.address, advertisement.manufacturer_data, advertisement.service_data
        )
        return decoded is not None and decoded.serial == serial and decoded.model == 12

    for attempt in range(1, STATUS_ATTEMPTS + 1):
        try:
            return await status_attempt(matches, key, timeout)
        except (BleakError, TimeoutError) as error:
            if attempt == STATUS_ATTEMPTS:
                raise RuntimeError(
                    f"BLE status failed after {attempt} attempts: {error}. "
                    "Wake the unit and close other BLE clients, then retry"
                ) from error
            print(
                f"BLE attempt {attempt}/{STATUS_ATTEMPTS} failed: {error}; reconnecting",
                file=sys.stderr,
            )
            await asyncio.sleep(RECONNECT_DELAY)


class StatusSession:
    """One BLE connection with a single pending status request."""

    def __init__(self, key):
        self.key = key
        self.decoder = None
        self.result = None
        self.disconnected = asyncio.Event()

    def on_disconnect(self, _client):
        self.disconnected.set()
        if self.result is not None and not self.result.done():
            self.result.set_exception(
                BleakError("Device disconnected before status completed")
            )

    def receive(self, _characteristic, fragment):
        if self.result is None or self.result.done():
            return
        try:
            status = self.decoder.feed(fragment)
            if status is not None:
                self.result.set_result(status)
        except (ValueError, TypeError) as error:
            self.result.set_exception(error)

    async def query(self, client, timeout):
        if self.disconnected.is_set():
            raise BleakError("Device disconnected before status request")
        if self.result is not None:
            raise RuntimeError("A status request is already pending")
        self.decoder = StatusDecoder(self.key)
        self.result = asyncio.get_running_loop().create_future()
        try:
            await client.write_gatt_char(
                COMMAND, status_command(self.key), response=False
            )
            try:
                return await asyncio.wait_for(self.result, timeout)
            except TimeoutError:
                raise TimeoutError(
                    f"No complete status response ({self.decoder.received} notification bytes)"
                ) from None
        finally:
            if self.result.done() and not self.result.cancelled():
                self.result.exception()
            else:
                self.result.cancel()
            self.result = None
            self.decoder = None


async def status_attempt(matches, key, timeout):
    device = await BleakScanner.find_device_by_filter(matches, timeout=timeout)
    if device is None:
        raise BleakError("Saved device identity not found in BLE advertisements")
    session = StatusSession(key)
    # Disconnect stops notifications; stop_notify can mask a transport failure.
    async with BleakClient(
        device, timeout=timeout, disconnected_callback=session.on_disconnect
    ) as client:
        await client.start_notify(NOTIFY, session.receive)
        return await session.query(client, timeout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--address", help="BLE address of your unit")
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--scan-only", action="store_true", help="No cloud requests")
    parser.add_argument(
        "--status",
        type=Path,
        metavar="KEY_FILE",
        help="Read model-12 status over BLE without cloud access",
    )
    parser.add_argument("--account", help="Jackery email address; otherwise prompted")
    parser.add_argument(
        "--region", default="", help="Account regionCode; APK default is empty"
    )
    parser.add_argument(
        "--client-id", default=uuid.uuid4().hex, help="Login client identifier"
    )
    parser.add_argument(
        "--bind",
        action="store_true",
        help="Bind the selected unit to your account and retrieve its key",
    )
    parser.add_argument("--output", type=Path, default=Path("jackery-key.json"))
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive")
    if args.status:
        if args.bind or args.scan_only:
            parser.error("--status cannot be combined with --bind or --scan-only")
        print(
            json.dumps(
                asyncio.run(read_status(args.status, args.address, args.timeout)),
                indent=2,
                ensure_ascii=True,
            )
        )
        return
    if args.bind and args.scan_only:
        parser.error("--bind and --scan-only cannot be combined")
    if not args.scan_only and (args.output.exists() or args.output.is_symlink()):
        parser.error("Output already exists; choose a new --output path")
    devices = asyncio.run(discover(args.address, args.timeout))
    for device in devices:
        print(json.dumps(asdict(device)))
    if not devices:
        raise RuntimeError("No valid HT advertisement found; wake the unit and retry")
    if args.scan_only:
        return
    if len(devices) != 1:
        raise RuntimeError("Multiple units found; select yours with --address")
    account = args.account or input("Jackery email: ").strip()
    password = getpass.getpass("Jackery password: ")
    cloud = Cloud()
    try:
        cloud.login(account, password, args.region, args.client_id)
        data = cloud.retrieve(devices[0], args.bind)
        save_key(args.output, data)
    finally:
        cloud.session.close()
    print(f"BLE key saved to {args.output}; not yet validated against the unit")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, TypeError, BleakError) as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
    except (KeyboardInterrupt, EOFError):
        sys.exit(130)
