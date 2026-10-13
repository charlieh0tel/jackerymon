//! `jackerymon key`: fetch a unit's BLE key from Jackery's cloud, the one
//! step that touches it.  Everything else uses only BLE.
//!
//! The account password and the session token are never printed or
//! saved; the key is written only to the key file.

use std::fs;
use std::fs::DirBuilder;
use std::fs::OpenOptions;
use std::io;
use std::io::BufRead;
use std::io::IsTerminal;
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use aes::Aes192;
use aes::cipher::BlockModeEncrypt;
use aes::cipher::KeyInit;
use aes::cipher::block_padding::Pkcs7;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Local;
use chrono::Offset;
use clap::Args;
use reqwest::Method;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use reqwest::redirect::Policy;
use rsa::Pkcs1v15Encrypt;
use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::rand_core::OsRng;
use serde::Serialize;
use serde_json::Value;

use crate::connection::KeyHome;
use crate::connection::key_path;
use crate::connection::seconds;
use crate::record::UnitRecord;
use crate::scan;
use crate::scan::DEFAULT_LISTEN;

const API: &str = "https://iot.jackeryapp.com/v1/";
/// The app's RSA key for wrapping the login's AES key (SPKI DER, base64).
const PUBLIC_KEY: &str = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQCVmzgJy/4XolxPnkfu32YtJqYGFLYqf9/rnVgURJED+8J9J3Pccd6+9L97/+7COZE5OkejsgOkqeLNC9C3r5mhpE4zk/HStss7Q8/5DqkGD1annQ+eoICo3oi0dITZ0Qll56Dowb8lXi6WHViVDdih/oeUwVJY89uJNtTWrz7t7QIDAQAB";
/// Headers the Android app 2.4.2 sends; names lowercase, as HTTP/2 wants.
const APP_HEADERS: [(&str, &str); 7] = [
    ("accept-language", "en"),
    ("platform", "2"),
    ("app_version", "2.4.2"),
    ("app_version_code", "101"),
    ("sys_version", "Android 14,level 34/[arm64-v8a]"),
    ("model", "jackerymon"),
    ("network", "wifi"),
];
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Random bytes behind the login's AES key; base64 makes them 24 ASCII
/// bytes, which the app uses as the key itself (AES-192).
const AES_SEED_LENGTH: usize = 16;
/// `loginType` for email and password.
const PASSWORD_LOGIN: u8 = 2;
const APP_ID: &str = "com.hbxn.jackery";
const SECONDS_PER_MINUTE: i32 = 60;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
/// Extension of the key file while it is written, before the process id.
const TEMPORARY_EXTENSION: &str = "partial-";

#[derive(Args)]
pub(crate) struct KeyArgs {
    /// BLE address of the unit; needed when several are in range.
    #[arg(long)]
    address: Option<String>,
    /// Seconds to listen for the unit.
    #[arg(long, default_value = DEFAULT_LISTEN, value_parser = seconds)]
    timeout: Duration,
    /// Jackery account email; prompted if absent.
    #[arg(long)]
    account: Option<String>,
    /// Account `regionCode`; the app's default is empty.
    #[arg(long, default_value = "")]
    region: String,
    /// Bind the unit to the account and take the key from the bind
    /// response.  Changes which account owns the unit.
    #[arg(long)]
    bind: bool,
    /// Where to write the key; default `~/.config/jackerymon/<serial>.json`.
    /// An existing file is never overwritten.
    #[arg(long)]
    output: Option<PathBuf>,
}

