//! The console (docs/25 §3): a single-page app built from `core/console` into the binary,
//! served at `/admin/`. Any path under it that is not a built file gets the app's page, so a
//! link to a section or an item opens directly. Behind a proxy path (`/frood`) the page learns
//! where it lives from a `<base>` and a meta tag the server puts in.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::AppState;

/// `core/console/dist`; empty when the console was not built (Rust-only builds and tests),
/// and then the previous one-page panel answers instead.
#[derive(rust_embed::RustEmbed)]
#[folder = "../../console/dist"]
#[allow_missing = true]
struct Built;

const LEGACY: &str = include_str!("admin.html");

/// The page talks only to this server.
const POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                      img-src 'self' data:; font-src 'self'; connect-src 'self'; \
                      base-uri 'self'; frame-ancestors 'none'";

pub fn routes() -> Router<AppState> {
    Router::new()
        // Relative, so that it stays under a proxy's path such as `/frood`.
        .route("/admin", get(|| async { Redirect::permanent("admin/") }))
        .route("/admin/", get(page))
        .route("/admin/{*path}", get(file))
}

async fn file(state: State<AppState>, Path(path): Path<String>) -> Response {
    let Some(built) = Built::get(&path) else {
        return page(state).await;
    };
    // Vite names built files by their content: they never change under the same name.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_owned()),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        built.data,
    )
        .into_response()
}

async fn page(State(state): State<AppState>) -> Response {
    let base = &state.auth.settings().base_path;
    let (html, policy) = match Built::get("index.html") {
        Some(index) => (
            String::from_utf8_lossy(&index.data).replace(
                "<!--frood-head-->",
                &format!(
                    "<base href=\"{0}/admin/\"><meta name=\"frood-base\" content=\"{0}\">",
                    attribute(base)
                ),
            ),
            POLICY,
        ),
        None => (
            LEGACY.to_owned(),
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; frame-ancestors 'none'",
        ),
    };
    let mut response = (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(policy),
    );
    response
}

/// The issuer's path comes from the config; keep it inert inside an attribute.
fn attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
