//! Agent runs through the real dispatcher and MCP, with a fake runtime module playing
//! `agents.execute@1` and the test playing the agent (FROOD_TEST_DATABASE_URL; skipped
//! without it): start → ask_user → a person answers → the same session resumes → propose →
//! finish; steering; a budget stop; a cancel; the run token never decides.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct FakeRun {
    state: String,
    session: String,
    cost: f64,
    result: Option<String>,
    events: Vec<Value>,
}

/// What the runtime was asked, and the runs it pretends to run.
#[derive(Default)]
struct Runtime {
    calls: Vec<Value>,
    runs: HashMap<String, FakeRun>,
}

type Shared = Arc<Mutex<Runtime>>;

impl Runtime {
    fn run(&mut self, id: &str) -> &mut FakeRun {
        self.runs.get_mut(id).expect("a run the runtime started")
    }

    fn ops_for(&self, id: &str) -> Vec<String> {
        self.calls
            .iter()
            .filter(|c| c["run_id"] == id)
            .map(|c| c["op"].as_str().unwrap().to_owned())
            .filter(|op| op != "status" && op != "events")
            .collect()
    }

    fn last(&self, op: &str) -> Value {
        self.calls
            .iter()
            .rev()
            .find(|c| c["op"] == op)
            .cloned()
            .unwrap_or(Value::Null)
    }
}

async fn execute(
    State(runtime): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bearer rt-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "bad token" })),
        );
    }
    let mut rt = runtime.lock().unwrap();
    rt.calls.push(body.clone());
    let id = body["run_id"].as_str().unwrap().to_owned();
    let reply = match body["op"].as_str().unwrap() {
        "start" => {
            let session = format!("session-{}", rt.runs.len() + 1);
            rt.runs.insert(
                id,
                FakeRun {
                    state: "running".into(),
                    session: session.clone(),
                    ..Default::default()
                },
            );
            json!({ "session_ref": session, "state": "running" })
        }
        "resume" => {
            let run = rt.run(&id);
            assert_eq!(body["session_ref"], run.session, "resumes the same session");
            run.state = "running".into();
            json!({ "session_ref": run.session, "state": "running" })
        }
        "send" => json!({ "delivered": true }),
        "stop" => {
            rt.run(&id).state = "failed".into();
            json!({})
        }
        "status" => {
            let run = rt.run(&id);
            json!({ "state": run.state, "session_ref": run.session, "cost_usd": run.cost, "result": run.result })
        }
        "events" => {
            let after = body["after"].as_u64().unwrap_or(0) as usize;
            let events = rt.run(&id).events.get(after..).unwrap_or_default().to_vec();
            json!({ "events": events })
        }
        other => panic!("unknown op {other}"),
    };
    (StatusCode::OK, Json(reply))
}

async fn fake_runtime(runtime: Shared) -> String {
    let app = Router::new()
        .route(
            "/manifest",
            get(|| async { Json(json!({ "name": "runtime-fake", "version": "0.1.0", "provides": ["agents.execute@1"], "frood_api": [] })) }),
        )
        .route("/v1/capabilities/agents.execute@1", post(execute))
        .with_state(runtime);
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
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
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

/// A JSON-RPC call to Frood's MCP endpoint, as the agent in the runtime makes it.
async fn mcp(app: &Router, token: &str, method: &str, params: Value) -> (StatusCode, Value) {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let request = Request::post("/mcp")
        .header(header::HOST, "frood:8080")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(body.to_string()))
        .unwrap();
    call(app, request).await
}