pub(crate) async fn run(arguments: KeyArgs) -> Result<()> {
    // The password prompt needs a terminal; find out before scanning.
    if !io::stdin().is_terminal() {
        bail!("`jackerymon key` prompts for a password; run it in a terminal");
    }
    let sightings = scan::find(arguments.timeout, arguments.address.as_deref()).await?;
    let [sighting] = sightings.as_slice() else {
        let addresses: Vec<_> = sightings
            .iter()
            .map(|sighting| sighting.address.as_str())
            .collect();
        bail!(
            "several units in range ({}); choose one with --address",
            addresses.join(", ")
        );
    };
    let unit = UnitRecord::from(sighting);
    let output = match arguments.output {
        Some(output) => output,
        None => {
            let directory = KeyHome::User.directory()?;
            DirBuilder::new()
                .recursive(true)
                .mode(PRIVATE_DIRECTORY_MODE)
                .create(&directory)
                .with_context(|| format!("creating {}", directory.display()))?;
            key_path(&directory, &unit.serial)
        }
    };
    // Checked before logging in; creating the file checks again.
    if output.symlink_metadata().is_ok() {
        bail!("{} exists; choose another --output", output.display());
    }
    eprintln!(
        "unit {} ({}), model {}",
        unit.serial, unit.address, unit.model
    );
    let account = match arguments.account {
        Some(account) => account,
        None => prompt("Jackery email: ")?,
    };
    if account.is_empty() {
        bail!("no Jackery email given");
    }
    let password = rpassword::prompt_password("Jackery password: ")
        .context("reading the password from the terminal")?;
    let cloud = Cloud::login(&account, &password, &arguments.region).await?;
    drop(password);
    let fetched = if arguments.bind {
        cloud.bind(&unit).await?
    } else {
        cloud.key(&unit).await?
    };
    save(&output, &KeyRecord { unit, fetched })?;
    eprintln!(
        "key saved to {}; check it with `jackerymon poll --count 1`",
        output.display()
    );
    Ok(())
}

fn prompt(question: &str) -> Result<String> {
    eprint!("{question}");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// The key file: the scan line plus what the cloud returned, as the Python
/// PoC writes it.
#[derive(Serialize)]
struct KeyRecord {
    #[serde(flatten)]
    unit: UnitRecord,
    #[serde(flatten)]
    fetched: Fetched,
}

/// The cloud's answer, as saved.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fetched {
    bluetooth_key: String,
    model_code: Value,
    region: Value,
}

/// Creates `path` with owner-only permissions; never replaces a file or
/// follows a symlink.  The key is written to a temporary file and linked
/// into place complete, so a failure leaves no partial key behind.
fn save(path: &Path, record: &KeyRecord) -> Result<()> {
    let temporary = path.with_extension(format!("{TEMPORARY_EXTENSION}{}", std::process::id()));
    let written = write_new(&temporary, record).and_then(|()| {
        // Linking fails if `path` exists, unlike renaming.
        fs::hard_link(&temporary, path).with_context(|| format!("creating {}", path.display()))
    });
    fs::remove_file(&temporary).ok();
    written
}

fn write_new(path: &Path, record: &KeyRecord) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    let text = serde_json::to_string_pretty(record)?;
    writeln!(file, "{text}").with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flushing {}", path.display()))
}

/// A logged-in API session.  The token lives only in its headers.
struct Cloud {
    client: reqwest::Client,
}

/// The login request body, before encryption.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Login<'a> {
    account: &'a str,
    password: &'a str,
    region_code: &'a str,
    login_type: u8,
    register_app_id: &'a str,
    mac_id: &'a str,
}

impl Cloud {
    async fn login(account: &str, password: &str, region: &str) -> Result<Self> {
        let client_id = uuid::Uuid::new_v4().simple().to_string();
        let login = Login {
            account,
            password,
            region_code: region,
            login_type: PASSWORD_LOGIN,
            register_app_id: APP_ID,
            mac_id: &client_id,
        };
        let fields = login_fields(&serde_json::to_vec(&login)?)?;
        let anonymous = client(HeaderMap::new())?;
        let (_, token) = request(&anonymous, Method::POST, "auth/login", &fields).await?;
        let token = token.context("login succeeded without a token")?;
        let mut headers = HeaderMap::new();
        let mut token =
            HeaderValue::from_str(&token).context("login token is not a header value")?;
        token.set_sensitive(true);
        headers.insert("token", token);
        Ok(Self {
            client: client(headers)?,
        })
    }

