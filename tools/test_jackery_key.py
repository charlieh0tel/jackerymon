"""Offline checks for the APK-derived protocol and cloud request boundaries."""

import base64
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, Mock, patch

import jackery_key as poc
from cryptography.hazmat.primitives import padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes


class ProtocolTests(unittest.TestCase):
    """Known crypto vectors and synthetic advertisement/API fixtures."""

    def test_known_vectors(self):
        self.assertEqual(poc.rc4(b"Plaintext", b"Key").hex(), "bbf316e8d940af0ad3")
        self.assertEqual(poc.crc16(b"123456789").hex(), "374b")

    def test_status_command_layout(self):
        with patch.object(poc.secrets, "randbelow", return_value=0):
            packet = poc.status_command(b"test-key")
        wrapped = poc.rc4(packet, b"test-key")
        self.assertEqual(wrapped[:-3], bytes.fromhex("deed01070201"))
        self.assertEqual(wrapped[-3], 1)
        self.assertEqual(wrapped[-2:], poc.crc16(wrapped[:-2]))

    @staticmethod
    def response(body, page=None, count=1, code=0):
        frame = bytes.fromhex("dfec") + bytes(
            [code | (0x80 if page else 0), 6, 3, len(body)]
        )
        if page:
            frame += page.to_bytes(2, "big") + count.to_bytes(2, "big")
        wrapped = bytes(byte ^ 0x42 for byte in frame + body) + b"\x42"
        return poc.rc4(wrapped + poc.crc16(wrapped), b"test-key")

    def test_fragmented_and_coalesced_pages(self):
        second = self.response(b"100}", page=2, count=2)
        first = self.response(b'{"rb":', page=1, count=2)
        decoder = poc.StatusDecoder(b"test-key")
        self.assertIsNone(decoder.feed(second[:3]))
        self.assertEqual(decoder.feed(second[3:] + first), {"rb": 100})

    def test_status_rejects_corruption_wrong_key_and_device_error(self):
        packet = self.response(b'{"rb":100}')
        corrupted = packet[:-1] + bytes([packet[-1] ^ 1])
        with self.assertRaisesRegex(ValueError, "CRC"):
            poc.StatusDecoder(b"test-key").feed(corrupted)
        with self.assertRaisesRegex(ValueError, "header"):
            poc.StatusDecoder(b"wrong-key").feed(packet)
        with self.assertRaisesRegex(ValueError, "code 1"):
            poc.StatusDecoder(b"test-key").feed(self.response(b"", code=1))

    def test_status_conflicting_duplicate_page(self):
        decoder = poc.StatusDecoder(b"test-key")
        self.assertIsNone(decoder.feed(self.response(b'{"rb":', page=1, count=2)))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            decoder.feed(self.response(b'{"op":', page=1, count=2))

    def test_advertisement(self):
        # Serial's first character occupies the high byte of company ID.
        serial = "123456789012345"
        manufacturer = {0x3102: serial[1:].encode()}
        for battery in (None, 73):
            body = bytes.fromhex("000c112233445566")
            if battery is not None:
                body += bytes([battery])
            body += bytes.fromhex("0007")
            plain = bytes(byte ^ 0x52 for byte in body) + b"\x52"
            plain += poc.crc16(plain)
            payload = (
                plain
                if battery is None
                else poc.rc4(plain, (serial[:3] + serial[-5:] + "LYx*G!6u9#").encode())
            )
            result = poc.decode_advertisement(
                "address", manufacturer, {poc.SERVICE: payload}
            )
            self.assertEqual(
                result, poc.Device("address", serial, "112233445566", 12, 7, battery)
            )
            damaged = payload[:-1] + bytes([payload[-1] ^ 1])
            self.assertIsNone(
                poc.decode_advertisement(
                    "address", manufacturer, {poc.SERVICE: damaged}
                )
            )

    def test_login_uses_base64_text_as_aes_key(self):
        random = bytes(range(16))
        with patch.object(poc.secrets, "token_bytes", return_value=random):
            fields = poc.login_fields("owner@example.com", "password", "US", "client")
        decryptor = Cipher(
            algorithms.AES(base64.b64encode(random)), modes.ECB()
        ).decryptor()
        padded = (
            decryptor.update(base64.b64decode(fields["aesEncryptData"]))
            + decryptor.finalize()
        )
        unpadder = padding.PKCS7(128).unpadder()
        body = json.loads(unpadder.update(padded) + unpadder.finalize())
        self.assertEqual(body["password"], "password")
        self.assertEqual(body["regionCode"], "US")
        self.assertEqual(len(base64.b64decode(fields["rsaForAesKey"])), 128)

    def test_api_token_rotation_and_failure_redaction(self):
        cloud = poc.Cloud()
        self.addCleanup(cloud.session.close)
        response = Mock(status_code=200)
        response.json.return_value = {"code": 0, "token": "new-token", "data": {}}
        with patch.object(cloud.session, "request", return_value=response) as request:
            cloud.request("GET", "device/bluetoothKey", {"guid": "abc"})
            self.assertEqual(cloud.session.headers["token"], "new-token")
            self.assertFalse(request.call_args.kwargs["allow_redirects"])
            response.json.return_value = {"code": 10402, "msg": "secret"}
            with self.assertRaisesRegex(RuntimeError, "API error code 10402"):
                cloud.request("GET", "device/bluetoothKey")

    def test_no_automatic_binding_fallback(self):
        cloud = poc.Cloud()
        self.addCleanup(cloud.session.close)
        device = poc.Device("address", "123456789", "112233445566", 12, 7, None)
        with patch.object(
            cloud, "request", side_effect=RuntimeError("rejected")
        ) as request:
            with self.assertRaises(RuntimeError):
                cloud.retrieve(device, False)
            request.assert_called_once_with(
                "GET",
                "device/bluetoothKey",
                {
                    "deviceSn": device.serial,
                    "guid": device.guid,
                },
            )

    def test_private_file_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "key.json"
            poc.save_key(path, {"bluetoothKey": "test"})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                poc.save_key(path, {})


