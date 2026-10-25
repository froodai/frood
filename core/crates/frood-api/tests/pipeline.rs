//! Steps 1–2 of the MVP scenario through the real pipeline, with recorded answers instead of
//! real providers (FROOD_TEST_DATABASE_URL; skipped without it):
//! a note → extract → an accepted commitment → found by meaning from another language;
//! a Telegram account linked by code → a voice message → transcribe → extract → a conflict.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, extract::State};
use base64::Engine;
use serde_json::{Value, json};
use tower::ServiceExt;

const NOTE: &str = "Договорились с Олей: релиз 2.4 — 14 ноября";
const VOICE_TRANSCRIPT: &str = "Оля пишет: релиз переносим на 28 ноября";

/// Recorded model answer for a prompt: the first note creates a commitment with a new person;
/// the transcript moves its date and contradicts it.
fn extraction_for(prompt: &str) -> Value {
    if prompt.contains("переносим на 28 ноября") {
        let known = prompt
            .split("<known_assertions>")
            .nth(1)
            .and_then(|rest| rest.split("</known_assertions>").next())
            .and_then(|block| serde_json::from_str::<Vec<Value>>(block.trim()).ok())
            .unwrap_or_default();
        let target = known
            .first()
            .map(|a| a["id"].clone())
            .unwrap_or(Value::Null);
        return json!({
            "entities": [],
            "changes": [{
                "action": "supersede", "kind": "commitment", "text": "Релиз 2.4 — 28 ноября",
                "subjects": [], "slots": [{ "key": "due", "value": "2026-11-28" }], "modality": "reported",
                "valid_from": null, "target_assertion_id": target, "close_status": null,
                "conflict": true, "confidence": 0.7, "quote": "релиз переносим на 28 ноября",
                "reason": "Olya reports a new date that contradicts the agreed one"
            }]
        });
    }
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

/// Embeddings by concept, so "release deadline in November" lands next to the Russian note.
fn embedding_for(text: &str) -> Vec<f32> {
    let text = text.to_lowercase();
    let concepts: [&[&str]; 4] = [
        &["релиз", "release"],
        &["ноябр", "november"],
        &["срок", "deadline", "due"],
        &["ол", "olya"],
    ];
    let mut vector: Vec<f32> = concepts
        .iter()
        .map(|words| {
            if words.iter().any(|w| text.contains(w)) {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    vector.push(0.1);
    vector
}

async fn messages(Json(body): Json<Value>) -> Json<Value> {
    let prompt = body["messages"][0]["content"].as_str().unwrap_or_default();
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    Json(json!({
        "model": body["model"], "stop_reason": "end_turn",
        "content": [{ "type": "text", "text": extraction_for(prompt).to_string() }],
        "usage": { "input_tokens": 100, "output_tokens": 50 }
    }))
}

async fn embeddings(Json(body): Json<Value>) -> Json<Value> {
    let data: Vec<Value> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, t)| json!({ "object": "embedding", "index": i, "embedding": embedding_for(t.as_str().unwrap()) }))
        .collect();
    Json(json!({ "object": "list", "data": data, "model": body["model"] }))
}

async fn transcribe(
    State(()): State<()>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bearer stt-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "bad token" })),
        );
    }
    let audio = base64::engine::general_purpose::STANDARD
        .decode(body["audio"]["data_base64"].as_str().unwrap_or_default())
        .unwrap_or_default();
    assert!(
        audio.starts_with(b"OggS"),
        "the module gets the file's bytes"
    );
    assert_eq!(body["audio"]["content_type"], "audio/ogg");
    (
        StatusCode::OK,
        Json(json!({ "text": VOICE_TRANSCRIPT, "language": "ru" })),
    )
}

