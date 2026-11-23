//! One bot for two installations (docs/34 §3 QW4, §4 W4). Telegram gives a bot's updates to
//! one poller only, so one side polls (`mode: poll`, the personal Frood) and forwards the other
//! side's updates to it; the other side (`mode: forwarded`, the work Frood) never polls and
//! takes them on `channels.link@1`, which its core relays from
//! `POST /v1/channels/{channel}/link`. Both send with the same bot token, each message
//! starting with its installation's label, and the forwarded side's buttons carry a mark, so a
//! reply or a press finds its way back without any state.
//!
//! This file holds what needs no network: the config, the words and their hashes, labels,
//! marks, prefixes and the order in which a message picks its installation.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

use crate::lang::Lang;

/// The first character of the callback data of the forwarded side's buttons.
pub(crate) const MARK: char = '~';
/// The forwarded side's link codes start with this in `/start`, so the polling side knows
/// where the code belongs (`https://t.me/<bot>?start=l-<code>`).
pub(crate) const START: &str = "l-";
/// The header the core passes the caller's link token in.
pub(crate) const TOKEN_HEADER: &str = "x-frood-link-token";
/// A message with no other signal follows the author's last routed one for this long.
pub(crate) const FOLLOW_UP: Duration = Duration::from_secs(600);
/// A message waits for «Personal or Work?» this long, in memory only.
pub(crate) const HOLD_FOR: Duration = Duration::from_secs(3600);
/// How often the polling side asks for the other side's work words.
pub(crate) const WORDS_EVERY: Duration = Duration::from_secs(600);
/// A shorter link token is refused: it guards a door on the internet.
const MIN_TOKEN: usize = 32;
/// How many words one answer teaches.
const LEARN_AT_MOST: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    /// Polls Telegram and forwards the other installation's updates.
    Poll,
    /// Takes its updates from the polling side.
    Forwarded,
}

/// The link's settings (`label`, `link` and `state_dir` in the module's config).
#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub(crate) mode: Mode,
    /// This installation's label.
    pub(crate) here: String,
    /// The other installation's label.
    pub(crate) there: String,
    /// The other installation's address, without a trailing slash.
    pub(crate) url: String,
    /// The Telegram module's name there.
    pub(crate) channel: String,
    pub(crate) token: String,
    /// The forwarded side: words that always mean it, besides what its knowledge names.
    pub(crate) words: Vec<String>,
    /// Where learned words are kept across restarts; memory only without it.
    pub(crate) state_dir: Option<PathBuf>,
}

/// Where a message goes, as the polling side sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    Here,
    There,
}

