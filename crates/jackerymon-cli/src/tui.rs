//! `jackerymon tui`: live status on one screen.
//!
//! The screen consumes poll events from a channel, not BLE, so a daemon
//! can feed it later without changes here.

mod monitor;
mod view;

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
use jackerymon::poll::PollEvent;
use ratatui::DefaultTerminal;
use tokio::signal::ctrl_c;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio::time::interval;

use crate::connection::ConnectionArgs;
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
    let (stop, stopped) = oneshot::channel::<()>();
    let poller = tokio::spawn(ble::poll(
        adapter,
        key_file,
        config,
        None,
        async {
            // A dropped sender also means stop.
            stopped.await.ok();
        },
        move |event| {
            if sender.send(event).is_ok() {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        },
    ));
    let mut terminal = ratatui::init();
    let shown = show(&mut terminal, &mut monitor, &mut events).await;
    ratatui::restore();
    // The poller may already have ended, closing the receiver.
    stop.send(()).ok();
    poller
        .await
        .context("poller task failed")?
        .context("polling failed")?;
    shown
}

/// Draws until the user quits or the poller ends.
async fn show(
    terminal: &mut DefaultTerminal,
    monitor: &mut Monitor,
    events: &mut mpsc::UnboundedReceiver<PollEvent>,
) -> Result<()> {
    let mut keys = EventStream::new();
    let mut redraw = interval(REDRAW_INTERVAL);
    // Raw mode turns a typed Ctrl-C into a key; this catches a signal
    // from elsewhere, so the terminal and connection are still restored.
    let mut interrupted = pin!(ctrl_c());
    loop {
        terminal.draw(|frame| view::draw(frame, monitor, Instant::now()))?;
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => monitor.apply(event, Instant::now()),
                None => return Ok(()),
            },
            key = keys.next() => match key {
                Some(Ok(Event::Key(key))) if is_quit(key) => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error).context("reading terminal events"),
                None => return Ok(()),
            },
            _ = redraw.tick() => {}
            result = &mut interrupted => {
                result.context("listening for Ctrl-C")?;
                return Ok(());
            }
        }
    }
}

fn is_quit(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && (matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)))
}
