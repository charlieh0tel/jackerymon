//! Model-12 GATT framing: the status request and response reassembly.
//!
//! Inner frame: `DFEC`, flags/result byte, action, message type, body
//! length, optional page index and count, body.  Every byte is XORed by a
//! random mask, the mask and a CRC follow, and the packet is RC4-encrypted
//! with the device key, restarting for each packet.

use std::collections::BTreeMap;
use std::num::NonZeroU8;

use serde_json::Map;
use serde_json::Value;
use thiserror::Error;

use crate::cipher::crc16;
use crate::cipher::rc4;
use crate::identity::BleKey;

const FRAME_HEADER: [u8; 2] = [0xdf, 0xec];
const READ_DEVICE_INFO: u8 = 6;
const QUERY_PROPERTY: u8 = 3;
const PAGINATED_FLAG: u8 = 0x80;
const RESULT_CODE_MASK: u8 = 0x7f;
const HEADER_LENGTH: usize = 6;
const PAGINATED_HEADER_LENGTH: usize = 10;
const FLAGS_OFFSET: usize = 2;
const ACTION_OFFSET: usize = 3;
const MESSAGE_TYPE_OFFSET: usize = 4;
const BODY_LENGTH_OFFSET: usize = 5;
const PAGE_INDEX_OFFSET: usize = 6;
const PAGE_COUNT_OFFSET: usize = 8;
const CRC_LENGTH: usize = 2;
/// XOR mask byte plus CRC.
const TRAILER_LENGTH: usize = 1 + CRC_LENGTH;
/// Bounds memory if a peer streams garbage that keeps decoding.
const MAX_RESPONSE_BYTES: usize = 65_536;
const MAX_PAGES: u16 = 256;

/// Status fields keyed by wire name, unknown keys included.
pub type RawStatus = Map<String, Value>;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("BLE response exceeds {MAX_RESPONSE_BYTES} bytes")]
    TooLarge,
    #[error("invalid BLE header; wrong key or unsupported codec")]
    InvalidHeader,
    #[error("invalid BLE CRC or XOR mask")]
    InvalidChecksum,
    #[error("device status query failed with code {0}")]
    Device(u8),
    #[error("invalid BLE pagination")]
    InvalidPagination,
    #[error("inconsistent BLE pagination")]
    InconsistentPagination,
    #[error("conflicting duplicate BLE page")]
    ConflictingPage,
    #[error("invalid status JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("expected a JSON object in BLE status response")]
    NotObject,
}

/// Encrypted read-device-info request.  The app picks `mask` at random for
/// each packet.
#[must_use]
pub fn status_command(key: &BleKey, mask: NonZeroU8) -> Vec<u8> {
    let frame = [
        FRAME_HEADER[0],
        FRAME_HEADER[1],
        0,
        READ_DEVICE_INFO,
        QUERY_PROPERTY,
        0,
    ];
    let wrapped: Vec<u8> = frame
        .iter()
        .map(|byte| byte ^ mask.get())
        .chain([mask.get()])
        .collect();
    rc4(
        &[wrapped.clone(), crc16(&wrapped).to_vec()].concat(),
        key.as_bytes(),
    )
}

/// Header fields every page of one paginated response must share.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pagination {
    count: u16,
    action: u8,
    message_type: u8,
}

/// Reassembles one status response from notification fragments.  Use a
/// fresh decoder for each request.
pub struct StatusDecoder {
    key: BleKey,
    /// Undecoded bytes, starting at a packet boundary.
    buffer: Vec<u8>,
    /// Bodies of the pages received so far, by page index.
    pages: BTreeMap<u16, Vec<u8>>,
    pagination: Option<Pagination>,
    received: usize,
}

impl StatusDecoder {
    #[must_use]
    pub fn new(key: BleKey) -> Self {
        Self {
            key,
            buffer: Vec::new(),
            pages: BTreeMap::new(),
            pagination: None,
            received: 0,
        }
    }

    /// Notification bytes fed so far.
    #[must_use]
    pub fn received(&self) -> usize {
        self.received
    }

