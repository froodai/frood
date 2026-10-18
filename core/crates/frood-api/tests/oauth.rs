//! Sign-in end to end over a real PostgreSQL (FROOD_TEST_DATABASE_URL; skipped without it):
//! an MCP client registers, a person signs in through a mock OpenID Connect provider and
//! consents, the client calls `/mcp` with its token; refresh rotation and reuse detection;
//! authorization codes are single-use.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use frood_api::{AppState, router};
use frood_auth::{Auth, AuthSettings};
use frood_ledger::{Ledger, PolicyConfig};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use url::Url;

const ISSUER: &str = "http://127.0.0.1:8080";
const REDIRECT: &str = "http://127.0.0.1:33333/callback";

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!(
                "not JSON ({}): {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn location(&self) -> Url {
        let location = self
            .headers
            .get(header::LOCATION)
            .unwrap_or_else(|| panic!("no Location ({}): {}", self.status, self.text()))
            .to_str()
            .unwrap();
        Url::parse(ISSUER).unwrap().join(location).unwrap()
    }

    fn header(&self, name: header::HeaderName) -> String {
        self.headers
            .get(name)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default()
    }
}

fn query(url: &Url, key: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| panic!("{url} has no {key}"))
}

fn path_and_query(url: &Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    }
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

async fn get_with(app: &Router, path: &str, cookie: Option<&str>) -> Reply {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    send(app, request.body(Body::empty()).unwrap()).await
}

async fn post_form(app: &Router, path: &str, form: &[(&str, &str)], cookie: Option<&str>) -> Reply {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    let mut request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ORIGIN, ISSUER);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    send(app, request.body(Body::from(body)).unwrap()).await
}

async fn mcp(app: &Router, token: &str, id: u64, method: &str, params: Value) -> Reply {
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let request = Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:8080")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(app, request).await
}

async fn call_tool(app: &Router, token: &str, name: &str, arguments: Value) -> Value {
    let reply = mcp(
        app,
        token,
        7,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    reply.json()["result"].clone()
}

fn tool_text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn pkce() -> (String, String) {
    let verifier = URL_SAFE_NO_PAD.encode([42u8; 40]);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// A minimal OpenID Connect provider: discovery, JWKS with an Ed25519 key made from a fixed
/// seed, and a token endpoint that checks the client, the code and PKCE.
#[derive(Clone)]
struct Idp {
    base: String,
    email: String,
    key: Arc<EncodingKey>,
    nonce: Arc<Mutex<Option<String>>>,
    challenge: Arc<Mutex<Option<String>>>,
}

impl Idp {
    async fn start(email: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seed = [7u8; 32];
        // PKCS#8 v1 wrapping of an Ed25519 seed (RFC 8410).
        let mut der = vec![
            0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22,
            0x04, 0x20,
        ];
        der.extend_from_slice(&seed);
        let public = ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes();
        let idp = Self {
            base: base.clone(),
            email: email.to_owned(),
            key: Arc::new(EncodingKey::from_ed_der(&der)),
            nonce: Arc::default(),
            challenge: Arc::default(),
        };
        let jwks = json!({ "keys": [{
            "kty": "OKP", "crv": "Ed25519", "x": URL_SAFE_NO_PAD.encode(public),
            "kid": "k1", "alg": "EdDSA", "use": "sig",
        }] });
        let discovery = json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "jwks_uri": format!("{base}/jwks"),
        });
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || async move { Json(discovery) }),
            )
            .route("/jwks", get(move || async move { Json(jwks) }))
            .route("/token", post(idp_token))
            .with_state(idp.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        idp
    }
}

