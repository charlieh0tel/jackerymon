//! The key file the Python bootstrap PoC writes (`jackery-key.json`).

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use thiserror::Error;

use crate::identity::BleKey;
use crate::identity::IdentityError;
use crate::identity::ModelCode;
use crate::identity::Serial;

/// Why a key file could not be loaded.
#[derive(Debug, Error)]
pub enum KeyFileError {
    #[error("cannot read key file {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid key file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("key file BLE key is not valid base64")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid key file identity: {0}")]
    Identity(#[from] IdentityError),
    #[error("model {0} is not supported; only model {supported} is", supported = ModelCode::EXPLORER_2000)]
    UnsupportedModel(ModelCode),
}

/// Identity of one unit and the key for talking to it.
#[derive(Debug)]
pub struct KeyFile {
    /// BLE address, as `XX:XX:XX:XX:XX:XX`.
    pub address: String,
    pub serial: Serial,
    pub model: ModelCode,
    pub key: BleKey,
}

/// On-disk layout; other fields the PoC saves are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    address: String,
    serial: Serial,
    model: ModelCode,
    bluetooth_key: String,
}

impl KeyFile {
    /// # Errors
    ///
    /// Fails if the file cannot be read or [`KeyFile::parse`] fails.
    pub fn load(path: &Path) -> Result<Self, KeyFileError> {
        let text = fs::read_to_string(path).map_err(|source| KeyFileError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text)
    }

    /// # Errors
    ///
    /// Fails on malformed JSON, an invalid serial or key, or a model other
    /// than the Explorer 2000.
    pub fn parse(text: &str) -> Result<Self, KeyFileError> {
        let stored: Stored = serde_json::from_str(text)?;
        if stored.model != ModelCode::EXPLORER_2000 {
            return Err(KeyFileError::UnsupportedModel(stored.model));
        }
        Ok(Self {
            address: stored.address,
            serial: stored.serial,
            model: stored.model,
            key: BleKey::new(STANDARD.decode(stored.bluetooth_key)?)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::KeyFile;
    use super::KeyFileError;
    use crate::identity::BleKey;
    use crate::identity::IdentityError;

    fn text(model: u16, key: &str) -> String {
        json!({
            "address": "90:E5:B1:21:ED:BA",
            "serial": "123456789",
            "guid": "112233445566",
            "model": model,
            "bind_counter": 3,
            "battery": 100,
            "bluetoothKey": key,
            "modelCode": null,
            "region": null,
        })
        .to_string()
    }

    #[test]
    fn parses_poc_layout() {
        let file = KeyFile::parse(&text(12, "dGVzdC1rZXk=")).expect("valid file");
        assert_eq!(file.address, "90:E5:B1:21:ED:BA");
        assert_eq!(file.serial.as_str(), "123456789");
        assert_eq!(file.key, BleKey::new(b"test-key".to_vec()).expect("key"));
    }

    #[test]
    fn rejects_bad_contents() {
        assert!(matches!(
            KeyFile::parse(&text(13, "dGVzdC1rZXk=")),
            Err(KeyFileError::UnsupportedModel(_))
        ));
        assert!(matches!(
            KeyFile::parse(&text(12, "")),
            Err(KeyFileError::Identity(IdentityError::EmptyKey))
        ));
        assert!(matches!(
            KeyFile::parse(&text(12, "not base64!")),
            Err(KeyFileError::Base64(_))
        ));
        assert!(matches!(KeyFile::parse("[]"), Err(KeyFileError::Json(_))));
    }
}
