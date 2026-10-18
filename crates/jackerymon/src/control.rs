//! Settings the unit accepts over BLE.  See `docs/protocol.md`, Control.
//!
//! A [`Setting`] is a decoded control and cannot reboot or power off the
//! unit.  A [`RawCommand`] sends anything, unchecked, for probing.

use std::fmt;

use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use thiserror::Error;

use crate::frame::Action;
use crate::frame::MessageType;
use crate::frame::Request;
use crate::identity::ModelCode;
use crate::status::BatteryProtection;
use crate::status::ChargeSpeed;
use crate::status::LightMode;
use crate::status::OffTimer;
use crate::status::ScreenMode;
use crate::status::Status;

/// Message type of every setting change.
const PROPERTY_CHANGE: MessageType = MessageType(4);
const MINUTES_PER_HOUR: u32 = 60;

/// Auto-off and energy-saving choices the app offers for model 12.
pub const OFF_TIMER_CHOICES: [OffTimer; 5] = [
    OffTimer::Never,
    OffTimer::Minutes(2 * MINUTES_PER_HOUR),
    OffTimer::Minutes(8 * MINUTES_PER_HOUR),
    OffTimer::Minutes(12 * MINUTES_PER_HOUR),
    OffTimer::Minutes(24 * MINUTES_PER_HOUR),
];

/// One setting change.  `Unknown(n)` values are sent as `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    AcOutput(bool),
    DcOutput(bool),
    Light(LightMode),
    Screen(ScreenMode),
    AutoOff(OffTimer),
    EnergySaving(OffTimer),
    ChargeSpeed(ChargeSpeed),
    BatteryProtection(BatteryProtection),
    EmergencyCharge(bool),
    Ups(bool),
}

/// Why a setting was not applied.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ControlError {
    #[error("unit rejected {setting} with code {code}")]
    Rejected { setting: Setting, code: u8 },
    #[error("unit acknowledged {0}, but status does not show it")]
    NotApplied(Setting),
    #[error("connection failed while applying {setting}: {error}")]
    Transport { setting: Setting, error: String },
    /// Never sent: the unit's model has no such control.
    #[error("{setting}: model {model} has no such control")]
    Unavailable { setting: Setting, model: ModelCode },
}

/// A request sent as given, for actions this crate does not decode.
/// Nothing checks what it does: it can reboot or power off the unit, or
/// start a firmware update.  The unit's echo is returned unread, and no
/// status read confirms it.
#[derive(Clone, Debug, PartialEq)]
pub struct RawCommand {
    action: u8,
    message_type: u8,
    /// Sent as JSON; an empty object is sent as no body, as the app does.
    body: Map<String, Value>,
}

/// A raw body too long for the one-byte length of a request frame.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error(
    "raw body is {length} bytes; a request frame carries at most {}",
    u8::MAX
)]
pub struct BodyTooLong {
    pub length: usize,
}

/// Why a raw command has no echo.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum RawError {
    #[error("unit rejected {command} with code {code}")]
    Rejected { command: RawCommand, code: u8 },
    #[error("unit answered {command} with invalid JSON: {error}")]
    InvalidReply { command: RawCommand, error: String },
    /// The link failed, or the reply could not be decoded.
    #[error("sending {command} failed: {error}")]
    Failed { command: RawCommand, error: String },
}

impl RawCommand {
    /// The message type of every [`Setting`].
    pub const DEFAULT_MESSAGE_TYPE: u8 = PROPERTY_CHANGE.0;

    /// Fails if `body` does not fit one request frame.
    pub fn new(
        action: u8,
        message_type: u8,
        body: Map<String, Value>,
    ) -> Result<Self, BodyTooLong> {
        let command = Self {
            action,
            message_type,
            body,
        };
        let length = command.encoded_body().len();
        if u8::try_from(length).is_err() {
            return Err(BodyTooLong { length });
        }
        Ok(command)
    }

    #[must_use]
    pub fn action(&self) -> u8 {
        self.action
    }

    #[must_use]
    pub fn message_type(&self) -> u8 {
        self.message_type
    }

    #[must_use]
    pub fn body(&self) -> &Map<String, Value> {
        &self.body
    }

    pub(crate) fn request(&self) -> Request {
        Request {
            action: Action(self.action),
            message_type: MessageType(self.message_type),
            body: self.encoded_body(),
        }
    }

    fn encoded_body(&self) -> Vec<u8> {
        // `PortableControlFormat.e`: `{}` goes out as no body.
        if self.body.is_empty() {
            return Vec::new();
        }
        Value::Object(self.body.clone()).to_string().into_bytes()
    }
}

