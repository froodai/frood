//! The MVP scenario, steps 1–5, in one go through the real core and a real PostgreSQL
//! (FROOD_TEST_DATABASE_URL; skipped without it). What lies outside Frood is played by fakes
//! that keep to the module contracts: Telegram, speech to text, the model, the agent runtime
//! and the agent itself (E1). Step 6 — backup and restore — runs in Docker (deploy/e2e).
//!
//! 1. A voice message in Telegram: «Договорились с Олей: релиз 2.4 — 14 ноября».
//! 2. Another client asks over MCP what is due in November: the answer cites the assertion and
//!    the source.
//! 3. The nightly agent reads the team chat, finds the release moved, proposes a conflict; it
//!    reaches the Inbox and Telegram with buttons.
//! 4. The server restarts: the proposal and the Inbox item are where they were.
//! 5. A button in Telegram decides: the old version is superseded, the new one active, and any
//!    client sees the history.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tower::ServiceExt;

const SAID: &str = "Договорились с Олей: релиз 2.4 — 14 ноября";
const IN_CHAT: &str = "Оля: релиз переносим на 28.11";

// ---------- what lies outside Frood ----------

/// The model's answer to extraction: the release date as an agreed commitment with Olya.
fn extraction() -> Value {
    json!({
        "entities": [{ "ref": "olya", "name": "Оля", "type": "person", "existing_id": null, "aliases": ["Олей"] }],
        "changes": [{
            "action": "create", "kind": "commitment", "text": "Релиз 2.4 — 14 ноября",
            "subjects": ["olya"], "slots": [{ "key": "due", "value": "2026-11-14" }, { "key": "owner", "value": "olya" }],
            "modality": "agreed", "valid_from": null, "target_assertion_id": null, "close_status": null,
            "conflict": false, "confidence": 0.9, "quote": "релиз 2.4 — 14 ноября",
            "reason": "An agreed release date"
        }]
    })
}

/// Embeddings by concept, so a question in other words lands next to the note.
fn embedding(text: &str) -> Vec<f32> {
    let text = text.to_lowercase();
    let concepts: [&[&str]; 4] = [
        &["релиз", "release"],
        &["ноябр", "november"],
        &["срок", "deadline", "due"],
        &["ол", "olya"],
    ];
    let mut v: Vec<f32> = concepts
        .iter()
        .map(|words| f32::from(u8::from(words.iter().any(|w| text.contains(w)))))
        .collect();
    v.push(0.1);
    v
}

/// Anthropic, OpenAI and the speech module on one fake server.
async fn providers() -> String {
    let app = Router::new()
        .route(
            "/v1/messages",
            post(|Json(body): Json<Value>| async move {
                Json(json!({
                    "model": body["model"], "stop_reason": "end_turn",
                    "content": [{ "type": "text", "text": extraction().to_string() }],
                    "usage": { "input_tokens": 100, "output_tokens": 50 }
                }))
            }),
        )
        .route(
            "/v1/embeddings",
            post(|Json(body): Json<Value>| async move {
                let data: Vec<Value> = body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .map(|(i, t)| json!({ "object": "embedding", "index": i, "embedding": embedding(t.as_str().unwrap()) }))
                    .collect();
                Json(json!({ "object": "list", "data": data, "model": body["model"] }))
            }),
        )
        .route(
            "/manifest",
            get(|| async { Json(json!({ "name": "stt-fake", "version": "0.1.0", "provides": ["media.transcribe@1"], "frood_api": [] })) }),
        )
        .route(
            "/v1/capabilities/media.transcribe@1",
            post(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer stt-token");
                Json(json!({ "text": SAID, "language": "ru" }))
            }),
        );
    serve(app).await
}

#[derive(Default)]
struct Telegram {
    sent: Vec<Value>,
    updated: Vec<Value>,
}

