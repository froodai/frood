//! Frood module `backup-restic`: `backup.target@1` with restic and the PostgreSQL client tools.
//!
//! Environment (written by `frood apply`): `FROOD_TOKEN` (the core presents it when calling),
//! `PORT` (8080). It has no settings and no secrets of its own: each call brings the database and
//! the targets. A folder target is a folder the config mounts into the module (`volumes`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

const MANIFEST: &str = include_str!("../frood-module.yaml");
/// What goes into a snapshot; restic keeps this path, so a restore names it.
const WORK: &str = "/backup";
const RESTORE: &str = "/restore";
/// Files the module writes itself; the core may not send files with these names.
const OWN_FILES: [&str; 2] = ["db.dump", "manifest.json"];
const KEEP_RULES: [&str; 6] = ["last", "hourly", "daily", "weekly", "monthly", "yearly"];
/// Dumps, uploads and restores of a big installation over a slow link.
const LONG: Duration = Duration::from_secs(6 * 3600);
/// Questions to a repository: restic retries a storage that does not answer for up to
/// 15 minutes; this much is enough to tell.
const SHORT: Duration = Duration::from_secs(3 * 60);

struct App {
    token: String,
    /// One backup or check at a time: they share the work folders.
    busy: Mutex<()>,
    work: String,
    restore: String,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    repository: String,
    password: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Database {
    url: String,
}

#[derive(Deserialize)]
struct FileIn {
    name: String,
    data_base64: String,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Backup {
        installation: String,
        database: Database,
        #[serde(default)]
        files: Vec<FileIn>,
        #[serde(default)]
        about: Value,
        targets: Vec<Target>,
        keep: BTreeMap<String, u32>,
    },
    Verify {
        installation: String,
        database: Database,
        targets: Vec<Target>,
    },
}

/// An error answer: problem details, `retryable` when trying later may help.
fn problem(status: StatusCode, detail: impl Into<String>, retryable: bool) -> Response {
    let body = json!({ "title": status.canonical_reason(), "status": status.as_u16(), "detail": detail.into(), "retryable": retryable });
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        Json(body),
    )
        .into_response()
}

fn router(app: App) -> Router {
    Router::new()
        .route("/manifest", get(manifest))
        .route(
            "/healthz",
            get(|| async { Json(json!({ "status": "ok" })) }),
        )
        .route("/v1/capabilities/backup.target@1", post(call))
        .with_state(Arc::new(app))
}

async fn manifest() -> Response {
    match serde_yaml_ng::from_str::<Value>(MANIFEST) {
        Ok(value) => Json(value).into_response(),
        Err(err) => problem(StatusCode::INTERNAL_SERVER_ERROR, err.to_string(), false),
    }
}

async fn call(State(app): State<Arc<App>>, headers: HeaderMap, body: String) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if presented != Some(app.token.as_str()) {
        return problem(
            StatusCode::UNAUTHORIZED,
            "only the Frood core may call this module",
            false,
        );
    }
    let request: Request = match serde_json::from_str(&body) {
        Ok(request) => request,
        Err(err) => return problem(StatusCode::BAD_REQUEST, err.to_string(), false),
    };
    let _busy = app.busy.lock().await;
    let result = match request {
        Request::Backup {
            installation,
            database,
            files,
            about,
            targets,
            keep,
        } => {
            backup(
                &app,
                &installation,
                &database,
                &files,
                &about,
                &targets,
                &keep,
            )
            .await
        }
        Request::Verify {
            installation,
            database,
            targets,
        } => verify(&app, &installation, &database, &targets).await,
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(Failure::Request(detail)) => problem(StatusCode::BAD_REQUEST, detail, false),
        Err(Failure::Database(detail)) => problem(StatusCode::SERVICE_UNAVAILABLE, detail, true),
    }
}

enum Failure {
    /// The request cannot work; trying again will not help.
    Request(String),
    /// The database did not answer or refused; later may work.
    Database(String),
}

fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Text with every secret of the call replaced, for errors that leave the module.
fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_owned();
    for secret in secrets.iter().filter(|s| s.len() >= 4) {
        out = out.replace(secret, "***");
    }
    out
}

/// The password in a database URL, if any.
fn url_password(database_url: &str) -> Option<String> {
    url::Url::parse(database_url)
        .ok()?
        .password()
        .map(str::to_owned)
}

/// The same server, another database.
fn with_database(database_url: &str, name: &str) -> Result<String, Failure> {
    let mut url = url::Url::parse(database_url)
        .map_err(|_| Failure::Request("database.url is not a URL".into()))?;
    url.set_path(&format!("/{name}"));
    Ok(url.to_string())
}