async fn tool(app: &Router, token: &str, name: &str, arguments: Value) -> Value {
    let (status, reply) = mcp(
        app,
        token,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    reply["result"].clone()
}

fn text(result: &Value) -> Value {
    let raw = result["content"][0]["text"].as_str().unwrap_or_default();
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

/// The run token the runtime was given for Frood's MCP server.
fn run_token(request: &Value) -> String {
    let servers = request["mcp_servers"].as_array().unwrap();
    let frood = servers.iter().find(|s| s["name"] == "frood").unwrap();
    assert_eq!(frood["path"], "/mcp");
    frood["headers"]["Authorization"]
        .as_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn agent_asks_waits_resumes_and_proposes() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "agents")
        .await
        .expect("test database");
    let runtime = Shared::default();
    let fake = fake_runtime(runtime.clone()).await;
    let raw = format!(
        "version: 1\ninstallation: {{name: test}}\nstorage: {{uses: postgres, url: 'postgres://unused'}}\n\
         deploy: {{server: host}}\n\
         accounts:\n  claude: {{ kind: api_key, provider: anthropic, secret: sk-test }}\n\
         jobs:\n  extract: {{ enabled: false }}\n  sweep:\n    uses: agents.execute\n    runtime: runtime\n    agent: claude-code\n    \
         model: claude-haiku-5-5\n    account: claude\n    instructions: Sweep the team chat.\n    \
         permissions: [knowledge:read, knowledge:propose]\n    on_behalf_of: alice@example.com\n    \
         ask_user: {{ expire_after: 3d }}\n    budget: {{ max_usd: 0.5 }}\n\
         modules:\n  runtime: {{ url: '{fake}' }}\n"
    );
    let config = frood_config::parse(
        &raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap()
    .config;
    let assembly = frood_api::assemble(&config, std::path::Path::new("."), pool, |name| {
        (name == "FROOD_MODULE_TOKEN_RUNTIME").then(|| "rt-token".to_owned())
    });
    assert!(assembly.warnings.is_empty(), "{:?}", assembly.warnings);
    assembly.modules.discover("runtime").await.unwrap();
    let auth = assembly.state.auth.clone();
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
    let dispatch = assembly.state.dispatch.clone();
    let app = frood_api::router(assembly.state);

    // A run by hand: the runtime gets the spec, the credentials and a run token.
    let (status, run) = call(
        &app,
        post_json(
            "/v1/runs",
            &pat,
            json!({ "job": "sweep", "input": { "since": "yesterday" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let run_id = run["id"].as_str().unwrap().to_owned();
    dispatch.drain().await.unwrap();
    let start = runtime.lock().unwrap().last("start");
    assert_eq!(start["run_id"], run_id.as_str());
    assert_eq!(start["agent"], "claude-code");
    assert_eq!(start["model"], "claude-haiku-5-5");
    assert_eq!(start["instructions"], "Sweep the team chat.");
    assert_eq!(start["credentials"]["provider"], "anthropic");
    assert_eq!(start["credentials"]["api_key"], "sk-test");
    assert_eq!(start["budget"]["max_usd"], 0.5);
    assert!(
        start["prompt"].as_str().unwrap().contains("yesterday"),
        "the prompt carries the input"
    );
    let token = run_token(&start);
    assert!(token.starts_with("frood_run_"), "{token}");
    let (_, now) = call(&app, get_json(&format!("/v1/runs/{run_id}"), &pat)).await;
    assert_eq!(now["status"], "running", "{now}");
    assert_eq!(now["session_ref"], "session-1");
    assert_eq!(now["on_behalf_of"], alice.to_string());

    // The agent sees the run's tools and not the person's ones.
    let (_, list) = mcp(&app, &token, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for name in [
        "search_knowledge",
        "propose_change",
        "ask_user",
        "report_progress",
    ] {
        assert!(names.contains(&name), "{name}: {names:?}");
    }
    for name in ["submit_verdict", "answer_question", "capture_note"] {
        assert!(!names.contains(&name), "{name}: {names:?}");
    }

    // It notes progress, asks, and ends its turn.
    tool(
        &app,
        &token,
        "report_progress",
        json!({ "message": "Read 12 messages" }),
    )
    .await;
    let asked = text(
        &tool(
            &app,
            &token,
            "ask_user",
            json!({ "question": "Decision or discussion?", "options": ["Decision", "Discussion"], "context": "«давай так и сделаем» — Оля, 18:02" }),
        )
        .await,
    );
    let question_id = asked["question_id"].as_str().unwrap().to_owned();
    assert!(
        asked["expires_at"].is_string(),
        "expire_after applies: {asked}"
    );
    {
        let mut rt = runtime.lock().unwrap();
        let run = rt.run(&run_id);
        run.state = "finished".into();
        run.cost = 0.01;
        run.events
            .push(json!({ "kind": "tool_use", "data": { "name": "mcp__frood__ask_user" } }));
    }
    dispatch.watch_once().await.unwrap();
    let (_, now) = call(&app, get_json(&format!("/v1/runs/{run_id}"), &pat)).await;
    assert_eq!(now["status"], "waiting_for_input", "{now}");
    assert_eq!(now["cost_usd"], 0.01);

    // The question waits for a person; the agent cannot answer it itself.
    let (_, open) = call(&app, get_json("/v1/questions", &pat)).await;
    assert_eq!(open[0]["id"], question_id.as_str(), "{open}");
    assert_eq!(open[0]["status"], "open");
    assert_eq!(open[0]["asked_for"], alice.to_string());
    let (status, _) = call(
        &app,
        post_json(
            &format!("/v1/questions/{question_id}/answer"),
            &token,
            json!({ "answer": "Decision" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a run token answers nothing");
    let refused = tool(
        &app,
        &token,
        "answer_question",
        json!({ "question_id": question_id, "answer": "Decision" }),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");

    // Steering while it waits: delivered with the answer.
    let (_, steered) = call(
        &app,
        post_json(
            &format!("/v1/runs/{run_id}/messages"),
            &pat,
            json!({ "text": "Also look at #release" }),
        ),
    )
    .await;
    assert_eq!(steered["outcome"], "queued", "{steered}");

    // Alice answers; nothing resumes until the watcher sees it.
    let (status, answered) = call(
        &app,
        post_json(
            &format!("/v1/questions/{question_id}/answer"),
            &pat,
            json!({ "answer": "Decision" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert_eq!(answered["answered_by"], format!("user:{alice}"));
    let (status, _) = call(
        &app,
        post_json(
            &format!("/v1/questions/{question_id}/answer"),
            &pat,
            json!({ "answer": "Discussion" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "answered once");
    dispatch.watch_once().await.unwrap();
    dispatch.drain().await.unwrap();

    // The same session continues with the answer and the message, under a fresh token.
    let resume = runtime.lock().unwrap().last("resume");
    assert_eq!(resume["session_ref"], "session-1");
    let message = resume["message"].as_str().unwrap();
    assert!(
        message.contains("Decision or discussion?\n  Decision"),
        "{message}"
    );
    assert!(message.contains("Also look at #release"), "{message}");
    let fresh = run_token(&resume);
    assert_ne!(fresh, token);
    let (status, _) = mcp(&app, &token, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old token is revoked");
    let (_, now) = call(&app, get_json(&format!("/v1/runs/{run_id}"), &pat)).await;
    assert_eq!(now["status"], "running", "{now}");
    assert_eq!(now["pending_messages"], json!([]), "delivered");

    // It proposes; an agent's proposal waits for a person, and the agent cannot decide it.
    let proposal = text(
        &tool(
            &app,
            &fresh,
            "propose_change",
            json!({
                "operations": [{ "op": "create", "assertion": { "kind": "decision", "text": "Релизы по четвергам", "modality": "agreed" } }],
                "rationale": "Agreed in #release at 18:02",
                "source_text": "давай так и сделаем: релизы по четвергам",
            }),
        )
        .await,
    );
    assert_eq!(proposal["state"], "open", "{proposal}");
    assert_eq!(proposal["author"], format!("agent:sweep/for/{alice}"));
    let decided = tool(
        &app,
        &fresh,
        "submit_verdict",
        json!({ "proposal_id": proposal["id"], "verdict": "accept" }),
    )
    .await;
    assert_eq!(decided["isError"], true, "{decided}");

    // It finishes: the run succeeds, its token dies, its log tells the story.
    {
        let mut rt = runtime.lock().unwrap();
        let run = rt.run(&run_id);
        run.state = "finished".into();
        run.cost = 0.02;
        run.result = Some("Proposed 1 decision".into());
        run.events
            .push(json!({ "kind": "tool_use", "data": { "name": "mcp__frood__propose_change" } }));
    }
    dispatch.watch_once().await.unwrap();
    let (_, now) = call(&app, get_json(&format!("/v1/runs/{run_id}"), &pat)).await;
    assert_eq!(now["status"], "succeeded", "{now}");
    assert_eq!(now["output"]["result"], "Proposed 1 decision");
    assert_eq!(now["cost_usd"], 0.02);
    let (status, _) = mcp(&app, &fresh, "tools/list", json!({})).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the token ends with the run"
    );
    let (_, events) = call(&app, get_json(&format!("/v1/runs/{run_id}/events"), &pat)).await;
    let kinds: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "started",
            "progress",
            "runtime.tool_use",
            "waiting",
            "resumed",
            "runtime.tool_use"
        ],
        "{events}"
    );
    assert_eq!(
        runtime.lock().unwrap().ops_for(&run_id),
        ["start", "resume"],
        "nothing was stopped"
    );

    // A second run: steering reaches it at once; going over the budget stops it.
    let (_, second) = call(&app, post_json("/v1/runs", &pat, json!({ "job": "sweep" }))).await;
    let second = second["id"].as_str().unwrap().to_owned();
    dispatch.drain().await.unwrap();
    let (_, steered) = call(
        &app,
        post_json(
            &format!("/v1/runs/{second}/messages"),
            &pat,
            json!({ "text": "Skip the bots" }),
        ),
    )
    .await;
    assert_eq!(steered["outcome"], "delivered", "{steered}");
    runtime.lock().unwrap().run(&second).cost = 0.9;
    dispatch.watch_once().await.unwrap();
    let (_, now) = call(&app, get_json(&format!("/v1/runs/{second}"), &pat)).await;
    assert_eq!(now["status"], "failed", "{now}");
    assert!(
        now["error"].as_str().unwrap().contains("budget exceeded"),
        "{now}"
    );
    assert_eq!(
        runtime.lock().unwrap().ops_for(&second),
        ["start", "send", "stop"]
    );

    // A third run waits on a question and is cancelled: the question goes with it.
    let (_, third) = call(&app, post_json("/v1/runs", &pat, json!({ "job": "sweep" }))).await;
    let third = third["id"].as_str().unwrap().to_owned();
    dispatch.drain().await.unwrap();
    let token = run_token(&runtime.lock().unwrap().last("start"));
    let asked = text(
        &tool(
            &app,
            &token,
            "ask_user",
            json!({ "question": "Which channel?" }),
        )
        .await,
    );
    runtime.lock().unwrap().run(&third).state = "finished".into();
    dispatch.watch_once().await.unwrap();
    let (status, _) = call(
        &app,
        post_json(&format!("/v1/runs/{third}/cancel"), &pat, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, q) = call(
        &app,
        get_json(
            &format!("/v1/questions/{}", asked["question_id"].as_str().unwrap()),
            &pat,
        ),
    )
    .await;
    assert_eq!(q["status"], "cancelled", "{q}");
    let (_, now) = call(&app, get_json(&format!("/v1/runs/{third}"), &pat)).await;
    assert_eq!(now["status"], "cancelled", "{now}");
}

#[tokio::test]
async fn schedules_tick_once_per_source() {
    use chrono::TimeZone;
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "schedules")
        .await
        .expect("test database");
    let raw = "version: 1\ninstallation: {name: test}\nstorage: {uses: postgres, url: 'postgres://unused'}\n\
         deploy: {server: host}\n\
         accounts:\n  claude: { kind: api_key, provider: anthropic, secret: sk-test }\n\
         sources:\n  team-chat: {}\n  wiki: {}\n\
         jobs:\n  extract: { enabled: false }\n  nightly:\n    uses: agents.execute\n    \
         on: [{ schedule: '0 3 * * *', timezone: Europe/Amsterdam, for_each: source }]\n    \
         runtime: runtime\n    agent: claude-code\n    model: claude-haiku-5-5\n    account: claude\n    \
         instructions: Sweep the source.\n\
         modules:\n  runtime: { url: 'http://127.0.0.1:1' }\n";
    let config = frood_config::parse(
        raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap()
    .config;
    let assembly = frood_api::assemble(&config, std::path::Path::new("."), pool.clone(), |_| {
        Some("rt-token".to_owned())
    });
    assert!(assembly.warnings.is_empty(), "{:?}", assembly.warnings);
    let dispatch = assembly.state.dispatch.clone();
    let at = |d: u32, h: u32| chrono::Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();

    // First seen on the 2nd at noon: nothing fires for the past.
    assert_eq!(dispatch.tick_schedules(at(2, 12)).await.unwrap(), 0);
    assert_eq!(
        dispatch.tick_schedules(at(3, 1)).await.unwrap(),
        0,
        "03:00 CET is 02:00 UTC"
    );
    // The 3rd, 02:00 UTC: one tick per source, once.
    assert_eq!(dispatch.tick_schedules(at(3, 2)).await.unwrap(), 2);
    assert_eq!(dispatch.tick_schedules(at(3, 5)).await.unwrap(), 0);
    // The server was down for three nights: one tick per source, not three.
    assert_eq!(dispatch.tick_schedules(at(6, 9)).await.unwrap(), 2);

    dispatch.dispatch_pending().await.unwrap();
    let runs = dispatch.runs(Some("nightly"), None, 50).await.unwrap();
    assert_eq!(runs.len(), 4);
    let mut seen: Vec<(String, String, i64)> = runs
        .iter()
        .map(|run| {
            let data = &run.input["event"]["data"];
            (
                data["source"].as_str().unwrap().to_owned(),
                data["scheduled_for"].as_str().unwrap().to_owned(),
                data["missed"].as_i64().unwrap(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("team-chat".into(), "2026-11-03T02:00:00Z".into(), 0),
            ("team-chat".into(), "2026-11-06T02:00:00Z".into(), 2),
            ("wiki".into(), "2026-11-03T02:00:00Z".into(), 0),
            ("wiki".into(), "2026-11-06T02:00:00Z".into(), 2),
        ]
    );
    assert_eq!(
        runs[0]
            .subject
            .as_deref()
            .map(|s| s.starts_with("schedule/nightly/")),
        Some(true)
    );
}
