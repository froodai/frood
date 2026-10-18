//! The `/v1` Ledger routes over a real PostgreSQL (FROOD_TEST_DATABASE_URL; skipped without it),
//! called with personal tokens.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use frood_api::{AppState, router};
use frood_auth::{Auth, AuthSettings};
use frood_ledger::{Ledger, PolicyConfig};
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
    let url = frood_store::testing::database_url()?;
    let pool = frood_store::testing::fresh_database(&url, name)
        .await
        .expect("test database");
    let config = frood_config::parse(
        "version: 1\ninstallation: {name: test}\nstorage: {uses: postgres, url: 'postgres://unused'}\ndeploy: {server: host}\n",
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
    let app = router(AppState {
        ledger: Ledger::new(pool, PolicyConfig::default()),
        auth,
        installation: "test".into(),
    });
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