    /// Looks the key up without changing the account.
    async fn key(&self, unit: &UnitRecord) -> Result<Fetched> {
        let fields = [
            ("deviceSn", unit.serial.clone()),
            ("guid", unit.guid.clone()),
        ];
        let (data, _) = request(&self.client, Method::GET, "device/bluetoothKey", &fields).await?;
        fetched(data)
    }

    /// Binds the unit to the account; the response carries the key.
    async fn bind(&self, unit: &UnitRecord) -> Result<Fetched> {
        let offset_minutes = Local::now().offset().fix().local_minus_utc() / SECONDS_PER_MINUTE;
        let fields = [
            ("devId", unit.serial.clone()),
            ("guid", unit.guid.clone()),
            ("bindKey", unit.bind_counter.to_string()),
            ("timezoneOffset", offset_minutes.to_string()),
        ];
        let (data, _) = request(&self.client, Method::POST, "device/bind", &fields).await?;
        fetched(data)
    }
}

fn client(extra: HeaderMap) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    for (name, value) in APP_HEADERS {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    headers.extend(extra);
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(REQUEST_TIMEOUT)
        .redirect(Policy::none())
        .build()
        .context("building the HTTP client")
}

/// Sends one API request; returns the envelope's `data` and `token`.
/// Errors name the endpoint and code only, never the request.
async fn request(
    client: &reqwest::Client,
    method: Method,
    path: &str,
    fields: &[(&str, String)],
) -> Result<(Value, Option<String>)> {
    let url = format!("{API}{path}");
    let builder = if method == Method::GET {
        client.get(url).query(fields)
    } else {
        client.request(method, url).form(fields)
    };
    let response = builder
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("{path}: network request failed"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("{path}: HTTP {status}");
    }
    let envelope: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("{path}: invalid JSON response"))?;
    open_envelope(path, envelope)
}

