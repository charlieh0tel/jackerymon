//! Typed view of the documented status keys.
//!
//! Scales follow `docs/protocol.md`; most are from public reverse
//! engineering and have not been calibrated against this unit.  There is
//! deliberately no battery current or battery power: the unit reports
//! neither, and input minus output is not a measurement of either.

use serde_json::Value;
use thiserror::Error;

use crate::frame::RawStatus;

/// Wire values in tenths of a unit (temperature, voltage, hours).
const TENTHS_PER_UNIT: f64 = 10.0;

#[derive(Debug, Error, PartialEq)]
#[error("status field `{key}` has invalid value {value}")]
pub struct StatusError {
    key: &'static str,
    value: Value,
}

/// `bs`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryState {
    Idle,
    Charging,
    Discharging,
    Unknown(i64),
}

impl From<i64> for BatteryState {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Charging,
            2 => Self::Discharging,
            other => Self::Unknown(other),
        }
    }
}

/// `lm`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightMode {
    Off,
    Low,
    High,
    Sos,
    Unknown(i64),
}

impl LightMode {
    /// Value on the wire; inverse of `From<i64>`.
    pub(crate) fn wire(self) -> i64 {
        match self {
            Self::Off => 0,
            Self::Low => 1,
            Self::High => 2,
            Self::Sos => 3,
            Self::Unknown(value) => value,
        }
    }
}

impl From<i64> for LightMode {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::Off,
            1 => Self::Low,
            2 => Self::High,
            3 => Self::Sos,
            other => Self::Unknown(other),
        }
    }
}

/// `cs`: the app's AC charging mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeSpeed {
    Fast,
    /// "Quiet Charging Mode": under about 30 dB, slower.
    Quiet,
    /// Charge at the power set by `csc`; the app hides this for model 12.
    Custom,
    Unknown(i64),
}

impl ChargeSpeed {
    /// Value on the wire; inverse of `From<i64>`.
    pub(crate) fn wire(self) -> i64 {
        match self {
            Self::Fast => 0,
            Self::Quiet => 1,
            Self::Custom => 2,
            Self::Unknown(value) => value,
        }
    }
}

impl From<i64> for ChargeSpeed {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::Fast,
            1 => Self::Quiet,
            2 => Self::Custom,
            other => Self::Unknown(other),
        }
    }
}

/// `lps`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryProtection {
    Full,
    Eco,
    Unknown(i64),
}

impl BatteryProtection {
    /// Value on the wire; inverse of `From<i64>`.
    pub(crate) fn wire(self) -> i64 {
        match self {
            Self::Full => 0,
            Self::Eco => 1,
            Self::Unknown(value) => value,
        }
    }
}

impl From<i64> for BatteryProtection {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::Full,
            1 => Self::Eco,
            other => Self::Unknown(other),
        }
    }
}

/// `sltb`: the app's screen setting.  0 selects nothing in the app.  The
/// setting is written as `slt` in minutes instead (see
/// [`ScreenMode::minutes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenMode {
    Off,
    /// "Shortly on": two minutes.
    Brief,
    /// "Steady on": two hours.
    Steady,
    Unknown(i64),
}

impl ScreenMode {
    /// Minutes written as `slt`; the unit reports the mode back as an
    /// index in `sltb`.  `Unknown(n)` is written as `n` minutes.
    pub(crate) fn minutes(self) -> i64 {
        match self {
            Self::Off => 0,
            Self::Brief => 2,
            Self::Steady => 120,
            Self::Unknown(value) => value,
        }
    }
}

impl From<i64> for ScreenMode {
    fn from(value: i64) -> Self {
        match value {
            1 => Self::Off,
            2 => Self::Brief,
            3 => Self::Steady,
            other => Self::Unknown(other),
        }
    }
}

/// `ta`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemperatureAlarm {
    None,
    High,
    Low,
    Unknown(i64),
}

impl From<i64> for TemperatureAlarm {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::None,
            1 => Self::High,
            2 => Self::Low,
            other => Self::Unknown(other),
        }
    }
}

/// A setting in minutes where 0 means never (`ast`, `pm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffTimer {
    Never,
    Minutes(u32),
}

impl OffTimer {
    /// Minutes on the wire, 0 for never.
    pub(crate) fn minutes(self) -> u32 {
        match self {
            Self::Never => 0,
            Self::Minutes(minutes) => minutes,
        }
    }
}

impl From<u32> for OffTimer {
    fn from(minutes: u32) -> Self {
        match minutes {
            0 => Self::Never,
            minutes => Self::Minutes(minutes),
        }
    }
}

