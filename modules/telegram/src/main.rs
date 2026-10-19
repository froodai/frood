//! Frood module `telegram`: a bot for quick capture (docs/14 §2) and the Inbox's Telegram
//! channel (docs/15, `notify.send@1` / `notify.update@1`).
//!
//! Long-polls the Bot API, so it needs no public address. `/link <code>` links the sender's
//! Telegram account to a person in Frood; after that, text, voice messages, audio and files go
//! to `/v1/capture` as that person. The bot answers at once and edits its answer when
//! processing is done: what it remembered, what waits for a decision. Inbox items arrive as
//! messages with buttons; a button acts as the person who pressed it.
//!
//! Environment (written by `frood apply`): `FROOD_URL`, `FROOD_TOKEN`, `FROOD_MODULE` (the
//! channel name), `FROOD_MODULE_CONFIG` (`{"language": "en" | "ru"}`), `TELEGRAM_BOT_TOKEN`,
//! optionally `TELEGRAM_API_URL`, `PORT` (8080, for `/manifest` and `/healthz`).

mod notify;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

const MANIFEST: &str = include_str!("../frood-module.yaml");
const TELEGRAM_URL: &str = "https://api.telegram.org";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lang {
    En,
    Ru,
}

/// The bot's words.
struct Texts {
    help: &'static str,
    not_linked: &'static str,
    linked: &'static str,
    link_failed: &'static str,
    unlinked: &'static str,
    link_usage: &'static str,
    saving: &'static str,
    saved: &'static str,
    known: &'static str,
    remembered: &'static str,
    waiting: &'static str,
    conflict: &'static str,
    nothing: &'static str,
    failed: &'static str,
    still_working: &'static str,
    error: &'static str,
}

const EN: Texts = Texts {
    help: "Send me notes, voice messages or files and I will put them into Frood. I also bring you what waits for your decision, with buttons; /digest shows what is new.\nFirst link your account: run `frood link telegram --email you@example.com` (or ask your admin for a code) and send me /link CODE.",
    not_linked: "This Telegram account is not linked to Frood yet. Send /link CODE (get a code with `frood link telegram --email …`).",
    linked: "Linked to",
    link_failed: "Could not link",
    unlinked: "Unlinked. I will not save your messages until you link again.",
    link_usage: "Send /link followed by the code, e.g. /link K7M2QX9P",
    saving: "Saved, processing…",
    saved: "Saved.",
    known: "Already saved earlier.",
    remembered: "Remembered:",
    waiting: "Waiting for your decision:",
    conflict: "conflicts with what is recorded",
    nothing: "Saved; nothing in it to remember.",
    failed: "Saved, but processing failed:",
    still_working: "Saved; still processing.",
    error: "Could not save:",
};

const RU: Texts = Texts {
    help: "Присылайте заметки, голосовые и файлы — я сохраню их во Frood. А ещё приношу то, что ждёт вашего решения, с кнопками; /digest — что нового.\nСначала свяжите аккаунт: выполните `frood link telegram --email you@example.com` (или попросите код у администратора) и пришлите мне /link КОД.",
    not_linked: "Этот аккаунт Telegram ещё не связан с Frood. Пришлите /link КОД (код даёт `frood link telegram --email …`).",
    linked: "Связано с",
    link_failed: "Не удалось связать",
    unlinked: "Связь удалена. Сообщения не сохраняю, пока не свяжете снова.",
    link_usage: "Пришлите /link и код, например /link K7M2QX9P",
    saving: "Сохранено, обрабатываю…",
    saved: "Сохранено.",
    known: "Это уже сохранено раньше.",
    remembered: "Запомнил:",
    waiting: "Ждёт вашего решения:",
    conflict: "противоречит записанному",
    nothing: "Сохранено; запоминать нечего.",
    failed: "Сохранено, но обработка не удалась:",
    still_working: "Сохранено; ещё обрабатываю.",
    error: "Не удалось сохранить:",
};