/// The `data` and `token` of a successful API envelope.  A failure names
/// the code only, since the rest may echo account details.
fn open_envelope(path: &str, mut envelope: Value) -> Result<(Value, Option<String>)> {
    let code = &envelope["code"];
    if !code.is_number() {
        bail!("{path}: response has no API code");
    }
    if code != 0 {
        bail!("{path}: API error code {code}");
    }
    let token = envelope["token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    Ok((envelope["data"].take(), token))
}

fn fetched(data: Value) -> Result<Fetched> {
    let key = data["bluetoothKey"]
        .as_str()
        .filter(|key| !key.is_empty())
        .context("response contains no BLE key; nothing saved")?;
    Ok(Fetched {
        bluetooth_key: key.to_owned(),
        model_code: data["modelCode"].clone(),
        region: data["region"].clone(),
    })
}

/// Encrypts the login body as the app does: AES-192-ECB with a random key,
/// that key wrapped with the app's RSA key.
fn login_fields(body: &[u8]) -> Result<[(&'static str, String); 2]> {
    let seed: [u8; AES_SEED_LENGTH] = rand::random();
    // The base64 text is the key; decoding it first would be wrong.
    let aes_key = STANDARD.encode(seed);
    let encrypted = encrypt_body(&aes_key, body)?;
    let public = RsaPublicKey::from_public_key_der(&STANDARD.decode(PUBLIC_KEY)?)
        .context("decoding the app's RSA key")?;
    let wrapped = public
        .encrypt(&mut OsRng, Pkcs1v15Encrypt, aes_key.as_bytes())
        .context("wrapping the AES key")?;
    Ok([
        ("aesEncryptData", STANDARD.encode(encrypted)),
        ("rsaForAesKey", STANDARD.encode(wrapped)),
    ])
}

/// AES-ECB with PKCS#7 padding under `key`'s ASCII bytes.
fn encrypt_body(key: &str, body: &[u8]) -> Result<Vec<u8>> {
    Ok(ecb::Encryptor::<Aes192>::new_from_slice(key.as_bytes())
        .context("AES key length")?
        .encrypt_padded_vec::<Pkcs7>(body))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use jackerymon::identity::ModelCode;
    use jackerymon::key_file::KeyFile;
    use serde_json::json;

    use super::Fetched;
    use super::KeyRecord;
    use super::encrypt_body;
    use super::login_fields;
    use super::open_envelope;
    use super::save;
    use crate::record::UnitRecord;

    /// Ciphertext from Python's `cryptography`, as the PoC encrypts.
    #[test]
    fn body_encryption_matches_the_poc() {
        let encrypted = encrypt_body(
            "AAECAwQFBgcICQoLDA0ODw==",
            br#"{"account":"a@example.com"}"#,
        )
        .expect("encrypt");
        let expected = "eb6f9431a327277efb21c5b69d62865662a889211af375c46e762b9f5e2aa3a6";
        let hex: String = encrypted.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, expected);
    }

    #[test]
    fn login_fields_wrap_a_key_for_the_app() {
        let [(data_name, data), (key_name, wrapped)] = login_fields(b"{}").expect("fields");
        assert_eq!((data_name, key_name), ("aesEncryptData", "rsaForAesKey"));
        /// `{}` pads to one AES block.
        const AES_BLOCK_LENGTH: usize = 16;
        /// The app's 1024-bit RSA key wraps anything to this many bytes.
        const RSA_CIPHERTEXT_LENGTH: usize = 128;
        assert_eq!(
            STANDARD.decode(data).expect("base64").len(),
            AES_BLOCK_LENGTH
        );
        assert_eq!(
            STANDARD.decode(wrapped).expect("base64").len(),
            RSA_CIPHERTEXT_LENGTH
        );
    }

    #[test]
    fn saved_record_loads_as_a_key_file() {
        let record = KeyRecord {
            unit: UnitRecord {
                address: "90:E5:B1:21:ED:BA".to_owned(),
                serial: "123456789012345".to_owned(),
                guid: "393931316135".to_owned(),
                model: ModelCode::EXPLORER_2000.0,
                bind_counter: 3,
                battery: Some(100),
            },
            fetched: Fetched {
                bluetooth_key: STANDARD.encode(b"0123456789abcdef"),
                model_code: json!(12),
                region: json!("US"),
            },
        };
        let text = serde_json::to_string_pretty(&record).expect("json");
        let key_file = KeyFile::parse(&text).expect("parse");
        assert_eq!(key_file.serial.as_str(), "123456789012345");
        assert!(text.contains("\"bind_counter\": 3"));
    }

    #[test]
    fn api_errors_name_only_the_code() {
        let failure = json!({"code": 10001, "msg": "bad password for a@example.com", "data": null});
        let error = open_envelope("auth/login", failure)
            .expect_err("failure")
            .to_string();
        assert_eq!(error, "auth/login: API error code 10001");
        let success = json!({"code": 0, "token": "t", "data": {"bluetoothKey": "k"}});
        let (data, token) = open_envelope("auth/login", success).expect("success");
        assert_eq!(
            (data["bluetoothKey"].as_str(), token.as_deref()),
            (Some("k"), Some("t"))
        );
        assert!(open_envelope("auth/login", json!({"data": 1})).is_err());
    }

    #[test]
    fn key_file_is_private_and_never_overwritten() {
        let path = std::env::temp_dir().join(format!("jackerymon-key-{}.json", std::process::id()));
        let record = || KeyRecord {
            unit: UnitRecord {
                address: "90:E5:B1:21:ED:BA".to_owned(),
                serial: "123456789012345".to_owned(),
                guid: "393931316135".to_owned(),
                model: ModelCode::EXPLORER_2000.0,
                bind_counter: 3,
                battery: None,
            },
            fetched: Fetched {
                bluetooth_key: "a2V5".to_owned(),
                model_code: json!(12),
                region: json!(null),
            },
        };
        save(&path, &record()).expect("first save");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        let again = save(&path, &record());
        fs::remove_file(&path).expect("cleanup");
        assert_eq!(mode & 0o777, 0o600);
        assert!(again.is_err(), "an existing key file is never replaced");
        let leftovers = fs::read_dir(std::env::temp_dir())
            .expect("temp dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("jackerymon-key-{}.", std::process::id()))
            })
            .count();
        assert_eq!(leftovers, 0, "no temporary key file is left behind");
    }
}