impl Link {
    /// The link from the module's config; none when `link` is not set. `env_token` is
    /// `TELEGRAM_LINK_TOKEN`, for installations that keep the token out of the config.
    pub(crate) fn from_config(
        config: &Value,
        lang: Lang,
        env_token: Option<String>,
    ) -> Result<Option<Link>, String> {
        let Some(link) = config.get("link").filter(|l| !l.is_null()) else {
            return Ok(None);
        };
        let text = |v: &Value, key: &str| v[key].as_str().map(str::trim).map(str::to_owned);
        let mode = match link["mode"].as_str().unwrap_or("poll") {
            "poll" => Mode::Poll,
            "forwarded" => Mode::Forwarded,
            other => return Err(format!("link.mode is poll or forwarded, got {other:?}")),
        };
        let there = text(link, "name")
            .filter(|n| !n.is_empty())
            .ok_or("link.name is the other installation's label, e.g. Work")?;
        let here = text(config, "label")
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                match mode {
                    Mode::Poll => lang.link().personal,
                    Mode::Forwarded => lang.link().work,
                }
                .to_owned()
            });
        if here.chars().count() > 40 || there.chars().count() > 40 {
            return Err("labels are up to 40 characters".into());
        }
        if normalize(&here) == normalize(&there) {
            return Err("label and link.name must differ".into());
        }
        let url = text(link, "url")
            .ok_or("link.url is the other installation's address")?
            .trim_end_matches('/')
            .to_owned();
        if !(url.starts_with("https://") && url.len() > "https://".len() || is_internal_http(&url))
        {
            return Err(format!(
                "link.url must be https://, or http:// to a service inside the deployment, got {url:?}"
            ));
        }
        let token = text(link, "token")
            .filter(|t| !t.is_empty())
            .or(env_token.map(|t| t.trim().to_owned()))
            .ok_or("link.token (or TELEGRAM_LINK_TOKEN) is not set")?;
        if token.len() < MIN_TOKEN {
            return Err(format!(
                "the link token is shorter than {MIN_TOKEN} characters; make a long random one, e.g. `openssl rand -hex 32`"
            ));
        }
        let channel = text(link, "channel").unwrap_or_else(|| "telegram".to_owned());
        let words = link["words"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let state_dir = text(config, "state_dir")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        Ok(Some(Link {
            mode,
            here,
            there,
            url,
            channel,
            token,
            words,
            state_dir,
        }))
    }

    /// The labels in the order the bot shows them: the polling side's first.
    pub(crate) fn ordered(&self) -> [(&str, Place); 2] {
        match self.mode {
            Mode::Poll => [(&self.here, Place::Here), (&self.there, Place::There)],
            Mode::Forwarded => [(&self.there, Place::There), (&self.here, Place::Here)],
        }
    }

    /// Words that name an installation in a prefix: its label, and the default labels in
    /// Russian and English («работа:», «work:»).
    fn prefixes(&self) -> Vec<(String, Place)> {
        let (polls, forwarded) = match self.mode {
            Mode::Poll => (Place::Here, Place::There),
            Mode::Forwarded => (Place::There, Place::Here),
        };
        let mut names = vec![
            (self.here.clone(), Place::Here),
            (self.there.clone(), Place::There),
        ];
        for lang in [Lang::Ru, Lang::En] {
            names.push((lang.link().personal.to_owned(), polls));
            names.push((lang.link().work.to_owned(), forwarded));
        }
        names
    }

    /// The label a message of the bot starts with, as a separate first line.
    pub(crate) fn label(&self, text: &str, html: bool) -> String {
        if html {
            format!("<b>{}</b>\n{text}", crate::notify::esc(&self.here))
        } else {
            format!("{}\n{text}", self.here)
        }
    }

    /// Which installation a message of the bot came from, by its first line.
    pub(crate) fn labelled(&self, text: &str) -> Option<Place> {
        let first = text.lines().next()?.trim();
        [(&self.here, Place::Here), (&self.there, Place::There)]
            .into_iter()
            .find(|(label, _)| first == label.as_str())
            .map(|(_, place)| place)
    }

    /// An explicit prefix («работа: …», case-insensitive): the place and the rest.
    pub(crate) fn prefixed(&self, text: &str) -> Option<(Place, String)> {
        let text = text.trim_start();
        self.prefixes().into_iter().find_map(|(name, place)| {
            let rest = strip_ci(text, &name)?;
            let rest = rest.trim_start().strip_prefix(':')?;
            Some((place, rest.trim_start().to_owned()))
        })
    }

    /// The hash of a normalized word, keyed with the link token: the polling side compares
    /// hashes and never holds the other side's words.
    pub(crate) fn hash(&self, term: &str) -> String {
        hash(&self.token, term)
    }
}

/// `text` without a leading `prefix`, compared without case.
fn strip_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let mut rest = text.char_indices();
    for want in prefix.chars() {
        let (_, got) = rest.next()?;
        if !got.to_lowercase().eq(want.to_lowercase()) {
            return None;
        }
    }
    Some(rest.next().map_or("", |(at, _)| &text[at..]))
}

/// Plain HTTP only to a service inside the deployment, as frood-config allows it for the
/// core's own outgoing addresses: loopback, a Compose service name without dots, a
/// Kubernetes Service (`*.svc`, `*.svc.cluster.local`). The host is read the way the HTTP
/// client reads it.
pub(crate) fn is_internal_http(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "http" {
        return false;
    }
    // The parser lowercases names and keeps an IPv6 address in brackets.
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
    {
        return ip.is_loopback();
    }
    host == "localhost"
        || !host.is_empty()
            && (!host.contains('.')
                || host.ends_with(".svc")
                || host.ends_with(".svc.cluster.local"))
}

