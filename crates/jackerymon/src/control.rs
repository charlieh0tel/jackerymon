//! Settings the unit accepts over BLE.  See `docs/protocol.md`, Control.
//!
//! Only decoded settings can be expressed; reboot and power off cannot.

use std::fmt;

use serde_json::json;
use thiserror::Error;

use crate::frame::Action;
use crate::frame::MessageType;
use crate::frame::Request;
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

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ControlError {
    #[error("unit rejected {setting} with code {code}")]
    Rejected { setting: Setting, code: u8 },
    #[error("unit acknowledged {0}, but status does not show it")]
    NotApplied(Setting),
    #[error("connection failed while applying {setting}: {error}")]
    Transport { setting: Setting, error: String },
}

impl Setting {
    /// Why this setting needs the user's confirmation, if it does.  Kept
    /// short enough for one line of the TUI menu.
    #[must_use]
    pub fn warning(self) -> Option<&'static str> {
        match self {
            Self::AcOutput(false) | Self::DcOutput(false) => Some("cuts power to connected loads"),
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

    pub(crate) fn request(self) -> Request {
        let (action, key, value) = self.wire();
        Request {
            action: Action(action),
            message_type: PROPERTY_CHANGE,
            body: json!({ key: value }).to_string().into_bytes(),
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

    use super::Setting;
    use crate::cipher::rc4;
    use crate::identity::BleKey;
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
    fn risky_settings_carry_a_warning() {
        assert!(Setting::AcOutput(false).warning().is_some());
        assert!(Setting::DcOutput(false).warning().is_some());
        assert!(Setting::EmergencyCharge(true).warning().is_some());
        assert!(Setting::AcOutput(true).warning().is_none());
        assert!(Setting::EmergencyCharge(false).warning().is_none());
        assert!(Setting::Light(LightMode::Off).warning().is_none());
    }
}
