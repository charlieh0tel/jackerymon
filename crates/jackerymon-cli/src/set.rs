//! `jackerymon set`: change one setting and confirm it took effect.

use std::num::NonZeroU32;
use std::ops::ControlFlow;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Args;
use clap::Subcommand;
use clap::ValueEnum;
use jackerymon::ble;
use jackerymon::control::Setting;
use jackerymon::poll::ControlRequest;
use jackerymon::status::BatteryProtection;
use jackerymon::status::ChargeSpeed;
use jackerymon::status::LightMode;
use jackerymon::status::OffTimer;
use jackerymon::status::ScreenMode;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::connection::ConnectionArgs;

/// Consecutive transport failures before giving up, as `poll` does.
const MAX_FAILURES: NonZeroU32 = NonZeroU32::new(3).expect("nonzero");
const MINUTES_PER_HOUR: u32 = 60;

#[derive(Args)]
pub(crate) struct SetArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    /// Allow turning an output off, which cuts power to its loads.
    #[arg(long)]
    yes: bool,
    #[command(subcommand)]
    setting: SettingArgs,
}

#[derive(Subcommand)]
enum SettingArgs {
    /// AC output.
    Ac {
        state: Switch,
    },
    /// DC output.
    Dc {
        state: Switch,
    },
    Light {
        mode: Light,
    },
    /// How long the screen stays on.
    Screen {
        mode: Screen,
    },
    /// Power off after this long under light load on battery.
    AutoOff {
        after: Timer,
    },
    /// Turn outputs off after this long at no or light load.
    EnergySaving {
        after: Timer,
    },
    ChargeSpeed {
        speed: Speed,
    },
    BatteryProtection {
        mode: Protection,
    },
    /// One-shot fast charge; the unit clears it after use.
    SuperCharge {
        state: Switch,
    },
    Ups {
        state: Switch,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum Light {
    Off,
    Low,
    High,
    Sos,
}

#[derive(Clone, Copy, ValueEnum)]
enum Screen {
    Off,
    #[value(name = "2min")]
    TwoMinutes,
    #[value(name = "2h")]
    TwoHours,
}

/// The app's choices for model 12.
#[derive(Clone, Copy, ValueEnum)]
enum Timer {
    Never,
    #[value(name = "2h")]
    TwoHours,
    #[value(name = "8h")]
    EightHours,
    #[value(name = "12h")]
    TwelveHours,
    #[value(name = "24h")]
    TwentyFourHours,
}

#[derive(Clone, Copy, ValueEnum)]
enum Speed {
    Fast,
    Mute,
}

#[derive(Clone, Copy, ValueEnum)]
enum Protection {
    Full,
    Eco,
}

impl Switch {
    fn on(self) -> bool {
        matches!(self, Self::On)
    }
}

impl Timer {
    fn off_timer(self) -> OffTimer {
        let hours = match self {
            Self::Never => return OffTimer::Never,
            Self::TwoHours => 2,
            Self::EightHours => 8,
            Self::TwelveHours => 12,
            Self::TwentyFourHours => 24,
        };
        OffTimer::Minutes(hours * MINUTES_PER_HOUR)
    }
}

impl From<SettingArgs> for Setting {
    fn from(arguments: SettingArgs) -> Self {
        match arguments {
            SettingArgs::Ac { state } => Self::AcOutput(state.on()),
            SettingArgs::Dc { state } => Self::DcOutput(state.on()),
            SettingArgs::Light { mode } => Self::Light(match mode {
                Light::Off => LightMode::Off,
                Light::Low => LightMode::Low,
                Light::High => LightMode::High,
                Light::Sos => LightMode::Sos,
            }),
            SettingArgs::Screen { mode } => Self::Screen(match mode {
                Screen::Off => ScreenMode::Off,
                Screen::TwoMinutes => ScreenMode::Brief,
                Screen::TwoHours => ScreenMode::Steady,
            }),
            SettingArgs::AutoOff { after } => Self::AutoOff(after.off_timer()),
            SettingArgs::EnergySaving { after } => Self::EnergySaving(after.off_timer()),
            SettingArgs::ChargeSpeed { speed } => Self::ChargeSpeed(match speed {
                Speed::Fast => ChargeSpeed::Fast,
                Speed::Mute => ChargeSpeed::Mute,
            }),
            SettingArgs::BatteryProtection { mode } => Self::BatteryProtection(match mode {
                Protection::Full => BatteryProtection::Full,
                Protection::Eco => BatteryProtection::Eco,
            }),
            SettingArgs::SuperCharge { state } => Self::SuperCharge(state.on()),
            SettingArgs::Ups { state } => Self::Ups(state.on()),
        }
    }
}

pub(crate) async fn run(arguments: SetArgs) -> Result<()> {
    let setting = Setting::from(arguments.setting);
    if setting.cuts_power() && !arguments.yes {
        bail!("{setting} cuts power to connected loads; pass --yes to confirm");
    }
    let key_file = arguments.connection.key_file()?;
    let config = arguments.connection.poll_config(Some(MAX_FAILURES));
    let adapter = ble::default_adapter().await?;
    let (sender, commands) = mpsc::channel(1);
    let (reply, answer) = oneshot::channel();
    sender
        .send(ControlRequest { setting, reply })
        .await
        .context("queuing the setting")?;
    let mut answered = None;
    let stop = async {
        // A dropped reply means polling ended first; its error says why.
        answered = answer.await.ok();
    };
    let polled = ble::poll(adapter, key_file, config, Some(commands), stop, |_| {
        ControlFlow::Continue(())
    })
    .await;
    match answered {
        Some(result) => {
            result?;
            println!("ok: {setting}");
            Ok(())
        }
        None => {
            polled.context("polling failed")?;
            bail!("polling ended before {setting} was applied")
        }
    }
}