    /// Feeds one notification.  Returns the status once the response is
    /// complete; packets for other actions are skipped.
    ///
    /// # Errors
    ///
    /// Fails on a wrong key, corruption, inconsistent pages, a device
    /// result code, or a body that is not a JSON object.
    pub fn feed(&mut self, fragment: &[u8]) -> Result<Option<RawStatus>, DecodeError> {
        self.received += fragment.len();
        if self.received > MAX_RESPONSE_BYTES {
            return Err(DecodeError::TooLarge);
        }
        self.buffer.extend_from_slice(fragment);
        while self.buffer.len() >= HEADER_LENGTH {
            let wrapped = rc4(&self.buffer, self.key.as_bytes());
            // DFEC yields the XOR mask before the trailing mask arrives.
            let mask = wrapped[0] ^ FRAME_HEADER[0];
            let frame: Vec<u8> = wrapped.iter().map(|byte| byte ^ mask).collect();
            if frame[..FRAME_HEADER.len()] != FRAME_HEADER {
                return Err(DecodeError::InvalidHeader);
            }
            let paginated = frame[FLAGS_OFFSET] & PAGINATED_FLAG != 0;
            let header_length = if paginated {
                PAGINATED_HEADER_LENGTH
            } else {
                HEADER_LENGTH
            };
            let packet_length =
                header_length + usize::from(frame[BODY_LENGTH_OFFSET]) + TRAILER_LENGTH;
            if wrapped.len() < packet_length {
                return Ok(None);
            }
            let (checked, crc) = wrapped[..packet_length].split_at(packet_length - CRC_LENGTH);
            if checked[checked.len() - 1] != mask || crc16(checked) != crc {
                return Err(DecodeError::InvalidChecksum);
            }
            self.buffer.drain(..packet_length);
            if frame[ACTION_OFFSET] != READ_DEVICE_INFO {
                continue;
            }
            let code = frame[FLAGS_OFFSET] & RESULT_CODE_MASK;
            if code != 0 {
                return Err(DecodeError::Device(code));
            }
            let body = &frame[header_length..packet_length - TRAILER_LENGTH];
            let body = if paginated {
                match self.add_page(&frame, body)? {
                    Some(joined) => joined,
                    None => continue,
                }
            } else {
                body.to_vec()
            };
            return parse_status(&body).map(Some);
        }
        Ok(None)
    }

    /// Records one page; returns the joined body once all pages are in.
    fn add_page(&mut self, frame: &[u8], body: &[u8]) -> Result<Option<Vec<u8>>, DecodeError> {
        let index = u16::from_be_bytes([frame[PAGE_INDEX_OFFSET], frame[PAGE_INDEX_OFFSET + 1]]);
        let count = u16::from_be_bytes([frame[PAGE_COUNT_OFFSET], frame[PAGE_COUNT_OFFSET + 1]]);
        if !(1 <= index && index <= count && count <= MAX_PAGES) {
            return Err(DecodeError::InvalidPagination);
        }
        let pagination = Pagination {
            count,
            action: frame[ACTION_OFFSET],
            message_type: frame[MESSAGE_TYPE_OFFSET],
        };
        if self
            .pagination
            .is_some_and(|expected| expected != pagination)
        {
            return Err(DecodeError::InconsistentPagination);
        }
        if self
            .pages
            .get(&index)
            .is_some_and(|existing| existing != body)
        {
            return Err(DecodeError::ConflictingPage);
        }
        self.pagination = Some(pagination);
        self.pages.insert(index, body.to_vec());
        Ok((self.pages.len() == usize::from(count))
            .then(|| self.pages.values().flatten().copied().collect()))
    }
}