fn database_name(database_url: &str) -> Result<String, Failure> {
    let url = url::Url::parse(database_url)
        .map_err(|_| Failure::Request("database.url is not a URL".into()))?;
    let name = url.path().trim_start_matches('/').to_owned();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Failure::Request(
            "database.url must name the database in plain letters, digits, _ or -".into(),
        ));
    }
    Ok(name)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn connect(database_url: &str) -> Result<tokio_postgres::Client, Failure> {
    let (client, connection) = tokio_postgres::connect(database_url, tokio_postgres::NoTls)
        .await
        .map_err(|e| Failure::Database(format!("cannot connect to the database: {e}")))?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "database connection ended");
        }
    });
    Ok(client)
}

/// The server's own message when there is one: `Display` alone says only "db error".
fn db_message(err: &tokio_postgres::Error) -> String {
    err.as_db_error()
        .map_or_else(|| err.to_string(), |db| db.message().to_owned())
}

fn db_error(err: tokio_postgres::Error) -> Failure {
    Failure::Database(format!("database: {}", db_message(&err)))
}

/// Row counts of every table, as `schema.table`, inside the client's current transaction.
async fn count_rows(client: &tokio_postgres::Client) -> Result<BTreeMap<String, i64>, Failure> {
    let tables = client
        .query(
            "SELECT schemaname, tablename FROM pg_tables \
             WHERE schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY 1, 2",
            &[],
        )
        .await
        .map_err(db_error)?;
    let mut counts = BTreeMap::new();
    for row in tables {
        let (schema, table): (String, String) = (row.get(0), row.get(1));
        let count: i64 = client
            .query_one(
                &format!(
                    "SELECT count(*) FROM {}.{}",
                    quote_ident(&schema),
                    quote_ident(&table)
                ),
                &[],
            )
            .await
            .map_err(db_error)?
            .get(0);
        counts.insert(format!("{schema}.{table}"), count);
    }
    Ok(counts)
}

/// Runs a program; its error text on failure.
async fn run(program: &str, args: &[&str], env: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    run_within(program, args, env, LONG).await
}

async fn run_within(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    limit: Duration,
) -> Result<Vec<u8>, String> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env_clear()
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for key in ["PATH", "HOME", "TZ", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    // The last thing it said: the reason it failed, or what it keeps retrying.
    let last = Arc::new(std::sync::Mutex::new(String::new()));
    let said = last.clone();
    let reader = tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty()
                && let Ok(mut said) = said.lock()
            {
                *said = line.trim().to_owned();
            }
        }
    });
    let collect = tokio::spawn(async move {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out).await;
        out
    });
    let status = match tokio::time::timeout(limit, child.wait()).await {
        Ok(status) => status.map_err(|e| format!("cannot run {program}: {e}"))?,
        Err(_) => {
            let _ = child.kill().await;
            let said = last.lock().map(|s| s.clone()).unwrap_or_default();
            return Err(format!(
                "{program} did not finish within {} minutes; it said: {}",
                limit.as_secs() / 60,
                readable(&said)
            ));
        }
    };
    let out = collect.await.unwrap_or_default();
    let _ = reader.await;
    if status.success() {
        return Ok(out);
    }
    let said = last.lock().map(|s| s.clone()).unwrap_or_default();
    Err(match status.code() {
        Some(code) => format!("{program} exited with {code}: {}", readable(&said)),
        None => format!("{program} was stopped: {}", readable(&said)),
    })
}

/// A line of output as a person reads it: restic with `--json` reports errors as JSON too.
fn readable(line: &str) -> String {
    if line.is_empty() {
        return "nothing".to_owned();
    }
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v["message"].as_str().map(|m| m.replace('\n', " ")))
        .unwrap_or_else(|| line.to_owned())
}

/// restic for one target: its repository, password and environment, nothing else.
struct Restic<'a> {
    target: &'a Target,
}