async fn idp_token(
    State(idp): State<Idp>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
    let field = |k: &str| form.get(k).map(String::as_str);
    if field("code") != Some("idp-code")
        || field("client_id") != Some("frood-test")
        || field("client_secret") != Some("s3cret")
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let verifier = field("code_verifier").ok_or(StatusCode::BAD_REQUEST)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if idp.challenge.lock().unwrap().as_deref() != Some(challenge.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let now = chrono::Utc::now().timestamp();
    let claims = json!({
        "iss": idp.base, "aud": "frood-test", "sub": "42", "email": idp.email,
        "email_verified": true, "name": "Alice", "nonce": idp.nonce.lock().unwrap().clone(),
        "iat": now, "exp": now + 300,
    });
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("k1".into());
    let id_token = jsonwebtoken::encode(&header, &claims, &idp.key).unwrap();
    Ok(Json(
        json!({ "access_token": "upstream", "token_type": "Bearer", "id_token": id_token }),
    ))
}

async fn setup(name: &str, idp: &Idp) -> Option<(Router, Auth)> {
    let url = frood_store::testing::database_url()?;
    let pool = frood_store::testing::fresh_database(&url, name)
        .await
        .expect("test database");
    let raw = format!(
        "version: 1\ninstallation: {{name: test}}\nstorage: {{uses: postgres, url: 'postgres://unused'}}\n\
         deploy: {{server: host}}\nauth:\n  allow: [alice@example.com]\n  providers:\n    - {{kind: oidc, id: corp, \
         label: Corp SSO, issuer: '{}', client_id: frood-test, client_secret: s3cret}}\n",
        idp.base
    );
    let config = frood_config::parse(
        &raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .expect("config")
    .config;
    let auth = Auth::new(pool.clone(), AuthSettings::from_config(&config));
    let app = router(AppState {
        ledger: Ledger::new(pool, PolicyConfig::default()),
        auth: auth.clone(),
        installation: "test".into(),
    });
    Some((app, auth))
}

/// Runs the browser part: authorize → provider → callback → consent. Returns the code.
async fn authorize(
    app: &Router,
    idp: &Idp,
    client_id: &str,
    challenge: &str,
    grant: &[&str],
) -> String {
    let authorize = format!(
        "/oauth/authorize?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", client_id),
                ("redirect_uri", REDIRECT),
                ("code_challenge", challenge),
                ("code_challenge_method", "S256"),
                (
                    "scope",
                    "knowledge:read knowledge:capture knowledge:propose"
                ),
                ("state", "client-state"),
                ("resource", &format!("{ISSUER}/mcp")),
            ])
            .finish()
    );
    let reply = get_with(app, &authorize, None).await;
    assert!(reply.status.is_redirection(), "{}", reply.text());
    // One provider: the chooser goes straight to it.
    let reply = get_with(app, &path_and_query(&reply.location()), None).await;
    let reply = get_with(app, &path_and_query(&reply.location()), None).await;
    let upstream = reply.location();
    assert!(
        upstream
            .as_str()
            .starts_with(&format!("{}/authorize", idp.base))
    );
    assert_eq!(query(&upstream, "client_id"), "frood-test");
    assert_eq!(query(&upstream, "code_challenge_method"), "S256");
    *idp.nonce.lock().unwrap() = Some(query(&upstream, "nonce"));
    *idp.challenge.lock().unwrap() = Some(query(&upstream, "code_challenge"));

    let callback = format!(
        "/oauth/callback/corp?code=idp-code&state={}",
        query(&upstream, "state")
    );
    let reply = get_with(app, &callback, None).await;
    assert!(reply.status.is_redirection(), "{}", reply.text());
    let cookie = reply.header(header::SET_COOKIE);
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"));
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let consent = reply.location();
    assert_eq!(consent.path(), "/oauth/consent");

    let page = get_with(app, &path_and_query(&consent), Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(html.contains("alice@example.com") && html.contains("knowledge:decide"));

    let tx = query(&consent, "tx");
    let mut form = vec![("tx", tx.as_str()), ("decision", "allow")];
    form.extend(grant.iter().map(|s| ("scope", *s)));
    let reply = post_form(app, "/oauth/consent", &form, Some(&cookie)).await;
    let back = reply.location();
    assert!(back.as_str().starts_with(REDIRECT));
    assert_eq!(query(&back, "state"), "client-state");
    assert_eq!(query(&back, "iss"), ISSUER);
    query(&back, "code")
}