fn parse_status(body: &[u8]) -> Result<RawStatus, DecodeError> {
    if body.is_empty() {
        return Ok(RawStatus::new());
    }
    match serde_json::from_slice(body)? {
        Value::Object(status) => Ok(status),
        _ => Err(DecodeError::NotObject),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU8;

    use serde_json::json;

    use super::DecodeError;
    use super::MAX_RESPONSE_BYTES;
    use super::RawStatus;
    use super::StatusDecoder;
    use super::status_command;
    use crate::cipher::crc16;
    use crate::cipher::rc4;
    use crate::identity::BleKey;
    use crate::test_support::hex;

    const KEY: &[u8] = b"test-key";
    const MASK: u8 = 0x42;

    fn key() -> BleKey {
        BleKey::new(KEY.to_vec()).expect("nonempty key")
    }

    /// Encrypted response packet; `page` is (index, count).
    fn response(body: &[u8], page: Option<(u16, u16)>, code: u8) -> Vec<u8> {
        let flags = code | if page.is_some() { 0x80 } else { 0 };
        let length = u8::try_from(body.len()).expect("short body");
        let header = [0xdf, 0xec, flags, 6, 3, length];
        let pagination: Vec<u8> = page
            .map(|(index, count)| [index.to_be_bytes(), count.to_be_bytes()].concat())
            .unwrap_or_default();
        let wrapped: Vec<u8> = [&header[..], &pagination, body]
            .concat()
            .iter()
            .map(|byte| byte ^ MASK)
            .chain([MASK])
            .collect();
        rc4(&[wrapped.clone(), crc16(&wrapped).to_vec()].concat(), KEY)
    }

    fn status(value: serde_json::Value) -> RawStatus {
        value.as_object().expect("object").clone()
    }

    #[test]
    fn status_command_layout() {
        let packet = status_command(&key(), NonZeroU8::MIN);
        let wrapped = rc4(&packet, KEY);
        assert_eq!(wrapped[..wrapped.len() - 3], hex("deed01070201"));
        assert_eq!(wrapped[wrapped.len() - 3], 1);
        assert_eq!(
            wrapped[wrapped.len() - 2..],
            crc16(&wrapped[..wrapped.len() - 2])
        );
    }

    #[test]
    fn fragmented_and_coalesced_pages() {
        let second = response(b"100}", Some((2, 2)), 0);
        let first = response(br#"{"rb":"#, Some((1, 2)), 0);
        let mut decoder = StatusDecoder::new(key());
        assert!(decoder.feed(&second[..3]).expect("partial").is_none());
        let rest = [&second[3..], &first[..]].concat();
        assert_eq!(
            decoder.feed(&rest).expect("complete"),
            Some(status(json!({"rb": 100})))
        );
    }

    #[test]
    fn skips_other_actions_and_accepts_empty_body() {
        let mut other = response(b"", None, 0);
        let mut wrapped = rc4(&other, KEY);
        wrapped[3] ^= 6 ^ 9;
        wrapped.truncate(wrapped.len() - 2);
        let crc = crc16(&wrapped);
        other = rc4(&[wrapped, crc.to_vec()].concat(), KEY);
        let mut decoder = StatusDecoder::new(key());
        assert!(decoder.feed(&other).expect("skipped").is_none());
        assert_eq!(
            decoder.feed(&response(b"", None, 0)).expect("empty"),
            Some(RawStatus::new())
        );
    }

    #[test]
    fn rejects_corruption_wrong_key_and_device_error() {
        let mut corrupted = response(br#"{"rb":100}"#, None, 0);
        *corrupted.last_mut().expect("nonempty") ^= 1;
        assert!(matches!(
            StatusDecoder::new(key()).feed(&corrupted),
            Err(DecodeError::InvalidChecksum)
        ));
        let wrong = BleKey::new(b"wrong-key".to_vec()).expect("nonempty key");
        assert!(matches!(
            StatusDecoder::new(wrong).feed(&response(br#"{"rb":100}"#, None, 0)),
            Err(DecodeError::InvalidHeader)
        ));
        assert!(matches!(
            StatusDecoder::new(key()).feed(&response(b"", None, 1)),
            Err(DecodeError::Device(1))
        ));
    }

    #[test]
    fn rejects_bad_pages() {
        let mut decoder = StatusDecoder::new(key());
        assert!(
            decoder
                .feed(&response(br#"{"rb":"#, Some((1, 2)), 0))
                .expect("page")
                .is_none()
        );
        assert!(matches!(
            decoder.feed(&response(br#"{"op":"#, Some((1, 2)), 0)),
            Err(DecodeError::ConflictingPage)
        ));
        let mut decoder = StatusDecoder::new(key());
        assert!(
            decoder
                .feed(&response(b"{", Some((1, 2)), 0))
                .expect("page")
                .is_none()
        );
        assert!(matches!(
            decoder.feed(&response(b"}", Some((2, 3)), 0)),
            Err(DecodeError::InconsistentPagination)
        ));
        assert!(matches!(
            StatusDecoder::new(key()).feed(&response(b"{}", Some((3, 2)), 0)),
            Err(DecodeError::InvalidPagination)
        ));
    }

    #[test]
    fn rejects_non_object_and_oversize() {
        assert!(matches!(
            StatusDecoder::new(key()).feed(&response(b"[1]", None, 0)),
            Err(DecodeError::NotObject)
        ));
        let mut decoder = StatusDecoder::new(key());
        assert!(matches!(
            decoder.feed(&vec![0; MAX_RESPONSE_BYTES + 1]),
            Err(DecodeError::TooLarge)
        ));
    }
}
