//! Sources agent runs read (docs/14 §3): which source a run is for, its MCP server for the
//! agent, and how far each job has read it.

use chrono::{DateTime, Utc};
use frood_config::Source;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::jobs::{AgentSettings, JobError};
use crate::runs::Run;

/// The source a run is for: from its schedule tick (`for_each: source`) or from a manual
/// run's input (`{"source": "team-chat"}`).
pub(crate) fn run_source(run: &Run) -> Option<String> {
    run.input
        .pointer("/event/data/source")
        .or_else(|| run.input.pointer("/manual/input/source"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The job's tools for this run, with `${source}` filled in.
pub(crate) fn tools(settings: &AgentSettings, run: &Run) -> Result<Vec<String>, JobError> {
    let source = run_source(run);
    settings
        .tools
        .iter()
        .map(|tool| match (tool.contains("${source}"), &source) {
            (false, _) => Ok(tool.clone()),
            (true, Some(source)) => Ok(tool.replace("${source}", source)),
            (true, None) => Err(JobError::permanent(format!(
                "tool {tool} needs a source: run the job with for_each: source, or start it with {{\"source\": …}} as input"
            ))),
        })
        .collect()
}

/// Name of a source's MCP server for the agent: `source-<name>`. Agents name tools after
/// their servers, and tool names allow only letters, digits, `_` and `-`.
pub(crate) fn server_name(source: &str) -> String {
    format!("source-{source}")
}

/// The MCP server entry of a source, for the runtime.
pub(crate) fn mcp_server(name: &str, source: &Source) -> Value {
    json!({
        "name": server_name(name),
        "url": source.server,
        "headers": source.headers,
    })
}

/// Where the job last finished reading the source.
pub(crate) async fn checkpoint(
    pool: &PgPool,
    source: &str,
    reader: &str,
) -> sqlx::Result<Option<DateTime<Utc>>> {
    let checkpoint: Option<Value> = sqlx::query_scalar(
        "SELECT checkpoint FROM source_checkpoints WHERE source = $1 AND reader = $2",
    )
    .bind(source)
    .bind(reader)
    .fetch_optional(pool)
    .await?;
    Ok(checkpoint
        .as_ref()
        .and_then(|c| c.get("read_until"))
        .and_then(|t| serde_json::from_value(t.clone()).ok()))
}

/// A run that read the source succeeded: the next one reads from when this one started.
pub(crate) async fn advance(
    pool: &PgPool,
    source: &str,
    reader: &str,
    read_until: DateTime<Utc>,
    run_id: Uuid,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO source_checkpoints (source, reader, checkpoint, updated_at) VALUES ($1, $2, $3, now()) \
         ON CONFLICT (source, reader) DO UPDATE SET checkpoint = excluded.checkpoint, updated_at = now() \
         WHERE (source_checkpoints.checkpoint->>'read_until')::timestamptz IS NULL \
            OR (source_checkpoints.checkpoint->>'read_until')::timestamptz < $4",
    )
    .bind(source)
    .bind(reader)
    .bind(json!({ "read_until": read_until, "run_id": run_id }))
    .bind(read_until)
    .execute(pool)
    .await?;
    Ok(())
}
