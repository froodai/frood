//! The link between two installations at work (docs/34 §4 W4): the polling side routes each
//! update, forwards the other side's at once and retries while it does not answer; the
//! forwarded side takes them on `channels.link@1` and serves the hashes of its work words.
//! What is forwarded is never stored, logged with its text or sent to this installation's
//! Frood; a message waiting for «Personal or Work?» lives in memory only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

use crate::Bot;
use crate::link::{self, Decision, HOLD_FOR, Link, Memory, Mode, Place, TOKEN_HEADER};

/// Entity types whose names make the work list (the extractor's types, docs/11).
const ENTITY_TYPES: [&str; 9] = [
    "person",
    "team",
    "project",
    "product",
    "organization",
    "system",
    "document",
    "place",
    "other",
];

/// The link and what it remembers while the module runs.
pub(crate) struct Linked {
    pub(crate) link: Link,
    pub(crate) memory: Mutex<Memory>,
    /// Messages waiting for «Personal or Work?», by the question's key.
    held: Mutex<HashMap<String, Held>>,
    /// The forwarded side's learned work words, plain (they are its own).
    learned: Mutex<HashSet<String>>,
    outbox: Mutex<VecDeque<Pending>>,
    wake: tokio::sync::Notify,
    /// The first wait before forwarding again; it doubles up to `retry_max`.
    pub(crate) retry_first: Duration,
    pub(crate) retry_max: Duration,
}

struct Held {
    update: Value,
    chat: i64,
    question: Option<i64>,
    at: Instant,
}

#[derive(Clone)]
struct Pending {
    body: Value,
    chat: Option<i64>,
    /// A press waiting for its answer; answered once while the other side is away.
    query: Option<String>,
}

enum Failure {
    /// Not reachable now, or a server error: try again.
    Transient(String),
    /// Refused for good, e.g. a wrong token.
    Refused(u16),
}

impl Linked {
    pub(crate) fn new(link: Link) -> Self {
        let words = link::load_words(link.state_dir.as_ref());
        let (personal, learned) = match link.mode {
            Mode::Poll => (words, HashSet::new()),
            Mode::Forwarded => (HashSet::new(), words),
        };
        Self {
            link,
            memory: Mutex::new(Memory {
                personal,
                ..Memory::default()
            }),
            held: Mutex::default(),
            learned: Mutex::new(learned),
            outbox: Mutex::default(),
            wake: tokio::sync::Notify::new(),
            retry_first: Duration::from_secs(2),
            retry_max: Duration::from_secs(120),
        }
    }

    /// Labels a message of the bot and, on the forwarded side, marks its buttons.
    pub(crate) fn dress(&self, method: &str, body: &mut Value) {
        if matches!(method, "sendMessage" | "editMessageText")
            && let Some(text) = body["text"].as_str()
        {
            let html = body["parse_mode"] == "HTML";
            body["text"] = Value::String(self.link.label(text, html));
        }
        if self.link.mode == Mode::Forwarded
            && let Some(markup) = body.get_mut("reply_markup")
        {
            link::mark(markup);
        }
    }

    pub(crate) fn polls(&self) -> bool {
        self.link.mode == Mode::Poll
    }
}

/// The chat an update came from.
fn chat_of(update: &Value) -> Option<i64> {
    update
        .pointer("/message/chat/id")
        .or_else(|| update.pointer("/callback_query/message/chat/id"))
        .and_then(Value::as_i64)
}

