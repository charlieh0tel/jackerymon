//! Cipher and checksum used by the Android app's BLE codec.

const CRC16_MODBUS_INIT: u16 = 0xFFFF;
const CRC16_MODBUS_POLY_REVERSED: u16 = 0xA001;
const RC4_STATE_SIZE: usize = 256;

/// RC4 restarted for each packet, accepting any nonempty key length as the
/// app does.
pub(crate) fn rc4(data: &[u8], key: &[u8]) -> Vec<u8> {
    assert!(!key.is_empty(), "RC4 key must be nonempty");
    let mut state: [u8; RC4_STATE_SIZE] =
        std::array::from_fn(|index| u8::try_from(index).expect("RC4 state index fits in a byte"));
    let mut j = 0u8;
    for i in 0..RC4_STATE_SIZE {
        j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
        state.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(state[usize::from(i)]);
            state.swap(usize::from(i), usize::from(j));
            let index = state[usize::from(i)].wrapping_add(state[usize::from(j)]);
            byte ^ state[usize::from(index)]
        })
        .collect()
}

/// CRC-16/MODBUS, little-endian as it appears on the wire.
pub(crate) fn crc16(data: &[u8]) -> [u8; 2] {
    data.iter()
        .fold(CRC16_MODBUS_INIT, |crc, &byte| {
            (0..u8::BITS).fold(crc ^ u16::from(byte), |crc, _| {
                if crc & 1 == 1 {
                    (crc >> 1) ^ CRC16_MODBUS_POLY_REVERSED
                } else {
                    crc >> 1
                }
            })
        })
        .to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::crc16;
    use super::rc4;
    use crate::test_support::hex;

    #[test]
    fn known_vectors() {
        assert_eq!(rc4(b"Plaintext", b"Key"), hex("bbf316e8d940af0ad3"));
        assert_eq!(crc16(b"123456789").to_vec(), hex("374b"));
    }

    #[test]
    fn rc4_round_trips() {
        let key = b"test-key";
        assert_eq!(rc4(&rc4(b"status", key), key), b"status");
    }
}