impl Restic<'_> {
    fn env(&self) -> Vec<(&str, &str)> {
        let mut env = vec![
            ("RESTIC_REPOSITORY", self.target.repository.as_str()),
            ("RESTIC_PASSWORD", self.target.password.as_str()),
        ];
        env.extend(
            self.target
                .env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
        );
        env
    }

    async fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        self.run_within(args, LONG).await
    }

    async fn run_within(&self, args: &[&str], limit: Duration) -> Result<Vec<u8>, String> {
        let mut all = vec!["--retry-lock", "2m", "--no-cache"];
        all.extend_from_slice(args);
        run_within("restic", &all, &self.env(), limit).await
    }

    /// Creates the repository (and an S3 bucket) on first use. Asking first would not do: for a
    /// bucket that is not there restic keeps retrying instead of saying so.
    async fn ensure(&self) -> Result<(), String> {
        match self.run_within(&["init"], SHORT).await {
            Ok(_) => Ok(()),
            Err(err) if err.contains("already exists") || err.contains("already initialized") => {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn secrets(&self) -> Vec<&str> {
        let mut out = vec![self.target.password.as_str()];
        out.extend(self.target.env.values().map(String::as_str));
        out
    }
}

async fn clean(dir: &str) -> Result<(), Failure> {
    let path = Path::new(dir);
    if let Ok(mut entries) = tokio::fs::read_dir(path).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            let removed = if p.is_dir() {
                tokio::fs::remove_dir_all(&p).await
            } else {
                tokio::fs::remove_file(&p).await
            };
            removed.map_err(|e| Failure::Request(format!("cannot clean {dir}: {e}")))?;
        }
    }
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| Failure::Request(format!("cannot create {dir}: {e}")))
}

async fn backup(
    app: &App,
    installation: &str,
    database: &Database,
    files: &[FileIn],
    about: &Value,
    targets: &[Target],
    keep: &BTreeMap<String, u32>,
) -> Result<Value, Failure> {
    if !is_name(installation) {
        return Err(Failure::Request("installation is not a valid name".into()));
    }
    if targets.is_empty() {
        return Err(Failure::Request("no targets".into()));
    }
    if keep.is_empty()
        || keep
            .iter()
            .any(|(rule, n)| !KEEP_RULES.contains(&rule.as_str()) || *n == 0)
    {
        return Err(Failure::Request(format!(
            "keep must give at least one of {} with a number of at least 1",
            KEEP_RULES.join(", ")
        )));
    }
    let mut decoded = Vec::new();
    for file in files {
        if !is_file_name(&file.name) || OWN_FILES.contains(&file.name.as_str()) {
            return Err(Failure::Request(format!(
                "file name {:?} cannot be used",
                file.name
            )));
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(file.data_base64.as_bytes())
            .map_err(|_| Failure::Request(format!("{}: data_base64 is not base64", file.name)))?;
        decoded.push((file.name.as_str(), data));
    }
    let db_password = url_password(&database.url);
    let mut secrets: Vec<&str> = vec![database.url.as_str()];
    if let Some(p) = &db_password {
        secrets.push(p);
    }

    clean(&app.work).await?;
    for (name, data) in &decoded {
        tokio::fs::write(Path::new(&app.work).join(name), data)
            .await
            .map_err(|e| Failure::Request(format!("cannot write {name}: {e}")))?;
    }

    // One snapshot of the database for the dump and the row counts, so a restore can be
    // compared with what was there.
    let made_at = chrono::Utc::now();
    let client = connect(&database.url).await?;
    client
        .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await
        .map_err(db_error)?;
    let snapshot: String = client
        .query_one("SELECT pg_export_snapshot()", &[])
        .await
        .map_err(db_error)?
        .get(0);
    let counts = count_rows(&client).await?;
    let dump_path = format!("{}/db.dump", app.work);
    let dumped = run(
        "pg_dump",
        &[
            "--format=custom",
            &format!("--snapshot={snapshot}"),
            &format!("--file={dump_path}"),
            &format!("--dbname={}", database.url),
        ],
        &[],
    )
    .await;
    let _ = client.batch_execute("COMMIT").await;
    dumped.map_err(|e| Failure::Database(redact(&e, &secrets)))?;
    let dump_bytes = tokio::fs::metadata(&dump_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let manifest = json!({
        "installation": installation,
        "made_at": made_at,
        "about": about,
        "files": decoded.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "counts": counts,
    });
    tokio::fs::write(
        Path::new(&app.work).join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )
    .await
    .map_err(|e| Failure::Request(format!("cannot write manifest.json: {e}")))?;

    let host = installation.to_owned();
    let keep_args: Vec<String> = keep
        .iter()
        .flat_map(|(rule, n)| [format!("--keep-{rule}"), n.to_string()])
        .collect();
    let mut results = Vec::new();
    for target in targets {
        let restic = Restic { target };
        let mut all_secrets = secrets.clone();
        all_secrets.extend(restic.secrets());
        let outcome: Result<Value, String> = async {
            restic.ensure().await?;
            let out = restic
                .run(&[
                    "backup",
                    &app.work,
                    "--host",
                    &host,
                    "--tag",
                    "frood",
                    "--json",
                ])
                .await?;
            // The last line is the summary.
            let summary = String::from_utf8_lossy(&out)
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .find(|v| v["message_type"] == "summary")
                .unwrap_or_default();
            let mut forget = vec![
                "forget",
                "--host",
                &host,
                "--tag",
                "frood",
                "--prune",
                "--quiet",
            ];
            forget.extend(keep_args.iter().map(String::as_str));
            restic.run(&forget).await?;
            Ok(json!({
                "name": target.name,
                "ok": true,
                "snapshot": summary["snapshot_id"].as_str().map(|s| s.chars().take(8).collect::<String>()),
                "added_bytes": summary["data_added"],
            }))
        }
        .await;
        results.push(match outcome {
            Ok(value) => value,
            Err(err) => {
                let error = redact(&err, &all_secrets);
                tracing::warn!(target = %target.name, %error, "backup to a target failed");
                json!({ "name": target.name, "ok": false, "error": error })
            }
        });
    }
    clean(&app.work).await?;
    Ok(json!({
        "made_at": made_at,
        "dump_bytes": dump_bytes,
        "tables": counts.len(),
        "targets": results,
    }))
}

async fn verify(
    app: &App,
    installation: &str,
    database: &Database,
    targets: &[Target],
) -> Result<Value, Failure> {
    if !is_name(installation) {
        return Err(Failure::Request("installation is not a valid name".into()));
    }
    if targets.is_empty() {
        return Err(Failure::Request("no targets".into()));
    }
    let name = database_name(&database.url)?;
    let check_db = format!("{name}_verify");
    let check_url = with_database(&database.url, &check_db)?;
    let db_password = url_password(&database.url);
    let mut secrets: Vec<&str> = vec![database.url.as_str(), check_url.as_str()];
    if let Some(p) = &db_password {
        secrets.push(p);
    }
    let admin = connect(&database.url).await?;
    let mut results = Vec::new();
    for target in targets {
        let restic = Restic { target };
        let mut all_secrets = secrets.clone();
        all_secrets.extend(restic.secrets());
        let outcome =
            verify_target(app, installation, &restic, &admin, &check_db, &check_url).await;
        let _ = admin
            .batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                quote_ident(&check_db)
            ))
            .await;
        results.push(match outcome {
            Ok(value) => value,
            Err((snapshot, err)) => {
                let error = redact(&err, &all_secrets);
                tracing::warn!(target = %target.name, %error, "restore check failed");
                json!({ "name": target.name, "ok": false, "snapshot": snapshot, "error": error })
            }
        });
    }
    clean(&app.restore).await?;
    Ok(json!({ "targets": results }))
}

