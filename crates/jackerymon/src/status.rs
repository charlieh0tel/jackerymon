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

/// `cs`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeSpeed {
    Fast,
    Mute,
    Unknown(i64),
}

impl From<i64> for ChargeSpeed {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::Fast,
            1 => Self::Mute,
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

impl From<i64> for BatteryProtection {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::Full,
            1 => Self::Eco,
            other => Self::Unknown(other),
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
    /// `ec`; 0 means no error.
    pub error_code: Option<i64>,
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
    use super::LightMode;
    use super::Status;
    use super::StatusError;

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
    fn keeps_unknown_enum_values() {
        let status = parse(json!({"bs": 7, "lm": 9})).expect("valid status");
        assert_eq!(status.battery_state, Some(BatteryState::Unknown(7)));
        assert_eq!(status.light_mode, Some(LightMode::Unknown(9)));
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