class ConnectionTests(unittest.IsolatedAsyncioTestCase):
    """Disconnect races must not erase valid data or hide the original error."""

    async def run_attempt(self, *, disconnect=False, write_error=None):
        client = Mock()
        client.__aenter__ = AsyncMock(return_value=client)
        client.__aexit__ = AsyncMock(return_value=False)
        client.start_notify = AsyncMock()
        client.stop_notify = AsyncMock(
            side_effect=poc.BleakError("Service Discovery has not been performed yet")
        )
        with (
            patch.object(
                poc.BleakScanner,
                "find_device_by_filter",
                new_callable=AsyncMock,
                return_value=Mock(),
            ),
            patch.object(poc, "BleakClient", return_value=client) as factory,
        ):

            async def write(*_args, **_kwargs):
                on_disconnect = factory.call_args.kwargs["disconnected_callback"]
                if write_error:
                    on_disconnect(client)
                    raise write_error
                if not disconnect:
                    receive = client.start_notify.call_args.args[1]
                    receive(None, ProtocolTests.response(b'{"rb":100}'))
                on_disconnect(client)

            client.write_gatt_char = AsyncMock(side_effect=write)
            try:
                return await poc.status_attempt(Mock(), b"test-key", 0.1)
            finally:
                client.stop_notify.assert_not_called()
                client.__aexit__.assert_awaited_once()

    async def test_disconnect_after_complete_status_preserves_result(self):
        self.assertEqual(await self.run_attempt(), {"rb": 100})

    async def test_disconnect_before_status_is_reported(self):
        with self.assertRaisesRegex(poc.BleakError, "disconnected before status"):
            await self.run_attempt(disconnect=True)

    async def test_write_failure_is_not_masked_by_cleanup(self):
        with self.assertRaisesRegex(poc.BleakError, "original write failure"):
            await self.run_attempt(write_error=poc.BleakError("original write failure"))

    async def test_transport_retry_and_protocol_fail_fast(self):
        path = Mock()
        path.read_text.return_value = json.dumps(
            {
                "model": 12,
                "serial": "123456789",
                "address": "address",
                "bluetoothKey": base64.b64encode(b"test-key").decode(),
            }
        )
        with (
            patch.object(poc, "status_attempt", new_callable=AsyncMock) as attempt,
            patch.object(poc.asyncio, "sleep", new_callable=AsyncMock),
            patch("sys.stderr"),
        ):
            attempt.side_effect = [poc.BleakError("discovery failed"), {"rb": 100}]
            self.assertEqual(await poc.read_status(path, None, 1), {"rb": 100})
            self.assertEqual(attempt.await_count, 2)
            attempt.reset_mock()
            attempt.side_effect = ValueError("Invalid BLE CRC")
            with self.assertRaisesRegex(ValueError, "CRC"):
                await poc.read_status(path, None, 1)
            attempt.assert_awaited_once()
            attempt.reset_mock()
            attempt.side_effect = poc.BleakError("discovery failed")
            with self.assertRaisesRegex(RuntimeError, "after 3 attempts"):
                await poc.read_status(path, None, 1)
            self.assertEqual(attempt.await_count, 3)


if __name__ == "__main__":
    unittest.main()
