//! Frood module `stt-openai`: `media.transcribe@1` over OpenAI's speech-to-text API.
//!
//! Environment (written by `frood apply`): `FROOD_TOKEN` (the core presents it when calling),
//! `FROOD_MODULE_CONFIG` (`{"model": …, "base_url": …}`), `OPENAI_API_KEY`, `PORT` (8080).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

const MANIFEST: &str = include_str!("../frood-module.yaml");
const DEFAULT_MODEL: &str = "gpt-transcribe";
const OPENAI_URL: &str = "https://api.openai.com";
/// Upload formats OpenAI documents, by content type.
const ACCEPTED: [(&str, &str); 9] = [
    ("audio/mpeg", "mp3"),
    ("audio/mp3", "mp3"),
    ("audio/mp4", "m4a"),
    ("audio/x-m4a", "m4a"),
    ("audio/m4a", "m4a"),
    ("audio/wav", "wav"),
    ("audio/x-wav", "wav"),
    ("audio/webm", "webm"),
    ("video/mp4", "mp4"),
];

#[derive(Clone)]
struct App {
    token: String,
    key: String,
    base_url: String,
    model: String,
    http: reqwest::Client,
}

#[derive(Deserialize, Default)]
struct Settings {
    model: Option<String>,
    base_url: Option<String>,
}

#[derive(Deserialize)]
struct Input {
    audio: Audio,
}

#[derive(Deserialize)]
struct Audio {
    content_type: String,
    #[serde(default)]
    filename: Option<String>,
    data_base64: String,
}

/// An error answer: problem details, `retryable` when trying later may help.
fn problem(status: StatusCode, detail: impl Into<String>, retryable: bool) -> Response {
    let body = json!({ "title": status.canonical_reason(), "status": status.as_u16(), "detail": detail.into(), "retryable": retryable });
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        Json(body),
    )
        .into_response()
}

fn router(app: App) -> Router {
    Router::new()
        .route("/manifest", get(manifest))
        .route(
            "/healthz",
            get(|| async { Json(json!({ "status": "ok" })) }),
        )
        .route("/v1/capabilities/media.transcribe@1", post(transcribe))
        .with_state(Arc::new(app))
}

async fn manifest() -> Response {
    match serde_yaml_ng::from_str::<Value>(MANIFEST) {
        Ok(value) => Json(value).into_response(),
        Err(err) => problem(StatusCode::INTERNAL_SERVER_ERROR, err.to_string(), false),
    }
}

/// The extension to upload under, if OpenAI takes the format as it is.
fn accepted_extension(content_type: &str) -> Option<&'static str> {
    let t = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    ACCEPTED.iter().find(|(k, _)| *k == t).map(|(_, ext)| *ext)
}

/// Converts audio to MP3 (mono, 16 kHz) with ffmpeg.
async fn to_mp3(data: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-b:a",
            "48k",
            "-f",
            "mp3",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no ffmpeg stdin")?;
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&data).await;
    });
    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
    let _ = writer.await;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!(
            "ffmpeg could not convert the audio: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

async fn transcribe(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<Input>,
) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if presented != Some(app.token.as_str()) {
        return problem(
            StatusCode::UNAUTHORIZED,
            "only the Frood core may call this module",
            false,
        );
    }
    let data = match base64::engine::general_purpose::STANDARD
        .decode(input.audio.data_base64.as_bytes())
    {
        Ok(data) if !data.is_empty() => data,
        _ => {
            return problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                "audio.data_base64 is empty or not base64",
                false,
            );
        }
    };
    let (data, extension) = match accepted_extension(&input.audio.content_type) {
        Some(ext) => (data, ext),
        None => match to_mp3(data).await {
            Ok(mp3) => (mp3, "mp3"),
            Err(err) => return problem(StatusCode::UNPROCESSABLE_ENTITY, err, false),
        },
    };
    let stem = input
        .audio
        .filename
        .as_deref()
        .and_then(|f| f.rsplit_once('.').map(|(s, _)| s.to_owned()))
        .unwrap_or_else(|| "audio".to_owned());
    let part = match reqwest::multipart::Part::bytes(data)
        .file_name(format!("{stem}.{extension}"))
        .mime_str(&format!("audio/{extension}"))
    {
        Ok(part) => part,
        Err(err) => return problem(StatusCode::INTERNAL_SERVER_ERROR, err.to_string(), false),
    };
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", app.model.clone());
    let response = match app
        .http
        .post(format!("{}/v1/audio/transcriptions", app.base_url))
        .bearer_auth(&app.key)
        .multipart(form)
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => {
            return problem(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("cannot reach OpenAI: {err}"),
                true,
            );
        }
    };
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("no details");
        let retryable = status.as_u16() == 429 || status.is_server_error();
        let code = if retryable {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::UNPROCESSABLE_ENTITY
        };
        return problem(
            code,
            format!("OpenAI answered {status}: {message}"),
            retryable,
        );
    }
    let text = body.get("text").and_then(Value::as_str).unwrap_or_default();
    let language = body
        .pointer("/languages/0/code")
        .or_else(|| body.get("language"))
        .cloned()
        .unwrap_or(Value::Null);
    Json(json!({ "text": text, "language": language, "segments": body.get("segments").cloned().unwrap_or(json!([])), "model": app.model })).into_response()
}

