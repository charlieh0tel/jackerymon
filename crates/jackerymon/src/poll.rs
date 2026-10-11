//! Sequential status polling with bounded reconnects.
//!
//! One connection serves many queries.  Transport failures reconnect with
//! fresh discovery; a successful sample resets the failure streak.  Invalid
//! protocol data stops polling at once: retrying cannot fix a wrong key.
//!
//! Setting changes arrive on a channel and run between queries, never
//! during one.  Each counts as applied only once a status read shows it.

use std::future::Future;
use std::future::pending;
use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::pin::pin;
use std::time::Duration;
use std::time::SystemTime;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio::time::sleep;

use crate::ble::BleError;
use crate::control::ControlError;
use crate::control::Setting;
use crate::frame::DecodeError;
use crate::frame::RawStatus;
use crate::frame::Request;
use crate::status::Status;

/// Status reads that may confirm a setting; the unit can apply it a
/// little after acknowledging.
const VERIFY_ATTEMPTS: u32 = 3;
const VERIFY_DELAY: Duration = Duration::from_millis(500);

/// Polling timing and failure policy.
#[derive(Clone, Copy, Debug)]
pub struct PollConfig {
    /// Bound on discovery, connection, and each query.
    pub timeout: Duration,
    /// Delay after each response before the next request.
    pub interval: Duration,
    /// Delay after a transport failure before reconnecting.
    pub reconnect_delay: Duration,
    /// Consecutive transport failures that stop polling; `None` retries
    /// forever, as an interactive monitor should while the unit's radio
    /// is off.
    pub max_consecutive_failures: Option<NonZeroU32>,
}

/// A setting change for the poller to apply, answered on `reply`.
#[derive(Debug)]
pub struct ControlRequest {
    pub setting: Setting,
    pub reply: oneshot::Sender<Result<(), ControlError>>,
}

/// Where setting changes come from; `None` when the caller has none.
pub type Commands = Option<mpsc::Receiver<ControlRequest>>;

/// One decoded response and the counters at the time it arrived.
#[derive(Clone, Debug)]
pub struct Sample {
    pub timestamp: SystemTime,
    /// 1-based count of successful queries.
    pub sample: u64,
    /// 1-based count of successful connections.
    pub connection: u64,
    /// Total transport failures so far.
    pub transport_failures: u64,
    /// Request write to complete response.
    pub query: Duration,
    pub status: RawStatus,
}

/// What polling reports to its caller.
#[derive(Debug)]
pub enum PollEvent {
    /// Connected and subscribed; `connection` is the 1-based count.
    Connected {
        connection: u64,
    },
    Sample(Sample),
    /// A transport failure that will be retried.
    TransportFailure {
        error: BleError,
        /// 1-based length of the current failure streak.
        consecutive: u32,
    },
}

#[derive(Debug, Error)]
pub enum PollError {
    #[error("polling stopped after {count} consecutive transport failures: {last}")]
    TooManyFailures { count: NonZeroU32, last: BleError },
    #[error(transparent)]
    Fatal(BleError),
}

/// Opens connections to the unit.
pub(crate) trait Connector {
    type Link: Link;

    async fn connect(&mut self, timeout: Duration) -> Result<Self::Link, BleError>;
}

/// One open connection to the unit.
pub(crate) trait Link {
    /// Sends `request` and returns the JSON body of its response.
    async fn exchange(
        &mut self,
        request: &Request,
        timeout: Duration,
    ) -> Result<RawStatus, BleError>;

    /// Waits `duration`, failing early if the unit disconnects.
    async fn idle(&mut self, duration: Duration) -> Result<(), BleError>;

    async fn close(self);
}

/// Running totals across connections.
#[derive(Default)]
struct Counters {
    samples: u64,
    connections: u64,
    transport_failures: u64,
    consecutive_failures: u32,
}