impl fmt::Display for RawCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "raw {}/{} {}",
            self.action,
            self.message_type,
            Value::Object(self.body.clone())
        )
    }
}

impl Setting {
    /// Whether `model` has the control this setting changes.
    #[must_use]
    pub fn available_on(self, model: ModelCode) -> bool {
        match self {
            Self::Light(_) => model.has_light(),
            _ => true,
        }
    }

    /// Why this setting needs the user's confirmation, if it does.  Kept
    /// short enough for one line of the TUI menu.
    #[must_use]
    pub fn warning(self) -> Option<&'static str> {
        match self {
            // Jackery: "For emergency situations only ... not recommended
            // for regular use."
            Self::EmergencyCharge(true) => Some("for emergencies only, not regular use"),
            _ => None,
        }
    }

    /// Whether `status` shows this setting in effect.
    #[must_use]
    pub fn applied(self, status: &Status) -> bool {
        match self {
            Self::AcOutput(on) => status.ac_output == Some(on),
            Self::DcOutput(on) => status.dc_output == Some(on),
            Self::Light(mode) => status.light_mode == Some(mode),
            Self::Screen(mode) => status.screen == Some(mode),
            Self::AutoOff(timer) => status.auto_off == Some(timer),
            Self::EnergySaving(timer) => status.energy_saving == Some(timer),
            Self::ChargeSpeed(speed) => status.charge_speed == Some(speed),
            Self::BatteryProtection(protection) => status.battery_protection == Some(protection),
            Self::EmergencyCharge(on) => status.emergency_charge == Some(on),
            Self::Ups(on) => status.ups == Some(on),
        }
    }

    /// Parses a body key and value as [`Display`](fmt::Display) prints
    /// them (`lm=1`), the same one-key form the unit takes.  Only values
    /// the unit documents are accepted: no `Unknown` variants.
    #[must_use]
    pub fn from_wire(key: &str, value: i64) -> Option<Self> {
        let switch = || match value {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        let timer = || {
            let timer = OffTimer::from(u32::try_from(value).ok()?);
            OFF_TIMER_CHOICES.contains(&timer).then_some(timer)
        };
        let setting = match key {
            "oac" => Self::AcOutput(switch()?),
            "odc" => Self::DcOutput(switch()?),
            "lm" => Self::Light(LightMode::from(value)),
            "slt" => Self::Screen(
                [ScreenMode::Off, ScreenMode::Brief, ScreenMode::Steady]
                    .into_iter()
                    .find(|mode| mode.minutes() == value)?,
            ),
            "ast" => Self::AutoOff(timer()?),
            "pm" => Self::EnergySaving(timer()?),
            "cs" => Self::ChargeSpeed(ChargeSpeed::from(value)),
            "lps" => Self::BatteryProtection(BatteryProtection::from(value)),
            "sfc" => Self::EmergencyCharge(switch()?),
            "ups" => Self::Ups(switch()?),
            _ => return None,
        };
        let known = !matches!(
            setting,
            Self::Light(LightMode::Unknown(_))
                | Self::ChargeSpeed(ChargeSpeed::Unknown(_) | ChargeSpeed::Custom)
                | Self::BatteryProtection(BatteryProtection::Unknown(_))
        );
        known.then_some(setting)
    }

    /// The one-key body the unit takes, such as `{"lm":1}`; the inverse of
    /// [`Setting::from_wire`].
    #[must_use]
    pub fn body(self) -> Value {
        let (_, key, value) = self.wire();
        json!({ key: value })
    }

    pub(crate) fn request(self) -> Request {
        let (action, _, _) = self.wire();
        Request {
            action: Action(action),
            message_type: PROPERTY_CHANGE,
            body: self.body().to_string().into_bytes(),
        }
    }

    /// Action id, body key, and value.
    fn wire(self) -> (u8, &'static str, i64) {
        match self {
            Self::DcOutput(on) => (10, "odc", i64::from(on)),
            Self::AcOutput(on) => (13, "oac", i64::from(on)),
            Self::Light(mode) => (17, "lm", mode.wire()),
            Self::Screen(mode) => (18, "slt", mode.minutes()),
            Self::AutoOff(timer) => (19, "ast", i64::from(timer.minutes())),
            Self::EnergySaving(timer) => (20, "pm", i64::from(timer.minutes())),
            Self::ChargeSpeed(speed) => (21, "cs", speed.wire()),
            Self::BatteryProtection(protection) => (22, "lps", protection.wire()),
            Self::EmergencyCharge(on) => (23, "sfc", i64::from(on)),
            Self::Ups(on) => (24, "ups", i64::from(on)),
        }
    }
}

