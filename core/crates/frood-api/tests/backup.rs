//! Backups through the real core with a fake backup module (FROOD_TEST_DATABASE_URL; skipped
//! without it): what the module is given — the secrets only encrypted to the recovery key —,
//! a target failing fails the run, and the detectors raise and clear backup alerts.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde_json::{Value, json};

#[derive(Default)]
struct Fake {
    calls: Vec<Value>,
    /// Targets that fail, by name.
    failing: Vec<String>,
}

type Shared = Arc<Mutex<Fake>>;

/// A backup module as the core sees it: `backup.target@1`.
async fn fake_backup(fake: Shared) -> String {
    let app = Router::new()
        .route(
            "/manifest",
            get(|| async { Json(json!({ "name": "backup-restic", "version": "0.0.1", "provides": ["backup.target@1"], "frood_api": [] })) }),
        )
        .route("/healthz", get(|| async { Json(json!({ "status": "ok" })) }))
        .route(
            "/v1/capabilities/backup.target@1",
            post(|State(f): State<Shared>, headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer backup-token");
                let mut f = f.lock().unwrap();
                let targets: Vec<Value> = body["targets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| {
                        let name = t["name"].as_str().unwrap().to_owned();
                        if f.failing.contains(&name) {
                            json!({ "name": name, "ok": false, "error": "403 Forbidden" })
                        } else {
                            json!({ "name": name, "ok": true, "snapshot": "4f2a9c1e" })
                        }
                    })
                    .collect();
                f.calls.push(body);
                Json(json!({ "made_at": chrono::Utc::now(), "tables": 30, "targets": targets }))
            }),
        )
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn backups_seal_the_secrets_and_alert_when_they_stop() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "backup_jobs")
        .await
        .expect("test database");
    let fake = Shared::default();
    let module = fake_backup(fake.clone()).await;
    let identity = age::x25519::Identity::generate();
    let raw = format!(
        "version: 1\ninstallation: {{name: test}}\n\
         storage: {{uses: postgres, url: 'postgres://frood:dbpw@127.0.0.1:5432/frood'}}\n\
         deploy: {{server: host}}\n\
         jobs:\n  extract: {{ enabled: false }}\n\
         \x20 backup: {{ on: [{{ schedule: '0 */6 * * *', timezone: Europe/Amsterdam }}] }}\n\
         \x20 backup-verify: {{ on: [{{ schedule: '0 4 * * 0', timezone: Europe/Amsterdam }}] }}\n\
         modules:\n  backup: {{ url: '{module}' }}\n\
         backup:\n  recovery_key: {}\n  targets:\n\
         \x20   disk: {{ repository: /repos/disk, password: disk-pw }}\n\
         \x20   b2: {{ repository: 's3:https://s3.example.com/b', password: b2-pw, env: {{ AWS_ACCESS_KEY_ID: key-id }} }}\n\
         \x20 keep: {{ daily: 30, monthly: 12 }}\n",
        identity.to_public()
    );
    let config = frood_config::parse(
        &raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap()
    .config;
    let assembly = frood_api::assemble(&config, std::path::Path::new("."), pool.clone(), |name| {
        (name == "FROOD_MODULE_TOKEN_BACKUP").then(|| "backup-token".to_owned())
    });
    assert!(assembly.warnings.is_empty(), "{:?}", assembly.warnings);
    assembly.modules.discover("backup").await.unwrap();
    let secrets = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        secrets.path(),
        "POSTGRES_PASSWORD=dbpw\nTELEGRAM_BOT_TOKEN=123:abc\n",
    )
    .unwrap();
    let dispatch = assembly
        .state
        .dispatch
        .clone()
        .with_backup_files(raw.clone(), secrets.path().to_owned());

    // A backup by hand.
    let run = dispatch.start_manual("backup", &json!({})).await.unwrap();
    dispatch.work_one().await.unwrap();
    let done = dispatch.run(run.id).await.unwrap().unwrap();
    assert_eq!(done.status, "succeeded", "{:?}", done.error);
    let call = fake.lock().unwrap().calls[0].clone();
    assert_eq!(call["op"], "backup");
    assert_eq!(call["installation"], "test");
    assert_eq!(
        call["database"]["url"], "postgres://frood:dbpw@postgres:5432/frood",
        "the module reaches the database inside the deployment"
    );
    assert_eq!(call["keep"], json!({ "daily": 30, "monthly": 12 }));
    assert_eq!(call["targets"][0]["name"], "b2");
    assert_eq!(call["targets"][0]["env"]["AWS_ACCESS_KEY_ID"], "key-id");
    let files: Vec<(String, Vec<u8>)> = call["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap().to_owned(),
                base64::engine::general_purpose::STANDARD
                    .decode(f["data_base64"].as_str().unwrap())
                    .unwrap(),
            )
        })
        .collect();
    assert_eq!(files[0].0, "frood.yaml");
    assert_eq!(files[0].1, raw.as_bytes());
    assert_eq!(files[1].0, "secrets.env.age");
    let sealed = &files[1].1;
    assert!(
        !String::from_utf8_lossy(sealed).contains("123:abc"),
        "the module never sees the secrets in the clear"
    );
    assert_eq!(
        age::decrypt(&identity, sealed).unwrap(),
        b"POSTGRES_PASSWORD=dbpw\nTELEGRAM_BOT_TOKEN=123:abc\n"
    );
    assert!(call["about"]["schema_version"].as_i64().unwrap() >= 6);

    // A target failing fails the run, and says which.
    fake.lock().unwrap().failing = vec!["b2".into()];
    let run = dispatch
        .start_manual("backup-verify", &json!({}))
        .await
        .unwrap();
    dispatch.work_one().await.unwrap();
    let failed = dispatch.run(run.id).await.unwrap().unwrap();
    assert_eq!(failed.status, "failed");
    let error = failed.error.unwrap();
    assert!(
        error.contains("b2: 403 Forbidden") && !error.contains("disk"),
        "{error}"
    );

    let alerts = |dispatch: frood_dispatch::Dispatch| async move {
        let mut keys: Vec<String> = dispatch
            .inbox(false, 50)
            .await
            .unwrap()
            .into_iter()
            .filter(|i| i.kind == "alert" && i.data.get("key").is_some())
            .map(|i| i.data["key"].as_str().unwrap().to_owned())
            .collect();
        keys.sort();
        keys
    };
    let now = chrono::Utc::now();
    dispatch.detect_once(now).await.unwrap();
    assert_eq!(alerts(dispatch.clone()).await, ["backup/verify"]);
    // Every six hours: nothing for two intervals is missed.
    dispatch
        .detect_once(now + chrono::Duration::hours(13))
        .await
        .unwrap();
    assert_eq!(
        alerts(dispatch.clone()).await,
        ["backup/missed", "backup/verify"]
    );
    let health = dispatch.health().await.unwrap();
    let backups = health.backups.unwrap();
    assert_eq!(backups.targets, ["b2", "disk"]);
    assert_eq!(backups.last_verify.unwrap().status, "failed");

    // Fixed: a check and a backup succeed, the alerts close themselves.
    fake.lock().unwrap().failing.clear();
    for job in ["backup-verify", "backup"] {
        dispatch.start_manual(job, &json!({})).await.unwrap();
        dispatch.work_one().await.unwrap();
    }
    dispatch.detect_once(now).await.unwrap();
    assert!(alerts(dispatch.clone()).await.is_empty());
}
