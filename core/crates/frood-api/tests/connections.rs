//! Connecting sources from the console (docs/26 §4) over a real PostgreSQL
//! (FROOD_TEST_DATABASE_URL; skipped without it), against a fake MCP server.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use frood_api::router;
use frood_auth::{Auth, AuthSettings};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// An MCP server that wants `Bearer tok-123` and lists its tools as server-sent events.
async fn fake_mcp() -> String {
    let app = Router::new().route(
        "/mcp",
        post(|headers: HeaderMap, axum::Json(message): axum::Json<Value>| async move {
            if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Bearer tok-123") {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            match message["method"].as_str() {
                Some("initialize") => (
                    [("mcp-session-id", "s-1")],
                    axum::Json(json!({ "jsonrpc": "2.0", "id": message["id"],
                        "result": { "protocolVersion": "2025-06-18", "capabilities": { "tools": {} },
                                    "serverInfo": { "name": "tracker", "version": "1" } } })),
                )
                    .into_response(),
                Some("tools/list") => {
                    assert_eq!(headers["mcp-session-id"], "s-1");
                    let answer = json!({ "jsonrpc": "2.0", "id": message["id"], "result": { "tools": [
                        { "name": "search_issues", "description": "Finds issues.\nMore.", "annotations": { "readOnlyHint": true } },
                        { "name": "update_issue", "description": "Changes an issue.", "annotations": { "readOnlyHint": false } }
                    ] } });
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from(format!("event: message\ndata: {answer}\n\n")))
                        .unwrap()
                }
                _ => StatusCode::ACCEPTED.into_response(),
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"));
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn sources_connect_from_the_console() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "api_connections")
        .await
        .expect("test database");
    let config = frood_config::parse(
        "version: 1\ninstallation: {name: test}\nstorage: {uses: postgres, url: 'postgres://unused', objects: {uses: database}}\n\
         deploy: {server: host}\nsources:\n  wiki: {uses: mcp, server: 'https://wiki.example/mcp'}\n",
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap()
    .config;
    let key = {
        use age::secrecy::ExposeSecret;
        age::x25519::Identity::generate()
            .to_string()
            .expose_secret()
            .to_owned()
    };
    let auth = Auth::new(pool.clone(), AuthSettings::from_config(&config));
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    let all: Vec<String> = frood_auth::scope::ALL
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let (_, admin) = auth
        .create_personal_token(alice, "admin", &all, None)
        .await
        .unwrap();
    let (_, reader) = auth
        .create_personal_token(alice, "reader", &["knowledge:read".to_owned()], Some(1))
        .await
        .unwrap();
    let state = frood_api::assemble(&config, std::path::Path::new("."), pool.clone(), |name| {
        (name == "FROOD_VAULT_KEY").then(|| key.clone())
    })
    .state;
    let dispatch = state.dispatch.clone();
    let app = router(state);
    let server = fake_mcp().await;

    let (status, listed) = call(&app, "GET", "/v1/connections", &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["vault"], true);
    assert_eq!(listed["sources"][0]["name"], "wiki");
    assert_eq!(listed["sources"][0]["from"], "config");
    let (status, _) = call(&app, "GET", "/v1/connections", &reader, None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "changing sources needs admin"
    );

    // The check tells a wrong token from a right one, and lists the tools.
    let (status, wrong) = call(
        &app,
        "POST",
        "/v1/connections/check",
        &admin,
        Some(json!({ "server": server, "token": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        wrong["detail"]
            .as_str()
            .unwrap()
            .contains("refused the token"),
        "{wrong}"
    );
    let (status, checked) = call(
        &app,
        "POST",
        "/v1/connections/check",
        &admin,
        Some(json!({ "server": server, "token": "tok-123" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["server"], "tracker");
    assert_eq!(checked["tools"][0]["name"], "search_issues");
    assert_eq!(checked["tools"][0]["description"], "Finds issues.");
    assert_eq!(checked["tools"][0]["read_only"], true);
    assert_eq!(checked["tools"][1]["read_only"], false);

    // Connected: agents get it with the token; the list and the database never show it.
    let (status, saved) = call(
        &app,
        "PUT",
        "/v1/connections/tracker",
        &admin,
        Some(json!({
        "server": server, "token": "tok-123", "scope": "Issues changed in the last day",
        "tools": ["search_issues"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let source = dispatch.source("tracker").unwrap();
    assert_eq!(source.headers["Authorization"], "Bearer tok-123");
    assert_eq!(
        source.tools.as_deref(),
        Some(&["search_issues".to_owned()][..])
    );
    let (_, listed) = call(&app, "GET", "/v1/connections", &admin, None).await;
    assert!(!listed.to_string().contains("tok-123"), "{listed}");
    assert_eq!(listed["sources"][1]["from"], "console");
    assert_eq!(listed["sources"][1]["has_token"], true);
    let stored: Vec<u8> =
        frood_store::sqlx::query_scalar("SELECT sealed FROM vault WHERE name = 'source.tracker'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        !String::from_utf8_lossy(&stored).contains("tok-123"),
        "sealed, not plain"
    );
    let overlay: String = frood_store::sqlx::query_scalar(
        "SELECT data::text FROM config_overlay ORDER BY revision DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!overlay.contains("tok-123"), "{overlay}");

    // A change without a token keeps the stored one; the check can use it too.
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/connections/tracker",
        &admin,
        Some(json!({
        "server": server, "scope": "Issues changed this week" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        dispatch.source("tracker").unwrap().headers["Authorization"],
        "Bearer tok-123"
    );
    let (status, _) = call(
        &app,
        "POST",
        "/v1/connections/check",
        &admin,
        Some(json!({ "server": server, "name": "tracker" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // frood.yaml's source is changed only there.
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/connections/wiki",
        &admin,
        Some(json!({ "server": server })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Disconnected, with its token.
    let (status, _) = call(&app, "DELETE", "/v1/connections/tracker", &admin, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(dispatch.source("tracker").is_none());
    let left: i64 = frood_store::sqlx::query_scalar("SELECT count(*) FROM vault")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}
