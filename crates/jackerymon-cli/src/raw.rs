//! `jackerymon raw`: send one command as given and print the unit's echo.

use anyhow::Result;
use anyhow::bail;
use clap::Args;
use jackerymon::control::RawCommand;
use jackerymon::poll::ControlRequest;
use jackerymon::poll::RawRequest;
use serde_json::Map;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::connection::ConnectionArgs;
use crate::daemon_client::DaemonClient;
use crate::daemon_client::Endpoint;
use crate::daemon_client::RouteArgs;
use crate::set;

/// Options of `jackerymon raw`.
#[derive(Args)]
pub(crate) struct RawArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    #[command(flatten)]
    route: RouteArgs,
    /// Action id, such as 33.
    action: u8,
    /// JSON object body, such as '{"cl":60}'; `{}` is sent as no body.
    #[arg(default_value = "{}", value_parser = parse_body)]
    body: Map<String, Value>,
    /// Message type.
    #[arg(long = "type", default_value_t = RawCommand::DEFAULT_MESSAGE_TYPE)]
    message_type: u8,
}

fn parse_body(text: &str) -> Result<Map<String, Value>, String> {
    serde_json::from_str(text).map_err(|error| format!("expected a JSON object: {error}"))
}

/// Prints the echo as one line of JSON.  Nothing confirms the command
/// took effect.
pub(crate) async fn run(arguments: RawArgs) -> Result<()> {
    let command = RawCommand::new(arguments.action, arguments.message_type, arguments.body)?;
    let unit = arguments.connection.named_unit()?;
    let echo = match arguments.route.endpoint(unit.as_deref())? {
        Some(endpoint @ Endpoint::Tcp(_)) => {
            bail!("raw commands go over a daemon's socket only, not {endpoint}")
        }
        Some(endpoint) => {
            let (client, _) = DaemonClient::connect(&endpoint, unit.as_deref()).await?;
            client
                .post_raw(&command)
                .await
                .map_err(anyhow::Error::msg)?
        }
        None => {
            let shown = command.to_string();
            let (reply, answer) = oneshot::channel();
            let request = ControlRequest::Raw(RawRequest { command, reply });
            set::direct(&arguments.connection, request, answer, &shown).await??
        }
    };
    println!("{echo}");
    Ok(())
}