impl Lang {
    fn texts(self) -> &'static Texts {
        match self {
            Self::En => &EN,
            Self::Ru => &RU,
        }
    }
}

#[derive(Clone)]
struct Bot {
    http: reqwest::Client,
    telegram: String,
    files: String,
    frood: String,
    token: String,
    channel: String,
    lang: Lang,
    poll_every: Duration,
    poll_for: Duration,
}

#[derive(Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    callback_query: Option<Value>,
}

#[derive(Deserialize, Clone)]
struct Message {
    message_id: i64,
    chat: Chat,
    #[serde(default)]
    from: Option<User>,
    #[serde(default)]
    date: Option<i64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    voice: Option<FileRef>,
    #[serde(default)]
    audio: Option<FileRef>,
    #[serde(default)]
    document: Option<FileRef>,
}

#[derive(Deserialize, Clone)]
struct Chat {
    id: i64,
}

#[derive(Deserialize, Clone)]
struct User {
    id: i64,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Deserialize, Clone)]
struct FileRef {
    file_id: String,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    file_name: Option<String>,
}

impl Bot {
    async fn telegram(&self, method: &str, body: Value) -> Result<Value, String> {
        let response = self
            .http
            .post(format!("{}/{method}", self.telegram))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let value: Value = response.json().await.map_err(|e| e.to_string())?;
        if value.get("ok") == Some(&json!(true)) {
            Ok(value.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(value
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Telegram error")
                .to_owned())
        }
    }

    async fn reply(&self, message: &Message, text: &str) -> Option<i64> {
        let sent = self
            .telegram(
                "sendMessage",
                json!({ "chat_id": message.chat.id, "text": text, "reply_parameters": { "message_id": message.message_id } }),
            )
            .await;
        match sent {
            Ok(result) => result.get("message_id").and_then(Value::as_i64),
            Err(err) => {
                tracing::warn!(error = %err, "cannot reply");
                None
            }
        }
    }

    async fn edit(&self, chat: i64, message_id: i64, text: &str) {
        if let Err(err) = self
            .telegram(
                "editMessageText",
                json!({ "chat_id": chat, "message_id": message_id, "text": text }),
            )
            .await
        {
            tracing::warn!(error = %err, "cannot edit the reply");
        }
    }

    fn frood(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token)
    }