impl fmt::Display for Setting {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (_, key, value) = self.wire();
        write!(formatter, "{key}={value}")
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU8;

    use serde_json::json;

    use super::BodyTooLong;
    use super::RawCommand;
    use super::Setting;
    use crate::cipher::rc4;
    use crate::identity::BleKey;
    use crate::status::BatteryProtection;
    use crate::status::ChargeSpeed;
    use crate::status::LightMode;
    use crate::status::OffTimer;
    use crate::status::ScreenMode;
    use crate::status::Status;
    use crate::test_support::hex;

    fn status(value: serde_json::Value) -> Status {
        Status::try_from(value.as_object().expect("object").clone()).expect("valid status")
    }

    #[test]
    fn light_request_matches_live_probe() {
        let key = BleKey::new(b"test-key".to_vec()).expect("key");
        let request = Setting::Light(LightMode::Low).request();
        let wrapped = rc4(&request.encode(&key, NonZeroU8::MIN), b"test-key");
        let unmasked: Vec<u8> = wrapped[..wrapped.len() - 3]
            .iter()
            .map(|byte| byte ^ 1)
            .collect();
        assert_eq!(unmasked[..6], hex("dfec00110408"));
        assert_eq!(&unmasked[6..], br#"{"lm":1}"#);
    }

    #[test]
    fn screen_is_written_in_minutes_and_read_as_index() {
        let setting = Setting::Screen(ScreenMode::Steady);
        assert_eq!(setting.request().body, br#"{"slt":120}"#);
        assert!(setting.applied(&status(json!({"sltb": 3}))));
        assert!(!setting.applied(&status(json!({"sltb": 2}))));
    }

    #[test]
    fn applied_requires_the_field() {
        let setting = Setting::AutoOff(OffTimer::Never);
        assert_eq!(setting.to_string(), "ast=0");
        assert!(setting.applied(&status(json!({"ast": 0}))));
        assert!(!setting.applied(&status(json!({}))));
    }

    #[test]
    fn wire_form_round_trips() {
        for setting in [
            Setting::AcOutput(true),
            Setting::DcOutput(false),
            Setting::Light(LightMode::Sos),
            Setting::Screen(ScreenMode::Steady),
            Setting::AutoOff(OffTimer::Never),
            Setting::EnergySaving(OffTimer::Minutes(720)),
            Setting::ChargeSpeed(ChargeSpeed::Quiet),
            Setting::BatteryProtection(BatteryProtection::Eco),
            Setting::EmergencyCharge(true),
            Setting::Ups(false),
        ] {
            let text = setting.to_string();
            let (key, value) = text.split_once('=').expect("key=value");
            let value = value.parse().expect("integer");
            assert_eq!(Setting::from_wire(key, value), Some(setting), "{text}");
        }
    }

    #[test]
    fn wire_form_rejects_undocumented_values() {
        for (key, value) in [
            ("oac", 2),
            ("lm", 9),
            ("slt", 5),
            ("ast", 90),
            ("cs", 2),
            ("xyz", 1),
        ] {
            assert_eq!(Setting::from_wire(key, value), None, "{key}={value}");
        }
    }

    #[test]
    fn raw_command_is_framed_as_given() {
        let body = json!({"dl": 10, "cl": 60})
            .as_object()
            .expect("object")
            .clone();
        let command = RawCommand::new(33, 22, body).expect("fits");
        let request = command.request();
        assert_eq!((request.action.0, request.message_type.0), (33, 22));
        assert_eq!(request.body, br#"{"cl":60,"dl":10}"#);
        assert_eq!(command.to_string(), r#"raw 33/22 {"cl":60,"dl":10}"#);
    }

    #[test]
    fn raw_empty_body_is_sent_as_none() {
        let command = RawCommand::new(2, 100, serde_json::Map::new()).expect("fits");
        assert!(command.request().body.is_empty());
        assert_eq!(command.to_string(), "raw 2/100 {}");
    }

    #[test]
    fn raw_body_must_fit_one_frame() {
        let long = json!({"k": "x".repeat(250)})
            .as_object()
            .expect("object")
            .clone();
        assert_eq!(
            RawCommand::new(17, 4, long),
            Err(BodyTooLong { length: 258 })
        );
    }

    #[test]
    fn risky_settings_carry_a_warning() {
        assert!(Setting::AcOutput(false).warning().is_none());
        assert!(Setting::DcOutput(false).warning().is_none());
        assert!(Setting::EmergencyCharge(true).warning().is_some());
        assert!(Setting::AcOutput(true).warning().is_none());
        assert!(Setting::EmergencyCharge(false).warning().is_none());
        assert!(Setting::Light(LightMode::Off).warning().is_none());
    }
}
