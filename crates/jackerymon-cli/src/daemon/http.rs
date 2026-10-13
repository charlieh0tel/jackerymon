//! HTTP routes.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::body::Bytes;
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
use chrono::DateTime;
use chrono::Utc;
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
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task;
use tokio::time::timeout;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::ReceiverStream;

use super::EVENT_KEEP_ALIVE;
use super::Shared;
use super::log;
use super::metrics;
use super::web;
use super::web::View;
use crate::record::Update;

/// How long `POST /settings` waits, including for room in the queue and
/// for the unit to come back.  A request still queued when this expires
/// is never sent; one already sent may still take effect.
const SETTING_TIMEOUT: Duration = Duration::from_secs(60);

/// Lines `/history` reads ahead of a slow client.
const HISTORY_BUFFER: usize = 256;
const HTML_CONTENT_TYPE: &str = "text/html; charset=utf-8";
const CSS_CONTENT_TYPE: &str = "text/css; charset=utf-8";
const JS_CONTENT_TYPE: &str = "text/javascript; charset=utf-8";
/// One JSON value per line.
const JSON_LINES_CONTENT_TYPE: &str = "application/x-ndjson";
/// Prometheus text exposition format, version 0.0.4.
const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The routes, with `POST /settings` only when `control` is set.
pub(super) fn router(shared: Arc<Shared>, control: bool) -> Router {
    let reads = Router::new()
        .route("/metrics", get(metrics))
        .route("/status", get(status))
        .route("/events", get(events))
        .route("/history", get(history))
        .route("/series", get(series))
        .route("/", get(|| async { page(HTML_CONTENT_TYPE, web::INDEX) }))
        .route(
            "/style.css",
            get(|| async { page(CSS_CONTENT_TYPE, web::STYLE) }),
        )
        .route(
            "/common.js",
            get(|| async { page(JS_CONTENT_TYPE, web::COMMON) }),
        );
    let routes = if control {
        reads.route("/settings", post(settings))
    } else {
        reads
    };
    routes.with_state(shared)
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
        "version": env!("CARGO_PKG_VERSION"),
        "view": snapshot.latest.as_ref().and_then(|latest| latest.status.as_ref()).map(View::from),
        "latest": snapshot.latest.map(|latest| latest.record),
    }))
}

/// A page or asset built into the binary.
fn page(content_type: &'static str, body: &'static str) -> Response {
    ([(CONTENT_TYPE, content_type)], body).into_response()
}

/// `GET /series` query parameters: RFC 3339 times and a bucket width in
/// seconds.
#[derive(Deserialize)]
struct SeriesQuery {
    since: String,
    /// Now when absent.
    until: Option<String>,
    /// One sample's worth when absent.
    every: Option<i64>,
}

/// Chart columns over a range, averaged per bucket; see [`web::series`].
async fn series(State(shared): State<Arc<Shared>>, Query(query): Query<SeriesQuery>) -> Response {
    let Some(directory) = shared.log_dir.clone() else {
        return error(
            StatusCode::NOT_FOUND,
            "no history: the daemon has no --log-dir",
        );
    };
    let (since, until) = match time_range(&query.since, query.until.as_deref()) {
        Ok(range) => range,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let every = query.every.unwrap_or(1);
    match task::spawn_blocking(move || web::series(&directory, since, until, every)).await {
        Ok(Ok(series)) => Json(series).into_response(),
        Ok(Err(failure)) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{failure:#}")),
        Err(failure) => error(StatusCode::INTERNAL_SERVER_ERROR, &failure.to_string()),
    }
}

/// `since` and `until` (now when absent) parsed as RFC 3339.
fn time_range(since: &str, until: Option<&str>) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let parse = |text: &str| {
        DateTime::parse_from_rfc3339(text)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| format!("{text:?} is not an RFC 3339 time"))
    };
    Ok((parse(since)?, until.map_or_else(|| Ok(Utc::now()), parse)?))
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

/// `GET /history` query parameters, RFC 3339.
#[derive(Deserialize)]
struct HistoryQuery {
    since: String,
    /// Now when absent.
    until: Option<String>,
}

/// Logged samples in `since..=until`, one JSON line each, oldest first,
/// streamed from the daily files.
async fn history(State(shared): State<Arc<Shared>>, Query(query): Query<HistoryQuery>) -> Response {
    let Some(directory) = shared.log_dir.clone() else {
        return error(
            StatusCode::NOT_FOUND,
            "no history: the daemon has no --log-dir",
        );
    };
    let (since, until) = match time_range(&query.since, query.until.as_deref()) {
        Ok(range) => range,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let (lines, received) = mpsc::channel::<Bytes>(HISTORY_BUFFER);
    task::spawn_blocking(move || {
        // A send fails once the client has gone, which ends the read.
        let read = log::read_history(&directory, since, until, |line| {
            lines
                .blocking_send(Bytes::from(format!("{line}\n")))
                .is_ok()
        });
        if let Err(error) = read {
            ::log::error!("history: {error:#}");
        }
    });
    let body = Body::from_stream(ReceiverStream::new(received).map(Ok::<_, Infallible>));
    ([(CONTENT_TYPE, JSON_LINES_CONTENT_TYPE)], body).into_response()
}

/// `POST /settings` query parameters.
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
        let (shared, _requests) = Shared::new("123456789".to_owned(), None);
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