pub(crate) fn hash(token: &str, term: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC takes any key");
    mac.update(term.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Two secrets compared in time that does not depend on where they differ.
pub(crate) fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A word as both sides compare it: lower case, «ё» as «е», no punctuation around it.
pub(crate) fn normalize(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
        .replace('ё', "е")
}

/// The words of a text, normalized, in order.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || ",;:!?()[]{}\"«»“”'`/".contains(c))
        .map(normalize)
        .filter(|w| !w.is_empty())
        .collect()
}

/// `proj-123` and the like: a tracker's key, a dash and a number.
fn ticket(word: &str) -> Option<&str> {
    let (key, number) = word.split_once('-')?;
    let key_ok = key.len() >= 2
        && key.chars().next().is_some_and(char::is_alphabetic)
        && key.chars().all(char::is_alphanumeric);
    (key_ok && !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).then_some(key)
}

/// What of a message is compared with the lists: each word, each two and three words in a row
/// (names like «анна петрова»), and the key of each ticket id.
pub(crate) fn terms(text: &str) -> BTreeSet<String> {
    let words = words(text);
    let mut out = BTreeSet::new();
    for n in 1..=3 {
        for run in words.windows(n) {
            out.insert(run.join(" "));
        }
    }
    for word in &words {
        if let Some(key) = ticket(word) {
            out.insert(key.to_owned());
        }
    }
    out
}

/// The terms a name gives the work list: the name and its aliases when short, and the ticket
/// ids and keys in it.
pub(crate) fn name_terms(name: &str) -> Vec<String> {
    let words = words(name);
    let mut out = Vec::new();
    if (1..=3).contains(&words.len()) {
        out.push(words.join(" "));
    }
    for word in &words {
        if let Some(key) = ticket(word) {
            out.push(word.clone());
            out.push(key.to_owned());
        }
    }
    out
}

/// The words of a message that tell its installation, learned when the author answers
/// «Personal or Work?»: ticket ids, words with digits and letters, names (a capital letter
/// not at a sentence's start) and abbreviations. Common words are left out, or the bot would
/// soon send everything one way.
pub(crate) fn distinctive(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut sentence_start = true;
    for raw in text.split_whitespace() {
        let word = normalize(raw);
        let bare = raw.trim_matches(|c: char| !c.is_alphanumeric());
        let capital = bare.chars().next().is_some_and(char::is_uppercase);
        let digits = bare.chars().any(|c| c.is_ascii_digit());
        let letters = bare.chars().any(char::is_alphabetic);
        let caps = bare.chars().filter(|c| c.is_uppercase()).count() >= 2
            && bare.chars().all(|c| !c.is_lowercase());
        let keep = !word.is_empty()
            && (ticket(&word).is_some()
                || digits && letters
                || caps
                || capital && !sentence_start && bare.chars().count() >= 3);
        if keep && !out.contains(&word) {
            out.push(word);
        }
        sentence_start = raw.ends_with(['.', '!', '?', '\n']);
        if out.len() >= LEARN_AT_MOST {
            break;
        }
    }
    out
}

/// The polling side's decision for one update.
#[derive(Debug, PartialEq)]
pub(crate) enum Decision {
    /// Handle it here.
    Here,
    /// Forward it.
    There,
    /// A read-only command: both answer, each with its label.
    Both,
    /// A bare prefix: what follows goes there.
    Switched(Place),
    /// Ask «Personal or Work?» and hold it until the answer.
    Ask,
    /// The answer to that question (`r:h:<key>` or `r:t:<key>`).
    Answer(Place, String),
}

/// What the polling side remembers to decide: the author's last routed message in each chat,
/// and the words of both sides. In memory: nothing here is a message.
#[derive(Default)]
pub(crate) struct Memory {
    pub(crate) last: HashMap<i64, (Place, Instant)>,
    /// Hashes of the other side's work words, as it last served them.
    pub(crate) work: HashSet<String>,
    /// Words the author said are personal, plain.
    pub(crate) personal: HashSet<String>,
}

impl Link {
    /// Where an update goes, first rule that applies (docs/34 «Куда идёт сообщение»). A
    /// prefix is cut off the message.
    pub(crate) fn decide(&self, update: &mut Value, memory: &mut Memory, now: Instant) -> Decision {
        if let Some(query) = update.get("callback_query") {
            let data = query["data"].as_str().unwrap_or_default();
            if data.starts_with(MARK) {
                return Decision::There;
            }
            let mut parts = data.splitn(3, ':');
            return match (parts.next(), parts.next(), parts.next()) {
                (Some("r"), Some("h"), Some(key)) => Decision::Answer(Place::Here, key.to_owned()),
                (Some("r"), Some("t"), Some(key)) => Decision::Answer(Place::There, key.to_owned()),
                _ => Decision::Here,
            };
        }
        let Some(message) = update.get_mut("message") else {
            return Decision::Here;
        };
        let chat = message["chat"]["id"].as_i64().unwrap_or_default();
        let routed = |place: Place, memory: &mut Memory| {
            memory.last.insert(chat, (place, now));
            match place {
                Place::Here => Decision::Here,
                Place::There => Decision::There,
            }
        };
        // 1. A reply to a bot message: the installation its label names, or its buttons.
        if let Some(replied) = message.get("reply_to_message") {
            let text = replied["text"].as_str().or(replied["caption"].as_str());
            let marked = replied
                .pointer("/reply_markup/inline_keyboard")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|row| row.as_array().cloned().unwrap_or_default())
                .any(|b| {
                    b["callback_data"]
                        .as_str()
                        .is_some_and(|d| d.starts_with(MARK))
                });
            let place = text
                .and_then(|t| self.labelled(t))
                .or(marked.then_some(Place::There));
            if let Some(place) = place {
                return routed(place, memory);
            }
        }
        let field = if message["text"].is_string() {
            "text"
        } else {
            "caption"
        };
        let text = message[field].as_str().unwrap_or_default().to_owned();
        // 2. An explicit prefix, cut off.
        if let Some((place, rest)) = self.prefixed(&text) {
            memory.last.insert(chat, (place, now));
            if rest.is_empty() && field == "text" {
                return Decision::Switched(place);
            }
            message[field] = Value::String(rest);
            return match place {
                Place::Here => Decision::Here,
                Place::There => Decision::There,
            };
        }
        // Commands, each decided once (docs/34 §4 W4).
        if field == "text" && text.starts_with('/') {
            let mut parts = text.split_whitespace();
            let command = parts.next().unwrap_or_default();
            let command = command.split('@').next().unwrap_or_default();
            let argument = parts.next().unwrap_or_default();
            match command {
                "/start" | "/link" if argument.starts_with(START) => return Decision::There,
                "/now" | "/inbox" | "/digest" | "/new" => return Decision::Both,
                // A question is a message like any other: its words decide.
                "/ask" => {}
                _ => return Decision::Here,
            }
        }
        let terms = terms(&text);
        // 3. Work words, by hash; a word the author called personal does not count.
        if terms
            .iter()
            .any(|t| !memory.personal.contains(t) && memory.work.contains(&self.hash(t)))
        {
            return routed(Place::There, memory);
        }
        // 4. Words learned from the author's answers.
        if terms.iter().any(|t| memory.personal.contains(t)) {
            return routed(Place::Here, memory);
        }
        // 5. A follow-up within ten minutes of the last routed message.
        if let Some((place, at)) = memory.last.get(&chat).copied()
            && now.duration_since(at) < FOLLOW_UP
        {
            return routed(place, memory);
        }
        // 6. Not sure: ask.
        Decision::Ask
    }
}

