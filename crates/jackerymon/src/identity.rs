//! Identifiers that tie an advertisement, a saved key, and a unit together.

use std::fmt;

use serde::Deserialize;
use thiserror::Error;

const MIN_SERIAL_LENGTH: usize = 8;
const GUID_LENGTH: usize = 6;

/// Why a serial or key is invalid.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("serial must be at least {MIN_SERIAL_LENGTH} decimal digits")]
    InvalidSerial,
    #[error("BLE key is empty")]
    EmptyKey,
}

/// Decimal serial number the unit advertises.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct Serial(String);

impl Serial {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Serial {
    type Error = IdentityError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if text.len() >= MIN_SERIAL_LENGTH && text.bytes().all(|byte| byte.is_ascii_digit()) {
            Ok(Self(text))
        } else {
            Err(IdentityError::InvalidSerial)
        }
    }
}

impl fmt::Display for Serial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Six-byte unit identifier from the advertisement; the cloud API takes it
/// as uppercase hex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Guid(pub [u8; GUID_LENGTH]);

impl fmt::Display for Guid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02X}"))
    }
}

/// Product model code from the advertisement; selects the codec variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct ModelCode(pub u16);

impl ModelCode {
    /// Explorer 2000 v2 ("E2000Pro2" in the app); the codec was validated
    /// on it (RC4, no IV).
    pub const EXPLORER_2000: Self = Self(12);
    /// Explorer 3000 ("E3000"); same codec, answers the same requests.
    pub const EXPLORER_3000: Self = Self(14);
    /// Models that have answered.  The HomePower 3000 (19, "HP3000")
    /// takes the same codec in the app but has not answered.
    pub const SUPPORTED: [Self; 2] = [Self::EXPLORER_2000, Self::EXPLORER_3000];

    /// [`Self::SUPPORTED`], for messages.
    #[must_use]
    pub fn supported_list() -> String {
        Self::SUPPORTED
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for ModelCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Per-device GATT RC4 key from Jackery's cloud.  `Debug` redacts it.
#[derive(Clone, PartialEq, Eq)]
pub struct BleKey(Vec<u8>);

impl BleKey {
    /// # Errors
    ///
    /// Returns [`IdentityError::EmptyKey`] for an empty key, which RC4
    /// cannot use.
    pub fn new(bytes: Vec<u8>) -> Result<Self, IdentityError> {
        if bytes.is_empty() {
            Err(IdentityError::EmptyKey)
        } else {
            Ok(Self(bytes))
        }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for BleKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BleKey(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::BleKey;
    use super::Guid;
    use super::IdentityError;
    use super::Serial;

    #[test]
    fn serial_requires_long_decimal_text() {
        assert!(Serial::try_from("12345678".to_owned()).is_ok());
        assert_eq!(
            Serial::try_from("1234567".to_owned()),
            Err(IdentityError::InvalidSerial)
        );
        assert_eq!(
            Serial::try_from("1234567a".to_owned()),
            Err(IdentityError::InvalidSerial)
        );
    }

    #[test]
    fn guid_displays_uppercase_hex() {
        assert_eq!(
            Guid([0x11, 0x22, 0x33, 0xaa, 0xbb, 0xcc]).to_string(),
            "112233AABBCC"
        );
    }

    #[test]
    fn key_is_nonempty_and_redacted() {
        assert_eq!(BleKey::new(Vec::new()), Err(IdentityError::EmptyKey));
        let key = BleKey::new(b"secret".to_vec()).expect("nonempty key");
        assert!(!format!("{key:?}").contains("secret"));
    }
}
