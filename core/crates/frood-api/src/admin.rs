//! The admin panel (N9′, C1): a page the core serves at `/admin/`, working only through the API
//! with a token it gets by the same sign-in as AI clients; plus health, recent logs and
//! Prometheus metrics for it and for outside monitoring.

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use frood_dispatch::health::Health;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{Admin, Caller};
use crate::error::ApiError;

const PAGE: &str = include_str!("admin.html");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(|| async { Redirect::permanent("/admin/") }))
        .route("/admin/", get(page))
        .route("/v1/health", get(health))
        .route("/v1/logs", get(logs))
        .route("/metrics", get(metrics))
}

fn db(err: frood_store::sqlx::Error) -> ApiError {
    frood_ledger::LedgerError::from(err).into()
}

async fn page() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // The page talks only to this server; no inline code from elsewhere.
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        PAGE,
    )
        .into_response()
}

async fn health(
    State(state): State<AppState>,
    _caller: Caller<Admin>,
) -> Result<Json<Value>, ApiError> {
    let health = state.dispatch.health().await.map_err(db)?;
    let mut value = serde_json::to_value(health).unwrap_or_default();
    value["version"] = json!(env!("CARGO_PKG_VERSION"));
    value["config_sha256"] = json!(state.config_sha256);
    Ok(Json(value))
}

#[derive(Deserialize)]
struct LogsQuery {
    after: Option<u64>,
    limit: Option<usize>,
}

async fn logs(_caller: Caller<Admin>, Query(q): Query<LogsQuery>) -> Json<Value> {
    let lines: Vec<Value> =
        crate::logs::after(q.after.unwrap_or(0), q.limit.unwrap_or(500).min(2000))
            .into_iter()
            .map(|(seq, line)| json!({ "seq": seq, "line": line }))
            .collect();
    Json(json!(lines))
}

/// Prometheus text format: what the health page shows, for outside monitoring.
async fn metrics(
    State(state): State<AppState>,
    _caller: Caller<Admin>,
) -> Result<Response, ApiError> {
    let h: Health = state.dispatch.health().await.map_err(db)?;
    let mut out = String::new();
    let mut gauge = |name: &str, help: &str, samples: Vec<(String, f64)>| {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n"));
        for (labels, value) in samples {
            out.push_str(&format!("{name}{labels} {value}\n"));
        }
    };
    let label = |pairs: &[(&str, &str)]| {
        let inner: Vec<String> = pairs
            .iter()
            .map(|(k, v)| format!("{k}=\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        format!("{{{}}}", inner.join(","))
    };
    gauge(
        "frood_module_up",
        "Whether a module answers its health check.",
        h.modules
            .iter()
            .map(|m| (label(&[("module", &m.name)]), f64::from(u8::from(m.up))))
            .collect(),
    );
    gauge(
        "frood_dispatcher_lag_seconds",
        "Age of the oldest journal event the dispatcher has not read.",
        vec![(String::new(), h.dispatcher_lag_seconds as f64)],
    );
    gauge(
        "frood_runs",
        "Runs by status (now), and finished in the last day.",
        vec![
            (label(&[("status", "queued")]), h.runs.queued as f64),
            (label(&[("status", "running")]), h.runs.running as f64),
            (
                label(&[("status", "waiting_for_input")]),
                h.runs.waiting_for_input as f64,
            ),
            (
                label(&[("status", "failed_last_day")]),
                h.runs.failed_last_day as f64,
            ),
            (
                label(&[("status", "succeeded_last_day")]),
                h.runs.succeeded_last_day as f64,
            ),
        ],
    );
    gauge(
        "frood_inbox_open",
        "Open Inbox items, and open alerts among them.",
        vec![
            (label(&[("kind", "all")]), h.inbox_open as f64),
            (label(&[("kind", "alert")]), h.alerts_open as f64),
        ],
    );
    gauge(
        "frood_account_spent_usd",
        "What agent runs on an account cost this month.",
        h.accounts
            .iter()
            .map(|a| (label(&[("account", &a.account)]), a.spent_usd))
            .collect(),
    );
    gauge(
        "frood_source_read_age_seconds",
        "Seconds since a job last read a source successfully.",
        h.sources
            .iter()
            .filter_map(|s| {
                s.seconds_since_read
                    .map(|age| (label(&[("job", &s.job), ("source", &s.source)]), age as f64))
            })
            .collect(),
    );
    if let Some(backups) = &h.backups {
        gauge(
            "frood_backup_age_seconds",
            "Seconds since the last backup that succeeded.",
            backups
                .seconds_since_backup
                .map(|age| (String::new(), age as f64))
                .into_iter()
                .collect(),
        );
        gauge(
            "frood_backup_verify_ok",
            "Whether the latest restore check passed.",
            backups
                .last_verify
                .as_ref()
                .map(|v| (String::new(), f64::from(u8::from(v.status == "succeeded"))))
                .into_iter()
                .collect(),
        );
    }
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        out,
    )
        .into_response())
}
