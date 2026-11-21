//! "Frood stopped learning" (docs/33 S1): why nothing new came in for three working days, on
//! `/v1/now` and in the digest.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use tower::ServiceExt;

struct World {
    app: Router,
    dispatch: frood_dispatch::Dispatch,
    pool: frood_store::sqlx::PgPool,
    token: String,
}

/// An installation with the source `youtrack` and a `daily-sweep`, scheduled or not.
async fn world(name: &str, scheduled: bool) -> Option<World> {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return None;
    };
    let pool = frood_store::testing::fresh_database(&url, name)
        .await
        .expect("test database");
    let on = if scheduled {
        "    on: [{ schedule: '0 3 * * *', timezone: UTC, for_each: source }]\n"
    } else {
        ""
    };
    let raw = format!(
        "version: 1\ninstallation: {{name: test}}\nstorage: {{uses: postgres, url: 'postgres://unused', objects: {{uses: database}}}}\n\
         deploy: {{server: host}}\n\
         accounts:\n  claude: {{ kind: api_key, provider: anthropic, secret: sk-test }}\n\
         sources:\n  youtrack: {{ uses: mcp, server: 'http://yt:8080/mcp' }}\n\
         jobs:\n  extract: {{ enabled: false }}\n  digest: {{}}\n  daily-sweep:\n    uses: agents.execute\n{on}    \
         runtime: runtime\n    agent: claude-code\n    model: claude-haiku-5-5\n    account: claude\n    \
         instructions: Sweep the source.\n    budget: {{ max_usd: 0.5 }}\n\
         modules:\n  runtime: {{ url: 'http://127.0.0.1:1' }}\n"
    );
    let config = frood_config::parse(
        &raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap()
    .config;
    let assembly = frood_api::assemble(&config, std::path::Path::new("."), pool.clone(), |_| {
        Some("rt-token".to_owned())
    });
    assert!(assembly.warnings.is_empty(), "{:?}", assembly.warnings);
    let dispatch = assembly.state.dispatch.clone();
    let auth = frood_auth::Auth::new(pool.clone(), frood_auth::AuthSettings::from_config(&config));
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    frood_store::sqlx::query(
        "INSERT INTO channel_links (channel, external_id, user_id) VALUES ('telegram', '42', $1)",
    )
    .bind(alice)
    .execute(&pool)
    .await
    .unwrap();
    let (_, token) = auth
        .create_personal_token(alice, "test", &["knowledge:read".to_owned()], None)
        .await
        .unwrap();
    Some(World {
        app: frood_api::router(assembly.state),
        dispatch,
        pool,
        token,
    })
}

impl World {
    async fn get(&self, path: &str) -> Value {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn run(&self, status: &str, error: Option<&str>, at: DateTime<Utc>) -> uuid::Uuid {
        let id = uuid::Uuid::now_v7();
        frood_store::sqlx::query(
            "INSERT INTO runs (id, job, idempotency_key, subject, input, status, error, created_at, finished_at) \
             VALUES ($1, 'daily-sweep', $1::text, 'youtrack', '{}', $2, $3, $4, $4)",
        )
        .bind(id)
        .bind(status)
        .bind(error)
        .bind(at)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    /// Something new in the Ledger: a proposal, from whoever.
    async fn learn(&self, at: DateTime<Utc>) {
        frood_store::sqlx::query(
            "INSERT INTO proposals (id, author, risk, alternatives, state, created_at) \
             VALUES ($1, 'agent:daily-sweep', 'new', '[]', 'open', $2)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(at)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn digests(&self) -> Vec<Value> {
        self.dispatch
            .inbox(true, 50)
            .await
            .unwrap()
            .into_iter()
            .filter(|i| i.kind == "digest")
            .map(|i| i.data)
            .collect()
    }

    async fn send_digest(&self) {
        self.dispatch
            .start_manual("digest", &json!({}))
            .await
            .unwrap();
        self.dispatch.drain().await.unwrap();
    }
}

#[tokio::test]
async fn a_sweep_nobody_scheduled_is_told_on_now_and_in_a_digest_sent_once() {
    let Some(w) = world("stalled_unscheduled", false).await else {
        return;
    };
    let stalled = w.dispatch.stalled(Utc::now()).await.unwrap().unwrap();
    assert_eq!(stalled["reason"], "not_scheduled");
    assert_eq!(stalled["source"], "youtrack");
    assert_eq!(stalled["job"], "daily-sweep");
    assert_eq!(stalled["run_id"], Value::Null);

    // The Now page's banner.
    let now = w.get("/v1/now").await;
    assert_eq!(now["stalled"]["reason"], "not_scheduled");

    // Nothing else to say, and the digest still goes out with that line, once.
    w.send_digest().await;
    let digests = w.digests().await;
    assert_eq!(digests.len(), 1);
    assert_eq!(digests[0]["stalled"]["reason"], "not_scheduled");
    assert_eq!(digests[0]["important"], json!([]));
    w.send_digest().await;
    w.send_digest().await;
    assert_eq!(w.digests().await.len(), 1, "not again within three days");

    // Something new: the line is gone from Now.
    w.learn(Utc::now()).await;
    assert_eq!(w.get("/v1/now").await["stalled"], Value::Null);
}

#[tokio::test]
async fn a_scheduled_sweep_that_brings_nothing_says_why() {
    let Some(w) = world("stalled_scheduled", true).await else {
        return;
    };
    // Ten days on, so that the window is clear of what the test database wrote just now.
    let later = Utc::now() + Duration::days(10);
    let hours = |h: i64| later - Duration::hours(h);
    let reason = || async { w.dispatch.stalled(later).await.unwrap() };

    // Scheduled, but no runs.
    assert_eq!(reason().await.unwrap()["reason"], "not_run");
    // Ran, found nothing.
    w.run("succeeded", None, hours(5)).await;
    let found = reason().await.unwrap();
    assert_eq!(found["reason"], "found_nothing");
    // Failed.
    let failed = w
        .run("failed", Some("the agent crashed: out of memory"), hours(4))
        .await;
    let stalled = reason().await.unwrap();
    assert_eq!(stalled["reason"], "failed");
    assert_eq!(stalled["run_id"], json!(failed));
    assert_eq!(stalled["source"], "youtrack", "the one source it reads");
    // Failed because the source refused.
    let refused = w
        .run("failed", Some("youtrack: HTTP 403 Forbidden"), hours(3))
        .await;
    let stalled = reason().await.unwrap();
    assert_eq!(stalled["reason"], "access_denied");
    assert_eq!(stalled["source"], "youtrack");
    assert_eq!(stalled["run_id"], json!(refused));
    // The next run went well: back to a pass that found nothing.
    w.run("succeeded", None, hours(2)).await;
    assert_eq!(reason().await.unwrap()["reason"], "found_nothing");
    // Runs outside the window do not count: ten days earlier there is nothing.
    assert_eq!(
        w.dispatch
            .stalled(later + Duration::days(10))
            .await
            .unwrap()
            .unwrap()["reason"],
        "not_run"
    );

    // Something new came in: no line.
    w.learn(hours(1)).await;
    assert!(reason().await.is_none());
    // And three working days later, with nothing new since, it is back.
    assert!(
        w.dispatch
            .stalled(later + Duration::days(5))
            .await
            .unwrap()
            .is_some()
    );
}