fn text_of(update: &Value) -> &str {
    update
        .pointer("/message/text")
        .or_else(|| update.pointer("/message/caption"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// A short key for a held message, unique within one run.
fn hold_key() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("{:x}", NEXT.fetch_add(1, Ordering::Relaxed))
}

impl Bot {
    fn bridge(&self) -> &Linked {
        self.link.as_deref().expect("called only with a link")
    }

    /// The polling side: where an update goes (docs/34 «Куда идёт сообщение»), decided at
    /// once in the order the updates came.
    pub(crate) fn route(&self, mut update: Value) {
        let linked = self.bridge();
        let decision = {
            let mut memory = linked.memory.lock().expect("memory");
            linked.link.decide(&mut update, &mut memory, Instant::now())
        };
        let bot = self.clone();
        match decision {
            Decision::Here => {
                tokio::spawn(async move { bot.accept(update).await });
            }
            Decision::There => self.forward(update, false),
            Decision::Both => {
                self.forward(update.clone(), false);
                tokio::spawn(async move { bot.accept(update).await });
            }
            Decision::Switched(place) => {
                tokio::spawn(async move { bot.switched(&update, place).await });
            }
            Decision::Ask => {
                tokio::spawn(async move { bot.ask_where(update).await });
            }
            Decision::Answer(place, key) => {
                tokio::spawn(async move { bot.answered(&update, place, &key).await });
            }
        }
    }

    /// An update handled here, as if no link were set.
    pub(crate) async fn accept(&self, update: Value) {
        let update: crate::Update = match serde_json::from_value(update) {
            Ok(update) => update,
            Err(err) => {
                tracing::warn!(error = %err, "an update Telegram sent does not read");
                return;
            }
        };
        if let Some(message) = update.message {
            self.handle(message).await;
        }
        if let Some(query) = update.callback_query {
            self.on_button(&query).await;
        }
    }

    /// Queues an update for the other side; the outbox sends it at once if it can.
    fn forward(&self, update: Value, learn: bool) {
        let linked = self.bridge();
        let pending = Pending {
            chat: chat_of(&update),
            query: update
                .pointer("/callback_query/id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            body: json!({ "op": "update", "update": update, "learn": learn }),
        };
        linked.outbox.lock().expect("outbox").push_back(pending);
        linked.wake.notify_one();
    }

    async fn post_link(&self, body: &Value) -> Result<Value, Failure> {
        let link = &self.bridge().link;
        let response = self
            .http
            .post(format!("{}/v1/channels/{}/link", link.url, link.channel))
            .bearer_auth(&link.token)
            .timeout(Duration::from_secs(30))
            .json(body)
            .send()
            .await
            // Without the URL: it says nothing secret, but the error is enough.
            .map_err(|e| Failure::Transient(crate::redact(e)))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response.json().await.unwrap_or(Value::Null));
        }
        match status.as_u16() {
            code @ (400 | 401 | 403 | 404 | 413 | 422) => Err(Failure::Refused(code)),
            code => Err(Failure::Transient(format!("HTTP {code}"))),
        }
    }

    /// Sends what waits for the other side, in order. While it does not answer, waits longer
    /// each time and tells the author once; nothing survives a restart (docs/34 W4).
    pub(crate) async fn deliver(&self) {
        let linked = self.bridge();
        let l = self.lang.link();
        let there = &linked.link.there;
        let mut wait = linked.retry_first;
        let mut told = false;
        loop {
            let front = linked.outbox.lock().expect("outbox").front().cloned();
            let Some(pending) = front else {
                linked.wake.notified().await;
                continue;
            };
            match self.post_link(&pending.body).await {
                Ok(_) => {
                    linked.outbox.lock().expect("outbox").pop_front();
                    wait = linked.retry_first;
                    told = false;
                }
                Err(Failure::Refused(status)) => {
                    linked.outbox.lock().expect("outbox").pop_front();
                    tracing::warn!(status, "the other installation refused a forwarded update");
                    let said = l
                        .refused
                        .replace("{name}", there)
                        .replace("{status}", &status.to_string());
                    if let Some(query) = &pending.query {
                        self.answer_query(query, &said).await;
                    }
                    if !told && let Some(chat) = pending.chat {
                        told = true;
                        self.say(chat, &said).await;
                    }
                }
                Err(Failure::Transient(err)) => {
                    tracing::warn!(error = %err, retry_in = ?wait, "the other installation does not answer; keeping the update in memory");
                    let said = l.later.replace("{name}", there);
                    if let Some(query) = &pending.query {
                        self.answer_query(query, &said).await;
                        if let Some(front) = linked.outbox.lock().expect("outbox").front_mut() {
                            front.query = None;
                        }
                    }
                    if !told && let Some(chat) = pending.chat {
                        told = true;
                        self.say(chat, &said).await;
                    }
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(linked.retry_max);
                }
            }
        }
    }

    /// How many updates wait for the other side.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.bridge().outbox.lock().expect("outbox").len()
    }

    /// Asks the other side for its work words now and then, and keeps the last list.
    pub(crate) async fn refresh_words(&self) {
        loop {
            match self.post_link(&json!({ "op": "words" })).await {
                Ok(answer) => {
                    let hashes: HashSet<String> = answer["hashes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    self.bridge().memory.lock().expect("memory").work = hashes;
                }
                Err(Failure::Transient(err)) => {
                    tracing::warn!(error = %err, "cannot ask for the work words; keeping the last list")
                }
                Err(Failure::Refused(status)) => {
                    tracing::warn!(
                        status,
                        "the other installation refused to give its work words"
                    )
                }
            }
            tokio::time::sleep(link::WORDS_EVERY).await;
        }
    }

    /// A message of the router itself, without a label: it speaks for neither installation.
    async fn say(&self, chat: i64, text: &str) -> Option<i64> {
        self.telegram_plain("sendMessage", json!({ "chat_id": chat, "text": text }))
            .await
            .ok()
            .and_then(|sent| sent["message_id"].as_i64())
    }

    async fn answer_query(&self, query: &str, text: &str) {
        let _ = self
            .telegram_plain(
                "answerCallbackQuery",
                json!({ "callback_query_id": query, "text": text }),
            )
            .await;
    }

    async fn switched(&self, update: &Value, place: Place) {
        let link = &self.bridge().link;
        let name = match place {
            Place::Here => &link.here,
            Place::There => &link.there,
        };
        if let Some(chat) = chat_of(update) {
            self.say(chat, &self.lang.link().switched.replace("{name}", name))
                .await;
        }
    }

    /// «Personal or Work?» with two buttons; until the answer the message stays in memory.
    async fn ask_where(&self, update: Value) {
        let linked = self.bridge();
        let Some(chat) = chat_of(&update) else {
            return;
        };
        let key = hold_key();
        let [(a, place_a), (b, place_b)] = linked.link.ordered();
        let data = |place: Place| match place {
            Place::Here => format!("r:h:{key}"),
            Place::There => format!("r:t:{key}"),
        };
        let mut body = json!({
            "chat_id": chat,
            "text": self.lang.link().which.replace("{a}", a).replace("{b}", b),
            "reply_markup": { "inline_keyboard": [[
                { "text": a, "callback_data": data(place_a) },
                { "text": b, "callback_data": data(place_b) }
            ]] }
        });
        if let Some(id) = update.pointer("/message/message_id") {
            body["reply_parameters"] = json!({ "message_id": id });
        }
        linked.held.lock().expect("held").insert(
            key.clone(),
            Held {
                update,
                chat,
                question: None,
                at: Instant::now(),
            },
        );
        let question = self
            .telegram_plain("sendMessage", body)
            .await
            .ok()
            .and_then(|sent| sent["message_id"].as_i64());
        if let Some(held) = linked.held.lock().expect("held").get_mut(&key) {
            held.question = question;
        }
    }

    /// The author answered: the message goes there, and its words teach the next decision.
    async fn answered(&self, press: &Value, place: Place, key: &str) {
        let linked = self.bridge();
        let l = self.lang.link();
        let query = press
            .pointer("/callback_query/id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(held) = linked.held.lock().expect("held").remove(key) else {
            self.answer_query(query, l.gone).await;
            return;
        };
        let name = match place {
            Place::Here => &linked.link.here,
            Place::There => &linked.link.there,
        };
        self.answer_query(query, &l.sent_to.replace("{name}", name))
            .await;
        if let Some(question) = held.question {
            let _ = self
                .telegram_plain(
                    "editMessageText",
                    json!({ "chat_id": held.chat, "message_id": question, "text": l.sent_to.replace("{name}", name) }),
                )
                .await;
        }
        linked
            .memory
            .lock()
            .expect("memory")
            .last
            .insert(held.chat, (place, Instant::now()));
        let words = link::distinctive(text_of(&held.update));
        match place {
            Place::There => self.forward(held.update, true),
            Place::Here => {
                {
                    let mut memory = linked.memory.lock().expect("memory");
                    memory.personal.extend(words.iter().cloned());
                    link::save_words(linked.link.state_dir.as_ref(), &memory.personal);
                }
                // The other side forgets them, if it learned them; it gets hashes, not words.
                if !words.is_empty() {
                    let hashes: Vec<String> = words.iter().map(|w| linked.link.hash(w)).collect();
                    let bot = self.clone();
                    tokio::spawn(async move {
                        let _ = bot
                            .post_link(&json!({ "op": "forget", "hashes": hashes }))
                            .await;
                    });
                }
                self.accept(held.update).await;
            }
        }
    }

    /// Drops messages that waited an hour for the answer, and says so.
    pub(crate) async fn sweep_held(&self, every: Duration) {
        loop {
            tokio::time::sleep(every).await;
            let gone: Vec<Held> = {
                let mut held = self.bridge().held.lock().expect("held");
                let old: Vec<String> = held
                    .iter()
                    .filter(|(_, h)| h.at.elapsed() >= HOLD_FOR)
                    .map(|(k, _)| k.clone())
                    .collect();
                old.iter().filter_map(|k| held.remove(k)).collect()
            };
            for held in gone {
                let text = self.lang.link().dropped;
                match held.question {
                    Some(question) => {
                        let _ = self
                            .telegram_plain(
                                "editMessageText",
                                json!({ "chat_id": held.chat, "message_id": question, "text": text }),
                            )
                            .await;
                    }
                    None => {
                        self.say(held.chat, text).await;
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn age_held(&self, by: Duration) {
        for held in self.bridge().held.lock().expect("held").values_mut() {
            held.at -= by;
        }
    }

    /// The forwarded side's `channels.link@1`: an update, the work words, or words to forget.
    /// The caller presents the link token (the core passes it on in `x-frood-link-token`).
    pub(crate) async fn link_call(&self, headers: &HeaderMap, body: Value) -> (StatusCode, Value) {
        let Some(linked) = self.link.as_deref().filter(|l| !l.polls()) else {
            return (
                StatusCode::NOT_FOUND,
                json!({ "detail": "this installation does not take forwarded updates" }),
            );
        };
        let presented = headers
            .get(TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !link::same_secret(presented, &linked.link.token) {
            return (
                StatusCode::UNAUTHORIZED,
                json!({ "detail": "bad or missing link token" }),
            );
        }
        match body["op"].as_str() {
            Some("update") => {
                let mut update = body["update"].clone();
                if !update.is_object() {
                    return (StatusCode::BAD_REQUEST, json!({ "detail": "no update" }));
                }
                link::unmark(&mut update);
                if body["learn"] == true {
                    let mut learned = linked.learned.lock().expect("learned");
                    learned.extend(link::distinctive(text_of(&update)));
                    link::save_words(linked.link.state_dir.as_ref(), &learned);
                }
                let bot = self.clone();
                tokio::spawn(async move { bot.accept(update).await });
                (StatusCode::ACCEPTED, json!({ "accepted": true }))
            }
            Some("words") => match self.vocabulary().await {
                Ok(terms) => {
                    let mut hashes: Vec<String> =
                        terms.iter().map(|t| linked.link.hash(t)).collect();
                    hashes.sort();
                    hashes.dedup();
                    (StatusCode::OK, json!({ "hashes": hashes }))
                }
                Err(err) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({ "detail": err, "retryable": true }),
                ),
            },
            Some("forget") => {
                let hashes: HashSet<&str> = body["hashes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let mut learned = linked.learned.lock().expect("learned");
                let before = learned.len();
                learned.retain(|w| !hashes.contains(linked.link.hash(w).as_str()));
                let forgotten = before - learned.len();
                if forgotten > 0 {
                    link::save_words(linked.link.state_dir.as_ref(), &learned);
                }
                (StatusCode::OK, json!({ "forgotten": forgotten }))
            }
            _ => (
                StatusCode::BAD_REQUEST,
                json!({ "detail": "op is update, words or forget" }),
            ),
        }
    }

    /// The forwarded side's work words: the names its knowledge holds (people, projects,
    /// tickets…), the config's words and what it learned.
    async fn vocabulary(&self) -> Result<HashSet<String>, String> {
        let linked = self.bridge();
        let mut terms = HashSet::new();
        for kind in ENTITY_TYPES {
            let entities = self
                .get(&format!("/v1/entities?type={kind}&limit=200"))
                .await?;
            for entity in entities.as_array().into_iter().flatten() {
                let names = std::iter::once(&entity["name"])
                    .chain(entity["aliases"].as_array().into_iter().flatten())
                    .filter_map(Value::as_str);
                for name in names {
                    terms.extend(link::name_terms(name));
                }
            }
        }
        for word in &linked.link.words {
            terms.extend(link::name_terms(word));
        }
        terms.extend(linked.learned.lock().expect("learned").iter().cloned());
        Ok(terms)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};

    use super::*;
    use crate::lang::Lang;
    use crate::tests::{bot, message, world};

    const TOKEN: &str = "a-long-random-link-token-for-tests-only-0123456789";

    /// The other installation's core, as the polling side sees it: up or down, and what it got.
    #[derive(Default)]
    struct Work {
        up: AtomicBool,
        got: Mutex<Vec<Value>>,
    }

    async fn work() -> (String, Arc<Work>) {
        let work = Arc::new(Work::default());
        let app = Router::new()
            .route(
                "/v1/channels/telegram/link",
                post(
                    |State(w): State<Arc<Work>>, headers: HeaderMap, Json(b): Json<Value>| async move {
                        if headers["authorization"] != format!("Bearer {TOKEN}").as_str() {
                            return (StatusCode::UNAUTHORIZED, Json(json!({})));
                        }
                        if !w.up.load(Ordering::SeqCst) {
                            return (StatusCode::BAD_GATEWAY, Json(json!({})));
                        }
                        let words = b["op"] == "words";
                        w.got.lock().unwrap().push(b);
                        if words {
                            let hashes = [link::hash(TOKEN, "proj"), link::hash(TOKEN, "анна петрова")];
                            return (StatusCode::OK, Json(json!({ "hashes": hashes })));
                        }
                        (StatusCode::ACCEPTED, Json(json!({ "accepted": true })))
                    },
                ),
            )
            .with_state(work.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, work)
    }

    fn linked(bot: Bot, config: Value) -> Bot {
        let link = Link::from_config(&config, bot.lang, Some(TOKEN.to_owned()))
            .unwrap()
            .unwrap();
        let mut linked = Linked::new(link);
        linked.retry_first = Duration::from_millis(20);
        linked.retry_max = Duration::from_millis(40);
        Bot {
            link: Some(Arc::new(linked)),
            ..bot
        }
    }

    /// The personal side: it polls, and forwards work to `work_url`.
    fn personal(url: &str, work_url: &str) -> Bot {
        linked(
            bot(url, Lang::Ru),
            json!({ "link": { "name": "Работа", "url": work_url } }),
        )
    }

    /// The work side: it takes forwarded updates.
    fn forwarded(url: &str) -> Bot {
        linked(
            bot(url, Lang::Ru),
            json!({ "label": "Работа", "link": { "mode": "forwarded", "name": "Личное", "url": "https://me.example.com" } }),
        )
    }

    fn update(text: &str) -> Value {
        json!({ "update_id": 1, "message": { "message_id": 5, "chat": { "id": 77 }, "from": { "id": 42, "username": "alice" }, "date": 1_792_281_600, "text": text } })
    }

    async fn until(what: impl Fn() -> bool) {
        for _ in 0..300 {
            if what() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out");
    }

    fn headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(TOKEN_HEADER, token.parse().unwrap());
        headers
    }

    #[tokio::test]
    async fn one_installation_alone_works_as_before() {
        let (url, world) = world().await;
        let bot = bot(&url, Lang::Ru);
        assert!(bot.link.is_none());
        bot.handle(message(42, json!({ "text": "Договорились с Олей" })))
            .await;
        {
            let w = world.lock().unwrap();
            assert_eq!(w.sent[0]["text"], "Сохранено, обрабатываю…", "no label");
            assert_eq!(
                w.sent[0]["reply_markup"]["inline_keyboard"][0][0]["callback_data"], "s:src-1",
                "no mark"
            );
        }
        // Nobody can forward to it.
        let (status, _) = bot
            .link_call(&headers(TOKEN), json!({ "op": "words" }))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn each_side_labels_its_messages_and_the_work_side_marks_its_buttons() {
        let (url, world) = world().await;
        let work = forwarded(&url);
        let out = work
            .notify_send(&json!({ "recipient": { "external_id": "42" }, "item": crate::tests::card("item-1") }))
            .await
            .unwrap();
        assert_eq!(out["message_ref"], "42:900");
        {
            let w = world.lock().unwrap();
            let text = w.sent[0]["text"].as_str().unwrap();
            assert!(text.starts_with("<b>Работа</b>\n<b>⚠️"), "{text}");
            let data = w.sent[0]["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
                .as_str()
                .unwrap();
            assert_eq!(data, "~i:item-1:0");
            assert!(data.len() <= 64);
        }
        let (url2, world2) = crate::tests::world().await;
        let personal = personal(&url2, "http://127.0.0.1:1");
        personal
            .reply(&message(42, json!({ "text": "x" })), "Готово")
            .await;
        let w = world2.lock().unwrap();
        assert_eq!(w.sent[0]["text"], "Личное\nГотово");
    }

    #[tokio::test]
    async fn the_work_side_takes_forwarded_updates_with_the_link_token_only() {
        let (url, world) = world().await;
        let work = forwarded(&url);
        for wrong in [
            "",
            "a-long-random-link-token-for-tests-only-0123456780",
            "short",
        ] {
            let (status, _) = work
                .link_call(
                    &headers(wrong),
                    json!({ "op": "update", "update": update("hi") }),
                )
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{wrong:?}");
        }
        assert!(world.lock().unwrap().captures.is_empty());

        // The message is handled as if this side had polled it: captured as the person, if
        // they are linked here (the core's allow-list still decides).
        let (status, _) = work
            .link_call(
                &headers(TOKEN),
                json!({ "op": "update", "update": update("PROJ-9 падает логин") }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        until(|| !world.lock().unwrap().sent.is_empty()).await;
        {
            let w = world.lock().unwrap();
            assert_eq!(w.captures[0]["text"], "PROJ-9 падает логин");
            assert_eq!(w.captures[0]["external_id"], "42");
            assert!(w.sent[0]["text"].as_str().unwrap().starts_with("Работа\n"));
        }
        let mut stranger = update("hello");
        stranger["message"]["from"]["id"] = json!(99);
        work.link_call(
            &headers(TOKEN),
            json!({ "op": "update", "update": stranger }),
        )
        .await;
        until(|| world.lock().unwrap().sent.len() == 2).await;
        assert!(
            world.lock().unwrap().sent[1]["text"]
                .as_str()
                .unwrap()
                .contains("not linked")
                || world.lock().unwrap().sent[1]["text"]
                    .as_str()
                    .unwrap()
                    .contains("не связан")
        );

        // A press of its own marked button acts on the item.
        let press = json!({ "update_id": 3, "callback_query": { "id": "q-1", "from": { "id": 42 }, "data": "~i:item-1:0",
            "message": { "message_id": 900, "chat": { "id": 42 } } } });
        work.link_call(&headers(TOKEN), json!({ "op": "update", "update": press }))
            .await;
        until(|| !world.lock().unwrap().acts.is_empty()).await;
        assert_eq!(world.lock().unwrap().acts[0]["external_id"], "42");
        // A link code from the work console.
        work.link_call(
            &headers(TOKEN),
            json!({ "op": "update", "update": update("/start l-K7M2QX9P") }),
        )
        .await;
        until(|| !world.lock().unwrap().links.is_empty()).await;
        assert_eq!(world.lock().unwrap().links[0]["code"], "K7M2QX9P");
    }

    #[tokio::test]
    async fn the_work_side_serves_hashes_of_its_words_and_learns() {
        let (url, _world) = world().await;
        let work = forwarded(&url);
        let words = |answer: &Value| -> HashSet<String> {
            answer["hashes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h.as_str().unwrap().to_owned())
                .collect()
        };
        let (status, answer) = work
            .link_call(&headers(TOKEN), json!({ "op": "words" }))
            .await;
        assert_eq!(status, StatusCode::OK);
        let hashes = words(&answer);
        for term in ["анна петрова", "аня", "proj-123", "proj"] {
            assert!(hashes.contains(&link::hash(TOKEN, term)), "{term}");
        }
        assert!(
            !answer.to_string().contains("Анна"),
            "hashes only, never words"
        );
        assert!(!hashes.contains(&link::hash(TOKEN, "мариной")));

        // The author answered «Работа»: the message's distinctive words join the list.
        let (status, _) = work
            .link_call(&headers(TOKEN), json!({ "op": "update", "update": update("Созвон с Мариной в 15:00"), "learn": true }))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let (_, answer) = work
            .link_call(&headers(TOKEN), json!({ "op": "words" }))
            .await;
        assert!(words(&answer).contains(&link::hash(TOKEN, "мариной")));
        // The personal side said they are personal after all: forgotten, by hash.
        let (_, answer) = work
            .link_call(
                &headers(TOKEN),
                json!({ "op": "forget", "hashes": [link::hash(TOKEN, "мариной")] }),
            )
            .await;
        assert_eq!(answer["forgotten"], 1);
        let (_, answer) = work
            .link_call(&headers(TOKEN), json!({ "op": "words" }))
            .await;
        assert!(!words(&answer).contains(&link::hash(TOKEN, "мариной")));
    }

    #[tokio::test]
    async fn work_goes_to_work_at_once_and_waits_in_memory_while_it_is_down() {
        let (url, world) = world().await;
        let (work_url, work) = work().await;
        let me = personal(&url, &work_url);
        let deliver = {
            let me = me.clone();
            tokio::spawn(async move { me.deliver().await })
        };
        me.route(update("работа: PROJ-1 горит"));
        me.route(update("и ещё одно"));
        // Told once, nothing kept by the personal Frood.
        until(|| world.lock().unwrap().sent.len() == 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        {
            let w = world.lock().unwrap();
            assert_eq!(w.sent.len(), 1, "said once: {:?}", w.sent);
            assert_eq!(
                w.sent[0]["text"],
                "«Работа» сейчас не отвечает. Доставлю, когда ответит; если бот перезапустится раньше, сообщение пропадёт."
            );
            assert!(w.captures.is_empty(), "work is never captured here");
        }
        assert_eq!(me.waiting(), 2);
        work.up.store(true, Ordering::SeqCst);
        until(|| me.waiting() == 0).await;
        let got = work.got.lock().unwrap().clone();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0]["update"]["message"]["text"], "PROJ-1 горит",
            "the prefix is cut"
        );
        assert_eq!(
            got[1]["update"]["message"]["text"], "и ещё одно",
            "a follow-up goes the same way"
        );
        assert!(world.lock().unwrap().captures.is_empty());
        deliver.abort();
    }

    #[tokio::test]
    async fn work_words_and_read_only_commands() {
        let (url, world) = world().await;
        let (work_url, work) = work().await;
        work.up.store(true, Ordering::SeqCst);
        let me = personal(&url, &work_url);
        let tasks = [me.clone(), me.clone()];
        let [a, b] = tasks;
        let deliver = tokio::spawn(async move { a.deliver().await });
        let refresh = tokio::spawn(async move { b.refresh_words().await });
        until(|| !me.bridge().memory.lock().unwrap().work.is_empty()).await;
        // «Анна Петрова» is a work name: the message goes there by hash.
        me.route(update("Анна Петрова просила отчёт"));
        until(|| work.got.lock().unwrap().iter().any(|g| g["op"] == "update")).await;
        assert!(world.lock().unwrap().captures.is_empty());
        // /now gets an answer from both.
        me.route(update("/now"));
        until(|| {
            work.got
                .lock()
                .unwrap()
                .iter()
                .filter(|g| g["op"] == "update")
                .count()
                == 2
        })
        .await;
        deliver.abort();
        refresh.abort();
    }

    #[tokio::test]
    async fn not_sure_asks_and_holds_in_memory_only() {
        let (url, world) = world().await;
        let (work_url, work) = work().await;
        work.up.store(true, Ordering::SeqCst);
        let me = personal(&url, &work_url);
        let deliver = {
            let me = me.clone();
            tokio::spawn(async move { me.deliver().await })
        };
        me.route(update("Созвон с Мариной перенесли"));
        until(|| !world.lock().unwrap().sent.is_empty()).await;
        let (question, there) = {
            let w = world.lock().unwrap();
            assert!(w.captures.is_empty());
            let q = &w.sent[0];
            assert_eq!(q["text"], "Личное или Работа?", "no label: the router asks");
            assert_eq!(q["reply_parameters"]["message_id"], 5);
            let buttons = &q["reply_markup"]["inline_keyboard"][0];
            assert_eq!(buttons[0]["text"], "Личное");
            (
                q.clone(),
                buttons[1]["callback_data"].as_str().unwrap().to_owned(),
            )
        };
        assert!(there.starts_with("r:t:"), "{question}");
        assert!(
            work.got.lock().unwrap().is_empty(),
            "nothing leaves before the answer"
        );
        let press = |data: &str| {
            json!({ "update_id": 2, "callback_query": { "id": "q-9", "from": { "id": 42 }, "data": data,
            "message": { "message_id": 900, "chat": { "id": 77 } } } })
        };
        me.route(press(&there));
        until(|| !work.got.lock().unwrap().is_empty()).await;
        {
            let got = work.got.lock().unwrap();
            assert_eq!(
                got[0]["learn"], true,
                "work learns its words from the answer"
            );
            assert_eq!(
                got[0]["update"]["message"]["text"],
                "Созвон с Мариной перенесли"
            );
        }
        until(|| !world.lock().unwrap().edited.is_empty()).await;
        assert_eq!(world.lock().unwrap().edited[0]["text"], "→ Работа");
        // The same button again: the question is gone.
        me.route(press(&there));
        until(|| world.lock().unwrap().answered.len() == 2).await;
        assert_eq!(
            world.lock().unwrap().answered[1]["text"],
            "Этот вопрос уже не действует, пришлите сообщение ещё раз."
        );

        // Answered «Личное»: kept here, and its words are personal from now on.
        me.bridge().memory.lock().unwrap().last.clear();
        let mut other = update("Ужин с Лизой в субботу");
        other["message"]["chat"]["id"] = json!(78);
        me.route(other);
        until(|| world.lock().unwrap().sent.len() == 2).await;
        let here =
            world.lock().unwrap().sent[1]["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
                .as_str()
                .unwrap()
                .to_owned();
        assert!(here.starts_with("r:h:"));
        me.route(press(&here));
        until(|| !world.lock().unwrap().captures.is_empty()).await;
        assert_eq!(
            world.lock().unwrap().captures[0]["text"],
            "Ужин с Лизой в субботу"
        );
        assert!(
            me.bridge()
                .memory
                .lock()
                .unwrap()
                .personal
                .contains("лизой")
        );
        // The other side is told to forget them, by hash.
        until(|| work.got.lock().unwrap().iter().any(|g| g["op"] == "forget")).await;
        let forget = work
            .got
            .lock()
            .unwrap()
            .iter()
            .find(|g| g["op"] == "forget")
            .cloned()
            .unwrap();
        assert!(!forget.to_string().contains("лиз"), "{forget}");
        deliver.abort();
    }

    #[tokio::test]
    async fn an_unanswered_question_is_dropped_after_an_hour() {
        let (url, world) = world().await;
        let me = personal(&url, "http://127.0.0.1:1");
        me.route(update("что-то неясное"));
        until(|| !world.lock().unwrap().sent.is_empty()).await;
        until(|| {
            me.bridge()
                .held
                .lock()
                .unwrap()
                .values()
                .all(|h| h.question.is_some())
        })
        .await;
        me.age_held(HOLD_FOR);
        let sweep = {
            let me = me.clone();
            tokio::spawn(async move { me.sweep_held(Duration::from_millis(10)).await })
        };
        until(|| !world.lock().unwrap().edited.is_empty()).await;
        assert_eq!(
            world.lock().unwrap().edited[0]["text"],
            "Ответа не было час, поэтому сообщение не сохранено. Пришлите его ещё раз."
        );
        assert!(me.bridge().held.lock().unwrap().is_empty());
        assert!(world.lock().unwrap().captures.is_empty());
        sweep.abort();
    }

    #[tokio::test]
    async fn a_wrong_token_is_said_once_and_not_retried() {
        let (url, world) = world().await;
        let (work_url, work) = work().await;
        work.up.store(true, Ordering::SeqCst);
        let me = linked(
            bot(&url, Lang::En),
            json!({ "link": { "name": "Work", "url": work_url, "token": "another-long-token-that-the-work-side-does-not-know" } }),
        );
        let deliver = {
            let me = me.clone();
            tokio::spawn(async move { me.deliver().await })
        };
        me.route(update("work: ship it"));
        until(|| me.waiting() == 0).await;
        until(|| !world.lock().unwrap().sent.is_empty()).await;
        assert_eq!(
            world.lock().unwrap().sent[0]["text"],
            "Work does not take messages (401). Check the link token."
        );
        assert!(work.got.lock().unwrap().is_empty());
        deliver.abort();
    }
}