    async fn handle(&self, message: Message) {
        let Some(from) = message.from.clone() else {
            return;
        };
        let external_id = from.id.to_string();
        let t = self.lang.texts();
        let text = message.text.clone().unwrap_or_default();
        let command = text.split_whitespace().next().unwrap_or_default();
        let command = command.split('@').next().unwrap_or_default();
        match command {
            "/start" | "/help" => {
                self.reply(&message, t.help).await;
                return;
            }
            "/link" => {
                let code = text.split_whitespace().nth(1).unwrap_or_default();
                if code.is_empty() {
                    self.reply(&message, t.link_usage).await;
                    return;
                }
                let label = from.username.as_ref().map(|u| format!("@{u}"));
                let response = self
                    .frood(
                        self.http
                            .post(format!("{}/v1/channels/{}/links", self.frood, self.channel)),
                    )
                    .json(&json!({ "code": code, "external_id": external_id, "label": label }))
                    .send()
                    .await;
                let answer = match response {
                    Ok(r) if r.status().is_success() => {
                        let body: Value = r.json().await.unwrap_or(Value::Null);
                        format!("{} {}", t.linked, body["email"].as_str().unwrap_or("Frood"))
                    }
                    Ok(r) => {
                        let body: Value = r.json().await.unwrap_or(Value::Null);
                        format!(
                            "{}: {}",
                            t.link_failed,
                            body["detail"].as_str().unwrap_or("?")
                        )
                    }
                    Err(err) => format!("{}: {err}", t.link_failed),
                };
                self.reply(&message, &answer).await;
                return;
            }
            "/digest" | "/new" => {
                let response = self
                    .frood(self.http.get(format!(
                        "{}/v1/digest?external_id={external_id}",
                        self.frood
                    )))
                    .send()
                    .await;
                let answer = match response {
                    Ok(r) if r.status().is_success() => {
                        let digest: Value = r.json().await.unwrap_or(Value::Null);
                        notify::digest_text(&digest, self.lang.words())
                    }
                    Ok(r) if r.status() == StatusCode::FORBIDDEN => t.not_linked.to_owned(),
                    Ok(r) => {
                        let body: Value = r.json().await.unwrap_or(Value::Null);
                        format!("{} {}", t.error, body["detail"].as_str().unwrap_or("?"))
                    }
                    Err(err) => format!("{} {err}", t.error),
                };
                self.reply(&message, &answer).await;
                return;
            }
            "/unlink" => {
                let _ = self
                    .frood(self.http.delete(format!(
                        "{}/v1/channels/{}/links/{external_id}",
                        self.frood, self.channel
                    )))
                    .send()
                    .await;
                self.reply(&message, t.unlinked).await;
                return;
            }
            _ => {}
        }
        let said_at = message.date.map(|d| d.to_string());
        let captured = match message
            .voice
            .as_ref()
            .map(|f| (f, "voice"))
            .or_else(|| message.audio.as_ref().map(|f| (f, "audio")))
            .or_else(|| message.document.as_ref().map(|f| (f, "file")))
        {
            Some((file, kind)) => {
                self.capture_file(
                    file,
                    kind,
                    message.caption.as_deref(),
                    &external_id,
                    said_at.as_deref(),
                )
                .await
            }
            None if !text.trim().is_empty() && !text.starts_with('/') => {
                self.capture_text(&text, &external_id, said_at.as_deref())
                    .await
            }
            None => return,
        };
        match captured {
            Ok((true, source_id)) => {
                let reply = self.reply(&message, t.saving).await;
                if let Some(reply) = reply {
                    let bot = self.clone();
                    let chat = message.chat.id;
                    tokio::spawn(async move { bot.follow(chat, reply, &source_id).await });
                }
            }
            Ok((false, _)) => {
                self.reply(&message, t.known).await;
            }
            Err(Captured::NotLinked) => {
                self.reply(&message, t.not_linked).await;
            }
            Err(Captured::Failed(err)) => {
                self.reply(&message, &format!("{} {err}", t.error)).await;
            }
        }
    }

    async fn capture_text(
        &self,
        text: &str,
        external_id: &str,
        said_at: Option<&str>,
    ) -> Result<(bool, String), Captured> {
        let mut body = json!({ "text": text, "kind": "message", "external_id": external_id });
        if let Some(at) = said_at.and_then(unix_to_rfc3339) {
            body["said_at"] = json!(at);
        }
        let response = self
            .frood(self.http.post(format!("{}/v1/capture", self.frood)))
            .json(&body)
            .send()
            .await
            .map_err(|e| Captured::Failed(e.to_string()))?;
        captured(response).await
    }

