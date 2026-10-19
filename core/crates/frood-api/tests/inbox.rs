//! Steps 3–5 of the MVP scenario through the real core, with a fake Telegram module
//! (FROOD_TEST_DATABASE_URL; skipped without it): an agent's conflict becomes an Inbox item and
//! a Telegram message with buttons; a restart loses nothing; a button decides as the person,
//! the knowledge changes, and the message is edited; a decision made elsewhere edits it too.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use frood_ledger::model::{Actor, NewProposal, NewSource, Operation, Risk};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Default)]
struct Channel {
    sent: Vec<Value>,
    updated: Vec<Value>,
}

type Shared = Arc<Mutex<Channel>>;

/// A Telegram module as the core sees it: `notify.send@1` and `notify.update@1`.
async fn fake_telegram(channel: Shared) -> String {
    let app = Router::new()
        .route(
            "/manifest",
            get(|| async { Json(json!({ "name": "telegram", "version": "0.0.1", "provides": ["notify.send@1", "notify.update@1"], "frood_api": [] })) }),
        )
        .route(
            "/v1/capabilities/notify.send@1",
            post(|State(c): State<Shared>, headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer tg-token");
                let mut c = c.lock().unwrap();
                c.sent.push(body);
                Json(json!({ "message_ref": format!("42:{}", 900 + c.sent.len()) }))
            }),
        )
        .route(
            "/v1/capabilities/notify.update@1",
            post(|State(c): State<Shared>, Json(body): Json<Value>| async move {
                c.lock().unwrap().updated.push(body);
                Json(json!({}))
            }),
        )
        .with_state(channel);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn get_json(path: &str, token: &str) -> Request<Body> {
    Request::get(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn post_json(path: &str, token: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn config(fake: &str, attention: &str) -> frood_config::Config {
    config_with_jobs(fake, "", attention)
}

fn config_with_jobs(fake: &str, jobs: &str, rest: &str) -> frood_config::Config {
    let raw = format!(
        "version: 1\ninstallation: {{name: test}}\nstorage: {{uses: postgres, url: 'postgres://unused'}}\n\
         deploy: {{server: host}}\n\
         jobs:\n  extract: {{ enabled: false }}\n{jobs}\
         modules:\n  telegram: {{ url: '{fake}' }}\n{rest}"
    );
    frood_config::parse(
        &raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap()
    .config
}

struct Setup {
    app: Router,
    dispatch: frood_dispatch::Dispatch,
    ledger: frood_ledger::Ledger,
}

async fn setup(config: &frood_config::Config, pool: frood_store::sqlx::PgPool) -> Setup {
    let assembly = frood_api::assemble(config, std::path::Path::new("."), pool, |name| {
        (name == "FROOD_MODULE_TOKEN_TELEGRAM").then(|| "tg-token".to_owned())
    });
    assert!(assembly.warnings.is_empty(), "{:?}", assembly.warnings);
    assembly.modules.discover("telegram").await.unwrap();
    Setup {
        dispatch: assembly.state.dispatch.clone(),
        ledger: assembly.state.ledger.clone(),
        app: frood_api::router(assembly.state),
    }
}

/// What the nightly agent proposes: the release moves, against what Alice recorded.
async fn agents_conflict(
    ledger: &frood_ledger::Ledger,
    alice: uuid::Uuid,
    known: &Value,
    new_date: &str,
) -> uuid::Uuid {
    let agent = Actor::parse(&format!("agent:daily-sweep/for/{alice}")).unwrap();
    let quote = format!("Оля: релиз переносим на {new_date}");
    let captured = ledger
        .capture_source(
            &agent,
            NewSource {
                kind: "conversation".into(),
                origin: "capture".into(),
                uri: None,
                title: None,
                content: quote.clone(),
                said_at: None,
                metadata: json!({ "source": "team-chat" }),
            },
        )
        .await
        .unwrap();
    let evidence = captured.source.evidence[0].id;
    let operations: Vec<Operation> = serde_json::from_value(json!([{
        "op": "supersede", "assertion_id": known["assertion_id"],
        "assertion": { "kind": "commitment", "text": format!("Релиз 2.4 — {new_date}"), "modality": "reported", "evidence": [evidence] }
    }]))
    .unwrap();
    let proposal = ledger
        .open_proposal(
            &agent,
            NewProposal {
                idempotency_key: None,
                rationale: Some("#release, Olya".into()),
                risk: Risk::Conflict,
                confidence: None,
                base: vec![serde_json::from_value(json!({ "assertion_id": known["assertion_id"], "version": known["version"] })).unwrap()],
                operations: Some(operations),
                alternatives: None,
                evidence: vec![evidence],
            },
        )
        .await
        .unwrap();
    proposal.id
}

#[tokio::test]
async fn conflicts_reach_telegram_and_buttons_decide() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "inbox")
        .await
        .expect("test database");
    let channel = Shared::default();
    let fake = fake_telegram(channel.clone()).await;
    let config = config(&fake, "attention:\n  max_push_per_day: 2\n");
    let Setup {
        app,
        dispatch,
        ledger,
    } = setup(&config, pool.clone()).await;

    let auth = frood_auth::Auth::new(pool.clone(), frood_auth::AuthSettings::from_config(&config));
    auth.register_service_token(
        "telegram",
        "tg-token",
        &["knowledge:read".to_owned(), "channels:act".to_owned()],
    )
    .await
    .unwrap();
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    let scopes: Vec<String> = [
        "knowledge:read",
        "knowledge:capture",
        "knowledge:propose",
        "knowledge:decide",
        "admin",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let (_, pat) = auth
        .create_personal_token(alice, "test", &scopes, None)
        .await
        .unwrap();
    // Alice's Telegram account is linked.
    let (_, code) = call(
        &app,
        post_json("/v1/channels/telegram/link-codes", &pat, json!({})),
    )
    .await;
    let (status, _) = call(
        &app,
        post_json(
            "/v1/channels/telegram/links",
            "tg-token",
            json!({ "code": code["code"], "external_id": "42" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // What Alice recorded herself: accepted at once, nothing in the Inbox.
    let actor = Actor::parse(&format!("user:{alice}")).unwrap();
    let captured = ledger
        .capture_source(
            &actor,
            NewSource {
                kind: "note".into(),
                origin: "capture".into(),
                uri: None,
                title: None,
                content: "Договорились с Олей: релиз 2.4 — 14 ноября".into(),
                said_at: None,
                metadata: json!({}),
            },
        )
        .await
        .unwrap();
    let evidence = captured.source.evidence[0].id;
    let (status, agreed) = call(
        &app,
        post_json(
            "/v1/proposals",
            &pat,
            json!({
                "operations": [{ "op": "create", "assertion": { "kind": "commitment", "text": "Релиз 2.4 — 14 ноября", "modality": "agreed", "slots": { "due": "2026-11-14" }, "evidence": [evidence] } }],
                "rationale": "agreed with Olya", "risk": "new", "base": [], "evidence": [evidence],
            }),
        ),
    )
    .await;
    assert!(status.is_success(), "{agreed}");
    assert_eq!(agreed["state"], "accepted", "{agreed}");
    let (_, list) = call(&app, get_json("/v1/assertions?kind=commitment", &pat)).await;
    let known = list[0].clone();
    assert_eq!(
        dispatch.inbox(false, 50).await.unwrap().len(),
        0,
        "own facts need nobody"
    );

    // Step 3: the nightly agent finds the move; Alice gets a message with buttons.
    let first = agents_conflict(&ledger, alice, &known, "28 ноября").await;
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 1);
    let sent = channel.lock().unwrap().sent[0].clone();
    assert_eq!(sent["recipient"]["external_id"], "42");
    let item = &sent["item"];
    assert_eq!(item["item"]["kind"], "proposal");
    assert_eq!(item["item"]["urgency"], "now");
    assert_eq!(item["content"]["risk"], "conflict");
    let change = &item["content"]["alternatives"][0]["changes"][0];
    assert_eq!(change["before"], "Релиз 2.4 — 14 ноября");
    assert_eq!(change["after"], "Релиз 2.4 — 28 ноября");
    assert_eq!(
        item["content"]["quote"],
        "Оля: релиз переносим на 28 ноября"
    );
    let actions: Vec<&str> = item["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["accept", "reject", "defer"]);
    let item_id = item["item"]["id"].as_str().unwrap().to_owned();

    // Step 4: the server restarts; the item and its message are where they were.
    drop((app, dispatch));
    let Setup {
        app,
        dispatch,
        ledger,
    } = setup(&config, pool.clone()).await;
    assert_eq!(dispatch.deliver_once().await.unwrap(), 0, "shown once");
    let (_, inbox) = call(&app, get_json("/v1/inbox", &pat)).await;
    assert_eq!(inbox[0]["item"]["id"], item_id.as_str(), "{inbox}");
    assert_eq!(inbox[0]["for_me"], true);
    assert_eq!(inbox[0]["seen"], false);

    // Step 5: Alice presses «Принять» in Telegram: the bot acts as her.
    let (status, outcome) = call(
        &app,
        post_json(
            &format!("/v1/inbox/{item_id}/act"),
            "tg-token",
            json!({ "action": "accept", "alternative": "default", "external_id": "42" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["outcome"], "done");
    assert_eq!(outcome["item"]["state"], "resolved");
    let (_, proposal) = call(&app, get_json(&format!("/v1/proposals/{first}"), &pat)).await;
    assert_eq!(proposal["state"], "accepted");
    assert_eq!(
        proposal["verdicts"][0]["decided_by"],
        format!("user:{alice}/via/telegram")
    );
    let id = known["assertion_id"].as_str().unwrap();
    let (_, history) = call(
        &app,
        get_json(&format!("/v1/assertions/{id}/history"), &pat),
    )
    .await;
    let texts: Vec<(&str, &str)> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["text"].as_str().unwrap(), v["status"].as_str().unwrap()))
        .collect();
    assert!(
        texts.contains(&("Релиз 2.4 — 28 ноября", "active")),
        "{history}"
    );
    assert!(
        texts.contains(&("Релиз 2.4 — 14 ноября", "superseded")),
        "{history}"
    );
    // The message loses its buttons and says what happened.
    assert_eq!(dispatch.deliver_once().await.unwrap(), 1);
    let updated = channel.lock().unwrap().updated[0].clone();
    assert_eq!(updated["message_ref"], "42:901");
    assert_eq!(updated["item"]["item"]["resolution"]["action"], "accept");
    assert_eq!(updated["item"]["actions"], json!([]));
    // Pressing again shows the current state.
    let (_, again) = call(
        &app,
        post_json(
            &format!("/v1/inbox/{item_id}/act"),
            "tg-token",
            json!({ "action": "reject", "external_id": "42" }),
        ),
    )
    .await;
    assert_eq!(again["outcome"], "already_resolved");
    // A stranger's button does nothing.
    let (status, _) = call(
        &app,
        post_json(
            &format!("/v1/inbox/{item_id}/act"),
            "tg-token",
            json!({ "action": "accept", "external_id": "99" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Decided in another client: the Telegram message is edited too.
    let (_, current) = call(&app, get_json(&format!("/v1/assertions/{id}"), &pat)).await;
    let second = agents_conflict(&ledger, alice, &current, "5 декабря").await;
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 1);
    let (status, decided) = call(
        &app,
        post_json(
            &format!("/v1/proposals/{second}/verdicts"),
            &pat,
            json!({ "decision": "reject", "reason": "Olya was joking" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{decided}");
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 1);
    let updated = channel.lock().unwrap().updated[1].clone();
    assert_eq!(updated["item"]["item"]["resolution"]["action"], "reject");
    assert_eq!(
        updated["item"]["item"]["resolution"]["by"],
        format!("user:{alice}")
    );

    // The daily fuse (2 here): the third conflict waits in the Inbox.
    let (_, current) = call(&app, get_json(&format!("/v1/assertions/{id}"), &pat)).await;
    agents_conflict(&ledger, alice, &current, "12 декабря").await;
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 0);
    let (_, inbox) = call(&app, get_json("/v1/inbox", &pat)).await;
    assert_eq!(inbox.as_array().unwrap().len(), 1, "{inbox}");
}

#[tokio::test]
async fn quiet_hours_hold_pushes() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "inbox_quiet")
        .await
        .expect("test database");
    let channel = Shared::default();
    let fake = fake_telegram(channel.clone()).await;
    // Quiet all day long, whatever the clock says.
    let config = config(
        &fake,
        "attention:\n  people:\n    alice@example.com: { timezone: Europe/Amsterdam, quiet_hours: '00:00-23:59' }\n",
    );
    let Setup {
        dispatch, ledger, ..
    } = setup(&config, pool.clone()).await;
    let auth = frood_auth::Auth::new(pool.clone(), frood_auth::AuthSettings::from_config(&config));
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    sqlx_link(&pool, alice).await;
    let actor = Actor::parse(&format!("user:{alice}")).unwrap();
    let captured = ledger
        .capture_source(
            &actor,
            NewSource {
                kind: "note".into(),
                origin: "capture".into(),
                uri: None,
                title: None,
                content: "Релиз 2.4 — 14 ноября".into(),
                said_at: None,
                metadata: json!({}),
            },
        )
        .await
        .unwrap();
    let evidence = captured.source.evidence[0].id;
    let operations: Vec<Operation> = serde_json::from_value(json!([{ "op": "create", "assertion": {
        "kind": "commitment", "text": "Релиз 2.4 — 14 ноября", "modality": "agreed", "evidence": [evidence] } }]))
    .unwrap();
    ledger
        .open_proposal(
            &actor,
            NewProposal {
                idempotency_key: None,
                rationale: None,
                risk: Risk::New,
                confidence: None,
                base: vec![],
                operations: Some(operations),
                alternatives: None,
                evidence: vec![evidence],
            },
        )
        .await
        .unwrap();
    let known = ledger
        .assertions(&frood_ledger::model::AssertionFilter {
            kind: Some("commitment".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let known = serde_json::to_value(&known[0]).unwrap();
    agents_conflict(&ledger, alice, &known, "28 ноября").await;
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 0);
    assert!(channel.lock().unwrap().sent.is_empty(), "quiet hours");
    assert_eq!(dispatch.inbox(false, 10).await.unwrap().len(), 1);
}

/// Links Alice's Telegram account directly.
async fn sqlx_link(pool: &frood_store::sqlx::PgPool, alice: uuid::Uuid) {
    frood_store::sqlx::query(
        "INSERT INTO channel_links (channel, external_id, user_id) VALUES ('telegram', '42', $1)",
    )
    .bind(alice)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn due_dates_remind_as_each_person_configured() {
    use chrono::TimeZone;
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "inbox_reminders")
        .await
        .expect("test database");
    let channel = Shared::default();
    let fake = fake_telegram(channel.clone()).await;
    let config = config(
        &fake,
        "attention:\n  people:\n    alice@example.com:\n      timezone: Europe/Amsterdam\n      \
         reminders: [{ days: -1, at: '09:00' }, { days: 0, at: '09:00' }, { days: 0, at: '18:00' }, { days: 1, at: '09:00' }]\n",
    );
    let Setup {
        dispatch, ledger, ..
    } = setup(&config, pool.clone()).await;
    let auth = frood_auth::Auth::new(pool.clone(), frood_auth::AuthSettings::from_config(&config));
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    let actor = Actor::parse(&format!("user:{alice}/via/telegram")).unwrap();
    let commit = |text: &'static str, due: &'static str| {
        let ledger = ledger.clone();
        let actor = actor.clone();
        async move {
            let captured = ledger
                .capture_source(
                    &actor,
                    NewSource {
                        kind: "note".into(),
                        origin: "capture".into(),
                        uri: None,
                        title: None,
                        content: text.into(),
                        said_at: None,
                        metadata: json!({}),
                    },
                )
                .await
                .unwrap();
            let evidence = captured.source.evidence[0].id;
            let operations: Vec<Operation> = serde_json::from_value(json!([{ "op": "create", "assertion": {
                "kind": "commitment", "text": text, "modality": "agreed", "slots": { "due": due }, "evidence": [evidence] } }]))
            .unwrap();
            let proposal = ledger
                .open_proposal(
                    &actor,
                    NewProposal {
                        idempotency_key: None,
                        rationale: None,
                        risk: Risk::New,
                        confidence: None,
                        base: vec![],
                        operations: Some(operations),
                        alternatives: None,
                        evidence: vec![evidence],
                    },
                )
                .await
                .unwrap();
            assert_eq!(serde_json::to_value(proposal.state).unwrap(), "accepted");
        }
    };
    commit("Сдать отчёт", "2026-11-14").await;
    commit("Позвонить Оле", "2026-11-14").await;
    let reminders = |dispatch: frood_dispatch::Dispatch| async move {
        let mut out: Vec<(String, String)> = dispatch
            .inbox(false, 50)
            .await
            .unwrap()
            .into_iter()
            .filter(|i| i.kind == "reminder")
            .map(|i| {
                (
                    i.data["text"].as_str().unwrap().to_owned(),
                    i.subject.split('/').nth(3).unwrap().to_owned(),
                )
            })
            .collect();
        out.sort();
        out
    };

    // Two days before: nothing yet.
    let at = |d, h| chrono::Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();
    assert_eq!(dispatch.remind_once(at(12, 10)).await.unwrap(), 0);
    // The day before at 9:00 Amsterdam (8:00 UTC in November): made when its time comes.
    assert_eq!(
        dispatch.remind_once(at(13, 6)).await.unwrap(),
        0,
        "too early"
    );
    assert_eq!(dispatch.remind_once(at(13, 8)).await.unwrap(), 2);
    assert_eq!(
        dispatch.remind_once(at(13, 9)).await.unwrap(),
        0,
        "once each"
    );
    let items = dispatch.inbox(false, 50).await.unwrap();
    assert_eq!(items[0].notify_after, at(13, 8));
    assert_eq!(items[0].for_users, [alice]);
    assert_eq!(items[0].data["days"], -1);
    // On the day, twice: at 9:00 and at 18:00; each closes the one before.
    assert_eq!(dispatch.remind_once(at(14, 8)).await.unwrap(), 2);
    assert_eq!(
        reminders(dispatch.clone()).await,
        [
            ("Позвонить Оле".into(), "0@09:00".into()),
            ("Сдать отчёт".into(), "0@09:00".into())
        ]
    );
    assert_eq!(dispatch.remind_once(at(14, 17)).await.unwrap(), 2);
    assert_eq!(
        reminders(dispatch.clone()).await,
        [
            ("Позвонить Оле".into(), "0@18:00".into()),
            ("Сдать отчёт".into(), "0@18:00".into())
        ]
    );

    // «Выполнено» closes the report; «Перенести» moves the call a week.
    let items = dispatch.inbox(false, 50).await.unwrap();
    let report = items
        .iter()
        .find(|i| i.data["text"] == "Сдать отчёт")
        .unwrap();
    let call_olya = items
        .iter()
        .find(|i| i.data["text"] == "Позвонить Оле")
        .unwrap();
    let act = |action: &str| frood_dispatch::Act {
        action: action.into(),
        ..Default::default()
    };
    let me = format!("user:{alice}/via/telegram");
    dispatch
        .inbox_act(report.id, alice, &me, &act("done"))
        .await
        .unwrap();
    dispatch
        .inbox_act(call_olya.id, alice, &me, &act("postpone"))
        .await
        .unwrap();
    dispatch.inbox_pending().await.unwrap();
    assert!(reminders(dispatch.clone()).await.is_empty());
    let all = ledger
        .assertions(&frood_ledger::model::AssertionFilter {
            kind: Some("commitment".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let call_now = all.iter().find(|a| a.text == "Позвонить Оле").unwrap();
    assert_eq!(call_now.slots["due"], "2026-11-21");
    let report_now = all.iter().find(|a| a.text == "Сдать отчёт").unwrap();
    assert_eq!(
        serde_json::to_value(report_now.status).unwrap(),
        "completed"
    );
    // The day after: the report is done and the call moved, so nothing reminds.
    assert_eq!(dispatch.remind_once(at(15, 8)).await.unwrap(), 0);
}

#[tokio::test]
async fn digests_and_investigations() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "inbox_digest")
        .await
        .expect("test database");
    let channel = Shared::default();
    let fake = fake_telegram(channel.clone()).await;
    // Conflicts wait for an investigation; the investigator here cannot start (its runtime
    // gives no agents), so the item waits for max_wait or the run's end.
    let config = config_with_jobs(
        &fake,
        "  digest: {}\n  investigate:\n    runtime: telegram\n    agent: claude-code\n    model: m\n    account: claude\n    budget: { max_usd: 0.1 }\n",
        "accounts:\n  claude: { kind: api_key, provider: anthropic, secret: sk-test }\n\
         attention:\n  before_notify:\n    - { when: { kind: proposal, risk: conflict }, run: investigate, max_wait: 30m }\n",
    );
    let Setup {
        app,
        dispatch,
        ledger,
    } = setup(&config, pool.clone()).await;
    let auth = frood_auth::Auth::new(pool.clone(), frood_auth::AuthSettings::from_config(&config));
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    sqlx_link(&pool, alice).await;
    let (_, pat) = auth
        .create_personal_token(alice, "test", &["knowledge:read".to_owned()], None)
        .await
        .unwrap();
    let actor = Actor::parse(&format!("user:{alice}")).unwrap();
    let captured = ledger
        .capture_source(
            &actor,
            NewSource {
                kind: "note".into(),
                origin: "capture".into(),
                uri: None,
                title: None,
                content: "Релиз 2.4 — 14 ноября".into(),
                said_at: None,
                metadata: json!({}),
            },
        )
        .await
        .unwrap();
    let evidence = captured.source.evidence[0].id;
    let operations: Vec<Operation> = serde_json::from_value(json!([{ "op": "create", "assertion": {
        "kind": "commitment", "text": "Релиз 2.4 — 14 ноября", "modality": "agreed", "evidence": [evidence] } }]))
    .unwrap();
    ledger
        .open_proposal(
            &actor,
            NewProposal {
                idempotency_key: None,
                rationale: None,
                risk: Risk::New,
                confidence: None,
                base: vec![],
                operations: Some(operations),
                alternatives: None,
                evidence: vec![evidence],
            },
        )
        .await
        .unwrap();
    let known = ledger
        .assertions(&frood_ledger::model::AssertionFilter {
            kind: Some("commitment".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let known = serde_json::to_value(&known[0]).unwrap();
    agents_conflict(&ledger, alice, &known, "28 ноября").await;
    dispatch.inbox_pending().await.unwrap();
    let item = dispatch.inbox(false, 10).await.unwrap().remove(0);
    let investigation = item.investigation.clone().unwrap();
    assert_eq!(investigation["state"], "running");
    assert!(item.notify_after > chrono::Utc::now() + chrono::Duration::minutes(29));
    assert_eq!(
        dispatch.deliver_once().await.unwrap(),
        0,
        "waits for the investigation"
    );
    // The investigation ends: Alice is told now.
    ledger
        .append_event(
            "agent:investigate",
            "run.failed",
            format!("run/{}", investigation["run_id"].as_str().unwrap()),
            json!({ "run_id": investigation["run_id"], "job": "investigate" }),
        )
        .await
        .unwrap();
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 1);
    let sent = channel.lock().unwrap().sent[0].clone();
    assert_eq!(sent["item"]["item"]["investigation"]["state"], "failed");
    assert_eq!(
        dispatch.inbox(false, 10).await.unwrap().len(),
        1,
        "the investigator's failure is not an alert of its own"
    );

    // What is new: the accepted fact, the conflict waiting.
    let (status, digest) = call(&app, get_json("/v1/digest", &pat)).await;
    assert_eq!(status, StatusCode::OK, "{digest}");
    assert_eq!(digest["accepted"], json!(["Релиз 2.4 — 14 ноября"]));
    assert_eq!(digest["waiting"]["count"], 1);
    assert_eq!(
        digest["waiting"]["items"][0]["what"],
        "Релиз 2.4 — 28 ноября"
    );
    // The digest job puts it into the Inbox and moves the checkpoint.
    dispatch.start_manual("digest", &json!({})).await.unwrap();
    dispatch.drain().await.unwrap();
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 1);
    let sent = channel.lock().unwrap().sent[1].clone();
    assert_eq!(sent["item"]["item"]["kind"], "digest");
    assert_eq!(
        sent["item"]["content"]["accepted"][0],
        "Релиз 2.4 — 14 ноября"
    );
    let (_, again) = call(&app, get_json("/v1/digest", &pat)).await;
    assert_eq!(again["accepted"], json!([]), "since the last digest");
}