/// Restores a target's latest backup into a temporary database and compares it with its
/// manifest. On failure: the snapshot it got to, and why.
async fn verify_target(
    app: &App,
    installation: &str,
    restic: &Restic<'_>,
    admin: &tokio_postgres::Client,
    check_db: &str,
    check_url: &str,
) -> Result<Value, (Option<String>, String)> {
    let fail = |snapshot: Option<&str>, e: String| (snapshot.map(str::to_owned), e);
    let listed = restic
        .run_within(
            &[
                "snapshots",
                "--host",
                installation,
                "--tag",
                "frood",
                "--latest",
                "1",
                "--json",
            ],
            SHORT,
        )
        .await
        .map_err(|e| fail(None, e))?;
    let snapshots: Vec<Value> = serde_json::from_slice(&listed).unwrap_or_default();
    let Some(id) = snapshots
        .iter()
        .filter_map(|s| s["id"].as_str())
        .next_back()
        .map(str::to_owned)
    else {
        return Err(fail(None, "there is no backup in this target yet".into()));
    };
    let short: String = id.chars().take(8).collect();
    clean(&app.restore)
        .await
        .map_err(|_| fail(Some(&short), "cannot clean the restore folder".into()))?;
    restic
        .run(&[
            "restore",
            &format!("{id}:{}", app.work),
            "--target",
            &app.restore,
        ])
        .await
        .map_err(|e| fail(Some(&short), e))?;
    let manifest: Value = tokio::fs::read(Path::new(&app.restore).join("manifest.json"))
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| {
            fail(
                Some(&short),
                "the backup has no readable manifest.json".into(),
            )
        })?;
    for file in manifest["files"].as_array().into_iter().flatten() {
        let file = file.as_str().unwrap_or_default();
        let present = tokio::fs::metadata(Path::new(&app.restore).join(file))
            .await
            .is_ok_and(|m| m.len() > 0);
        if !present {
            return Err(fail(Some(&short), format!("{file} is missing or empty")));
        }
    }
    // Separately: several statements in one query run in a transaction, and CREATE DATABASE
    // cannot.
    for statement in [
        "DROP DATABASE IF EXISTS {db} WITH (FORCE)",
        "CREATE DATABASE {db}",
    ] {
        admin
            .batch_execute(&statement.replace("{db}", &quote_ident(check_db)))
            .await
            .map_err(|e| {
                fail(
                    Some(&short),
                    format!("cannot create {check_db}: {}", db_message(&e)),
                )
            })?;
    }
    run(
        "pg_restore",
        &[
            "--no-owner",
            "--exit-on-error",
            &format!("--dbname={check_url}"),
            &format!("{}/db.dump", app.restore),
        ],
        &[],
    )
    .await
    .map_err(|e| fail(Some(&short), e))?;
    let restored = connect(check_url)
        .await
        .map_err(|_| fail(Some(&short), format!("cannot connect to {check_db}")))?;
    let counts = count_rows(&restored)
        .await
        .map_err(|_| fail(Some(&short), format!("cannot count rows in {check_db}")))?;
    drop(restored);
    let expected: BTreeMap<String, i64> =
        serde_json::from_value(manifest["counts"].clone()).unwrap_or_default();
    let mismatches: Vec<String> = expected
        .iter()
        .filter_map(|(table, want)| match counts.get(table) {
            Some(got) if got == want => None,
            Some(got) => Some(format!("{table}: {got} rows, the backup says {want}")),
            None => Some(format!("{table} is missing")),
        })
        .collect();
    if !mismatches.is_empty() {
        return Err(fail(Some(&short), mismatches.join("; ")));
    }
    Ok(json!({
        "name": restic.target.name,
        "ok": true,
        "snapshot": short,
        "made_at": manifest["made_at"],
        "tables": expected.len(),
    }))
}