fn app_from_env() -> Result<App, String> {
    let settings: Settings = match std::env::var("FROOD_MODULE_CONFIG") {
        Ok(raw) if !raw.trim().is_empty() => {
            serde_json::from_str(&raw).map_err(|e| format!("FROOD_MODULE_CONFIG: {e}"))?
        }
        _ => Settings::default(),
    };
    Ok(App {
        token: std::env::var("FROOD_TOKEN").map_err(|_| "FROOD_TOKEN is not set")?,
        key: std::env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY is not set")?,
        base_url: settings
            .base_url
            .unwrap_or_else(|| OPENAI_URL.to_owned())
            .trim_end_matches('/')
            .to_owned(),
        model: settings.model.unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| e.to_string())?,
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().json().init();
    let app = match app_from_env() {
        Ok(app) => app,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(2);
        }
    };
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind");
    tracing::info!(port, model = %app.model, "stt-openai is listening");
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}

#[cfg(test)]
mod tests {
    use axum::extract::Multipart;
    use tower_service::Service as _;

    use super::*;

    /// Uploads the fake provider saw: file name, model, size.
    type Seen = Arc<std::sync::Mutex<Vec<(String, String, usize)>>>;

    async fn fake_openai() -> (String, Seen) {
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route(
                "/v1/audio/transcriptions",
                post(|State(seen): State<Seen>, mut form: Multipart| async move {
                    let (mut file, mut model, mut size) = (String::new(), String::new(), 0);
                    while let Some(field) = form.next_field().await.unwrap() {
                        match field.name() {
                            Some("file") => {
                                file = field.file_name().unwrap_or_default().to_owned();
                                size = field.bytes().await.unwrap().len();
                            }
                            Some("model") => model = field.text().await.unwrap(),
                            _ => {}
                        }
                    }
                    seen.lock().unwrap().push((file, model, size));
                    Json(json!({ "text": "релиз переносим на 28 ноября", "languages": [{ "code": "ru" }] }))
                }),
            )
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, seen)
    }

    async fn call(app: &mut Router, token: &str, input: Value) -> (StatusCode, Value) {
        let request = axum::http::Request::post("/v1/capabilities/media.transcribe@1")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(input.to_string()))
            .unwrap();
        let response = app.call(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    fn app(url: &str) -> Router {
        router(App {
            token: "core".into(),
            key: "k".into(),
            base_url: url.into(),
            model: DEFAULT_MODEL.into(),
            http: reqwest::Client::new(),
        })
    }

    #[tokio::test]
    async fn transcribes_accepted_formats_as_they_are() {
        let (url, seen) = fake_openai().await;
        let mut app = app(&url);
        let audio = base64::engine::general_purpose::STANDARD.encode(b"ID3fake-mp3");
        let (status, out) = call(&mut app, "core", json!({ "audio": { "content_type": "audio/mpeg", "filename": "memo.mp3", "data_base64": audio } })).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["text"], "релиз переносим на 28 ноября");
        assert_eq!(out["language"], "ru");
        assert_eq!(
            seen.lock().unwrap()[0],
            ("memo.mp3".to_owned(), "gpt-transcribe".to_owned(), 11)
        );

        let (status, _) = call(
            &mut app,
            "stranger",
            json!({ "audio": { "content_type": "audio/mpeg", "data_base64": audio } }),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn converts_ogg_voice_messages() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffmpeg is not installed; skipping");
            return;
        }
        // One second of silence as OGG/Opus, made by ffmpeg itself.
        let ogg = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=mono",
                "-t",
                "1",
                "-c:a",
                "libopus",
                "-f",
                "ogg",
                "pipe:1",
            ])
            .output()
            .unwrap()
            .stdout;
        assert!(ogg.starts_with(b"OggS"));
        let (url, seen) = fake_openai().await;
        let mut app = app(&url);
        let audio = base64::engine::general_purpose::STANDARD.encode(&ogg);
        let (status, out) = call(&mut app, "core", json!({ "audio": { "content_type": "audio/ogg", "filename": "voice.oga", "data_base64": audio } })).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        let (file, _, size) = seen.lock().unwrap()[0].clone();
        assert_eq!(file, "voice.mp3");
        assert!(size > 0);
    }

    #[test]
    fn manifest_parses_and_names_the_capability() {
        let manifest: Value = serde_yaml_ng::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["provides"], json!(["media.transcribe@1"]));
        assert_eq!(accepted_extension("audio/ogg; codecs=opus"), None);
        assert_eq!(accepted_extension("audio/MPEG"), Some("mp3"));
    }
}