/// One status response.  A field is `None` when the unit omits its key;
/// some models never send some keys (the Explorer 2000 reports DC output
/// only as `odc`).
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// `rb`
    pub battery_percent: Option<u8>,
    /// `bt`
    pub battery_celsius: Option<f64>,
    pub battery_state: Option<BatteryState>,
    /// `ip`
    pub input_watts: Option<u32>,
    /// `op`
    pub output_watts: Option<u32>,
    /// `it`
    pub input_hours_remaining: Option<f64>,
    /// `ot`
    pub output_hours_remaining: Option<f64>,
    /// `oac`
    pub ac_output: Option<bool>,
    /// `odc`
    pub dc_output: Option<bool>,
    /// `odcu`
    pub usb_output: Option<bool>,
    /// `odcc`
    pub car_output: Option<bool>,
    /// `iac`
    pub ac_input: Option<bool>,
    /// `idc`
    pub dc_input: Option<bool>,
    /// `acov`
    pub ac_output_volts: Option<f64>,
    /// `acohz`
    pub ac_output_hertz: Option<u16>,
    pub light_mode: Option<LightMode>,
    pub charge_speed: Option<ChargeSpeed>,
    /// `ups`
    pub ups: Option<bool>,
    pub battery_protection: Option<BatteryProtection>,
    /// `ec`; 0 means no error, n shows as fault F(n-1) in the app.
    pub error_code: Option<i64>,
    /// `acip`
    pub ac_input_watts: Option<u32>,
    /// `cip`
    pub car_input_watts: Option<u32>,
    /// `acpsp`; the app adds it to `cip` as DC input.
    pub solar_input_watts: Option<f64>,
    /// `ast`: power off after this long under light load on battery.
    pub auto_off: Option<OffTimer>,
    /// `pm`: turn outputs off after this long at no or light load.
    pub energy_saving: Option<OffTimer>,
    pub screen: Option<ScreenMode>,
    /// `sfc`: the app's "Emergency Charging", for emergencies only; the
    /// unit clears it after one charge.
    pub emergency_charge: Option<bool>,
    pub temperature_alarm: Option<TemperatureAlarm>,
    /// Every field as received, including undocumented ones.
    pub raw: RawStatus,
}

impl TryFrom<RawStatus> for Status {
    type Error = StatusError;

    fn try_from(raw: RawStatus) -> Result<Self, Self::Error> {
        Ok(Self {
            battery_percent: integer(&raw, "rb")?,
            battery_celsius: tenths(&raw, "bt")?,
            battery_state: integer::<i64>(&raw, "bs")?.map(BatteryState::from),
            input_watts: integer(&raw, "ip")?,
            output_watts: integer(&raw, "op")?,
            input_hours_remaining: tenths(&raw, "it")?,
            output_hours_remaining: tenths(&raw, "ot")?,
            ac_output: flag(&raw, "oac")?,
            dc_output: flag(&raw, "odc")?,
            usb_output: flag(&raw, "odcu")?,
            car_output: flag(&raw, "odcc")?,
            ac_input: flag(&raw, "iac")?,
            dc_input: flag(&raw, "idc")?,
            ac_output_volts: tenths(&raw, "acov")?,
            ac_output_hertz: integer(&raw, "acohz")?,
            light_mode: integer::<i64>(&raw, "lm")?.map(LightMode::from),
            charge_speed: integer::<i64>(&raw, "cs")?.map(ChargeSpeed::from),
            ups: flag(&raw, "ups")?,
            battery_protection: integer::<i64>(&raw, "lps")?.map(BatteryProtection::from),
            error_code: integer(&raw, "ec")?,
            ac_input_watts: integer(&raw, "acip")?,
            car_input_watts: integer(&raw, "cip")?,
            solar_input_watts: tenths(&raw, "acpsp")?,
            auto_off: integer::<u32>(&raw, "ast")?.map(OffTimer::from),
            energy_saving: integer::<u32>(&raw, "pm")?.map(OffTimer::from),
            screen: integer::<i64>(&raw, "sltb")?.map(ScreenMode::from),
            emergency_charge: flag(&raw, "sfc")?,
            temperature_alarm: integer::<i64>(&raw, "ta")?.map(TemperatureAlarm::from),
            raw,
        })
    }
}

fn integer<T: TryFrom<i64>>(raw: &RawStatus, key: &'static str) -> Result<Option<T>, StatusError> {
    raw.get(key)
        .map(|value| {
            value
                .as_i64()
                .and_then(|number| T::try_from(number).ok())
                .ok_or_else(|| StatusError {
                    key,
                    value: value.clone(),
                })
        })
        .transpose()
}

