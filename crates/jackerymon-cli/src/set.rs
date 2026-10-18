//! `jackerymon set`: change one setting and confirm it took effect.

use std::fmt;
use std::num::NonZeroU32;
use std::ops::ControlFlow;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Args;
use clap::Subcommand;
use clap::ValueEnum;
use jackerymon::ble;
use jackerymon::control::OFF_TIMER_CHOICES;
use jackerymon::control::Setting;
use jackerymon::poll::ControlRequest;
use jackerymon::poll::SettingRequest;
use jackerymon::status::BatteryProtection;
use jackerymon::status::ChargeSpeed;
use jackerymon::status::LightMode;
use jackerymon::status::OffTimer;
use jackerymon::status::ScreenMode;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::connection::ConnectionArgs;
use crate::connection::KeyHome;
use crate::daemon_client::DaemonClient;
use crate::daemon_client::RouteArgs;

/// Consecutive transport failures before giving up, as `poll` does.
const MAX_FAILURES: NonZeroU32 = NonZeroU32::new(3).expect("nonzero");
const MINUTES_PER_HOUR: u32 = 60;

/// Options of `jackerymon set`.
#[derive(Args)]
pub(crate) struct SetArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    #[command(flatten)]
    route: RouteArgs,
    /// Confirm turning emergency charging on, which Jackery reserves for
    /// emergencies.
    // Global so it may follow the setting: `set emergency-charge on --yes`.
    #[arg(long, global = true)]
    yes: bool,
    #[command(subcommand)]
    setting: SettingArgs,
}

/// The setting to change, one subcommand each.
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
        /// never, 2h, 8h, 12h, or 24h.
        #[arg(value_parser = parse_timer)]
        after: OffTimer,
    },
    /// Turn outputs off after this long at no or light load.
    EnergySaving {
        /// never, 2h, 8h, 12h, or 24h.
        #[arg(value_parser = parse_timer)]
        after: OffTimer,
    },
    ChargeSpeed {
        speed: Speed,
    },
    BatteryProtection {
        mode: Protection,
    },
    /// One-shot fast charge; the unit clears it after use.
    EmergencyCharge {
        state: Switch,
    },
    Ups {
        state: Switch,
    },
}

/// On or off, as typed.
#[derive(Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

/// Light modes, as typed.
#[derive(Clone, Copy, ValueEnum)]
enum Light {
    Off,
    Low,
    High,
    Sos,
}

/// Screen timeouts, as typed.
#[derive(Clone, Copy, ValueEnum)]
enum Screen {
    Off,
    #[value(name = "2min")]
    TwoMinutes,
    #[value(name = "2h")]
    TwoHours,
}

/// Charge speeds that can be set, as typed.
#[derive(Clone, Copy, ValueEnum)]
enum Speed {
    Fast,
    Quiet,
}

/// Battery protection modes, as typed.
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

/// One of [`OFF_TIMER_CHOICES`], written `never` or as hours (`2h`).
fn parse_timer(text: &str) -> Result<OffTimer, String> {
    OFF_TIMER_CHOICES
        .into_iter()
        .find(|timer| timer_name(*timer) == text)
        .ok_or_else(|| {
            let names: Vec<String> = OFF_TIMER_CHOICES.into_iter().map(timer_name).collect();
            format!("expected one of {}", names.join(", "))
        })
}

fn timer_name(timer: OffTimer) -> String {
    match timer {
        OffTimer::Never => "never".to_owned(),
        OffTimer::Minutes(minutes) => format!("{}h", minutes / MINUTES_PER_HOUR),
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
            SettingArgs::AutoOff { after } => Self::AutoOff(after),
            SettingArgs::EnergySaving { after } => Self::EnergySaving(after),
            SettingArgs::ChargeSpeed { speed } => Self::ChargeSpeed(match speed {
                Speed::Fast => ChargeSpeed::Fast,
                Speed::Quiet => ChargeSpeed::Quiet,
            }),
            SettingArgs::BatteryProtection { mode } => Self::BatteryProtection(match mode {
                Protection::Full => BatteryProtection::Full,
                Protection::Eco => BatteryProtection::Eco,
            }),
            SettingArgs::EmergencyCharge { state } => Self::EmergencyCharge(state.on()),
            SettingArgs::Ups { state } => Self::Ups(state.on()),
        }
    }
}

pub(crate) async fn run(arguments: SetArgs) -> Result<()> {
    let setting = Setting::from(arguments.setting);
    if let Some(warning) = setting.warning()
        && !arguments.yes
    {
        bail!("{setting}: {warning}; pass --yes to confirm");
    }
    let unit = arguments.connection.named_unit()?;
    if let Some(endpoint) = arguments.route.endpoint(unit.as_deref())? {
        let (client, _) = DaemonClient::connect(&endpoint, unit.as_deref()).await?;
        // Confirmation, if needed, was given above.
        client
            .post_setting(setting, arguments.yes)
            .await
            .map_err(anyhow::Error::msg)?;
        println!("ok: {setting}");
        return Ok(());
    }
    let (reply, answer) = oneshot::channel();
    let request = ControlRequest::Setting(SettingRequest { setting, reply });
    direct(&arguments.connection, request, answer, &setting).await??;
    println!("ok: {setting}");
    Ok(())
}

/// Connects to the unit, has the poller run `request`, and returns its
/// `answer`.  Fails if polling ends first.
pub(crate) async fn direct<T>(
    connection: &ConnectionArgs,
    request: ControlRequest,
    answer: oneshot::Receiver<T>,
    what: &dyn fmt::Display,
) -> Result<T> {
    let (key_file, _lock) = connection.claim(KeyHome::User)?;
    let config = connection.poll_config(Some(MAX_FAILURES));
    let adapter = ble::default_adapter().await?;
    let (sender, commands) = mpsc::channel(1);
    sender.send(request).await.context("queuing the request")?;
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
        Some(result) => Ok(result),
        None => {
            polled.context("polling failed")?;
            bail!("polling ended before {what} was answered")
        }
    }
}