    async fn capture_file(
        &self,
        file: &FileRef,
        kind: &str,
        caption: Option<&str>,
        external_id: &str,
        said_at: Option<&str>,
    ) -> Result<(bool, String), Captured> {
        let info = self
            .telegram("getFile", json!({ "file_id": file.file_id }))
            .await
            .map_err(Captured::Failed)?;
        let path = info["file_path"]
            .as_str()
            .ok_or_else(|| Captured::Failed("Telegram gave no file path".into()))?;
        let data = self
            .http
            .get(format!("{}/{path}", self.files))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| Captured::Failed(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| Captured::Failed(e.to_string()))?;
        let content_type = file.mime_type.clone().unwrap_or_else(|| match kind {
            "voice" => "audio/ogg".to_owned(),
            _ => "application/octet-stream".to_owned(),
        });
        let filename = file
            .file_name
            .clone()
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or("file").to_owned());
        let part = reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(filename)
            .mime_str(&content_type)
            .map_err(|e| Captured::Failed(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new()
            .text("kind", kind.to_owned())
            .text("external_id", external_id.to_owned())
            .part("file", part);
        if let Some(caption) = caption.filter(|c| !c.trim().is_empty()) {
            form = form.text("text", caption.to_owned());
        }
        if let Some(at) = said_at.and_then(unix_to_rfc3339) {
            form = form.text("said_at", at);
        }
        let response = self
            .frood(self.http.post(format!("{}/v1/capture", self.frood)))
            .multipart(form)
            .send()
            .await
            .map_err(|e| Captured::Failed(e.to_string()))?;
        captured(response).await
    }

    /// Waits for processing and edits the "saved" reply into a summary.
    async fn follow(&self, chat: i64, reply: i64, source_id: &str) {
        let t = self.lang.texts();
        let deadline = tokio::time::Instant::now() + self.poll_for;
        loop {
            tokio::time::sleep(self.poll_every).await;
            let status: Value = match self
                .frood(
                    self.http
                        .get(format!("{}/v1/sources/{source_id}/status", self.frood)),
                )
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
                _ => Value::Null,
            };
            match status["state"].as_str() {
                // Nothing to process: no job is set up for this kind of message.
                Some("idle") => {
                    self.edit(chat, reply, t.saved).await;
                    return;
                }
                Some("done") => {
                    self.edit(chat, reply, &summary(&status, t)).await;
                    return;
                }
                Some("failed") => {
                    let error = status["runs"]
                        .as_array()
                        .and_then(|runs| runs.iter().rev().find_map(|r| r["error"].as_str()))
                        .unwrap_or("?");
                    self.edit(chat, reply, &format!("{} {error}", t.failed))
                        .await;
                    return;
                }
                _ if tokio::time::Instant::now() >= deadline => {
                    self.edit(chat, reply, t.still_working).await;
                    return;
                }
                _ => {}
            }
        }
    }