fn app_from_env() -> Result<App, String> {
    Ok(App {
        token: std::env::var("FROOD_TOKEN").map_err(|_| "FROOD_TOKEN is not set")?,
        busy: Mutex::new(()),
        work: std::env::var("BACKUP_WORK").unwrap_or_else(|_| WORK.to_owned()),
        restore: std::env::var("BACKUP_RESTORE").unwrap_or_else(|_| RESTORE.to_owned()),
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().json().init();
    let app = match app_from_env() {
        Ok(app) => app,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(2);
        }
    };
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind");
    tracing::info!(port, "backup-restic is listening");
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn app() -> Router {
        router(App {
            token: "t0ken".into(),
            busy: Mutex::new(()),
            work: "/nonexistent/work".into(),
            restore: "/nonexistent/restore".into(),
        })
    }

    async fn post(body: Value, token: &str) -> (StatusCode, Value) {
        let response = app()
            .oneshot(
                HttpRequest::post("/v1/capabilities/backup.target@1")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn only_the_core_may_call() {
        let (status, _) = post(json!({ "op": "verify" }), "wrong").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bad_requests_are_final() {
        let target = json!({ "name": "disk", "repository": "/repos/disk", "password": "pw" });
        let base = json!({
            "op": "backup", "installation": "personal", "database": { "url": "postgres://u:p@h/frood" },
            "targets": [target], "keep": { "daily": 7 },
        });
        let mut no_keep = base.clone();
        no_keep["keep"] = json!({});
        let (status, body) = post(no_keep, "t0ken").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["retryable"], false);
        let mut own_file = base.clone();
        own_file["files"] = json!([{ "name": "db.dump", "data_base64": "" }]);
        assert_eq!(post(own_file, "t0ken").await.0, StatusCode::BAD_REQUEST);
        let mut sneaky = base.clone();
        sneaky["files"] = json!([{ "name": "../etc/passwd", "data_base64": "" }]);
        assert_eq!(post(sneaky, "t0ken").await.0, StatusCode::BAD_REQUEST);
        let mut bad_name = base;
        bad_name["installation"] = json!("Personal; rm");
        assert_eq!(post(bad_name, "t0ken").await.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn secrets_never_leave_in_errors() {
        let text = "pg_dump: connection to postgres://frood:s3cretpw@postgres/frood failed";
        let out = redact(text, &["s3cretpw"]);
        assert!(!out.contains("s3cretpw"));
        assert_eq!(
            url_password("postgres://frood:s3cretpw@postgres:5432/frood").as_deref(),
            Some("s3cretpw")
        );
        assert_eq!(
            with_database("postgres://frood:pw@postgres:5432/frood", "frood_verify")
                .ok()
                .unwrap(),
            "postgres://frood:pw@postgres:5432/frood_verify"
        );
        assert!(database_name("postgres://h/").is_err());
    }
}