/// The forwarded side marks its buttons, so the polling side forwards a press unread.
pub(crate) fn mark(markup: &mut Value) {
    for row in markup
        .get_mut("inline_keyboard")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        for button in row.as_array_mut().into_iter().flatten() {
            if let Some(data) = button["callback_data"].as_str()
                && !data.starts_with(MARK)
            {
                // Telegram takes up to 64 bytes; the longest data here is under 50.
                button["callback_data"] = Value::String(format!("{MARK}{data}"));
            }
        }
    }
}

/// A forwarded update with the marks taken off, as if this side had sent its buttons plain.
pub(crate) fn unmark(update: &mut Value) {
    if let Some(data) = update.pointer_mut("/callback_query/data")
        && let Some(plain) = data.as_str().and_then(|d| d.strip_prefix(MARK))
    {
        *data = Value::String(plain.to_owned());
    }
    for path in [
        "/message/reply_to_message/reply_markup",
        "/callback_query/message/reply_markup",
    ] {
        if let Some(markup) = update.pointer_mut(path) {
            for row in markup
                .get_mut("inline_keyboard")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                for button in row.as_array_mut().into_iter().flatten() {
                    if let Some(plain) = button["callback_data"]
                        .as_str()
                        .and_then(|d| d.strip_prefix(MARK))
                    {
                        button["callback_data"] = Value::String(plain.to_owned());
                    }
                }
            }
        }
    }
    // A link code from the other side's console: `/start l-CODE`.
    if let Some(text) = update.pointer_mut("/message/text")
        && let Some(s) = text.as_str()
    {
        let mut parts = s.splitn(2, ' ');
        let (command, rest) = (parts.next().unwrap_or_default(), parts.next());
        if let Some(code) = rest.and_then(|r| r.trim().strip_prefix(START)) {
            *text = Value::String(format!("{command} {code}"));
        }
    }
}