#[tokio::test]
async fn mcp_client_signs_in_and_uses_tools() {
    let idp = Idp::start("Alice@Example.com").await;
    let Some((app, _auth)) = setup("oauth_flow", &idp).await else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };

    // Discovery, as an MCP client does it.
    let reply = send(
        &app,
        Request::post("/mcp")
            .header(header::HOST, "127.0.0.1:8080")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    let challenge = reply.header(header::WWW_AUTHENTICATE);
    assert!(challenge.contains(&format!(
        "resource_metadata=\"{ISSUER}/.well-known/oauth-protected-resource/mcp\""
    )));
    let metadata = get_with(&app, "/.well-known/oauth-protected-resource/mcp", None)
        .await
        .json();
    assert_eq!(metadata["resource"], format!("{ISSUER}/mcp"));
    assert_eq!(metadata["authorization_servers"], json!([ISSUER]));
    let server = get_with(&app, "/.well-known/oauth-authorization-server", None)
        .await
        .json();
    assert_eq!(
        server["authorization_response_iss_parameter_supported"],
        true
    );

    // Dynamic client registration.
    let registration = json!({ "client_name": "Test client", "redirect_uris": [REDIRECT] });
    let reply = send(
        &app,
        Request::post("/oauth/register")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(registration.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let client_id = reply.json()["client_id"].as_str().unwrap().to_owned();

    // A redirect URI that was not registered is refused on the page, not redirected to.
    let reply = get_with(
        &app,
        &format!("/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=https://evil.example/cb"),
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);

    // Sign in and consent. `admin` was not requested, so ticking it grants nothing.
    let (verifier, challenge) = pkce();
    let code = authorize(
        &app,
        &idp,
        &client_id,
        &challenge,
        &[
            "knowledge:read",
            "knowledge:capture",
            "knowledge:propose",
            "admin",
        ],
    )
    .await;

    let token_form = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", REDIRECT),
        ("client_id", client_id.as_str()),
        (
            "code_verifier",
            "wrong-verifier-wrong-verifier-wrong-verifier",
        ),
    ];
    let reply = post_form(&app, "/oauth/token", &token_form, None).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.json()["error"], "invalid_grant");

    let mut token_form = token_form;
    token_form[4].1 = &verifier;
    let reply = post_form(&app, "/oauth/token", &token_form, None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.header(header::CACHE_CONTROL), "no-store");
    let tokens = reply.json();
    assert_eq!(
        tokens["scope"],
        "knowledge:capture knowledge:propose knowledge:read"
    );
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    let refresh = tokens["refresh_token"].as_str().unwrap().to_owned();

    // The token is bound to /mcp; /v1 does not accept it.
    let reply = send(
        &app,
        Request::get("/v1/assertions")
            .header(header::AUTHORIZATION, format!("Bearer {access}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // MCP: handshake, the tool list follows the scopes, tools work.
    let init = mcp(
        &app,
        &access,
        1,
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }),
    )
    .await;
    assert_eq!(init.status, StatusCode::OK, "{}", init.text());
    assert_eq!(init.json()["result"]["serverInfo"]["name"], "frood");

    let list = mcp(&app, &access, 2, "tools/list", json!({})).await.json();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 9, "{names:?}");
    assert!(!names.contains(&"submit_verdict"));

    let captured = call_tool(
        &app,
        &access,
        "capture_note",
        json!({ "text": "Release 2.4 moves to Nov 28", "about": "release 2.4" }),
    )
    .await;
    let captured: Value = serde_json::from_str(&tool_text(&captured)).unwrap();
    assert!(captured["source_id"].is_string());

    let proposal = call_tool(
        &app,
        &access,
        "propose_change",
        json!({
            "rationale": "Olya said so in the chat",
            "source_text": "Olya: we agreed to release 2.4 on Nov 28",
            "operations": [{ "op": "create", "assertion": { "kind": "commitment", "text": "Release 2.4 on Nov 28", "modality": "agreed", "slots": { "due": "2026-11-28" } } }],
        }),
    )
    .await;
    assert_ne!(proposal["isError"], true, "{}", tool_text(&proposal));
    let proposal: Value = serde_json::from_str(&tool_text(&proposal)).unwrap();
    // The person told their client to remember it: their own new fact, accepted by policy,
    // with the client kept for the audit.
    assert_eq!(proposal["state"], "accepted");
    let author = proposal["author"].as_str().unwrap();
    assert!(
        author.starts_with("user:") && author.ends_with(&format!("/via/{client_id}")),
        "{author}"
    );

    // A change is not covered by the own-facts policy: it waits for a verdict.
    let proposal = call_tool(
        &app,
        &access,
        "propose_change",
        json!({
            "rationale": "maybe the date moved again",
            "source_text": "Olya: Nov 28 might slip",
            "risk": "change",
            "operations": [{ "op": "create", "assertion": { "kind": "status", "text": "Release 2.4 date at risk", "modality": "mentioned" } }],
        }),
    )
    .await;
    assert_ne!(proposal["isError"], true, "{}", tool_text(&proposal));
    let proposal: Value = serde_json::from_str(&tool_text(&proposal)).unwrap();
    assert_eq!(proposal["state"], "open");

    let inbox = call_tool(&app, &access, "list_inbox", json!({})).await;
    assert!(tool_text(&inbox).contains(proposal["id"].as_str().unwrap()));

    let refused = call_tool(
        &app,
        &access,
        "submit_verdict",
        json!({ "proposal_id": proposal["id"], "verdict": "accept" }),
    )
    .await;
    assert_eq!(refused["isError"], true);
    assert!(tool_text(&refused).contains("knowledge:decide"));

    // Refresh rotates; presenting a used refresh token revokes the whole family.
    let refresh_form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
        ("client_id", client_id.as_str()),
    ];
    let reply = post_form(&app, "/oauth/token", &refresh_form, None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let rotated = reply.json()["access_token"].as_str().unwrap().to_owned();
    assert_eq!(
        mcp(&app, &rotated, 3, "tools/list", json!({})).await.status,
        StatusCode::OK
    );

    let reply = post_form(&app, "/oauth/token", &refresh_form, None).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.json()["error"], "invalid_grant");
    assert_eq!(
        mcp(&app, &rotated, 4, "tools/list", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );

    // The authorization code was used once already.
    let reply = post_form(&app, "/oauth/token", &token_form, None).await;
    assert_eq!(reply.json()["error"], "invalid_grant");
}

