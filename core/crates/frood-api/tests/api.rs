//! The `/v1` Ledger routes over a real PostgreSQL (FROOD_TEST_DATABASE_URL; skipped without it),
//! called with personal tokens.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use frood_api::router;
use frood_auth::{Auth, AuthSettings};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Tokens {
    /// Alice's personal token with every scope but admin.
    alice: String,
    /// Only `knowledge:read`.
    reader: String,
}

async fn app(name: &str) -> Option<(Router, Tokens)> {
    app_in(name, "{uses: database}", std::path::Path::new(".")).await
}

/// An app whose files go to the `objects` store, with `base` as the config's folder.
async fn app_in(name: &str, objects: &str, base: &std::path::Path) -> Option<(Router, Tokens)> {
    let url = frood_store::testing::database_url()?;
    let pool = frood_store::testing::fresh_database(&url, name)
        .await
        .expect("test database");
    let config = frood_config::parse(
        // The policies these tests were written against; docs/27's own tests are elsewhere.
        &format!("version: 1\ninstallation: {{name: test}}\nstorage: {{uses: postgres, url: 'postgres://unused', objects: {objects}}}\ndeploy: {{server: host}}\n\
                  policies: {{auto_accept_own_changes: false, accept_agent_findings: false}}\n"),
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap()
    .config;
    let auth = Auth::new(pool.clone(), AuthSettings::from_config(&config));
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    let all: Vec<String> = frood_auth::scope::ALL
        .iter()
        .filter(|s| **s != frood_auth::scope::ADMIN)
        .map(|s| (*s).to_owned())
        .collect();
    let (_, alice_token) = auth
        .create_personal_token(alice, "test", &all, None)
        .await
        .unwrap();
    let (_, reader) = auth
        .create_personal_token(alice, "reader", &["knowledge:read".to_owned()], Some(1))
        .await
        .unwrap();
    let app = router(frood_api::assemble(&config, base, pool, |_| None).state);
    Some((
        app,
        Tokens {
            alice: alice_token,
            reader,
        },
    ))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value, content_type)
}

fn commitment(text: &str, due: &str, said_at: &str, evidence: &Value) -> Value {
    json!({
        "kind": "commitment",
        "text": text,
        "slots": { "due": due },
        "modality": "agreed",
        "said_at": said_at,
        "evidence": [evidence],
    })
}