fn tenths(raw: &RawStatus, key: &'static str) -> Result<Option<f64>, StatusError> {
    Ok(integer::<i32>(raw, key)?.map(|value| f64::from(value) / TENTHS_PER_UNIT))
}

fn flag(raw: &RawStatus, key: &'static str) -> Result<Option<bool>, StatusError> {
    integer::<u8>(raw, key)?
        .map(|value| match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(StatusError {
                key,
                value: raw[key].clone(),
            }),
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::BatteryState;
    use super::ChargeSpeed;
    use super::LightMode;
    use super::OffTimer;
    use super::ScreenMode;
    use super::Status;
    use super::StatusError;
    use super::TemperatureAlarm;

    fn parse(value: serde_json::Value) -> Result<Status, StatusError> {
        Status::try_from(value.as_object().expect("object").clone())
    }

    #[test]
    fn scales_live_explorer_2000_fields() {
        let status = parse(json!({
            "rb": 100, "bt": 331, "ip": 0, "op": 1, "oac": 1, "odc": 1,
            "lm": 1, "acov": 1209, "acohz": 60, "ec": 0, "sltb": 2,
        }))
        .expect("valid status");
        assert_eq!(status.battery_percent, Some(100));
        assert_eq!(status.battery_celsius, Some(33.1));
        assert_eq!(status.output_watts, Some(1));
        assert_eq!(status.ac_output, Some(true));
        assert_eq!(status.dc_output, Some(true));
        assert_eq!(status.usb_output, None);
        assert_eq!(status.light_mode, Some(LightMode::Low));
        assert_eq!(status.ac_output_volts, Some(120.9));
        assert_eq!(status.ac_output_hertz, Some(60));
        assert_eq!(status.error_code, Some(0));
        assert_eq!(status.raw["sltb"], json!(2));
    }

    #[test]
    fn parses_live_poll_response() {
        let status = parse(json!({
            "acip": 0, "acohz": 0, "acov": 0, "acpsp": 0, "acpss": 0, "ast": 120,
            "bs": 0, "bt": 279, "cip": 0, "cs": 0, "ec": 0, "ip": 0, "it": 0,
            "lm": 0, "lps": 0, "oac": 0, "odc": 0, "op": 0, "ot": 999, "pal": 0,
            "pm": 720, "pmb": 0, "rb": 100, "sfc": 0, "sltb": 1, "ta": 0,
        }))
        .expect("valid status");
        assert_eq!(status.battery_state, Some(BatteryState::Idle));
        assert_eq!(status.battery_celsius, Some(27.9));
        assert_eq!(status.output_hours_remaining, Some(99.9));
        assert_eq!(status.light_mode, Some(LightMode::Off));
        assert_eq!(status.auto_off, Some(OffTimer::Minutes(120)));
        assert_eq!(status.energy_saving, Some(OffTimer::Minutes(720)));
        assert_eq!(status.screen, Some(ScreenMode::Off));
        assert_eq!(status.emergency_charge, Some(false));
        assert_eq!(status.temperature_alarm, Some(TemperatureAlarm::None));
        assert_eq!(status.solar_input_watts, Some(0.0));
        assert_eq!(status.ac_input_watts, Some(0));
    }

    #[test]
    fn decodes_settings_and_alarms() {
        let status = parse(json!({"ast": 0, "pm": 1440, "sltb": 3, "ta": 1, "acpsp": 1234}))
            .expect("valid status");
        assert_eq!(status.auto_off, Some(OffTimer::Never));
        assert_eq!(status.energy_saving, Some(OffTimer::Minutes(1440)));
        assert_eq!(status.screen, Some(ScreenMode::Steady));
        assert_eq!(status.temperature_alarm, Some(TemperatureAlarm::High));
        assert_eq!(status.solar_input_watts, Some(123.4));
    }

    #[test]
    fn keeps_unknown_enum_values() {
        let status = parse(json!({"bs": 7, "lm": 9})).expect("valid status");
        assert_eq!(status.battery_state, Some(BatteryState::Unknown(7)));
        assert_eq!(status.light_mode, Some(LightMode::Unknown(9)));
        let status = parse(json!({"cs": 2})).expect("valid status");
        assert_eq!(status.charge_speed, Some(ChargeSpeed::Custom));
    }

    #[test]
    fn rejects_wrong_types_and_ranges() {
        for invalid in [
            json!({"rb": "100"}),
            json!({"rb": 256}),
            json!({"ip": -1}),
            json!({"oac": 2}),
        ] {
            assert!(parse(invalid).is_err());
        }
    }
}