/// One fake server for Anthropic, OpenAI and the speech module.
async fn providers() -> String {
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/embeddings", post(embeddings))
        .route(
            "/manifest",
            get(|| async { Json(json!({ "name": "stt-mock", "version": "0.1.0", "provides": ["media.transcribe@1"], "frood_api": [] })) }),
        )
        .route("/v1/capabilities/media.transcribe@1", post(transcribe))
        .with_state(());
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
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
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

fn voice(token: &str, external_id: &str) -> Request<Body> {
    let boundary = "frood-test-boundary";
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
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn voice_and_notes_become_knowledge() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "pipeline")
        .await
        .expect("test database");
    let fake = providers().await;
    let raw = format!(
        "version: 1\ninstallation: {{name: test}}\nstorage: {{uses: postgres, url: 'postgres://unused', objects: {{uses: database}}}}\n\
         deploy: {{server: host}}\n\
         accounts:\n  claude: {{ kind: api_key, provider: anthropic, secret: test, base_url: '{fake}' }}\n  \
         openai: {{ kind: api_key, provider: openai, secret: test, base_url: '{fake}' }}\n\
         search:\n  embeddings: {{ account: openai, model: text-embedding-3-small }}\n\
         jobs:\n  extract: {{ with: {{ account: claude, model: claude-haiku-5-5 }} }}\n\
         modules:\n  stt: {{ url: '{fake}' }}\n  telegram: {{ url: 'http://127.0.0.1:1' }}\n"
    );
    let config = frood_config::parse(
        &raw,
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
            _ => None,
        },
    );
    assert!(assembly.warnings.is_empty(), "{:?}", assembly.warnings);
    let jobs = assembly.state.dispatch.jobs();
    let names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
    assert_eq!(names, ["transcribe", "extract", "embed"]);
    assembly.modules.discover("stt").await.unwrap();
    let auth = assembly.state.auth.clone();
    auth.register_service_token(
        "telegram",
        "tg-token",
        &["knowledge:capture".to_owned(), "channels:act".to_owned()],
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

    // 1. A note: answered at once, then extracted in the background.
    let (status, captured) = call(
        &app,
        post_json("/v1/capture", &pat, json!({ "text": NOTE })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{captured}");
    let source = captured["source_id"].as_str().unwrap().to_owned();
    let (_, pending) = call(
        &app,
        get_json(&format!("/v1/sources/{source}/status"), &pat),
    )
    .await;
    assert_eq!(pending["state"], "queued", "not dispatched yet: {pending}");
    dispatch.drain().await.unwrap();

    let (_, status_now) = call(
        &app,
        get_json(&format!("/v1/sources/{source}/status"), &pat),
    )
    .await;
    assert_eq!(status_now["state"], "done", "{status_now}");
    let proposal = &status_now["proposals"][0];
    assert_eq!(
        proposal["state"], "accepted",
        "the person's own new fact: {proposal}"
    );
    assert_eq!(proposal["author"], format!("user:{alice}/via/extract"));

    let (_, list) = call(&app, get_json("/v1/assertions?kind=commitment", &pat)).await;
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    let assertion = &list[0];
    assert_eq!(assertion["slots"]["due"], "2026-11-14");
    let owner = assertion["slots"]["owner"].as_str().unwrap();
    assert_eq!(
        assertion["subjects"],
        json!([owner]),
        "owner and subject are the new entity"
    );
    let (_, entity) = call(&app, get_json(&format!("/v1/entities/{owner}"), &pat)).await;
    assert_eq!(entity["name"], "Оля", "{entity}");

    // 2. Another client asks in English: found by meaning.
    let (status, hits) = call(
        &app,
        get_json("/v1/search?q=release%20deadline%20in%20November", &pat),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hits}");
    assert_eq!(hits[0]["text"], "Релиз 2.4 — 14 ноября", "{hits}");
    assert_eq!(hits[0]["matched"], "meaning");

    // A Telegram account is linked by a one-time code.
    let (status, code) = call(
        &app,
        post_json("/v1/channels/telegram/link-codes", &pat, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{code}");
    let (status, linked) = call(
        &app,
        post_json(
            "/v1/channels/telegram/links",
            "tg-token",
            json!({ "code": code["code"], "external_id": "42", "label": "@alice" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["email"], "alice@example.com");
    let (status, used) = call(
        &app,
        post_json(
            "/v1/channels/telegram/links",
            "tg-token",
            json!({ "code": code["code"], "external_id": "43" }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "codes work once: {used}"
    );

    // Strangers cannot write through the bot.
    let (status, refused) = call(&app, voice("tg-token", "99")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(refused["type"], "https://frood.dev/problems/not-linked");

    // 3. A voice message from the linked account: transcribed, then extracted into a conflict.
    let (status, captured) = call(&app, voice("tg-token", "42")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{captured}");
    assert_eq!(captured["file"]["content_type"], "audio/ogg");
    let voice_source = captured["source_id"].as_str().unwrap().to_owned();
    // Only the transcription: its event is not dispatched yet, so the source is not done.
    dispatch.dispatch_pending().await.unwrap();
    dispatch.work_one().await.unwrap();
    let (_, midway) = call(
        &app,
        get_json(&format!("/v1/sources/{voice_source}/status"), &pat),
    )
    .await;
    assert_eq!(
        midway["state"], "queued",
        "extraction is still to come: {midway}"
    );
    dispatch.drain().await.unwrap();

    let (_, source_now) = call(&app, get_json(&format!("/v1/sources/{voice_source}"), &pat)).await;
    assert_eq!(source_now["content"], VOICE_TRANSCRIPT);
    assert_eq!(
        source_now["captured_by"],
        format!("user:{alice}/via/telegram")
    );
    let (_, status_now) = call(
        &app,
        get_json(&format!("/v1/sources/{voice_source}/status"), &pat),
    )
    .await;
    assert_eq!(status_now["state"], "done", "{status_now}");
    let jobs: Vec<&str> = status_now["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["job"].as_str().unwrap())
        .collect();
    assert_eq!(jobs, ["transcribe", "extract"]);
    let conflict = &status_now["proposals"][0];
    assert_eq!(
        conflict["state"], "open",
        "a contradiction waits for a person: {conflict}"
    );
    assert_eq!(conflict["risk"], "conflict");
    let id = assertion["assertion_id"].as_str().unwrap();
    let (_, contested) = call(&app, get_json(&format!("/v1/assertions/{id}"), &pat)).await;
    assert_eq!(contested["flags"], json!(["contested"]), "{contested}");
}