    /// Long polling until the process stops.
    async fn run(self) {
        let mut offset: i64 = 0;
        loop {
            let updates = self
                .telegram(
                    "getUpdates",
                    json!({ "offset": offset, "timeout": 30, "allowed_updates": ["message", "callback_query"] }),
                )
                .await;
            match updates
                .and_then(|v| serde_json::from_value::<Vec<Update>>(v).map_err(|e| e.to_string()))
            {
                Ok(updates) => {
                    for update in updates {
                        offset = offset.max(update.update_id + 1);
                        if let Some(message) = update.message {
                            let bot = self.clone();
                            tokio::spawn(async move { bot.handle(message).await });
                        }
                        if let Some(query) = update.callback_query {
                            let bot = self.clone();
                            tokio::spawn(async move { bot.on_button(&query).await });
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "getUpdates failed; retrying");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}

enum Captured {
    NotLinked,
    Failed(String),
}

async fn captured(response: reqwest::Response) -> Result<(bool, String), Captured> {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        let id = body["source_id"].as_str().unwrap_or_default().to_owned();
        return Ok((body["created"] == json!(true), id));
    }
    if body["type"]
        .as_str()
        .is_some_and(|t| t.ends_with("/not-linked"))
    {
        return Err(Captured::NotLinked);
    }
    Err(Captured::Failed(
        body["detail"]
            .as_str()
            .unwrap_or("unknown error")
            .to_owned(),
    ))
}

fn unix_to_rfc3339(seconds: &str) -> Option<String> {
    let seconds: i64 = seconds.parse().ok()?;
    // Days since the epoch → civil date (Howard Hinnant's algorithm), no date crate needed.
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    ))
}

/// What processing produced: the assertions remembered, the proposals waiting.
fn summary(status: &Value, t: &Texts) -> String {
    let mut remembered = Vec::new();
    let mut waiting = Vec::new();
    for proposal in status["proposals"].as_array().into_iter().flatten() {
        let texts: Vec<String> = proposal["alternatives"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|alt| alt["operations"].as_array().cloned().unwrap_or_default())
            .filter_map(|op| {
                op.pointer("/assertion/text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        match proposal["state"].as_str() {
            Some("accepted") => remembered.extend(texts),
            Some("open") => {
                let mut line = texts.first().cloned().unwrap_or_else(|| "?".to_owned());
                if proposal["risk"] == "conflict" {
                    line = format!("{line} ({})", t.conflict);
                }
                waiting.push(line);
            }
            _ => {}
        }
    }
    if remembered.is_empty() && waiting.is_empty() {
        return t.nothing.to_owned();
    }
    let mut out = Vec::new();
    if !remembered.is_empty() {
        out.push(t.remembered.to_owned());
        out.extend(remembered.iter().map(|r| format!("• {r}")));
    }
    if !waiting.is_empty() {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(t.waiting.to_owned());
        out.extend(waiting.iter().map(|w| format!("• {w}")));
    }
    out.join("\n")
}

fn router(bot: Arc<Bot>) -> Router {
    Router::new()
        .route(
            "/manifest",
            get(|| async {
                Json(serde_yaml_ng::from_str::<Value>(MANIFEST).unwrap_or(Value::Null))
            }),
        )
        .route(
            "/healthz",
            get(|| async { Json(json!({ "status": "ok" })) }),
        )
        .route("/v1/capabilities/notify.send@1", post(notify_send))
        .route("/v1/capabilities/notify.update@1", post(notify_update))
        .with_state(bot)
}

/// The core presents the module's token when it calls a capability.
fn from_core(bot: &Bot, headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        == Some(bot.token.as_str())
}

fn problem(status: StatusCode, detail: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        Json(json!({ "title": status.canonical_reason(), "status": status.as_u16(), "detail": detail })),
    )
        .into_response()
}

async fn notify_send(
    State(bot): State<Arc<Bot>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !from_core(&bot, &headers) {
        return problem(StatusCode::UNAUTHORIZED, "bad or missing token");
    }
    match bot.notify_send(&body).await {
        Ok(out) => Json(out).into_response(),
        // Telegram refuses for good (blocked bot, unknown chat): the core stops trying.
        Err(err) => problem(StatusCode::UNPROCESSABLE_ENTITY, &err),
    }
}

async fn notify_update(
    State(bot): State<Arc<Bot>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !from_core(&bot, &headers) {
        return problem(StatusCode::UNAUTHORIZED, "bad or missing token");
    }
    match bot.notify_update(&body).await {
        Ok(out) => Json(out).into_response(),
        Err(err) => problem(StatusCode::UNPROCESSABLE_ENTITY, &err),
    }
}

fn bot_from_env() -> Result<Bot, String> {
    let config: Value = std::env::var("FROOD_MODULE_CONFIG")
        .ok()
        .filter(|c| !c.trim().is_empty())
        .map(|c| serde_json::from_str(&c).map_err(|e| format!("FROOD_MODULE_CONFIG: {e}")))
        .transpose()?
        .unwrap_or(Value::Null);
    let bot_token =
        std::env::var("TELEGRAM_BOT_TOKEN").map_err(|_| "TELEGRAM_BOT_TOKEN is not set")?;
    let api = std::env::var("TELEGRAM_API_URL").unwrap_or_else(|_| TELEGRAM_URL.to_owned());
    let api = api.trim_end_matches('/');
    Ok(Bot {
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?,
        telegram: format!("{api}/bot{bot_token}"),
        files: format!("{api}/file/bot{bot_token}"),
        frood: std::env::var("FROOD_URL")
            .map_err(|_| "FROOD_URL is not set")?
            .trim_end_matches('/')
            .to_owned(),
        token: std::env::var("FROOD_TOKEN").map_err(|_| "FROOD_TOKEN is not set")?,
        channel: std::env::var("FROOD_MODULE").unwrap_or_else(|_| "telegram".to_owned()),
        lang: if config["language"] == "ru" {
            Lang::Ru
        } else {
            Lang::En
        },
        poll_every: Duration::from_secs(2),
        poll_for: Duration::from_secs(180),
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().json().init();
    let bot = match bot_from_env() {
        Ok(bot) => bot,
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
    tracing::info!(port, channel = %bot.channel, lang = ?bot.lang, "telegram module started");
    let bot = Arc::new(bot);
    tokio::spawn({
        let bot = bot.clone();
        async move { (*bot).clone().run().await }
    });
    axum::serve(listener, router(bot.clone()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::extract::{Multipart, Path, State};
    use axum::http::HeaderMap;
    use axum::routing::{delete, post};

    use super::*;

    #[derive(Default)]
    struct World {
        sent: Vec<Value>,
        edited: Vec<Value>,
        captures: Vec<Value>,
        links: Vec<Value>,
        acts: Vec<Value>,
        answered: Vec<Value>,
    }

    type Shared = Arc<Mutex<World>>;

    /// Telegram and Frood in one fake server.
    async fn world() -> (String, Shared) {
        let shared: Shared = Arc::default();
        let app = Router::new()
            .route("/botTOKEN/sendMessage", post(|State(w): State<Shared>, Json(b): Json<Value>| async move {
                w.lock().unwrap().sent.push(b);
                Json(json!({ "ok": true, "result": { "message_id": 900 } }))
            }))
            .route("/botTOKEN/editMessageText", post(|State(w): State<Shared>, Json(b): Json<Value>| async move {
                w.lock().unwrap().edited.push(b);
                Json(json!({ "ok": true, "result": true }))
            }))
            .route("/botTOKEN/getFile", post(|Json(b): Json<Value>| async move {
                assert_eq!(b["file_id"], "voice-1");
                Json(json!({ "ok": true, "result": { "file_id": "voice-1", "file_path": "voice/file_7.oga" } }))
            }))
            .route("/file/botTOKEN/voice/file_7.oga", get(|| async { b"OggS-voice".to_vec() }))
            .route("/v1/channels/telegram/links", post(|State(w): State<Shared>, headers: HeaderMap, Json(b): Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer module-token");
                w.lock().unwrap().links.push(b);
                Json(json!({ "linked": true, "email": "alice@example.com" }))
            }))
            .route("/v1/channels/telegram/links/{id}", delete(|Path(_id): Path<String>| async { axum::http::StatusCode::NO_CONTENT }))
            .route("/v1/capture", post(|State(w): State<Shared>, headers: HeaderMap, request: axum::extract::Request| async move {
                let is_multipart = headers.get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("multipart/"));
                let mut record = json!({});
                if is_multipart {
                    use axum::extract::FromRequest;
                    let mut form = Multipart::from_request(request, &()).await.unwrap();
                    while let Some(field) = form.next_field().await.unwrap() {
                        let name = field.name().unwrap().to_owned();
                        if name == "file" {
                            record["content_type"] = json!(field.content_type());
                            record["file"] = json!(String::from_utf8(field.bytes().await.unwrap().to_vec()).unwrap());
                        } else {
                            record[name] = json!(field.text().await.unwrap());
                        }
                    }
                } else {
                    let bytes = axum::body::to_bytes(request.into_body(), usize::MAX).await.unwrap();
                    record = serde_json::from_slice(&bytes).unwrap();
                }
                let stranger = record["external_id"] == "99";
                w.lock().unwrap().captures.push(record);
                if stranger {
                    return (axum::http::StatusCode::FORBIDDEN, Json(json!({ "type": "https://frood.dev/problems/not-linked", "detail": "not linked" })));
                }
                (axum::http::StatusCode::ACCEPTED, Json(json!({ "source_id": "src-1", "created": true })))
            }))
            .route("/botTOKEN/answerCallbackQuery", post(|State(w): State<Shared>, Json(b): Json<Value>| async move {
                w.lock().unwrap().answered.push(b);
                Json(json!({ "ok": true, "result": true }))
            }))
            .route("/v1/inbox/{id}", get(|Path(id): Path<String>| async move {
                assert_eq!(id, "item-1");
                Json(json!({
                    "item": { "id": "item-1", "kind": "proposal", "state": "open" },
                    "content": { "risk": "conflict", "alternatives": [{ "key": "default", "changes": [{ "before": "Релиз — 14.11", "after": "Релиз — 28.11" }] }] },
                    "actions": [{ "action": "accept", "alternative": "default" }, { "action": "reject" }]
                }))
            }))
            .route("/v1/inbox/{id}/act", post(|State(w): State<Shared>, headers: HeaderMap, Json(b): Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer module-token");
                w.lock().unwrap().acts.push(b);
                Json(json!({ "outcome": "done", "item": { "id": "item-1", "kind": "proposal", "state": "resolved",
                    "resolution": { "action": "accept", "by": "user:u1/via/telegram" } } }))
            }))
            .route("/v1/sources/src-1/status", get(|| async {
                Json(json!({ "state": "done", "proposals": [
                    { "state": "accepted", "risk": "new", "alternatives": [{ "key": "default", "operations": [
                        { "op": "create_entity", "name": "Оля" },
                        { "op": "create", "assertion": { "text": "Релиз 2.4 — 14 ноября" } }
                    ] }] },
                    { "state": "open", "risk": "conflict", "alternatives": [
                        { "key": "apply", "operations": [{ "op": "supersede", "assertion": { "text": "Релиз 2.4 — 28 ноября" } }] },
                        { "key": "keep", "operations": [] }
                    ] }
                ] }))
            }))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, shared)
    }

    fn bot(url: &str, lang: Lang) -> Bot {
        Bot {
            http: reqwest::Client::new(),
            telegram: format!("{url}/botTOKEN"),
            files: format!("{url}/file/botTOKEN"),
            frood: url.to_owned(),
            token: "module-token".into(),
            channel: "telegram".into(),
            lang,
            poll_every: Duration::from_millis(10),
            poll_for: Duration::from_secs(2),
        }
    }

    fn message(from: i64, extra: Value) -> Message {
        let mut base = json!({ "message_id": 5, "chat": { "id": 77 }, "from": { "id": from, "username": "alice" }, "date": 1_792_281_600 });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    #[tokio::test]
    async fn links_captures_and_reports() {
        let (url, world) = world().await;
        let bot = bot(&url, Lang::Ru);

        bot.handle(message(42, json!({ "text": "/link k7m2-qx9p" })))
            .await;
        {
            let w = world.lock().unwrap();
            assert_eq!(
                w.links[0],
                json!({ "code": "k7m2-qx9p", "external_id": "42", "label": "@alice" })
            );
            assert_eq!(w.sent[0]["text"], "Связано с alice@example.com");
            assert_eq!(w.sent[0]["reply_parameters"]["message_id"], 5);
        }

        bot.handle(message(
            42,
            json!({ "text": "Договорились с Олей: релиз 2.4 — 14 ноября" }),
        ))
        .await;
        {
            let w = world.lock().unwrap();
            let capture = &w.captures[0];
            assert_eq!(capture["external_id"], "42");
            assert_eq!(capture["kind"], "message");
            assert_eq!(capture["said_at"], "2026-10-18T00:00:00Z");
            assert_eq!(w.sent[1]["text"], "Сохранено, обрабатываю…");
        }
        // The reply turns into a summary when processing is done.
        for _ in 0..100 {
            if !world.lock().unwrap().edited.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let edited = world.lock().unwrap().edited[0].clone();
        assert_eq!(edited["message_id"], 900);
        assert_eq!(
            edited["text"],
            "Запомнил:\n• Релиз 2.4 — 14 ноября\n\nЖдёт вашего решения:\n• Релиз 2.4 — 28 ноября (противоречит записанному)"
        );
    }

    #[tokio::test]
    async fn voice_goes_as_a_file_and_strangers_are_told_to_link() {
        let (url, world) = world().await;
        let bot = bot(&url, Lang::En);
        bot.handle(message(42, json!({ "voice": { "file_id": "voice-1", "mime_type": "audio/ogg", "duration": 3 }, "caption": "about the release" }))).await;
        {
            let w = world.lock().unwrap();
            let capture = &w.captures[0];
            assert_eq!(capture["file"], "OggS-voice");
            assert_eq!(capture["content_type"], "audio/ogg");
            assert_eq!(capture["kind"], "voice");
            assert_eq!(capture["text"], "about the release");
            assert_eq!(capture["external_id"], "42");
        }
        bot.handle(message(99, json!({ "text": "hello" }))).await;
        let w = world.lock().unwrap();
        assert!(
            w.sent.last().unwrap()["text"]
                .as_str()
                .unwrap()
                .starts_with("This Telegram account is not linked")
        );
    }

    #[tokio::test]
    async fn inbox_items_come_with_buttons_that_act_as_the_person() {
        let (url, world) = world().await;
        let bot = bot(&url, Lang::Ru);
        let shown = json!({
            "item": { "id": "item-1", "kind": "proposal", "state": "open" },
            "content": { "risk": "conflict", "alternatives": [{ "key": "default", "changes": [{ "before": "Релиз — 14.11", "after": "Релиз — 28.11" }] }] },
            "actions": [{ "action": "accept", "alternative": "default" }, { "action": "reject" }]
        });
        let out = bot
            .notify_send(
                &json!({ "recipient": { "user_id": "u1", "external_id": "42" }, "item": shown }),
            )
            .await
            .unwrap();
        assert_eq!(out["message_ref"], "42:900");
        {
            let w = world.lock().unwrap();
            assert_eq!(w.sent[0]["chat_id"], 42);
            assert!(
                w.sent[0]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("⚠️ Противоречит записанному")
            );
            assert_eq!(
                w.sent[0]["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
                "i:item-1:0"
            );
        }

        // The person presses «Принять».
        bot.on_button(&json!({
            "id": "q-1", "from": { "id": 42 }, "data": "i:item-1:0",
            "message": { "message_id": 900, "chat": { "id": 42 } }
        }))
        .await;
        let w = world.lock().unwrap();
        assert_eq!(
            w.acts[0],
            json!({ "action": "accept", "alternative": "default", "external_id": "42" })
        );
        assert_eq!(w.answered[0]["text"], "Готово");
        let edited = &w.edited[0];
        assert_eq!(edited["message_id"], 900);
        assert!(
            edited["text"].as_str().unwrap().ends_with("✅ Принято"),
            "{edited}"
        );
        assert_eq!(edited["reply_markup"]["inline_keyboard"], json!([]));
    }

    #[test]
    fn telegram_dates_become_rfc3339() {
        assert_eq!(
            unix_to_rfc3339("0").as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert_eq!(
            unix_to_rfc3339("1792281600").as_deref(),
            Some("2026-10-18T00:00:00Z")
        );
        assert_eq!(
            unix_to_rfc3339("951782400").as_deref(),
            Some("2000-02-29T00:00:00Z")
        );
    }

    #[test]
    fn manifest_asks_for_capture_and_channel_rights() {
        let manifest: Value = serde_yaml_ng::from_str(MANIFEST).unwrap();
        assert_eq!(
            manifest["frood_api"],
            json!(["knowledge:capture", "knowledge:read", "channels:act"])
        );
    }
}
