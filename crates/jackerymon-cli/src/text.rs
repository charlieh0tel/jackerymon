//! How the unit's values read in the TUI and the web view, so both say
//! the same thing.

use jackerymon::status::BatteryProtection;
use jackerymon::status::BatteryState;
use jackerymon::status::ChargeSpeed;
use jackerymon::status::LightMode;
use jackerymon::status::OffTimer;
use jackerymon::status::ScreenMode;
use jackerymon::status::TemperatureAlarm;

const MINUTES_PER_HOUR: u32 = 60;

pub(crate) fn on_off(state: bool) -> String {
    if state { "on" } else { "off" }.to_owned()
}

pub(crate) fn hours_text(hours: f64) -> String {
    format!("{hours:.1} h")
}

pub(crate) fn battery_state_text(state: BatteryState) -> String {
    match state {
        BatteryState::Idle => "idle".to_owned(),
        BatteryState::Charging => "charging".to_owned(),
        BatteryState::Discharging => "discharging".to_owned(),
        BatteryState::Unknown(value) => format!("unknown ({value})"),
    }
}

pub(crate) fn light_text(mode: LightMode) -> String {
    match mode {
        LightMode::Off => "off".to_owned(),
        LightMode::Low => "low".to_owned(),
        LightMode::High => "high".to_owned(),
        LightMode::Sos => "SOS".to_owned(),
        LightMode::Unknown(value) => format!("unknown ({value})"),
    }
}

pub(crate) fn screen_text(mode: ScreenMode) -> String {
    match mode {
        ScreenMode::Off => "off".to_owned(),
        ScreenMode::Brief => "2 min".to_owned(),
        ScreenMode::Steady => "2 h".to_owned(),
        ScreenMode::Unknown(value) => format!("unknown ({value})"),
    }
}

pub(crate) fn temperature_alarm_text(alarm: TemperatureAlarm) -> String {
    match alarm {
        TemperatureAlarm::None => "none".to_owned(),
        TemperatureAlarm::High => "high".to_owned(),
        TemperatureAlarm::Low => "low".to_owned(),
        TemperatureAlarm::Unknown(value) => format!("unknown ({value})"),
    }
}

pub(crate) fn timer_text(timer: OffTimer) -> String {
    match timer {
        OffTimer::Never => "never".to_owned(),
        OffTimer::Minutes(minutes) if minutes % MINUTES_PER_HOUR == 0 => {
            format!("{} h", minutes / MINUTES_PER_HOUR)
        }
        OffTimer::Minutes(minutes) => format!("{minutes} min"),
    }
}

/// The app shows code n as fault F(n-1).
pub(crate) fn fault_text(code: i64) -> String {
    if code == 0 {
        "none".to_owned()
    } else {
        format!("F{}", code - 1)
    }
}

pub(crate) fn charge_speed_text(speed: ChargeSpeed) -> String {
    match speed {
        ChargeSpeed::Fast => "fast".to_owned(),
        ChargeSpeed::Quiet => "quiet".to_owned(),
        ChargeSpeed::Custom => "custom".to_owned(),
        ChargeSpeed::Unknown(value) => format!("unknown ({value})"),
    }
}

pub(crate) fn battery_protection_text(protection: BatteryProtection) -> String {
    match protection {
        BatteryProtection::Full => "full".to_owned(),
        BatteryProtection::Eco => "eco".to_owned(),
        BatteryProtection::Unknown(value) => format!("unknown ({value})"),
    }
}