#[tokio::test]
async fn people_outside_the_allow_list_cannot_sign_in() {
    let idp = Idp::start("mallory@example.org").await;
    let Some((app, _auth)) = setup("oauth_denied", &idp).await else {
        return;
    };
    let registration = json!({ "redirect_uris": [REDIRECT] });
    let reply = send(
        &app,
        Request::post("/oauth/register")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(registration.to_string()))
            .unwrap(),
    )
    .await;
    let client_id = reply.json()["client_id"].as_str().unwrap().to_owned();
    let (_, challenge) = pkce();
    let authorize = format!(
        "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri={REDIRECT}&code_challenge={challenge}&code_challenge_method=S256&state=s"
    );
    let reply = get_with(&app, &authorize, None).await;
    let reply = get_with(&app, &path_and_query(&reply.location()), None).await;
    let reply = get_with(&app, &path_and_query(&reply.location()), None).await;
    let upstream = reply.location();
    *idp.nonce.lock().unwrap() = Some(query(&upstream, "nonce"));
    *idp.challenge.lock().unwrap() = Some(query(&upstream, "code_challenge"));
    let reply = get_with(
        &app,
        &format!(
            "/oauth/callback/corp?code=idp-code&state={}",
            query(&upstream, "state")
        ),
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text());
    assert!(reply.header(header::SET_COOKIE).is_empty());
}