/// Polls until `on_event` breaks, `stop` completes, or polling fails.
/// Every exit closes the open connection.
pub(crate) async fn poll_with<C: Connector>(
    mut connector: C,
    config: PollConfig,
    mut commands: Commands,
    stop: impl Future<Output = ()>,
    mut on_event: impl FnMut(PollEvent) -> ControlFlow<()>,
) -> Result<(), PollError> {
    let mut stop = pin!(stop);
    let mut counters = Counters::default();
    loop {
        let connected = tokio::select! {
            () = &mut stop => return Ok(()),
            connected = connector.connect(config.timeout) => connected,
        };
        let error = match connected {
            Ok(mut link) => {
                counters.connections += 1;
                let flow = on_event(PollEvent::Connected {
                    connection: counters.connections,
                });
                let outcome = match flow {
                    ControlFlow::Break(()) => Ok(ControlFlow::Break(())),
                    ControlFlow::Continue(()) => {
                        tokio::select! {
                            () = &mut stop => Ok(ControlFlow::Break(())),
                            outcome = serve(&mut link, config, &mut commands, &mut counters, &mut on_event) => outcome,
                        }
                    }
                };
                link.close().await;
                match outcome {
                    Ok(ControlFlow::Break(())) => return Ok(()),
                    Ok(ControlFlow::Continue(())) => unreachable!("serve only returns to stop"),
                    Err(error) => error,
                }
            }
            Err(error) => error,
        };
        if !error.is_transport() {
            return Err(PollError::Fatal(error));
        }
        counters.transport_failures += 1;
        counters.consecutive_failures += 1;
        if let Some(limit) = config.max_consecutive_failures
            && counters.consecutive_failures >= limit.get()
        {
            return Err(PollError::TooManyFailures {
                count: limit,
                last: error,
            });
        }
        let flow = on_event(PollEvent::TransportFailure {
            error,
            consecutive: counters.consecutive_failures,
        });
        if flow.is_break() {
            return Ok(());
        }
        tokio::select! {
            () = &mut stop => return Ok(()),
            () = sleep(config.reconnect_delay) => {}
        }
    }
}

/// Queries one link until the caller stops or the link fails.
async fn serve<L: Link>(
    link: &mut L,
    config: PollConfig,
    commands: &mut Commands,
    counters: &mut Counters,
    on_event: &mut impl FnMut(PollEvent) -> ControlFlow<()>,
) -> Result<ControlFlow<()>, BleError> {
    loop {
        let started = Instant::now();
        let status = link.exchange(&Request::status(), config.timeout).await?;
        if record(status, started, counters, on_event).is_break() {
            return Ok(ControlFlow::Break(()));
        }
        // A setting change restarts the wait: its status reads are samples.
        loop {
            tokio::select! {
                result = link.idle(config.interval) => {
                    result?;
                    break;
                }
                Some(request) = next_command(commands) => {
                    if apply(link, config, request, counters, on_event).await?.is_break() {
                        return Ok(ControlFlow::Break(()));
                    }
                }
            }
        }
    }
}

async fn next_command(commands: &mut Commands) -> Option<ControlRequest> {
    match commands {
        Some(receiver) => receiver.recv().await,
        None => pending().await,
    }
}

/// Reports one status response as a sample.
fn record(
    status: RawStatus,
    started: Instant,
    counters: &mut Counters,
    on_event: &mut impl FnMut(PollEvent) -> ControlFlow<()>,
) -> ControlFlow<()> {
    counters.samples += 1;
    counters.consecutive_failures = 0;
    on_event(PollEvent::Sample(Sample {
        timestamp: SystemTime::now(),
        sample: counters.samples,
        connection: counters.connections,
        transport_failures: counters.transport_failures,
        query: started.elapsed(),
        status,
    }))
}

