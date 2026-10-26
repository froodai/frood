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
use frood_api::router;
use frood_auth::{Auth, AuthSettings};
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
    setup_with(name, idp, "deploy: {server: host}").await
}

async fn setup_with(name: &str, idp: &Idp, deploy: &str) -> Option<(Router, Auth)> {
    let url = frood_store::testing::database_url()?;
    let pool = frood_store::testing::fresh_database(&url, name)
        .await
        .expect("test database");
    let raw = format!(
        "version: 1\ninstallation: {{name: test}}\nstorage: {{uses: postgres, url: 'postgres://unused', objects: {{uses: database}}}}\n\
         {deploy}\nauth:\n  allow: [alice@example.com]\n  providers:\n    - {{kind: oidc, id: corp, \
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
    let app = router(frood_api::assemble(&config, std::path::Path::new("."), pool, |_| None).state);
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
    let request = Authorize {
        client_id,
        redirect: REDIRECT,
        scope: "knowledge:read knowledge:capture knowledge:propose",
        resource: &format!("{ISSUER}/mcp"),
    };
    authorize_with(app, idp, &request, challenge, grant).await
}

struct Authorize<'a> {
    client_id: &'a str,
    redirect: &'a str,
    scope: &'a str,
    resource: &'a str,
}

async fn authorize_with(
    app: &Router,
    idp: &Idp,
    request: &Authorize<'_>,
    challenge: &str,
    grant: &[&str],
) -> String {
    let authorize = format!(
        "/oauth/authorize?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", request.client_id),
                ("redirect_uri", request.redirect),
                ("code_challenge", challenge),
                ("code_challenge_method", "S256"),
                ("scope", request.scope),
                ("state", "client-state"),
                ("resource", request.resource),
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
    assert!(back.as_str().starts_with(request.redirect));
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
    assert_eq!(
        metadata["scopes_supported"],
        json!([
            "knowledge:read",
            "knowledge:capture",
            "knowledge:propose",
            "knowledge:decide"
        ])
    );
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
    assert_eq!(names.len(), 11, "{names:?}");
    assert!(!names.contains(&"submit_verdict"));
    // A person answers questions; only a run asks them.
    assert!(names.contains(&"answer_question"));
    assert!(!names.contains(&"ask_user"));

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

/// The admin panel the core serves signs in as the built-in client `frood-admin`, which may come
/// back only to this installation's `/admin/`; its token reads health, logs and metrics.
#[tokio::test]
async fn admin_panel_signs_in_with_its_own_client() {
    let idp = Idp::start("alice@example.com").await;
    let Some((app, _auth)) = setup("oauth_admin", &idp).await else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    let reply = get_with(&app, "/admin", None).await;
    assert_eq!(reply.location().path(), "/admin/");
    let page = get_with(&app, "/admin/", None).await;
    assert_eq!(page.status, StatusCode::OK);
    // The built console learns its place from the server; without a build, the old page.
    let html = page.text();
    assert!(
        html.contains("<meta name=\"frood-base\" content=\"\">") || html.contains("frood-admin"),
        "{html}"
    );
    assert!(
        page.header(header::CONTENT_SECURITY_POLICY)
            .contains("frame-ancestors 'none'")
    );
    // A link to a section opens the same page.
    let deep = get_with(&app, "/admin/inbox/0199", None).await;
    assert_eq!(deep.status, StatusCode::OK);
    assert_eq!(deep.text(), html);

    // Anywhere else than the installation's own panel is refused on the page.
    let (verifier, challenge) = pkce();
    let reply = get_with(
        &app,
        &format!("/oauth/authorize?response_type=code&client_id=frood-admin&redirect_uri=https://evil.example/admin/&code_challenge={challenge}&code_challenge_method=S256"),
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);

    let home = format!("{ISSUER}/admin/");
    let request = Authorize {
        client_id: "frood-admin",
        redirect: &home,
        scope: "knowledge:read knowledge:capture knowledge:decide journal:read admin",
        resource: &format!("{ISSUER}/v1"),
    };
    let grant = [
        "knowledge:read",
        "knowledge:capture",
        "knowledge:decide",
        "journal:read",
        "admin",
    ];
    let code = authorize_with(&app, &idp, &request, &challenge, &grant).await;
    let reply = post_form(
        &app,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", home.as_str()),
            ("client_id", "frood-admin"),
            ("code_verifier", verifier.as_str()),
            ("resource", request.resource),
        ],
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let tokens = reply.json();
    assert!(tokens["scope"].as_str().unwrap().contains("admin"));
    let access = tokens["access_token"].as_str().unwrap();

    let get = |path: &'static str| {
        Request::get(path)
            .header(header::AUTHORIZATION, format!("Bearer {access}"))
            .body(Body::empty())
            .unwrap()
    };
    let health = send(&app, get("/v1/health")).await;
    assert_eq!(health.status, StatusCode::OK, "{}", health.text());
    let health = health.json();
    assert_eq!(health["runs"]["queued"], 0);
    assert!(health["version"].is_string());
    let logs = send(&app, get("/v1/logs?after=0&limit=5")).await;
    assert_eq!(logs.status, StatusCode::OK);
    assert!(logs.json().is_array());
    let metrics = send(&app, get("/metrics")).await;
    assert_eq!(metrics.status, StatusCode::OK);
    assert!(
        metrics
            .text()
            .contains("# TYPE frood_dispatcher_lag_seconds gauge")
    );
    assert!(metrics.text().contains("frood_runs{status=\"queued\"} 0"));
    let journal = send(&app, get("/v1/journal?last=5")).await;
    assert_eq!(journal.status, StatusCode::OK, "{}", journal.text());

    // The token is the API's, not the MCP server's.
    assert_eq!(
        mcp(&app, access, 1, "tools/list", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Without a token there is no health or metrics.
    let reply = get_with(&app, "/metrics", None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn behind_a_proxy_under_a_path() {
    const BASE: &str = "https://mem.example.com/frood";
    let idp = Idp::start("alice@example.com").await;
    let deploy = format!("deploy: {{server: host}}\nserver: {{public_url: '{BASE}'}}");
    let Some((app, _auth)) = setup_with("oauth_path", &idp, &deploy).await else {
        eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
        return;
    };
    // The proxy strips `/frood`; the browser sees it in every link.
    let behind = |location: &Url| {
        let path = path_and_query(location);
        assert!(path.starts_with("/frood/"), "{path}");
        path["/frood".len()..].to_owned()
    };

    let reply = send(
        &app,
        Request::post("/mcp")
            .header(header::HOST, "mem.example.com")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(reply.header(header::WWW_AUTHENTICATE).contains(
        "resource_metadata=\"https://mem.example.com/.well-known/oauth-protected-resource/frood/mcp\""
    ));
    // RFC 9728 and RFC 8414 put the path after the well-known part, at the root of the host;
    // behind the proxy the plain paths answer the same.
    for path in [
        "/.well-known/oauth-protected-resource/frood/mcp",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let metadata = get_with(&app, path, None).await.json();
        assert_eq!(metadata["resource"], format!("{BASE}/mcp"), "{path}");
        assert_eq!(metadata["authorization_servers"], json!([BASE]));
    }
    for path in [
        "/.well-known/oauth-authorization-server/frood",
        "/.well-known/oauth-authorization-server",
    ] {
        let server = get_with(&app, path, None).await.json();
        assert_eq!(server["issuer"], BASE, "{path}");
        assert_eq!(server["token_endpoint"], format!("{BASE}/oauth/token"));
    }
    for path in [
        "/.well-known/oauth-authorization-server/other",
        "/.well-known/oauth-protected-resource/other/mcp",
    ] {
        assert_eq!(
            get_with(&app, path, None).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }

    // Sign-in stays under the path: redirects, the provider's callback and the cookie.
    let (_, challenge) = pkce();
    let authorize = format!(
        "/oauth/authorize?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", "frood-admin"),
                ("redirect_uri", &format!("{BASE}/admin/")),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("scope", "knowledge:read"),
                ("state", "s"),
                ("resource", &format!("{BASE}/v1")),
            ])
            .finish()
    );
    let reply = get_with(&app, &authorize, None).await;
    let reply = get_with(&app, &behind(&reply.location()), None).await;
    let reply = get_with(&app, &behind(&reply.location()), None).await;
    let upstream = reply.location();
    assert_eq!(
        query(&upstream, "redirect_uri"),
        format!("{BASE}/oauth/callback/corp")
    );
    *idp.nonce.lock().unwrap() = Some(query(&upstream, "nonce"));
    *idp.challenge.lock().unwrap() = Some(query(&upstream, "code_challenge"));
    let callback = format!(
        "/oauth/callback/corp?code=idp-code&state={}",
        query(&upstream, "state")
    );
    let reply = get_with(&app, &callback, None).await;
    assert!(
        reply
            .header(header::SET_COOKIE)
            .contains("Path=/frood/oauth;")
    );
    let consent = behind(&reply.location());
    assert!(consent.starts_with("/oauth/consent?tx="), "{consent}");
    let cookie = reply.header(header::SET_COOKIE);
    let cookie = cookie.split(';').next().unwrap();
    let page = get_with(&app, &consent, Some(cookie)).await.text();
    assert!(page.contains("action=\"/frood/oauth/consent\""));
}

/// A laptop signs in on this computer (docs/27 §1): no password, the one listed address
/// filled in; a whole allowed domain does not count.
#[tokio::test]
async fn a_laptop_signs_in_on_this_computer() {
    let Some(url) = frood_store::testing::database_url() else {
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "oauth_local")
        .await
        .expect("test database");
    let raw = "version: 1\ninstallation: {name: laptop}\nserver: {listen: '127.0.0.1:8080'}\n\
               storage: {uses: postgres, url: 'postgres://unused', objects: {uses: database}}\n\
               deploy: {server: host}\nauth:\n  allow: [alice@example.com]\n  allow_domains: [example.org]\n  \
               providers: [{kind: local, id: local}]\n";
    let config = frood_config::parse(
        raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .expect("config")
    .config;
    let app = router(frood_api::assemble(&config, std::path::Path::new("."), pool, |_| None).state);
    let (_, challenge) = pkce();
    let authorize = format!(
        "/oauth/authorize?response_type=code&client_id=frood-admin&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=s",
        url::form_urlencoded::byte_serialize(format!("{ISSUER}/admin/").as_bytes())
            .collect::<String>()
    );
    let reply = get_with(&app, &authorize, None).await;
    assert!(reply.status.is_redirection(), "{}", reply.text());
    let reply = get_with(&app, &path_and_query(&reply.location()), None).await;
    let form = path_and_query(&reply.location());
    let page = get_with(&app, &form, None).await;
    let html = page.text();
    assert!(html.contains("Sign in on this computer"), "{html}");
    assert!(html.contains("value=\"alice@example.com\""), "{html}");
    let tx = query(&reply.location(), "tx");

    let refused = post_form(
        &app,
        "/oauth/login/local",
        &[("tx", &tx), ("email", "bob@example.org")],
        None,
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text());
    let signed = post_form(
        &app,
        "/oauth/login/local",
        &[("tx", &tx), ("email", "alice@example.com")],
        None,
    )
    .await;
    assert!(signed.status.is_redirection(), "{}", signed.text());
    assert_eq!(signed.location().path(), "/oauth/consent");

    // A server with a public address cannot use it.
    let public = raw.replace(
        "server: {listen: '127.0.0.1:8080'}",
        "server: {listen: '127.0.0.1:8080', public_url: 'https://mem.example.com'}",
    );
    let problems = frood_config::parse(
        &public,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .unwrap_err()
    .to_string();
    assert!(problems.contains("sign-in on this computer"), "{problems}");
}

/// Sign-in with a button in Telegram (docs/28 §2): the page asks for the address, the bot
/// shows the code and two buttons, a press from the linked account lets the browser in; a
/// press from anyone else does nothing, "Not me" refuses, and nobody new gets in.
#[tokio::test]
async fn a_person_signs_in_with_a_button_in_telegram() {
    let Some(url) = frood_store::testing::database_url() else {
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "oauth_telegram")
        .await
        .expect("test database");
    let sent: Arc<Mutex<Vec<Value>>> = Arc::default();
    let edited: Arc<Mutex<Vec<Value>>> = Arc::default();
    let module = Router::new()
        .route(
            "/manifest",
            get(|| async { Json(json!({ "name": "telegram", "version": "0.0.1", "provides": ["notify.send@1", "notify.update@1"], "frood_api": [] })) }),
        )
        .route("/healthz", get(|| async { Json(json!({ "status": "ok" })) }))
        .route(
            "/v1/capabilities/notify.send@1",
            post({
                let sent = sent.clone();
                move |Json(body): Json<Value>| async move {
                    sent.lock().unwrap().push(body);
                    Json(json!({ "message_ref": "42:7" }))
                }
            }),
        )
        .route(
            "/v1/capabilities/notify.update@1",
            post({
                let edited = edited.clone();
                move |Json(body): Json<Value>| async move {
                    edited.lock().unwrap().push(body);
                    Json(json!({}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, module).await.unwrap() });

    let raw = format!(
        "version: 1\ninstallation: {{name: home}}\nserver: {{listen: '127.0.0.1:8080'}}\n\
         storage: {{uses: postgres, url: 'postgres://unused', objects: {{uses: database}}}}\n\
         deploy: {{server: host}}\njobs:\n  extract: {{ enabled: false }}\n\
         modules:\n  telegram: {{ url: '{fake}' }}\n\
         auth:\n  allow: [alice@example.com, carol@example.com]\n  providers: [{{kind: telegram, id: telegram}}]\n"
    );
    let config = frood_config::parse(
        &raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .expect("config")
    .config;
    let assembly = frood_api::assemble(&config, std::path::Path::new("."), pool.clone(), |name| {
        (name == "FROOD_MODULE_TOKEN_TELEGRAM").then(|| "tg-token".to_owned())
    });
    assembly.modules.discover("telegram").await.unwrap();
    let dispatch = assembly.state.dispatch.clone();
    let auth = assembly.state.auth.clone();
    let app = router(assembly.state);
    // At start the server registers each module's token; the bot acts with it.
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
    auth.user_id_by_email("carol@example.com", true)
        .await
        .unwrap();
    frood_store::sqlx::query(
        "INSERT INTO channel_links (channel, external_id, user_id) VALUES ('telegram', '42', $1)",
    )
    .bind(alice)
    .execute(&pool)
    .await
    .unwrap();

    // The console asks to sign in; the page offers the button.
    let start = async || {
        let (_, challenge) = pkce();
        let authorize = format!(
            "/oauth/authorize?response_type=code&client_id=frood-admin&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=s",
            url::form_urlencoded::byte_serialize(format!("{ISSUER}/admin/").as_bytes())
                .collect::<String>()
        );
        let reply = get_with(&app, &authorize, None).await;
        let reply = get_with(&app, &path_and_query(&reply.location()), None).await;
        query(&reply.location(), "tx")
    };
    let tx = start().await;
    let page = get_with(&app, &format!("/oauth/login/telegram?tx={tx}"), None).await;
    let html = page.text();
    assert!(html.contains("Sign in with Telegram"), "{html}");
    assert!(
        html.contains("action=\"/oauth/login/telegram/ask\""),
        "{html}"
    );

    // Nobody new, and nobody without a linked Telegram.
    let ask = |email: &'static str, tx: String| {
        let app = app.clone();
        async move {
            post_form(
                &app,
                "/oauth/login/telegram/ask",
                &[("tx", &tx), ("email", email)],
                None,
            )
            .await
        }
    };
    let refused = ask("bob@example.com", tx.clone()).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text());
    let unlinked = ask("carol@example.com", tx.clone()).await;
    assert_eq!(
        unlinked.status,
        StatusCode::FORBIDDEN,
        "{}",
        unlinked.text()
    );
    assert!(
        unlinked.text().contains("No telegram account is linked"),
        "{}",
        unlinked.text()
    );
    assert!(sent.lock().unwrap().is_empty());

    // Alice gets the button with the code the page shows.
    let asked = ask("alice@example.com", tx.clone()).await;
    assert!(asked.status.is_redirection(), "{}", asked.text());
    let wait = path_and_query(&asked.location());
    assert_eq!(asked.location().path(), "/oauth/login/telegram/wait");
    let message = sent
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("a button was sent");
    assert_eq!(message["recipient"]["external_id"], "42");
    assert_eq!(message["item"]["item"]["kind"], "sign_in");
    assert_eq!(message["item"]["content"]["client"], "Frood admin panel");
    let actions: Vec<&str> = message["item"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["approve", "deny"]);
    let code = message["item"]["content"]["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let waiting = get_with(&app, &wait, None).await;
    assert_eq!(waiting.status, StatusCode::OK);
    let html = waiting.text();
    assert!(html.contains(&code), "{html}");
    assert!(html.contains("http-equiv=\"refresh\""), "{html}");

    // A press from another Telegram account does nothing; Alice's lets the browser in.
    let item = message["item"]["item"]["id"].as_str().unwrap().to_owned();
    let press = |external_id: &'static str, action: &'static str| {
        let app = app.clone();
        let item = item.clone();
        async move {
            send(
                &app,
                Request::post(format!("/v1/inbox/{item}/act"))
                    .header(header::AUTHORIZATION, "Bearer tg-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "action": action, "external_id": external_id }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
        }
    };
    let stranger = press("99", "approve").await;
    assert!(!stranger.status.is_success(), "{}", stranger.text());
    assert_eq!(get_with(&app, &wait, None).await.status, StatusCode::OK);
    let pressed = press("42", "approve").await;
    assert!(pressed.status.is_success(), "{}", pressed.text());
    let signed = get_with(&app, &wait, None).await;
    assert!(signed.status.is_redirection(), "{}", signed.text());
    assert_eq!(signed.location().path(), "/oauth/consent");
    assert!(
        signed
            .header(header::SET_COOKIE)
            .starts_with("frood_session=")
    );
    // Once: the same page again only goes on to consent, with no new session.
    let again = get_with(&app, &wait, None).await;
    assert_eq!(again.location().path(), "/oauth/consent");
    assert!(again.header(header::SET_COOKIE).is_empty());
    // The message says how it ended, and the console's Inbox never lists the button.
    dispatch.deliver_once().await.unwrap();
    assert_eq!(edited.lock().unwrap().len(), 1);
    assert!(dispatch.inbox(true, 50).await.unwrap().is_empty());

    // "Not me" refuses.
    let tx = start().await;
    let asked = ask("alice@example.com", tx).await;
    let wait = path_and_query(&asked.location());
    let item = sent.lock().unwrap().last().unwrap()["item"]["item"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let denied = send(
        &app,
        Request::post(format!("/v1/inbox/{item}/act"))
            .header(header::AUTHORIZATION, "Bearer tg-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "action": "deny", "external_id": "42" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert!(denied.status.is_success(), "{}", denied.text());
    let refused = get_with(&app, &wait, None).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(refused.text().contains("declined"), "{}", refused.text());

    // A newer button replaces a waiting one; the old one closes and its message says so.
    let tx = start().await;
    let first = ask("alice@example.com", tx.clone()).await;
    let first_wait = path_and_query(&first.location());
    let tx2 = start().await;
    ask("alice@example.com", tx2).await;
    let expired = get_with(&app, &first_wait, None).await;
    assert!(
        expired.text().contains("The button expired"),
        "{}",
        expired.text()
    );
    let before = edited.lock().unwrap().len();
    dispatch.deliver_once().await.unwrap();
    assert_eq!(
        edited.lock().unwrap().len(),
        before + 2,
        "the denied and the replaced messages"
    );

    // A fuse: no more than five buttons an hour.
    let tx = start().await;
    let fifth = ask("alice@example.com", tx.clone()).await;
    assert!(fifth.status.is_redirection(), "{}", fifth.text());
    let sixth = ask("alice@example.com", tx).await;
    assert_eq!(sixth.status, StatusCode::FORBIDDEN, "{}", sixth.text());
    assert!(sixth.text().contains("Too many"), "{}", sixth.text());
}

/// Passkeys (docs/28 §2): the console makes a one-time link, the device creates a passkey
/// there, and the sign-in page then lets the person in with it; nobody else, no unknown
/// passkey, and not after the person removes it. A software passkey stands in for the device.
#[tokio::test]
async fn a_person_adds_a_passkey_and_signs_in_with_it() {
    use webauthn_authenticator_rs::WebauthnAuthenticator;
    use webauthn_authenticator_rs::prelude::{
        CreationChallengeResponse, RequestChallengeResponse, Url,
    };
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;

    let Some(url) = frood_store::testing::database_url() else {
        return;
    };
    let pool = frood_store::testing::fresh_database(&url, "oauth_passkey")
        .await
        .expect("test database");
    const SITE: &str = "https://mem.example.com";
    let raw = "version: 1\ninstallation: {name: home}\n\
               server: {listen: '127.0.0.1:8080', public_url: 'https://mem.example.com'}\n\
               storage: {uses: postgres, url: 'postgres://unused', objects: {uses: database}}\n\
               deploy: {server: host}\njobs:\n  extract: { enabled: false }\n\
               auth:\n  allow: [alice@example.com, carol@example.com]\n  providers: [{kind: passkey, id: passkey}]\n";
    let config = frood_config::parse(
        raw,
        &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
    )
    .expect("config")
    .config;
    let assembly = frood_api::assemble(&config, std::path::Path::new("."), pool, |_| None);
    let auth = assembly.state.auth.clone();
    let app = router(assembly.state);
    let alice = auth
        .user_id_by_email("alice@example.com", true)
        .await
        .unwrap();
    let (_, console) = auth
        .create_personal_token(
            alice,
            "console",
            &[frood_auth::scope::ADMIN.to_owned()],
            None,
        )
        .await
        .unwrap();
    let api = |method: &str, path: &str| {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {console}"))
            .body(Body::empty())
            .unwrap()
    };
    // The browser posts from the installation's own pages.
    let post_json = |path: &str, body: Value| {
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, SITE)
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let post_form = |path: &str, form: &[(&str, &str)]| {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, SITE)
            .body(Body::from(body))
            .unwrap()
    };
    let origin = Url::parse(SITE).unwrap();
    let mut device = WebauthnAuthenticator::new(SoftPasskey::new(true));

    // No passkeys yet; the console makes a link.
    assert_eq!(
        send(&app, api("GET", "/v1/me/passkeys")).await.json(),
        json!({ "on": true, "passkeys": [] })
    );
    let link = send(&app, api("POST", "/v1/me/passkeys/links")).await;
    assert_eq!(link.status, StatusCode::OK, "{}", link.text());
    let link = Url::parse(link.json()["url"].as_str().unwrap()).unwrap();
    assert_eq!(link.path(), "/oauth/passkey");
    let code = query(&link, "code");
    let page = get_with(&app, &path_and_query(&link), None).await;
    assert!(page.text().contains("Add a passkey"), "{}", page.text());
    assert!(page.text().contains("alice@example.com"), "{}", page.text());

    // The device creates the passkey.
    let options = send(
        &app,
        post_json("/oauth/passkey/options", json!({ "code": code })),
    )
    .await;
    assert_eq!(options.status, StatusCode::OK, "{}", options.text());
    let options: CreationChallengeResponse = serde_json::from_slice(&options.body).unwrap();
    let created = device.do_registration(origin.clone(), options).unwrap();
    let created = serde_json::to_string(&created).unwrap();
    let added = send(
        &app,
        post_form(
            "/oauth/passkey",
            &[
                ("code", &code),
                ("credential", &created),
                ("label", "Test phone"),
            ],
        ),
    )
    .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.text());
    assert!(added.text().contains("Passkey added"), "{}", added.text());
    // The link works once.
    let again = send(
        &app,
        post_json("/oauth/passkey/options", json!({ "code": code })),
    )
    .await;
    assert_eq!(again.status, StatusCode::FORBIDDEN, "{}", again.text());
    let listed = send(&app, api("GET", "/v1/me/passkeys")).await.json()["passkeys"].clone();
    assert_eq!(listed[0]["label"], "Test phone");
    assert!(listed[0]["last_used_at"].is_null());

    // Sign in from the console's sign-in page.
    let start = async || {
        let (_, challenge) = pkce();
        let authorize = format!(
            "/oauth/authorize?response_type=code&client_id=frood-admin&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=s",
            url::form_urlencoded::byte_serialize(format!("{SITE}/admin/").as_bytes())
                .collect::<String>()
        );
        let reply = get_with(&app, &authorize, None).await;
        let reply = get_with(&app, &path_and_query(&reply.location()), None).await;
        query(&reply.location(), "tx")
    };
    let tx = start().await;
    let page = get_with(&app, &format!("/oauth/login/passkey?tx={tx}"), None).await;
    let html = page.text();
    assert!(html.contains("Sign in with a passkey"), "{html}");
    assert!(
        html.contains("data-options=\"/oauth/login/passkey/passkey/options\""),
        "{html}"
    );
    let ask = |tx: &str, email: &str| {
        post_json(
            "/oauth/login/passkey/passkey/options",
            json!({ "tx": tx, "email": email }),
        )
    };
    let stranger = send(&app, ask(&tx, "bob@example.com")).await;
    assert_eq!(
        stranger.status,
        StatusCode::FORBIDDEN,
        "{}",
        stranger.text()
    );
    let without = send(&app, ask(&tx, "carol@example.com")).await;
    assert_eq!(without.status, StatusCode::FORBIDDEN, "{}", without.text());
    assert!(without.text().contains("No passkey"), "{}", without.text());

    // Another device's passkey does not match.
    let options = send(&app, ask(&tx, "alice@example.com")).await;
    assert_eq!(options.status, StatusCode::OK, "{}", options.text());
    let challenge: RequestChallengeResponse = serde_json::from_slice(&options.body).unwrap();
    let mut other = WebauthnAuthenticator::new(SoftPasskey::new(true));
    assert!(other.do_authentication(origin.clone(), challenge).is_err());

    let options = send(&app, ask(&tx, "alice@example.com")).await;
    let challenge: RequestChallengeResponse = serde_json::from_slice(&options.body).unwrap();
    let answer =
        serde_json::to_string(&device.do_authentication(origin.clone(), challenge).unwrap())
            .unwrap();
    let signed = send(
        &app,
        post_form(
            "/oauth/login/passkey/passkey",
            &[("tx", &tx), ("credential", &answer)],
        ),
    )
    .await;
    assert!(signed.status.is_redirection(), "{}", signed.text());
    assert_eq!(signed.location().path(), "/oauth/consent");
    assert!(
        signed
            .header(header::SET_COOKIE)
            .starts_with("frood_session=")
    );
    // The same answer cannot sign in twice.
    let replayed = send(
        &app,
        post_form(
            "/oauth/login/passkey/passkey",
            &[("tx", &tx), ("credential", &answer)],
        ),
    )
    .await;
    assert!(!replayed.status.is_redirection(), "{}", replayed.text());
    let listed = send(&app, api("GET", "/v1/me/passkeys")).await.json()["passkeys"].clone();
    assert!(listed[0]["last_used_at"].is_string(), "{listed}");

    // Removed, it signs in no more.
    let id = listed[0]["id"].as_str().unwrap().to_owned();
    let removed = send(&app, api("DELETE", &format!("/v1/me/passkeys/{id}"))).await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let tx = start().await;
    let gone = send(&app, ask(&tx, "alice@example.com")).await;
    assert_eq!(gone.status, StatusCode::FORBIDDEN, "{}", gone.text());
}