#[tokio::test]
async fn ledger_routes_end_to_end() {
    let Some((app, tokens)) = app("api_ledger").await else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };

    let alice = Some(tokens.alice.as_str());

    // Every route needs a token.
    let note = json!({ "kind": "note", "content": "agreed with Olya: release 2.4 on Nov 14" });
    let (status, problem, content_type) =
        call(&app, "POST", "/v1/sources", None, Some(note.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(problem["type"], "https://frood.dev/problems/unauthorized");

    let (status, captured, _) = call(&app, "POST", "/v1/sources", alice, Some(note.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, again, _) = call(&app, "POST", "/v1/sources", alice, Some(note)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(captured["source"]["id"], again["source"]["id"]);
    let evidence = captured["source"]["evidence"][0]["id"].clone();

    // The user's own fact is accepted by policy.
    let (status, own, _) = call(
        &app,
        "POST",
        "/v1/proposals",
        alice,
        Some(json!({ "risk": "new", "operations": [{ "op": "create", "assertion": commitment("Release 2.4 on Nov 14", "2026-11-14", "2026-09-02T12:00:00Z", &evidence) }] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(own["state"], "accepted");

    let (status, list, _) = call(
        &app,
        "GET",
        "/v1/assertions?kind=commitment&slot.due.gte=2026-11-01&slot.due.lt=2026-12-01",
        alice,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let id = list[0]["assertion_id"].as_str().unwrap().to_owned();

    let (status, _, _) = call(&app, "GET", "/v1/assertions?colour=blue", alice, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("/v1/assertions/{}", uuid::Uuid::now_v7()),
        alice,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A read-only token reads but cannot propose.
    let reader = Some(tokens.reader.as_str());
    let (status, _, _) = call(&app, "GET", &format!("/v1/assertions/{id}"), reader, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, problem, _) = call(
        &app,
        "POST",
        "/v1/proposals",
        reader,
        Some(json!({ "risk": "new", "operations": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        problem["type"],
        "https://frood.dev/problems/insufficient-scope"
    );

    // An agent proposes a conflicting change with two alternatives; the user picks one.
    let conflict = json!({
        "risk": "conflict",
        "base": [{ "assertion_id": id, "version": 1 }],
        "alternatives": [
            { "key": "move", "operations": [{ "op": "supersede", "assertion_id": id, "assertion": commitment("Release 2.4 on Nov 28", "2026-11-28", "2026-10-10T12:00:00Z", &evidence) }] },
            { "key": "keep", "operations": [] }
        ]
    });
    let (status, opened, _) =
        call(&app, "POST", "/v1/proposals", alice, Some(conflict.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(opened["state"], "open");
    let (_, contested, _) = call(&app, "GET", &format!("/v1/assertions/{id}"), alice, None).await;
    assert_eq!(contested["flags"], json!(["contested"]));

    let competing = call(&app, "POST", "/v1/proposals", alice, Some(conflict))
        .await
        .1;
    let verdict_path = format!("/v1/proposals/{}/verdicts", opened["id"].as_str().unwrap());
    let (status, decided, _) = call(
        &app,
        "POST",
        &verdict_path,
        alice,
        Some(json!({ "decision": "accept", "alternative": "move" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(decided["state"], "accepted");

    let (_, oct1, _) = call(
        &app,
        "GET",
        &format!("/v1/assertions/{id}?valid_at=2026-10-01"),
        alice,
        None,
    )
    .await;
    assert_eq!(oct1["slots"]["due"], "2026-11-14");
    let (_, nov, _) = call(
        &app,
        "GET",
        &format!("/v1/assertions/{id}?valid_at=2026-11-01"),
        alice,
        None,
    )
    .await;
    assert_eq!(nov["slots"]["due"], "2026-11-28");
    let (_, history, _) = call(
        &app,
        "GET",
        &format!("/v1/assertions/{id}/history"),
        alice,
        None,
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 3);

    // The competing proposal was built on version 1: accepting it now is a conflict.
    let competing_path = format!(
        "/v1/proposals/{}/verdicts",
        competing["id"].as_str().unwrap()
    );
    let (status, stale, _) = call(
        &app,
        "POST",
        &competing_path,
        alice,
        Some(json!({ "decision": "accept", "alternative": "move" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["type"], "https://frood.dev/problems/stale-base");
    assert_eq!(stale["current_version"], 3);

    // Idempotency-Key makes a retried proposal return the first one.
    let retry = json!({ "risk": "new", "operations": [{ "op": "create", "assertion": commitment("Docs freeze", "2026-11-10", "2026-10-01T12:00:00Z", &evidence) }] });
    let request = |body: &Value| {
        Request::post("/v1/proposals")
            .header("authorization", format!("Bearer {}", tokens.alice))
            .header("idempotency-key", "extract:note-1:0")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let first = app
        .clone()
        .oneshot(request(&retry))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let second = app
        .clone()
        .oneshot(request(&retry))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let (first, second): (Value, Value) = (
        serde_json::from_slice(&first).unwrap(),
        serde_json::from_slice(&second).unwrap(),
    );
    assert_eq!(first["id"], second["id"]);
    // Sent with Alice's personal token, it is her own new fact: accepted by policy.
    assert_eq!(first["state"], "accepted");

    let (_, open, _) = call(&app, "GET", "/v1/proposals?state=open", alice, None).await;
    assert_eq!(open.as_array().unwrap().len(), 0);

    let (_, events, _) = call(&app, "GET", "/v1/journal?after=0&limit=500", alice, None).await;
    let types: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    for expected in [
        "source.captured",
        "proposal.opened",
        "proposal.verdict",
        "assertion.created",
        "assertion.superseded",
        "proposal.stale",
    ] {
        assert!(
            types.contains(&expected),
            "journal lacks {expected}: {types:?}"
        );
    }
}

/// Spheres over HTTP (docs/26 §3): the list, a filter, and a person's move accepted at once.
#[tokio::test]
async fn spheres_list_filter_and_move() {
    let Some((app, tokens)) = app("api_spheres").await else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let alice = Some(tokens.alice.as_str());

    let (status, spheres, _) = call(&app, "GET", "/v1/spheres", alice, None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = spheres
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["tech", "business", "personal"]);
    assert_eq!(spheres[0]["built_in"], true);
    assert!(spheres[0]["name"].is_null(), "clients name built-in ones");

    let note = json!({ "kind": "note", "content": "Atlas moves to Postgres 18" });
    let (_, captured, _) = call(&app, "POST", "/v1/sources", alice, Some(note)).await;
    let evidence = captured["source"]["evidence"][0]["id"].clone();
    let mut fact = commitment(
        "Atlas moves to Postgres 18",
        "2026-11-14",
        "2026-09-02T12:00:00Z",
        &evidence,
    );
    fact["sphere"] = json!("family");
    let (status, problem, _) = call(
        &app,
        "POST",
        "/v1/proposals",
        alice,
        Some(json!({ "risk": "new", "operations": [{ "op": "create", "assertion": fact }] })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");

    fact["sphere"] = json!("tech");
    let (status, own, _) = call(
        &app,
        "POST",
        "/v1/proposals",
        alice,
        Some(json!({ "risk": "new", "operations": [{ "op": "create", "assertion": fact }] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(own["state"], "accepted");

    let (_, tech, _) = call(&app, "GET", "/v1/assertions?sphere=tech", alice, None).await;
    assert_eq!(tech.as_array().unwrap().len(), 1);
    assert_eq!(tech[0]["sphere"], "tech");
    let id = tech[0]["assertion_id"].as_str().unwrap().to_owned();

    let path = format!("/v1/assertions/{id}/sphere");
    let (status, moved, _) = call(
        &app,
        "PUT",
        &path,
        alice,
        Some(json!({ "sphere": "business" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["state"], "accepted");
    assert_eq!(moved["verdicts"][0]["decided_by"], "policy:own-sphere");
    let (_, business, _) = call(&app, "GET", "/v1/assertions?sphere=business", alice, None).await;
    assert_eq!(business.as_array().unwrap().len(), 1);
    let (_, now, _) = call(&app, "GET", "/v1/now?sphere=business", alice, None).await;
    assert!(
        now["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["assertion"]["assertion_id"] == id.as_str()),
        "{now}"
    );
    assert_eq!(
        now["changes"].as_array().unwrap().len(),
        1,
        "the move itself is not news: {now}"
    );

    let (status, _, _) = call(
        &app,
        "PUT",
        &path,
        Some(tokens.reader.as_str()),
        Some(json!({ "sphere": "tech" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A change waiting in the Inbox is accepted into the sphere the person picked.
    let (_, current, _) = call(&app, "GET", &format!("/v1/assertions/{id}"), alice, None).await;
    let change = commitment(
        "Atlas moves to Postgres 18 in December",
        "2026-12-01",
        "2026-09-03T12:00:00Z",
        &evidence,
    );
    let (status, waiting, _) = call(
        &app,
        "POST",
        "/v1/proposals",
        alice,
        Some(json!({ "risk": "change",
                     "base": [{ "assertion_id": id, "version": current["version"] }],
                     "operations": [{ "op": "supersede", "assertion_id": id, "assertion": change }] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(waiting["state"], "open");
    let (_, inbox, _) = call(&app, "GET", "/v1/inbox", alice, None).await;
    let item = inbox
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["item"]["data"]["proposal_id"] == waiting["id"])
        .unwrap_or_else(|| panic!("{inbox}"));
    assert_eq!(
        item["content"]["alternatives"][0]["changes"][0]["sphere"],
        "business"
    );
    let item_id = item["item"]["id"].as_str().unwrap();
    let (status, acted, _) = call(
        &app,
        "POST",
        &format!("/v1/inbox/{item_id}/act"),
        alice,
        Some(json!({ "action": "accept", "sphere": "personal" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{acted}");
    let (_, after, _) = call(&app, "GET", &format!("/v1/assertions/{id}"), alice, None).await;
    assert_eq!(after["sphere"], "personal");
    assert_eq!(after["text"], "Atlas moves to Postgres 18 in December");
}

/// A raw request: a body of any type, the answer's bytes and headers.
async fn raw(
    app: &Router,
    method: &str,
    path: &str,
    token: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"));
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

fn multipart(text: &str, filename: &str, content_type: &str, data: &[u8]) -> (String, Vec<u8>) {
    let boundary = "frood-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"text\"\r\n\r\n{text}\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Attachments over HTTP (docs/26 §2): a photo and a link on knowledge, the file to show, a
/// page that would run downloads instead, one taken off, one on a change that waits.
#[tokio::test]
async fn attachments_on_knowledge_and_changes() {
    // Files in a folder, as on a server.
    let folder = tempfile::tempdir().unwrap();
    let Some((app, tokens)) = app_in("api_attachments", "{uses: disk}", folder.path()).await else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let token = tokens.alice.as_str();
    let alice = Some(token);
    let note = json!({ "kind": "note", "content": "Atlas demo on Friday" });
    let (_, captured, _) = call(&app, "POST", "/v1/sources", alice, Some(note)).await;
    let evidence = captured["source"]["evidence"][0]["id"].clone();
    let fact = commitment(
        "Atlas demo on Friday",
        "2026-09-04",
        "2026-09-02T12:00:00Z",
        &evidence,
    );
    call(
        &app,
        "POST",
        "/v1/proposals",
        alice,
        Some(json!({ "risk": "new", "operations": [{ "op": "create", "assertion": fact }] })),
    )
    .await;
    let (_, list, _) = call(&app, "GET", "/v1/assertions?kind=commitment", alice, None).await;
    let id = list[0]["assertion_id"].as_str().unwrap().to_owned();
    let path = format!("/v1/assertions/{id}/attachments");

    let photo = [0xff, 0xd8, 0xff, 0xe0, 1, 2, 3];
    let (content_type, body) = multipart("the demo board", "board.jpg", "image/jpeg", &photo);
    let (status, _, bytes) = raw(&app, "POST", &path, token, Some(&content_type), body).await;
    let attached: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::CREATED, "{attached}");
    assert_eq!(attached[0]["kind"], "photo");
    assert_eq!(attached[0]["text"], "the demo board");
    let source = attached[0]["source_id"].as_str().unwrap().to_owned();

    let (status, link, _) = call(
        &app,
        "POST",
        &path,
        alice,
        Some(json!({ "uri": "https://docs.example/demo", "title": "Demo plan" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    assert_eq!(link[0]["kind"], "link");
    let (_, all, _) = call(&app, "GET", &path, alice, None).await;
    assert_eq!(all.as_array().unwrap().len(), 2);

    // The picture opens in place.
    let (status, headers, bytes) = raw(
        &app,
        "GET",
        &format!("/v1/sources/{source}/file"),
        token,
        None,
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/jpeg");
    assert_eq!(bytes, photo);
    let objects = folder.path().join(".frood/objects");
    let kept: Vec<_> = std::fs::read_dir(&objects)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "the photo is in the folder, not in the database"
    );
    // A page someone attached downloads, never runs.
    let (content_type, body) =
        multipart("", "page.html", "text/html", b"<script>alert(1)</script>");
    let (_, _, bytes) = raw(&app, "POST", &path, token, Some(&content_type), body).await;
    let page: Value = serde_json::from_slice(&bytes).unwrap();
    let page_source = page[0]["source_id"].as_str().unwrap();
    let (_, headers, _) = raw(
        &app,
        "GET",
        &format!("/v1/sources/{page_source}/file"),
        token,
        None,
        vec![],
    )
    .await;
    assert_eq!(headers["content-type"], "application/octet-stream");
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );

    // Taken off.
    let page_id = page[0]["id"].as_str().unwrap();
    let (status, _, _) = call(
        &app,
        "DELETE",
        &format!("/v1/attachments/{page_id}"),
        alice,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, all, _) = call(&app, "GET", &path, alice, None).await;
    assert_eq!(all.as_array().unwrap().len(), 2);

    // A change that waits shows what is attached to it and to the knowledge it changes.
    let (_, current, _) = call(&app, "GET", &format!("/v1/assertions/{id}"), alice, None).await;
    let change = commitment(
        "Atlas demo moves to Monday",
        "2026-09-07",
        "2026-09-03T12:00:00Z",
        &evidence,
    );
    let (_, waiting, _) = call(
        &app,
        "POST",
        "/v1/proposals",
        alice,
        Some(json!({ "risk": "change",
                     "base": [{ "assertion_id": id, "version": current["version"] }],
                     "operations": [{ "op": "supersede", "assertion_id": id, "assertion": change }] })),
    )
    .await;
    let pid = waiting["id"].as_str().unwrap();
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/v1/proposals/{pid}/attachments"),
        alice,
        Some(json!({ "uri": "https://chat.example/thread/42", "title": "Thread: demo moves" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, inbox, _) = call(&app, "GET", "/v1/inbox", alice, None).await;
    let item = inbox
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["item"]["data"]["proposal_id"] == pid)
        .unwrap_or_else(|| panic!("{inbox}"));
    let titles: Vec<&str> = item["content"]["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["title"].as_str())
        .collect();
    assert!(
        titles.contains(&"Thread: demo moves") && titles.contains(&"Demo plan"),
        "{titles:?}"
    );
}