/// Sends one setting, then reads status until it shows the change.  A
/// device rejection is answered and polling goes on; a transport failure
/// is answered and returned so the link reconnects.
async fn apply<L: Link>(
    link: &mut L,
    config: PollConfig,
    request: ControlRequest,
    counters: &mut Counters,
    on_event: &mut impl FnMut(PollEvent) -> ControlFlow<()>,
) -> Result<ControlFlow<()>, BleError> {
    let setting = request.setting;
    let answer = |reply: oneshot::Sender<_>, result| {
        // The requester may have given up; polling goes on regardless.
        reply.send(result).ok();
    };
    match link.exchange(&setting.request(), config.timeout).await {
        Ok(_echo) => {}
        Err(BleError::Decode(DecodeError::Device { code, .. })) => {
            answer(request.reply, Err(ControlError::Rejected { setting, code }));
            return Ok(ControlFlow::Continue(()));
        }
        Err(error) => {
            answer(request.reply, Err(transport(setting, &error)));
            return Err(error);
        }
    }
    for attempt in 1..=VERIFY_ATTEMPTS {
        let started = Instant::now();
        let status = match link.exchange(&Request::status(), config.timeout).await {
            Ok(status) => status,
            Err(error) => {
                answer(request.reply, Err(transport(setting, &error)));
                return Err(error);
            }
        };
        let applied = Status::try_from(status.clone()).is_ok_and(|status| setting.applied(&status));
        let flow = record(status, started, counters, on_event);
        if applied {
            answer(request.reply, Ok(()));
            return Ok(flow);
        }
        if flow.is_break() || attempt == VERIFY_ATTEMPTS {
            answer(request.reply, Err(ControlError::NotApplied(setting)));
            return Ok(flow);
        }
        sleep(VERIFY_DELAY).await;
    }
    unreachable!("the last attempt answers")
}

