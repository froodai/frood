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

/// An MCP server that signs people in itself (MCP authorization): it names its authorization
/// server, lets Frood register, takes PKCE, and gives a token that runs out at once.
async fn fake_oauth_mcp() -> String {
    use axum::extract::{Form, Query, State};
    use axum::routing::get;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Shared {
        base: String,
        challenge: Option<String>,
    }
    let shared: Arc<Mutex<Shared>> = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    shared.lock().unwrap().base = base.clone();
    let app = Router::new()
        .route("/mcp", post(|State(s): State<Arc<Mutex<Shared>>>, headers: HeaderMap, axum::Json(message): axum::Json<Value>| async move {
            let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or_default();
            if auth != "Bearer access-2" {
                let base = s.lock().unwrap().base.clone();
                return Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("www-authenticate", format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\""))
                    .body(Body::empty())
                    .unwrap();
            }
            let result = match message["method"].as_str() {
                Some("initialize") => json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": { "name": "notes" } }),
                Some("tools/list") => json!({ "tools": [{ "name": "search_pages", "annotations": { "readOnlyHint": true } }] }),
                _ => return StatusCode::ACCEPTED.into_response(),
            };
            axum::Json(json!({ "jsonrpc": "2.0", "id": message["id"], "result": result })).into_response()
        }))
        .route("/.well-known/oauth-protected-resource/mcp", get(|State(s): State<Arc<Mutex<Shared>>>| async move {
            let base = s.lock().unwrap().base.clone();
            axum::Json(json!({ "resource": format!("{base}/mcp"), "authorization_servers": [format!("{base}/auth")] }))
        }))
        .route("/.well-known/oauth-authorization-server/auth", get(|State(s): State<Arc<Mutex<Shared>>>| async move {
            let base = s.lock().unwrap().base.clone();
            axum::Json(json!({ "issuer": format!("{base}/auth"), "authorization_endpoint": format!("{base}/auth/authorize"),
                               "token_endpoint": format!("{base}/auth/token"), "registration_endpoint": format!("{base}/auth/register") }))
        }))
        .route("/auth/register", post(|axum::Json(body): axum::Json<Value>| async move {
            assert_eq!(body["token_endpoint_auth_method"], "none");
            axum::Json(json!({ "client_id": "client-1", "redirect_uris": body["redirect_uris"] }))
        }))
        .route("/auth/authorize", get(|State(s): State<Arc<Mutex<Shared>>>, Query(q): Query<std::collections::BTreeMap<String, String>>| async move {
            assert_eq!(q["code_challenge_method"], "S256");
            assert!(q["resource"].ends_with("/mcp"));
            s.lock().unwrap().challenge = Some(q["code_challenge"].clone());
            let back = format!("{}?code=code-1&state={}", q["redirect_uri"], q["state"]);
            Response::builder().status(StatusCode::FOUND).header("location", back).body(Body::empty()).unwrap()
        }))
        .route("/auth/token", post(|State(s): State<Arc<Mutex<Shared>>>, Form(f): Form<std::collections::BTreeMap<String, String>>| async move {
            use base64::Engine;
            use sha2::Digest;
            match f["grant_type"].as_str() {
                "authorization_code" => {
                    let verifier = &f["code_verifier"];
                    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
                    assert_eq!(Some(challenge), s.lock().unwrap().challenge.clone(), "PKCE");
                    assert_eq!(f["code"], "code-1");
                    axum::Json(json!({ "access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 1, "token_type": "Bearer" }))
                }
                _ => {
                    assert_eq!(f["refresh_token"], "refresh-1");
                    axum::Json(json!({ "access_token": "access-2", "expires_in": 3600, "token_type": "Bearer" }))
                }
            }
        }))
        .with_state(shared);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("{base}/mcp")
}

#[tokio::test]
async fn a_source_connects_by_signing_in() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "api_sign_in")
        .await
        .expect("test database");
    let config = frood_config::parse(
        "version: 1\ninstallation: {name: test}\nstorage: {uses: postgres, url: 'postgres://unused', objects: {uses: database}}\n\
         server: {public_url: 'https://frood.example/frood'}\ndeploy: {server: host}\n",
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
    let state = frood_api::assemble(&config, std::path::Path::new("."), pool.clone(), |name| {
        (name == "FROOD_VAULT_KEY").then(|| key.clone())
    })
    .state;
    let dispatch = state.dispatch.clone();
    let app = router(state);
    let server = fake_oauth_mcp().await;

    let (status, started) = call(
        &app,
        "POST",
        "/v1/connections/notes/sign-in",
        &admin,
        Some(json!({ "server": server, "scope": "Pages changed today" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let authorize = started["authorize_url"].as_str().unwrap();
    assert!(
        authorize.contains(
            "redirect_uri=https%3A%2F%2Ffrood.example%2Ffrood%2Fv1%2Fconnections%2Fsigned-in"
        ),
        "{authorize}"
    );

    // The person signs in and the server sends them back with a code.
    let back = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(authorize)
        .send()
        .await
        .unwrap();
    let location = back.headers()["location"].to_str().unwrap().to_owned();
    let query = location.split_once('?').unwrap().1;
    let request = Request::builder()
        .uri(format!("/v1/connections/signed-in?{query}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert!(response.status().is_redirection());
    assert_eq!(
        response.headers()["location"],
        "https://frood.example/frood/admin/sources?connected=notes"
    );

    // Connected, with no tools until a person chooses them.
    let (_, listed) = call(&app, "GET", "/v1/connections", &admin, None).await;
    let notes = &listed["sources"][0];
    assert_eq!(notes["name"], "notes");
    assert_eq!(notes["signed_in"], true);
    assert_eq!(notes["tools"], json!([]));
    assert!(!listed.to_string().contains("access-"), "{listed}");

    // The first token ran out at once: the next use refreshes it.
    let source = dispatch.signed_source("notes").await.unwrap().unwrap();
    assert_eq!(source.headers["Authorization"], "Bearer access-2");
    let (status, checked) = call(
        &app,
        "POST",
        "/v1/connections/check",
        &admin,
        Some(json!({ "server": server, "name": "notes" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["tools"][0]["name"], "search_pages");

    // A return with a state nobody started.
    let request = Request::builder()
        .uri("/v1/connections/signed-in?code=x&state=nobody")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert!(
        response.headers()["location"]
            .to_str()
            .unwrap()
            .contains("connect_error=")
    );
}

/// Jobs changed from the console (QY1 — b): a schedule, off and on, back to the default; checked
/// as frood.yaml is, and frood.yaml's own change wins.
#[tokio::test]
async fn jobs_change_from_the_console() {
    let Some(url) = frood_store::testing::database_url() else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "api_jobs")
        .await
        .expect("test database");
    let config = frood_config::parse(
        "version: 1\ninstallation: {name: test}\nstorage: {uses: postgres, url: 'postgres://unused', objects: {uses: database}}\n\
         deploy: {server: host}\njobs:\n  transcribe: {enabled: false}\n",
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
        .map(|s| (*s).to_owned())
        .collect();
    let (_, admin) = auth
        .create_personal_token(alice, "admin", &all, None)
        .await
        .unwrap();
    let state =
        frood_api::assemble(&config, std::path::Path::new("."), pool.clone(), |_| None).state;
    let dispatch = state.dispatch.clone();
    let app = router(state);
    let running = |name: &str| dispatch.jobs().iter().any(|j| j.name == name);
    let setting = |listed: &Value, name: &str| {
        listed["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == name)
            .unwrap()
            .clone()
    };

    let (status, listed) = call(&app, "GET", "/v1/jobs/settings", &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let digest = setting(&listed, "digest");
    assert_eq!(digest["from"], "default");
    assert_eq!(digest["running"], false);
    assert_eq!(setting(&listed, "transcribe")["from"], "config");
    assert_eq!(setting(&listed, "extract")["on_events"], true);
    assert!(!running("digest"));

    // A schedule from the console: the digest runs, the next tick uses it.
    let (status, changed) = call(
        &app,
        "PUT",
        "/v1/jobs/digest",
        &admin,
        Some(json!({
        "schedules": [{ "schedule": "30 8 * * 1-5", "timezone": "Europe/Belgrade" }] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert!(changed["why"].is_null(), "{changed}");
    assert!(running("digest"));
    let (_, listed) = call(&app, "GET", "/v1/jobs/settings", &admin, None).await;
    let digest = setting(&listed, "digest");
    assert_eq!(digest["from"], "console");
    assert_eq!(digest["schedules"][0]["schedule"], "30 8 * * 1-5");

    // Checked as frood.yaml is: a schedule that is not cron, a job that starts on events, a job
    // frood.yaml changes.
    let (status, bad) = call(
        &app,
        "PUT",
        "/v1/jobs/digest",
        &admin,
        Some(json!({ "schedules": [{ "schedule": "every morning" }] })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    assert!(running("digest"), "a refused change changes nothing");
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/jobs/extract",
        &admin,
        Some(json!({ "schedules": [{ "schedule": "0 3 * * *" }] })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/jobs/transcribe",
        &admin,
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Off, then back to the default.
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/jobs/digest",
        &admin,
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!running("digest"));
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/jobs/digest",
        &admin,
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(running("digest"));
    let (status, _) = call(&app, "DELETE", "/v1/jobs/digest", &admin, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!running("digest"));
}
