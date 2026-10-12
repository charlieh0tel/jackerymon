//! HTTP routes.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::Event;
use axum::response::sse::KeepAlive;
use axum::response::sse::Sse;
use axum::routing::get;
use axum::routing::post;
use futures::Stream;
use futures::StreamExt;
use futures::stream;
use jackerymon::control::ControlError;
use jackerymon::control::Setting;
use jackerymon::poll::ControlRequest;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_stream::wrappers::BroadcastStream;

use super::EVENT_KEEP_ALIVE;
use super::Shared;
use super::metrics;
use crate::record::Update;

/// How long `POST /settings` waits, including for room in the queue and
/// for the unit to come back.  A request still queued when this expires
/// is never sent; one already sent may still take effect.
const SETTING_TIMEOUT: Duration = Duration::from_secs(60);

/// Prometheus text exposition format, version 0.0.4.
const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

pub(super) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .route("/status", get(status))
        .route("/events", get(events))
        .route("/settings", post(settings))
        .with_state(shared)
}

async fn metrics(State(shared): State<Arc<Shared>>) -> impl IntoResponse {
    let snapshot = shared.snapshot.lock().expect("snapshot lock").clone();
    (
        [(CONTENT_TYPE, METRICS_CONTENT_TYPE)],
        metrics::render(&snapshot, SystemTime::now()),
    )
}

/// Link state, counters, and the latest sample in the `poll` line shape.
async fn status(State(shared): State<Arc<Shared>>) -> Json<Value> {
    let snapshot = shared.snapshot.lock().expect("snapshot lock").clone();
    Json(json!({
        "serial": snapshot.serial,
        "link": snapshot.link,
        "samples": snapshot.samples,
        "connections": snapshot.connections,
        "transport_failures": snapshot.transport_failures,
        "latest": snapshot.latest.map(|latest| latest.record),
    }))
}

/// Server-sent events, one [`Update`] each as JSON: first the current
/// link state and latest sample, then every change until polling ends.
/// A client that falls behind skips the updates it missed.
async fn events(
    State(shared): State<Arc<Shared>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Subscribe before taking the snapshot so nothing falls between them;
    // at worst one update arrives twice, which clients tolerate.
    let live = BroadcastStream::new(shared.updates.subscribe())
        .filter_map(|update| async move { update.ok() });
    let replay = shared.snapshot.lock().expect("snapshot lock").replay();
    let updates = stream::iter(replay)
        .chain(live)
        .take_until(shared.stopped())
        .map(|update: Update| {
            Ok(Event::default()
                .json_data(&update)
                .expect("updates serialize to JSON"))
        });
    Sse::new(updates).keep_alive(KeepAlive::new().interval(EVENT_KEEP_ALIVE))
}

#[derive(Deserialize)]
struct SettingQuery {
    /// Required for a setting with a warning, as `set --yes` is.
    #[serde(default)]
    confirm: bool,
}

/// Applies one setting given as the unit's one-key body, such as
/// `{"lm":1}`, and answers once a status read confirms it.
async fn settings(
    State(shared): State<Arc<Shared>>,
    Query(query): Query<SettingQuery>,
    Json(body): Json<Map<String, Value>>,
) -> Response {
    let setting = match parse_setting(&body) {
        Ok(setting) => setting,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    if let Some(warning) = setting.warning()
        && !query.confirm
    {
        return error(
            StatusCode::CONFLICT,
            &format!("{setting}: {warning}; repeat with ?confirm=true"),
        );
    }
    let (reply, answer) = oneshot::channel();
    // `None` once polling has stopped.
    let answered = timeout(SETTING_TIMEOUT, async {
        shared
            .commands
            .send(ControlRequest { setting, reply })
            .await
            .ok()?;
        answer.await.ok()
    })
    .await;
    match answered {
        Ok(Some(Ok(()))) => Json(json!({ "applied": setting.to_string() })).into_response(),
        Ok(Some(Err(control))) => {
            let code = match control {
                ControlError::Rejected { .. } | ControlError::NotApplied(_) => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
                ControlError::Transport { .. } => StatusCode::SERVICE_UNAVAILABLE,
            };
            error(code, &control.to_string())
        }
        Ok(None) => error(StatusCode::SERVICE_UNAVAILABLE, "polling has stopped"),
        Err(_) => error(
            StatusCode::GATEWAY_TIMEOUT,
            &format!("{setting}: no answer within {SETTING_TIMEOUT:?}; not sent if still queued"),
        ),
    }
}

fn parse_setting(body: &Map<String, Value>) -> Result<Setting, String> {
    let mut entries = body.iter();
    let (Some((key, value)), None) = (entries.next(), entries.next()) else {
        return Err("expected one key, such as {\"lm\":1}".to_owned());
    };
    value
        .as_i64()
        .and_then(|value| Setting::from_wire(key, value))
        .ok_or_else(|| format!("unsupported setting {key}={value}"))
}

fn error(code: StatusCode, message: &str) -> Response {
    (code, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use axum::Json;
    use axum::extract::Query;
    use axum::extract::State;
    use axum::http::StatusCode;
    use jackerymon::control::Setting;
    use jackerymon::poll::ControlRequest;
    use jackerymon::status::LightMode;
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::SettingQuery;
    use super::parse_setting;
    use super::settings;
    use crate::daemon::COMMAND_BUFFER;
    use crate::daemon::Shared;

    #[test]
    fn parses_one_key_bodies_only() {
        let parse = |value: serde_json::Value| parse_setting(value.as_object().expect("object"));
        assert_eq!(parse(json!({"lm": 2})), Ok(Setting::Light(LightMode::High)));
        assert!(parse(json!({})).is_err());
        assert!(parse(json!({"lm": 1, "oac": 1})).is_err());
        assert!(parse(json!({"lm": "high"})).is_err());
        assert!(parse(json!({"lm": 9})).is_err());
    }

    /// With the unit away and the queue full, a request still times out
    /// instead of waiting for room.
    #[tokio::test(start_paused = true)]
    async fn full_queue_counts_against_the_deadline() {
        let (shared, _requests) = Shared::new("123456789".to_owned());
        let light = Setting::Light(LightMode::Low);
        for _ in 0..COMMAND_BUFFER {
            let (reply, _) = oneshot::channel();
            shared
                .commands
                .try_send(ControlRequest {
                    setting: light,
                    reply,
                })
                .expect("room");
        }
        let response = settings(
            State(shared),
            Query(SettingQuery { confirm: false }),
            Json(json!({"lm": 1}).as_object().expect("object").clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }
}