fn transport(setting: Setting, error: &BleError) -> ControlError {
    ControlError::Transport {
        setting,
        error: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::future::pending;
    use std::num::NonZeroU32;
    use std::ops::ControlFlow;
    use std::rc::Rc;
    use std::time::Duration;

    use serde_json::json;

    use tokio::sync::mpsc;
    use tokio::sync::oneshot;

    use super::Connector;
    use super::ControlRequest;
    use super::Link;
    use super::PollConfig;
    use super::PollError;
    use super::PollEvent;
    use super::poll_with;
    use crate::ble::BleError;
    use crate::control::ControlError;
    use crate::control::Setting;
    use crate::frame::DecodeError;
    use crate::frame::RawStatus;
    use crate::frame::Request;
    use crate::status::LightMode;

    const LIMIT: NonZeroU32 = NonZeroU32::new(3).expect("nonzero");

    /// Scripted outcome of one connect or exchange.
    enum Step {
        ConnectFails,
        Status(i64),
        /// Status showing light mode `lm`.
        Light(i64),
        /// The unit echoes a setting.
        Ack,
        /// The unit rejects a setting with this code.
        Reject(u8),
        QueryFails,
        BadData,
        /// Query succeeds, then the unit disconnects while idle.
        StatusThenDrop(i64),
    }

    /// Shared script, actions sent, and a count of closed links.
    #[derive(Clone, Default)]
    struct Script {
        steps: Rc<RefCell<VecDeque<Step>>>,
        actions: Rc<RefCell<Vec<u8>>>,
        connects: Rc<RefCell<u32>>,
        closed: Rc<RefCell<u32>>,
    }

    struct FakeLink {
        script: Script,
        drop_while_idle: bool,
    }

    impl Connector for Script {
        type Link = FakeLink;

        async fn connect(&mut self, _timeout: Duration) -> Result<FakeLink, BleError> {
            *self.connects.borrow_mut() += 1;
            let mut steps = self.steps.borrow_mut();
            if matches!(steps.front(), Some(Step::ConnectFails)) {
                steps.pop_front();
                return Err(BleError::NotFound);
            }
            Ok(FakeLink {
                script: self.clone(),
                drop_while_idle: false,
            })
        }
    }

    impl Link for FakeLink {
        async fn exchange(
            &mut self,
            request: &Request,
            _timeout: Duration,
        ) -> Result<RawStatus, BleError> {
            self.script.actions.borrow_mut().push(request.action.0);
            let step = self.script.steps.borrow_mut().pop_front();
            match step {
                Some(Step::Status(battery)) => Ok(status(battery)),
                Some(Step::Light(mode)) => {
                    Ok(json!({"lm": mode}).as_object().expect("object").clone())
                }
                Some(Step::Ack) => Ok(RawStatus::new()),
                Some(Step::Reject(code)) => Err(BleError::Decode(DecodeError::Device {
                    action: request.action.0,
                    code,
                })),
                Some(Step::StatusThenDrop(battery)) => {
                    self.drop_while_idle = true;
                    Ok(status(battery))
                }
                Some(Step::QueryFails) => Err(BleError::Disconnected),
                Some(Step::BadData) => Err(BleError::Decode(DecodeError::InvalidChecksum)),
                Some(Step::ConnectFails) | None => pending().await,
            }
        }

        async fn idle(&mut self, duration: Duration) -> Result<(), BleError> {
            if self.drop_while_idle {
                return Err(BleError::Disconnected);
            }
            tokio::time::sleep(duration).await;
            Ok(())
        }

        async fn close(self) {
            *self.script.closed.borrow_mut() += 1;
        }
    }

    fn status(battery: i64) -> RawStatus {
        json!({"rb": battery}).as_object().expect("object").clone()
    }

    fn config() -> PollConfig {
        PollConfig {
            timeout: Duration::from_secs(1),
            interval: Duration::from_secs(5),
            reconnect_delay: Duration::from_secs(1),
            max_consecutive_failures: Some(LIMIT),
        }
    }

    /// Runs a script until `count` samples; returns the result, the
    /// samples as (sample, connection, failures, battery), and the script.
    async fn run(
        steps: Vec<Step>,
        count: u64,
    ) -> (Result<(), PollError>, Vec<(u64, u64, u64, i64)>, Script) {
        let script = Script::default();
        script.steps.borrow_mut().extend(steps);
        let mut samples = Vec::new();
        let result = poll_with(script.clone(), config(), None, pending(), |event| {
            if let PollEvent::Sample(sample) = event {
                samples.push((
                    sample.sample,
                    sample.connection,
                    sample.transport_failures,
                    sample.status["rb"].as_i64().expect("battery"),
                ));
                if sample.sample == count {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        })
        .await;
        (result, samples, script)
    }

    #[tokio::test(start_paused = true)]
    async fn samples_share_one_connection() {
        let (result, samples, script) = run(
            vec![Step::Status(100), Step::Status(99), Step::Status(98)],
            3,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(samples, [(1, 1, 0, 100), (2, 1, 0, 99), (3, 1, 0, 98)]);
        assert_eq!(*script.connects.borrow(), 1);
        assert_eq!(*script.closed.borrow(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_keeps_numbering_and_resets_streak() {
        let (result, samples, script) = run(
            vec![
                Step::ConnectFails,
                Step::QueryFails,
                Step::Status(100),
                Step::QueryFails,
                Step::Status(99),
            ],
            2,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(samples, [(1, 2, 2, 100), (2, 3, 3, 99)]);
        assert_eq!(*script.connects.borrow(), 4);
        assert_eq!(*script.closed.borrow(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn consecutive_failures_stop_polling() {
        let (result, _, _) = run(
            vec![Step::ConnectFails, Step::QueryFails, Step::ConnectFails],
            1,
        )
        .await;
        assert!(matches!(
            result,
            Err(PollError::TooManyFailures { count, .. }) if count == LIMIT
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn protocol_error_is_not_retried() {
        let (result, _, script) = run(vec![Step::BadData, Step::Status(100)], 1).await;
        assert!(matches!(result, Err(PollError::Fatal(BleError::Decode(_)))));
        assert_eq!(*script.connects.borrow(), 1);
        assert_eq!(*script.closed.borrow(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_disconnect_reconnects() {
        let (result, samples, _) = run(vec![Step::StatusThenDrop(100), Step::Status(99)], 2).await;
        assert!(result.is_ok());
        assert_eq!(samples, [(1, 1, 0, 100), (2, 2, 1, 99)]);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_closes_connection() {
        let script = Script::default();
        script.steps.borrow_mut().push_back(Step::Status(100));
        let result = poll_with(
            script.clone(),
            config(),
            None,
            tokio::time::sleep(Duration::from_secs(1)),
            |_| ControlFlow::Continue(()),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(*script.closed.borrow(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_retries_continue() {
        let script = Script::default();
        script.steps.borrow_mut().extend(
            (0..10)
                .map(|_| Step::ConnectFails)
                .chain([Step::Status(100)]),
        );
        let mut failures = 0;
        let result = poll_with(
            script.clone(),
            PollConfig {
                max_consecutive_failures: None,
                ..config()
            },
            None,
            pending(),
            |event| match event {
                PollEvent::TransportFailure { .. } => {
                    failures += 1;
                    ControlFlow::Continue(())
                }
                PollEvent::Sample(_) => ControlFlow::Break(()),
                PollEvent::Connected { .. } => ControlFlow::Continue(()),
            },
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(failures, 10);
    }

    /// Polls with one setting queued until `samples` samples; returns the
    /// reply, the actions sent, and the poll result.
    async fn control(
        steps: Vec<Step>,
        setting: Setting,
        samples: u64,
    ) -> (Result<(), ControlError>, Vec<u8>, Result<(), PollError>) {
        let script = Script::default();
        script.steps.borrow_mut().extend(steps);
        let (sender, receiver) = mpsc::channel(1);
        let (reply, answer) = oneshot::channel();
        sender
            .send(ControlRequest { setting, reply })
            .await
            .expect("queued");
        let result = poll_with(
            script.clone(),
            config(),
            Some(receiver),
            pending(),
            |event| match event {
                PollEvent::Sample(sample) if sample.sample == samples => ControlFlow::Break(()),
                _ => ControlFlow::Continue(()),
            },
        )
        .await;
        let actions = script.actions.borrow().clone();
        (answer.await.expect("answered"), actions, result)
    }

    const LIGHT: Setting = Setting::Light(LightMode::Low);

    #[tokio::test(start_paused = true)]
    async fn setting_is_verified_by_status() {
        let (reply, actions, result) = control(
            vec![Step::Status(100), Step::Ack, Step::Light(0), Step::Light(1)],
            LIGHT,
            3,
        )
        .await;
        assert_eq!(reply, Ok(()));
        assert_eq!(actions, [6, 17, 6, 6]);
        assert!(result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn rejection_is_answered_and_polling_continues() {
        let (reply, actions, result) = control(
            vec![Step::Status(100), Step::Reject(5), Step::Status(99)],
            LIGHT,
            2,
        )
        .await;
        assert_eq!(
            reply,
            Err(ControlError::Rejected {
                setting: LIGHT,
                code: 5
            })
        );
        assert_eq!(actions, [6, 17, 6]);
        assert!(result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn unconfirmed_setting_is_reported() {
        let (reply, _, _) = control(
            vec![
                Step::Status(100),
                Step::Ack,
                Step::Light(0),
                Step::Light(0),
                Step::Light(0),
            ],
            LIGHT,
            4,
        )
        .await;
        assert_eq!(reply, Err(ControlError::NotApplied(LIGHT)));
    }

    #[tokio::test(start_paused = true)]
    async fn transport_failure_is_answered_and_reconnects() {
        let (reply, actions, result) = control(
            vec![Step::Status(100), Step::QueryFails, Step::Status(99)],
            LIGHT,
            2,
        )
        .await;
        assert!(matches!(reply, Err(ControlError::Transport { .. })));
        assert_eq!(actions, [6, 17, 6]);
        assert!(result.is_ok());
    }
}