async fn telegram(shared: Arc<Mutex<Telegram>>) -> String {
    let app = Router::new()
        .route(
            "/manifest",
            get(|| async { Json(json!({ "name": "telegram", "version": "0.0.1", "provides": ["notify.send@1", "notify.update@1"], "frood_api": [] })) }),
        )
        .route("/healthz", get(|| async { Json(json!({ "status": "ok" })) }))
        .route(
            "/v1/capabilities/notify.send@1",
            post(|State(t): State<Arc<Mutex<Telegram>>>, Json(body): Json<Value>| async move {
                let mut t = t.lock().unwrap();
                t.sent.push(body);
                Json(json!({ "message_ref": format!("42:{}", 900 + t.sent.len()) }))
            }),
        )
        .route(
            "/v1/capabilities/notify.update@1",
            post(|State(t): State<Arc<Mutex<Telegram>>>, Json(body): Json<Value>| async move {
                t.lock().unwrap().updated.push(body);
                Json(json!({}))
            }),
        )
        .with_state(shared);
    serve(app).await
}

/// Runs the agent runtime pretends to run, by run id: their state.
type Runs = Arc<Mutex<Vec<(String, Value, String)>>>;

/// `agents.execute@1`: remembers each start (with the run's MCP servers) and reports the state
/// the test gives the run.
async fn runtime(runs: Runs) -> String {
    let app = Router::new()
        .route(
            "/manifest",
            get(|| async { Json(json!({ "name": "runtime-fake", "version": "0.1.0", "provides": ["agents.execute@1"], "frood_api": [] })) }),
        )
        .route(
            "/v1/capabilities/agents.execute@1",
            post(|State(runs): State<Runs>, Json(body): Json<Value>| async move {
                let id = body["run_id"].as_str().unwrap().to_owned();
                let mut runs = runs.lock().unwrap();
                Json(match body["op"].as_str().unwrap() {
                    "start" => {
                        runs.push((id, body.clone(), "running".into()));
                        json!({ "session_ref": "session-1", "state": "running" })
                    }
                    "status" => {
                        let state = runs.iter().find(|r| r.0 == id).map_or("running".into(), |r| r.2.clone());
                        json!({ "state": state, "session_ref": "session-1", "cost_usd": 0.01 })
                    }
                    "events" => json!({ "events": [] }),
                    _ => json!({}),
                })
            }),
        )
        .with_state(runs);
    serve(app).await
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

// ---------- calling Frood ----------

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

/// A tool call over MCP, as Claude, ChatGPT or an agent makes it; the tool's JSON answer.
async fn tool(app: &Router, token: &str, name: &str, arguments: Value) -> Value {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
    let request = Request::post("/mcp")
        .header(header::HOST, "frood:8080")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, reply) = call(app, request).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_ne!(reply["result"]["isError"], true, "{name}: {reply}");
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

/// A voice message as the Telegram module sends it: the file and the sender's account.
fn voice(external_id: &str) -> Request<Body> {
    let boundary = "frood-scenario";
    let mut body = Vec::new();
    for (name, value) in [("external_id", external_id), ("kind", "voice")] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"voice.oga\"\r\nContent-Type: audio/ogg\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(b"OggS\x00\x02fake-opus-frames");
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Request::post("/v1/capture")
        .header(header::AUTHORIZATION, "Bearer tg-token")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

struct Server {
    app: Router,
    dispatch: frood_dispatch::Dispatch,
}

/// The core as `frood serve` assembles it: config, modules found, the bot's token registered.
async fn start(raw: &str, pool: frood_store::sqlx::PgPool) -> Server {
    let config = frood_config::parse(
        raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap()
    .config;
    let assembly = frood_api::assemble(
        &config,
        std::path::Path::new("."),
        pool,
        |name| match name {
            "FROOD_MODULE_TOKEN_STT" => Some("stt-token".into()),
            "FROOD_MODULE_TOKEN_TELEGRAM" => Some("tg-token".into()),
            "FROOD_MODULE_TOKEN_RUNTIME" => Some("rt-token".into()),
            _ => None,
        },
    );
    assert!(assembly.warnings.is_empty(), "{:?}", assembly.warnings);
    for module in ["stt", "telegram", "runtime"] {
        assembly.modules.discover(module).await.unwrap();
    }
    assembly
        .state
        .auth
        .register_service_token(
            "telegram",
            "tg-token",
            &[
                "knowledge:read".to_owned(),
                "knowledge:capture".to_owned(),
                "channels:act".to_owned(),
            ],
        )
        .await
        .unwrap();
    Server {
        dispatch: assembly.state.dispatch.clone(),
        app: frood_api::router(assembly.state),
    }
}

#[tokio::test]
async fn the_mvp_scenario() {
    use chrono::TimeZone;
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "scenario")
        .await
        .expect("test database");
    let providers = providers().await;
    let bot = Arc::new(Mutex::new(Telegram::default()));
    let telegram = telegram(bot.clone()).await;
    let runs = Runs::default();
    let runtime = runtime(runs.clone()).await;
    let raw = format!(
        "version: 1\ninstallation: {{name: scenario}}\nstorage: {{uses: postgres, url: 'postgres://unused'}}\n\
         deploy: {{server: host}}\n\
         accounts:\n  claude: {{ kind: api_key, provider: anthropic, secret: test, base_url: '{providers}' }}\n  \
         openai: {{ kind: api_key, provider: openai, secret: test, base_url: '{providers}' }}\n\
         search:\n  embeddings: {{ account: openai, model: text-embedding-3-small }}\n\
         sources:\n  team-chat: {{ uses: mcp, server: 'http://team-chat:8080/mcp', scope: {{ channels: ['#release'] }}, tools: [search_messages] }}\n\
         jobs:\n  extract: {{ with: {{ account: claude, model: claude-haiku-5-5 }} }}\n  daily-sweep:\n    \
         on: [{{ schedule: '0 3 * * *', timezone: Europe/Amsterdam, for_each: source }}]\n    \
         runtime: runtime\n    agent: claude-code\n    model: claude-haiku-5-5\n    account: claude\n    \
         on_behalf_of: alice@example.com\n    budget: {{ max_usd: 0.5 }}\n\
         modules:\n  stt: {{ url: '{providers}' }}\n  telegram: {{ url: '{telegram}' }}\n  runtime: {{ url: '{runtime}' }}\n"
    );
    let Server { app, dispatch } = start(&raw, pool.clone()).await;

    // Alice signs in once (a personal token stands in for the browser sign-in) and links her
    // Telegram account with a one-time code.
    let auth = frood_auth::Auth::new(
        pool.clone(),
        frood_auth::AuthSettings::from_config(
            &frood_config::parse(
                &raw,
                &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
            )
            .unwrap()
            .config,
        ),
    );
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    let scopes: Vec<String> = [
        "knowledge:read",
        "knowledge:capture",
        "knowledge:propose",
        "knowledge:decide",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let (_, client) = auth
        .create_personal_token(alice, "another client", &scopes, None)
        .await
        .unwrap();
    // The code comes from the admin panel, which signs in with `admin`.
    let (_, panel) = auth
        .create_personal_token(alice, "admin panel", &["admin".to_owned()], None)
        .await
        .unwrap();
    let (status, code) = call(
        &app,
        post_json("/v1/channels/telegram/link-codes", &panel, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{code}");
    let (status, linked) = call(
        &app,
        post_json(
            "/v1/channels/telegram/links",
            "tg-token",
            json!({ "code": code["code"], "external_id": "42" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{linked}");

    // 1. A voice message in Telegram becomes knowledge: transcribed, extracted, and — Alice's
    //    own new fact — accepted.
    let (status, captured) = call(&app, voice("42")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{captured}");
    let voice_source = captured["source_id"].as_str().unwrap().to_owned();
    dispatch.drain().await.unwrap();
    let (_, processed) = call(
        &app,
        get_json(&format!("/v1/sources/{voice_source}/status"), &client),
    )
    .await;
    assert_eq!(processed["state"], "done", "{processed}");
    assert_eq!(processed["proposals"][0]["state"], "accepted");
    assert!(
        bot.lock().unwrap().sent.is_empty(),
        "nobody is bothered with their own fact"
    );

    // 2. Another client asks in other words: the answer cites the assertion and its source.
    let hits = tool(
        &app,
        &client,
        "search_knowledge",
        json!({ "query": "какие сроки в ноябре?" }),
    )
    .await;
    let known = hits[0].clone();
    assert_eq!(known["text"], "Релиз 2.4 — 14 ноября", "{hits}");
    assert_eq!(known["slots"]["due"], "2026-11-14");
    let cited = &known["sources"][0];
    assert_eq!(cited["source_id"], voice_source.as_str(), "{known}");
    assert_eq!(cited["captured_by"], format!("user:{alice}/via/telegram"));
    assert_eq!(cited["quote"], "релиз 2.4 — 14 ноября");
    let due = tool(
        &app,
        &client,
        "list_assertions",
        json!({ "kind": "commitment", "due_from": "2026-11-01", "due_to": "2026-12-01" }),
    )
    .await;
    assert_eq!(due.as_array().unwrap().len(), 1, "{due}");

    // 3. At night the agent reads the team chat for Alice: the release moved. It finds what
    //    Frood knows and proposes the change; a contradiction waits for a person.
    // The schedule starts counting at its first tick; the next night it fires.
    let day = chrono::Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap();
    assert_eq!(dispatch.tick_schedules(day).await.unwrap(), 0);
    let night = chrono::Utc.with_ymd_and_hms(2026, 11, 10, 2, 0, 0).unwrap();
    assert_eq!(dispatch.tick_schedules(night).await.unwrap(), 1);
    dispatch.drain().await.unwrap();
    let started = runs.lock().unwrap()[0].1.clone();
    assert_eq!(started["job"], "daily-sweep");
    let frood = started["mcp_servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "frood")
        .unwrap()
        .clone();
    let agent = frood["headers"]["Authorization"]
        .as_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .to_owned();
    let seen = tool(
        &app,
        &agent,
        "search_knowledge",
        json!({ "query": "релиз 2.4" }),
    )
    .await;
    assert_eq!(seen[0]["assertion_id"], known["assertion_id"]);
    let proposed = tool(
        &app,
        &agent,
        "propose_change",
        json!({
            "operations": [{ "op": "supersede", "assertion_id": known["assertion_id"], "assertion": { "kind": "commitment", "text": "Релиз 2.4 — 28 ноября", "modality": "reported", "slots": { "due": "2026-11-28" } } }],
            "base": [{ "assertion_id": known["assertion_id"], "version": known["version"] }],
            "risk": "conflict",
            "rationale": "#release, Olya, 2026-11-09 18:02",
            "source_text": IN_CHAT,
        }),
    )
    .await;
    assert_eq!(proposed["state"], "open", "{proposed}");
    assert_eq!(proposed["author"], format!("agent:daily-sweep/for/{alice}"));
    // The words it quotes are its own material: extraction does not read them again.
    dispatch.drain().await.unwrap();
    let (_, open) = call(&app, get_json("/v1/proposals?state=open", &client)).await;
    assert_eq!(open.as_array().unwrap().len(), 1, "{open}");
    // The next night the agent finds it waiting and does not propose it again (F2).
    let again = tool(
        &app,
        &agent,
        "search_knowledge",
        json!({ "query": "релиз 2.4" }),
    )
    .await;
    let pending: Vec<&Value> = again
        .as_array()
        .unwrap()
        .iter()
        .filter(|hit| hit.get("pending").is_some())
        .collect();
    assert_eq!(pending.len(), 1, "{again}");
    assert_eq!(pending[0]["proposal_id"], proposed["id"]);
    assert_eq!(pending[0]["changes"][0]["text"], "Релиз 2.4 — 28 ноября");
    // A model that runs its next parameter into a text is asked to send the call again.
    let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "propose_change", "arguments": {
        "operations": [{ "op": "create", "assertion": { "kind": "fact", "text": "x", "modality": "reported" } }],
        "rationale": "seen in chat</rationale>\n<parameter name=\"risk\">new",
        "source_text": IN_CHAT,
    } } });
    let (_, refused) = call(
        &app,
        Request::post("/mcp")
            .header(header::HOST, "frood:8080")
            .header(header::AUTHORIZATION, format!("Bearer {agent}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    runs.lock().unwrap()[0].2 = "finished".into();
    dispatch.watch_once().await.unwrap();

    // It reaches Alice in Telegram: before → after, the words it rests on, buttons.
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 1);
    let message = bot.lock().unwrap().sent[0].clone();
    assert_eq!(message["recipient"]["external_id"], "42");
    let item = &message["item"];
    assert_eq!(item["item"]["kind"], "proposal");
    assert_eq!(item["content"]["risk"], "conflict");
    let change = &item["content"]["alternatives"][0]["changes"][0];
    assert_eq!(change["before"], "Релиз 2.4 — 14 ноября");
    assert_eq!(change["after"], "Релиз 2.4 — 28 ноября");
    assert_eq!(item["content"]["quote"], IN_CHAT);
    let buttons: Vec<&str> = item["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap())
        .collect();
    assert_eq!(buttons, ["accept", "reject", "defer"]);
    let item_id = item["item"]["id"].as_str().unwrap().to_owned();

    // 4. The server restarts: nothing is lost and nothing is sent twice.
    drop((app, dispatch));
    let Server { app, dispatch } = start(&raw, pool.clone()).await;
    assert_eq!(dispatch.deliver_once().await.unwrap(), 0, "shown once");
    let (_, inbox) = call(&app, get_json("/v1/inbox", &client)).await;
    assert_eq!(inbox[0]["item"]["id"], item_id.as_str(), "{inbox}");
    assert_eq!(inbox[0]["for_me"], true);
    let (_, still) = call(
        &app,
        get_json(
            &format!("/v1/proposals/{}", proposed["id"].as_str().unwrap()),
            &client,
        ),
    )
    .await;
    assert_eq!(still["state"], "open");

    // 5. Alice presses «Принять» in Telegram: the bot decides as her.
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
    dispatch.inbox_pending().await.unwrap();
    assert_eq!(dispatch.deliver_once().await.unwrap(), 1);
    let edited = bot.lock().unwrap().updated[0].clone();
    assert_eq!(edited["item"]["item"]["resolution"]["action"], "accept");
    assert_eq!(edited["item"]["actions"], json!([]), "the buttons are gone");

    // Any client sees the history: the old date superseded, the new one active, and who decided.
    let history = tool(
        &app,
        &client,
        "get_history",
        json!({ "assertion_id": known["assertion_id"] }),
    )
    .await;
    let versions: Vec<(&str, &str)> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["text"].as_str().unwrap(), v["status"].as_str().unwrap()))
        .collect();
    assert!(
        versions.contains(&("Релиз 2.4 — 14 ноября", "superseded")),
        "{history}"
    );
    assert!(
        versions.contains(&("Релиз 2.4 — 28 ноября", "active")),
        "{history}"
    );
    let decided = tool(
        &app,
        &client,
        "get_proposal",
        json!({ "proposal_id": proposed["id"] }),
    )
    .await;
    assert_eq!(decided["state"], "accepted");
    assert_eq!(
        decided["verdicts"][0]["decided_by"],
        format!("user:{alice}/via/telegram")
    );
    let now = tool(
        &app,
        &client,
        "search_knowledge",
        json!({ "query": "какие сроки в ноябре?" }),
    )
    .await;
    assert_eq!(now[0]["text"], "Релиз 2.4 — 28 ноября", "{now}");
    assert_eq!(now[0]["sources"][0]["quote"], IN_CHAT);

    // Alice takes it back in Telegram: the old date holds again and the item waits for her.
    let (status, reopened) = call(
        &app,
        post_json(
            &format!("/v1/inbox/{item_id}/undo"),
            "tg-token",
            json!({ "external_id": "42" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["state"], "open");
    let (_, again) = call(
        &app,
        get_json(
            &format!("/v1/proposals/{}", proposed["id"].as_str().unwrap()),
            &client,
        ),
    )
    .await;
    assert_eq!(again["state"], "open");
    assert_eq!(again["verdicts"][1]["decision"], "undo");
    // The console's first page shows what holds and what changed this week.
    let (status, page) = call(&app, get_json("/v1/now", &client)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let changes = page["changes"].as_array().unwrap();
    assert!(
        changes
            .iter()
            .any(|c| c["assertion"]["text"] == "Релиз 2.4 — 14 ноября"
                && c["before"] == "Релиз 2.4 — 28 ноября"),
        "{page}"
    );

    // Alice takes her voice note back from Telegram. The date it gave was changed since by the
    // agent's proposal and its undo, so that knowledge stays: undoing would lose the later steps.
    let (status, withdrawn) = call(
        &app,
        post_json(
            &format!("/v1/sources/{voice_source}/withdraw"),
            "tg-token",
            json!({ "external_id": "42" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{withdrawn}");
    assert_eq!(withdrawn["kept"], 1, "{withdrawn}");
    // Someone else's account cannot.
    let (status, _) = call(
        &app,
        post_json(
            &format!("/v1/sources/{voice_source}/withdraw"),
            "tg-token",
            json!({ "external_id": "7" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
