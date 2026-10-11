//! `jackerymon tui`: live status on one screen.
//!
//! The screen consumes poll events from a channel, not BLE, so a daemon
//! can feed it later without changes here.

mod menu;
mod monitor;
mod view;

use std::future::pending;
use std::ops::ControlFlow;
use std::pin::pin;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use clap::Args;
use crossterm::event::Event;
use crossterm::event::EventStream;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use futures::StreamExt;
use jackerymon::ble;
use jackerymon::control::ControlError;
use jackerymon::control::Setting;
use jackerymon::poll::ControlRequest;
use ratatui::DefaultTerminal;
use tokio::signal::ctrl_c;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::RecvError;
use tokio::time::Instant;
use tokio::time::interval;

use crate::connection::ConnectionArgs;
use crate::record::Update;
use crate::tui::menu::Menu;
use crate::tui::menu::controls;
use crate::tui::monitor::ControlState;
use crate::tui::monitor::Monitor;

/// Redraw at least this often so the data age stays current.
const REDRAW_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Args)]
pub(crate) struct TuiArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
}

pub(crate) async fn run(arguments: TuiArgs) -> Result<()> {
    let key_file = arguments.connection.key_file()?;
    let mut monitor = Monitor::new(key_file.serial.to_string());
    // A monitor keeps trying while the unit's radio is off.
    let config = arguments.connection.poll_config(None);
    let adapter = ble::default_adapter().await?;
    let (sender, mut events) = mpsc::unbounded_channel();
    // One setting at a time; the screen refuses another while one runs.
    let (commands, requests) = mpsc::channel(1);
    let (stop, stopped) = oneshot::channel::<()>();
    let poller = tokio::spawn(ble::poll(
        adapter,
        key_file,
        config,
        Some(requests),
        async {
            // A dropped sender also means stop.
            stopped.await.ok();
        },
        move |event| {
            if sender.send(Update::from(event)).is_ok() {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        },
    ));
    let mut terminal = ratatui::init();
    let shown = show(&mut terminal, &mut monitor, &mut events, &commands).await;
    ratatui::restore();
    // The poller may already have ended, closing the receiver.
    stop.send(()).ok();
    poller
        .await
        .context("poller task failed")?
        .context("polling failed")?;
    shown
}

/// The setting in progress, if any, and where its answer arrives.
type InProgress = Option<(Setting, oneshot::Receiver<Result<(), ControlError>>)>;

/// Draws until the user quits or the poller ends.
async fn show(
    terminal: &mut DefaultTerminal,
    monitor: &mut Monitor,
    events: &mut mpsc::UnboundedReceiver<Update>,
    commands: &mpsc::Sender<ControlRequest>,
) -> Result<()> {
    let controls = controls();
    let mut menu = Menu::Closed;
    let mut in_progress: InProgress = None;
    let mut keys = EventStream::new();
    let mut redraw = interval(REDRAW_INTERVAL);
    // Raw mode turns a typed Ctrl-C into a key; this catches a signal
    // from elsewhere, so the terminal and connection are still restored.
    let mut interrupted = pin!(ctrl_c());
    loop {
        terminal.draw(|frame| view::draw(frame, monitor, menu, &controls, Instant::now()))?;
        // Biased so a status sample is shown before the reply that it
        // confirmed, never the reverse.
        tokio::select! {
            biased;
            event = events.recv() => match event {
                Some(event) => monitor.apply(event, Instant::now()),
                None => return Ok(()),
            },
            key = keys.next() => match key {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    if is_quit(key, menu) {
                        return Ok(());
                    }
                    let status = monitor.latest.as_ref().map(|reading| &reading.status);
                    if let Some(setting) = menu.handle(key.code, &controls, status) {
                        request(setting, commands, &mut in_progress, monitor);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error).context("reading terminal events"),
                None => return Ok(()),
            },
            _ = redraw.tick() => {}
            (setting, answer) = wait(&mut in_progress) => {
                in_progress = None;
                monitor.control = Some(match answer {
                    Ok(Ok(())) => ControlState::Applied(setting),
                    Ok(Err(error)) => ControlState::Failed(error.to_string()),
                    Err(_) => ControlState::Failed("polling stopped".to_owned()),
                });
            }
            result = &mut interrupted => {
                result.context("listening for Ctrl-C")?;
                return Ok(());
            }
        }
    }
}

/// Ctrl-C always quits; q and Esc only outside the menu, where Esc backs
/// out instead.
fn is_quit(key: KeyEvent, menu: Menu) -> bool {
    (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (menu == Menu::Closed && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc))
}

/// Queues `setting` for the poller unless one is already in progress.
fn request(
    setting: Setting,
    commands: &mpsc::Sender<ControlRequest>,
    in_progress: &mut InProgress,
    monitor: &mut Monitor,
) {
    if in_progress.is_some() {
        monitor.control = Some(ControlState::Failed(format!(
            "{setting} not sent: another setting is in progress"
        )));
        return;
    }
    let (reply, answer) = oneshot::channel();
    monitor.control = Some(match commands.try_send(ControlRequest { setting, reply }) {
        Ok(()) => {
            *in_progress = Some((setting, answer));
            ControlState::Pending(setting)
        }
        Err(_) => ControlState::Failed(format!("{setting} not sent: polling stopped")),
    });
}

/// The answer for the setting in progress; never resolves without one.
async fn wait(
    in_progress: &mut InProgress,
) -> (Setting, Result<Result<(), ControlError>, RecvError>) {
    match in_progress {
        Some((setting, answer)) => (*setting, answer.await),
        None => pending().await,
    }
}