/// Learned words kept across restarts in `state_dir`, when there is one.
pub(crate) fn load_words(dir: Option<&PathBuf>) -> HashSet<String> {
    dir.and_then(|d| std::fs::read(d.join("link-words.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
        .map(|words| words.into_iter().collect())
        .unwrap_or_default()
}

pub(crate) fn save_words(dir: Option<&PathBuf>, words: &HashSet<String>) {
    let Some(dir) = dir else {
        return;
    };
    let mut sorted: Vec<&String> = words.iter().collect();
    sorted.sort();
    let path = dir.join("link-words.json");
    let written = serde_json::to_vec(&sorted)
        .map_err(|e| e.to_string())
        .and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| e.to_string()));
    if let Err(err) = written {
        tracing::warn!(error = %err, "cannot keep the learned words; they stay in memory");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef-test";

    fn link() -> Link {
        Link::from_config(
            &json!({ "link": { "name": "Работа", "url": "https://work.example.com/", "token": TOKEN } }),
            Lang::Ru,
            None,
        )
        .unwrap()
        .unwrap()
    }

    fn text_update(text: &str) -> Value {
        json!({ "update_id": 1, "message": { "message_id": 5, "chat": { "id": 77 }, "from": { "id": 42 }, "text": text } })
    }

    #[test]
    fn the_config_names_both_sides_and_refuses_what_is_unsafe() {
        let link = link();
        assert_eq!(link.mode, Mode::Poll);
        assert_eq!(
            (link.here.as_str(), link.there.as_str()),
            ("Личное", "Работа")
        );
        assert_eq!(link.url, "https://work.example.com");
        assert_eq!(link.channel, "telegram");
        let config = |link: Value| Link::from_config(&json!({ "link": link }), Lang::En, None);
        assert!(
            config(json!({ "name": "Work", "url": "http://work.example.com", "token": TOKEN }))
                .is_err()
        );
        assert!(
            config(
                json!({ "name": "Work", "url": "http://127.0.0.1.example.com", "token": TOKEN })
            )
            .is_err()
        );
        assert!(
            config(json!({ "name": "Work", "url": "http://frood.team.svc:8080", "token": TOKEN }))
                .is_ok()
        );
        assert!(
            config(json!({ "name": "Work", "url": "https://w.example.com", "token": "short" }))
                .is_err()
        );
        assert!(
            config(json!({ "name": "Personal", "url": "https://w.example.com", "token": TOKEN }))
                .is_err(),
            "same label twice"
        );
        // The token can come from the environment instead.
        let forwarded = Link::from_config(
            &json!({ "label": "Job", "link": { "mode": "forwarded", "name": "Home", "url": "https://me.example.com" } }),
            Lang::En,
            Some(TOKEN.to_owned()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(forwarded.mode, Mode::Forwarded);
        assert_eq!(forwarded.here, "Job");
        assert_eq!(
            forwarded.ordered()[0].0,
            "Home",
            "the polling side's label first"
        );
        assert_eq!(
            Link::from_config(&json!({ "language": "en" }), Lang::En, None)
                .unwrap()
                .map(|l| l.here),
            None
        );
    }

    #[test]
    fn labels_are_a_first_line_and_read_back() {
        let link = link();
        assert_eq!(link.label("a <b>", false), "Личное\na <b>");
        assert_eq!(link.label("x", true), "<b>Личное</b>\nx");
        assert_eq!(link.labelled("Работа\nСохранено"), Some(Place::There));
        assert_eq!(link.labelled("Личное"), Some(Place::Here));
        assert_eq!(link.labelled("Работа над ошибками"), None);
    }

    #[test]
    fn prefixes_are_cut_without_case() {
        let link = link();
        assert_eq!(
            link.prefixed("РАБОТА: PROJ-1 горит"),
            Some((Place::There, "PROJ-1 горит".into()))
        );
        assert_eq!(
            link.prefixed("  личное :купить хлеб"),
            Some((Place::Here, "купить хлеб".into()))
        );
        assert_eq!(
            link.prefixed("Work: ship it"),
            Some((Place::There, "ship it".into()))
        );
        assert_eq!(link.prefixed("работаю сегодня до шести"), None);
        assert_eq!(link.prefixed("работа без двоеточия"), None);
    }

    #[test]
    fn hashes_need_the_token_and_terms_cover_names_and_tickets() {
        assert_eq!(hash(TOKEN, "proj-123"), hash(TOKEN, "proj-123"));
        assert_ne!(
            hash(TOKEN, "proj-123"),
            hash("another-token-another-token-xxxxx", "proj-123")
        );
        assert_eq!(hash(TOKEN, "x").len(), 64);
        let terms = terms("Анна Петрова, глянь PROJ-123!");
        for want in ["анна петрова", "proj-123", "proj", "глянь"] {
            assert!(terms.contains(want), "{want}: {terms:?}");
        }
        assert_eq!(name_terms("Ёлка Проект"), ["елка проект"]);
        assert_eq!(
            name_terms("PROJ-123: a very long ticket title"),
            ["proj-123", "proj"]
        );
    }

    #[test]
    fn distinctive_words_are_ids_names_and_abbreviations() {
        assert_eq!(
            distinctive("Созвон с Мариной по PROJ-77 и API v2 завтра. Потом обед"),
            ["мариной", "proj-77", "api", "v2"]
        );
        assert!(distinctive("купить хлеб и молоко").is_empty());
    }

    #[test]
    fn secrets_compare_whole() {
        assert!(same_secret(TOKEN, TOKEN));
        assert!(!same_secret(TOKEN, &TOKEN.replace('f', "e")));
        assert!(!same_secret(TOKEN, &TOKEN[1..]));
    }

    #[test]
    fn the_routing_order() {
        let link = link();
        let now = Instant::now();
        let mut memory = Memory::default();
        memory.work.insert(link.hash("proj-123"));
        memory.work.insert(link.hash("релиз"));

        // 1. A reply to a work message goes to work, whatever its words.
        let mut reply = text_update("купить хлеб");
        reply["message"]["reply_to_message"] =
            json!({ "message_id": 3, "text": "Работа\nСохранено" });
        assert_eq!(link.decide(&mut reply, &mut memory, now), Decision::There);
        // A work card that lost its label still has marked buttons.
        let mut card = text_update("ок");
        card["message"]["reply_to_message"] = json!({ "message_id": 3, "reply_markup": { "inline_keyboard": [[{ "text": "OK", "callback_data": "~i:x:0" }]] } });
        assert_eq!(link.decide(&mut card, &mut memory, now), Decision::There);
        // A reply to a personal message stays, even with work words.
        let mut mine = text_update("PROJ-123");
        mine["message"]["reply_to_message"] =
            json!({ "message_id": 3, "text": "Личное\nСохранено" });
        assert_eq!(link.decide(&mut mine, &mut memory, now), Decision::Here);

        // 2. A prefix wins over words, and is cut.
        let mut prefixed = text_update("личное: релиз дачи");
        assert_eq!(link.decide(&mut prefixed, &mut memory, now), Decision::Here);
        assert_eq!(prefixed["message"]["text"], "релиз дачи");
        let mut bare = text_update("Работа:");
        assert_eq!(
            link.decide(&mut bare, &mut memory, now),
            Decision::Switched(Place::There)
        );

        // 3. Work words by hash.
        let mut memory = Memory {
            work: memory.work,
            ..Memory::default()
        };
        assert_eq!(
            link.decide(&mut text_update("что с PROJ-123?"), &mut memory, now),
            Decision::There
        );
        // 4. A word the author called personal outweighs the work list and routes here.
        memory.personal.insert("релиз".into());
        memory.last.clear();
        assert_eq!(
            link.decide(&mut text_update("релиз альбома"), &mut memory, now),
            Decision::Here
        );

        // 5. A follow-up within ten minutes goes the same way; after that the bot asks.
        let mut memory = Memory::default();
        memory.last.insert(77, (Place::There, now));
        assert_eq!(
            link.decide(
                &mut text_update("и ещё"),
                &mut memory,
                now + Duration::from_secs(60)
            ),
            Decision::There
        );
        let late = now + Duration::from_secs(60) + FOLLOW_UP;
        assert_eq!(
            link.decide(&mut text_update("и ещё"), &mut memory, late),
            Decision::Ask
        );
        // A message in another chat has no follow-up.
        let mut other = text_update("и ещё");
        other["message"]["chat"]["id"] = json!(78);
        assert_eq!(
            link.decide(&mut other, &mut Memory::default(), now),
            Decision::Ask
        );
    }

    #[test]
    fn commands_and_buttons() {
        let link = link();
        let now = Instant::now();
        let mut memory = Memory::default();
        for (text, want) in [
            ("/now", Decision::Both),
            ("/inbox@frood_bot", Decision::Both),
            ("/digest", Decision::Both),
            ("/help", Decision::Here),
            ("/start", Decision::Here),
            ("/link K7M2QX9P", Decision::Here),
            ("/start l-K7M2QX9P", Decision::There),
            ("/unlink", Decision::Here),
            ("/ask когда релиз", Decision::Ask),
        ] {
            assert_eq!(
                link.decide(&mut text_update(text), &mut memory, now),
                want,
                "{text}"
            );
        }
        assert_eq!(
            link.decide(&mut text_update("работа: /unlink"), &mut memory, now),
            Decision::There
        );
        let press =
            |data: &str| json!({ "update_id": 2, "callback_query": { "id": "q", "data": data } });
        assert_eq!(
            link.decide(&mut press("~i:item:0"), &mut memory, now),
            Decision::There
        );
        assert_eq!(
            link.decide(&mut press("i:item:0"), &mut memory, now),
            Decision::Here
        );
        assert_eq!(
            link.decide(&mut press("r:t:k1"), &mut memory, now),
            Decision::Answer(Place::There, "k1".into())
        );
    }

    #[test]
    fn marks_go_on_and_come_off() {
        let mut markup = json!({ "inline_keyboard": [[{ "text": "OK", "callback_data": "g:e:weekly:0199a0b1-0000-7000-8000-000000000000" }, { "text": "Open", "url": "https://x" }]] });
        mark(&mut markup);
        let data = markup["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap();
        assert!(data.starts_with('~') && data.len() <= 64, "{data}");
        assert!(
            markup["inline_keyboard"][0][1]
                .get("callback_data")
                .is_none()
        );
        let mut update = json!({ "callback_query": { "data": data, "message": { "reply_markup": markup.clone() } } });
        unmark(&mut update);
        assert!(
            update["callback_query"]["data"]
                .as_str()
                .unwrap()
                .starts_with("g:e:")
        );
        let mut start = json!({ "message": { "text": "/start l-K7M2QX9P" } });
        unmark(&mut start);
        assert_eq!(start["message"]["text"], "/start K7M2QX9P");
    }
}
