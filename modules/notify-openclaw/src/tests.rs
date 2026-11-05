//! The module end to end: a fake `openclaw mcp serve` on stdio (real MCP, `tests/fake_bridge.py`)
//! and a fake Frood over HTTP.

use std::path::Path;
use std::sync::Mutex as StdMutex;

use axum::extract::Path as UrlPath;

use super::*;

#[derive(Default)]
struct World {
    links: Vec<Value>,
    acts: Vec<(String, Value)>,
}

type Shared = Arc<StdMutex<World>>;

async fn fake_frood(world: Shared) -> String {
    let app = Router::new()
        .route(
            "/v1/channels/openclaw/links",
            post(|State(w): State<Shared>, headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer module-token");
                if body["code"] == "BADCODE22" {
                    return (StatusCode::BAD_REQUEST, Json(json!({ "detail": "this code expired" })));
                }
                w.lock().unwrap().links.push(body);
                (StatusCode::OK, Json(json!({ "linked": true, "email": "alice@example.com" })))
            }),
        )
        .route(
            "/v1/inbox/{id}/act",
            post(|State(w): State<Shared>, UrlPath(id): UrlPath<String>, Json(body): Json<Value>| async move {
                if id == "gone" {
                    return (StatusCode::CONFLICT, Json(json!({ "detail": "the proposal changed" })));
                }
                w.lock().unwrap().acts.push((id, body));
                (StatusCode::OK, Json(json!({ "outcome": "done", "item": {} })))
            }),
        )
        .with_state(world);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

struct Setup {
    module: Arc<Module>,
    app: Router,
    world: Shared,
    events: std::path::PathBuf,
    sent: std::path::PathBuf,
    _dir: std::path::PathBuf,
}

