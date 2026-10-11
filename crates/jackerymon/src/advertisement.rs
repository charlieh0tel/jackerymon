//! Decoding of the identity and battery fields a unit advertises.

use std::iter;
use std::ops::Range;

use crate::cipher::crc16;
use crate::cipher::rc4;
use crate::identity::Guid;
use crate::identity::ModelCode;
use crate::identity::Serial;

/// Manufacturer-data category byte for Jackery power stations; accessories
/// use other values.
const JACKERY_DEVICE_CATEGORY: u8 = 2;
/// Appended to part of the serial to form the advertisement RC4 key, which
/// is distinct from the per-device GATT key.
const ADVERTISEMENT_KEY_SUFFIX: &str = "LYx*G!6u9#";
const SERIAL_PREFIX_LENGTH: usize = 3;
const SERIAL_SUFFIX_LENGTH: usize = 5;
/// Older units send 13 bytes in the clear; newer ones 14 RC4-encrypted
/// bytes that include battery percentage.
const PLAIN_SERVICE_DATA_LENGTH: usize = 13;
const ENCRYPTED_SERVICE_DATA_LENGTH: usize = 14;
const CRC_LENGTH: usize = 2;
const MODEL_RANGE: Range<usize> = 0..2;
const GUID_RANGE: Range<usize> = 2..8;
const BATTERY_OFFSET: usize = 8;

/// Identity and provisioning fields from one advertisement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advertisement {
    pub serial: Serial,
    pub guid: Guid,
    pub model: ModelCode,
    /// Bind/reset counter; the cloud bind request echoes it.
    pub bind_counter: u16,
    /// Only the encrypted advertisement variant carries this.
    pub battery_percent: Option<u8>,
}

/// Decodes an advertisement from its manufacturer data (company id to
/// value) and its service data for the Jackery service.  Returns `None`
/// for anything that is not a valid Jackery power-station advertisement.
pub fn decode<'a>(
    manufacturer: impl IntoIterator<Item = (u16, &'a [u8])>,
    service_data: &[u8],
) -> Option<Advertisement> {
    let encrypted = match service_data.len() {
        ENCRYPTED_SERVICE_DATA_LENGTH => true,
        PLAIN_SERVICE_DATA_LENGTH => false,
        _ => return None,
    };
    let mut entries: Vec<_> = manufacturer.into_iter().collect();
    entries.sort_unstable_by_key(|(company, _)| *company);
    entries.into_iter().find_map(|(company, value)| {
        // The company id's high byte is the serial's first character.
        let [category, first] = company.to_le_bytes();
        if category != JACKERY_DEVICE_CATEGORY {
            return None;
        }
        let text = String::from_utf8(iter::once(first).chain(value.iter().copied()).collect());
        let serial = Serial::try_from(text.ok()?).ok()?;
        let plain = if encrypted {
            rc4(service_data, &advertisement_key(&serial))
        } else {
            service_data.to_vec()
        };
        let (checked, crc) = plain.split_at(plain.len() - CRC_LENGTH);
        if crc16(checked) != crc {
            return None;
        }
        let (masked, mask) = checked.split_at(checked.len() - 1);
        let body: Vec<u8> = masked.iter().map(|byte| byte ^ mask[0]).collect();
        let (_, bind) = body.split_at(body.len() - 2);
        Some(Advertisement {
            serial,
            guid: Guid(body[GUID_RANGE].try_into().ok()?),
            model: ModelCode(u16::from_be_bytes(body[MODEL_RANGE].try_into().ok()?)),
            bind_counter: u16::from_be_bytes(bind.try_into().ok()?),
            battery_percent: encrypted.then(|| body[BATTERY_OFFSET]),
        })
    })
}

fn advertisement_key(serial: &Serial) -> Vec<u8> {
    let text = serial.as_str();
    [
        &text[..SERIAL_PREFIX_LENGTH],
        &text[text.len() - SERIAL_SUFFIX_LENGTH..],
        ADVERTISEMENT_KEY_SUFFIX,
    ]
    .concat()
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::Advertisement;
    use super::advertisement_key;
    use super::decode;
    use crate::cipher::crc16;
    use crate::cipher::rc4;
    use crate::identity::Guid;
    use crate::identity::ModelCode;
    use crate::identity::Serial;
    use crate::test_support::hex;

    const SERIAL: &str = "123456789012345";
    const MASK: u8 = 0x52;

    fn service_data(battery: Option<u8>) -> Vec<u8> {
        let body = [
            hex("000c112233445566"),
            battery.into_iter().collect(),
            hex("0007"),
        ]
        .concat();
        let masked: Vec<u8> = body.iter().map(|byte| byte ^ MASK).chain([MASK]).collect();
        let plain = [masked.clone(), crc16(&masked).to_vec()].concat();
        match battery {
            Some(_) => rc4(&plain, &advertisement_key(&serial())),
            None => plain,
        }
    }

    fn serial() -> Serial {
        Serial::try_from(SERIAL.to_owned()).expect("valid serial")
    }

    fn decode_with(payload: &[u8]) -> Option<Advertisement> {
        // The serial's first character occupies the company id's high byte.
        let company = u16::from_le_bytes([2, SERIAL.as_bytes()[0]]);
        decode([(company, &SERIAL.as_bytes()[1..])], payload)
    }

    #[test]
    fn decodes_both_variants() {
        for battery in [None, Some(73)] {
            assert_eq!(
                decode_with(&service_data(battery)),
                Some(Advertisement {
                    serial: serial(),
                    guid: Guid([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]),
                    model: ModelCode::EXPLORER_2000,
                    bind_counter: 7,
                    battery_percent: battery,
                })
            );
        }
    }

    #[test]
    fn rejects_damage_and_other_categories() {
        for battery in [None, Some(73)] {
            let mut damaged = service_data(battery);
            *damaged.last_mut().expect("nonempty") ^= 1;
            assert_eq!(decode_with(&damaged), None);
        }
        let accessory = u16::from_le_bytes([3, SERIAL.as_bytes()[0]]);
        assert_eq!(
            decode([(accessory, &SERIAL.as_bytes()[1..])], &service_data(None)),
            None
        );
        assert_eq!(decode_with(&service_data(None)[1..]), None);
    }
}