async fn setup(name: &str) -> Setup {
    let dir = std::env::temp_dir().join(format!("frood-openclaw-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (events, sent) = (dir.join("events.jsonl"), dir.join("sent.jsonl"));
    std::fs::write(&events, "").unwrap();
    std::fs::write(&sent, "").unwrap();
    let world = Shared::default();
    let module = Arc::new(Module {
        http: reqwest::Client::new(),
        frood: fake_frood(world.clone()).await,
        token: "module-token".to_owned(),
        channel: "openclaw".to_owned(),
        words: Lang::Ru.words(),
        launch: Launch {
            command: "python3".to_owned(),
            args: vec![
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_bridge.py").to_owned(),
                events.display().to_string(),
                sent.display().to_string(),
            ],
            gateway: "ws://gateway.test:18789".to_owned(),
            token_file: dir.join("token"),
        },
        retry_after: Duration::from_millis(100),
        peer: tokio::sync::RwLock::default(),
        waiting: Mutex::default(),
    });
    tokio::spawn(module.clone().run());
    for _ in 0..100 {
        if module.peer.read().await.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        module.peer.read().await.is_some(),
        "the bridge did not start"
    );
    Setup {
        app: router(module.clone()),
        module,
        world,
        events,
        sent,
        _dir: dir,
    }
}

/// A message arrives in a conversation.
fn says(events: &Path, cursor: u64, session: &str, text: &str, owner: bool) {
    use std::io::Write;
    let event = json!({
        "cursor": cursor, "type": "message", "sessionKey": session, "role": "user", "text": text,
        "conversation": { "channel": "whatsapp", "to": "+100" },
        "raw": { "senderIsOwner": owner },
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(events)
        .unwrap();
    writeln!(file, "{event}").unwrap();
}

fn sent(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Waits until `count` messages went out.
async fn until_sent(path: &Path, count: usize) -> Vec<Value> {
    for _ in 0..100 {
        let out = sent(path);
        if out.len() >= count {
            return out;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{count} messages expected, got {:?}", sent(path));
}

async fn capability(app: &Router, name: &str, token: &str, body: Value) -> (StatusCode, Value) {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/capabilities/{name}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn proposal(id: &str, session: &str) -> Value {
    json!({
        "recipient": { "user_id": "u1", "external_id": session },
        "item": {
            "item": { "id": id, "kind": "proposal", "state": "open" },
            "content": {
                "risk": "change",
                "alternatives": [{ "key": "default", "label": null, "changes": [{ "kind": "commitment", "before": "14 ноября", "after": "Релиз 2.4 — 28 ноября" }] }]
            },
            "actions": [{ "action": "accept", "alternative": "default" }, { "action": "reject" }, { "action": "defer" }]
        }
    })
}

#[tokio::test]
async fn a_conversation_links_gets_an_item_and_a_reply_decides() {
    let s = setup("flow").await;

    // Someone else's code in a group does nothing; the owner's links the conversation.
    says(&s.events, 1, "agent:main:wa", "frood ABCD2345", false);
    says(&s.events, 2, "agent:main:wa", "frood BADCODE22", true);
    says(&s.events, 3, "agent:main:wa", "frood ABCD2345", true);
    let out = until_sent(&s.sent, 2).await;
    assert_eq!(out[0]["session_key"], "agent:main:wa");
    assert_eq!(out[0]["text"], "Не удалось связать: this code expired");
    assert_eq!(out[1]["text"], "Связано с alice@example.com");
    assert_eq!(
        s.world.lock().unwrap().links,
        [json!({ "code": "ABCD2345", "external_id": "agent:main:wa", "label": "whatsapp" })]
    );

    // The core shows an item: text with numbered choices.
    let (status, answer) = capability(
        &s.app,
        "notify.send@1",
        "module-token",
        proposal("i1", "agent:main:wa"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["message_ref"], "agent:main:wa|i1");
    let out = until_sent(&s.sent, 3).await;
    assert!(
        out[2]["text"].as_str().unwrap().contains("1 — Принять"),
        "{}",
        out[2]
    );

    // Talk for the agent stays with the agent; someone else's "1" does nothing; the owner's decides.
    says(&s.events, 4, "agent:main:wa", "а что по релизу?", true);
    says(&s.events, 5, "agent:main:wa", "1", false);
    says(&s.events, 6, "agent:main:wa", "2", true);
    let out = until_sent(&s.sent, 4).await;
    assert_eq!(out[3]["text"], Lang::Ru.words().rejected);
    assert_eq!(
        s.world.lock().unwrap().acts,
        [(
            "i1".to_owned(),
            json!({ "action": "reject", "external_id": "agent:main:wa" })
        )]
    );

    // Answered: a late update says nothing more, and a further "1" is talk.
    let done = json!({
        "recipient": { "external_id": "agent:main:wa" },
        "message_ref": "agent:main:wa|i1",
        "item": { "item": { "id": "i1", "state": "resolved", "resolution": { "action": "reject", "by": "user:u1/via/openclaw" } }, "content": {}, "actions": [] }
    });
    assert_eq!(
        capability(&s.app, "notify.update@1", "module-token", done)
            .await
            .0,
        StatusCode::OK
    );
    says(&s.events, 7, "agent:main:wa", "1", true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sent(&s.sent).len(), 4);
    assert_eq!(s.world.lock().unwrap().acts.len(), 1);
}

#[tokio::test]
async fn what_is_decided_elsewhere_comes_as_a_new_message() {
    let s = setup("elsewhere").await;
    capability(
        &s.app,
        "notify.send@1",
        "module-token",
        proposal("i2", "s1"),
    )
    .await;
    until_sent(&s.sent, 1).await;
    let body = json!({
        "recipient": { "external_id": "s1" },
        "message_ref": "s1|i2",
        "item": {
            "item": { "id": "i2", "state": "resolved", "resolution": { "action": "accept", "by": "user:u1/via/claude-code" } },
            "content": proposal("i2", "s1")["item"]["content"],
            "actions": []
        }
    });
    let (status, _) = capability(&s.app, "notify.update@1", "module-token", body).await;
    assert_eq!(status, StatusCode::OK);
    let out = until_sent(&s.sent, 2).await;
    let w = Lang::Ru.words();
    assert_eq!(
        out[1]["text"],
        format!("{} {}: Релиз 2.4 — 28 ноября", w.accepted, w.elsewhere)
    );
    // No longer waiting: a reply is talk now.
    says(&s.events, 1, "s1", "1", true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s.world.lock().unwrap().acts.is_empty());
}

#[tokio::test]
async fn the_newest_item_is_answered_and_a_refusal_is_said() {
    let s = setup("newest").await;
    capability(
        &s.app,
        "notify.send@1",
        "module-token",
        proposal("i3", "s1"),
    )
    .await;
    capability(
        &s.app,
        "notify.send@1",
        "module-token",
        proposal("gone", "s1"),
    )
    .await;
    until_sent(&s.sent, 2).await;
    says(&s.events, 1, "s1", "да", true);
    let out = until_sent(&s.sent, 3).await;
    assert_eq!(out[2]["text"], "⚠️ the proposal changed");
    assert!(s.world.lock().unwrap().acts.is_empty());
    assert_eq!(s.module.waiting.lock().unwrap()["s1"].len(), 2);
}

#[tokio::test]
async fn the_core_is_told_when_to_stop_and_when_to_try_again() {
    let s = setup("refusals").await;
    let (status, _) = capability(&s.app, "notify.send@1", "wrong", proposal("i4", "s1")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A conversation the Gateway no longer routes: for good.
    let (status, body) = capability(
        &s.app,
        "notify.send@1",
        "module-token",
        proposal("i4", "no-route"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["retryable"], false);
    assert_eq!(
        body["detail"],
        "Conversation not found for session no-route"
    );
    assert!(s.module.waiting.lock().unwrap().get("no-route").is_none());
    // OpenClaw away: try again later.
    *s.module.peer.write().await = None;
    let (status, body) = capability(
        &s.app,
        "notify.send@1",
        "module-token",
        proposal("i4", "s1"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["retryable"], true);
}

#[test]
fn the_manifest_offers_both_capabilities() {
    let manifest: Value = serde_yaml_ng::from_str(MANIFEST).unwrap();
    assert_eq!(
        manifest["provides"],
        json!(["notify.send@1", "notify.update@1"])
    );
    assert_eq!(manifest["frood_api"], json!(["channels:act"]));
    assert_eq!(manifest["config"]["required"], json!(["gateway"]));
}
