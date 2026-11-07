//! `notify.send@1` and `notify.update@1` (contracts/notify.send/1): Inbox items as Telegram
//! messages with buttons, edited when the situation is resolved. A pressed button acts through
//! `POST /v1/inbox/{id}/act` for the person whose Telegram account pressed it.

use serde_json::{Value, json};

use crate::Bot;
use crate::lang::{KINDS, Words};
use crate::spheres::Spheres;

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

/// `agent:daily-sweep/for/…` → `daily-sweep`.
fn agent_job(author: &str) -> Option<&str> {
    author
        .strip_prefix("agent:")
        .map(|rest| rest.split('/').next().unwrap_or(rest))
}

/// The message text of a shown item (`{item, content, actions}`).
/// Text for Telegram's HTML: what came from people and agents must not read as markup.
pub(crate) fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The message of a shown item (`{item, content, actions}`), in Telegram's HTML: a bold
/// headline, short labelled lines, the quote in italics.
pub(crate) fn text(shown: &Value, w: &Words, spheres: &Spheres) -> String {
    let item = &shown["item"];
    let content = &shown["content"];
    let mut lines: Vec<String> = Vec::new();
    match item["kind"].as_str().unwrap_or_default() {
        "proposal" => {
            let headline = match content["risk"].as_str() {
                Some("conflict") => w.conflict,
                Some("change") => w.change,
                Some("stale_knowledge") => w.stale,
                Some("entity_merge") => w.merge,
                _ => w.new,
            };
            let alternatives = content["alternatives"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            // What happened, to what kind of knowledge, in which sphere:
            // «🆕 New knowledge · Decision · 💼 Work · business».
            let first = alternatives.first().map(|a| &a["changes"][0]);
            let kind = first
                .and_then(|c| c["kind"].as_str())
                .and_then(|k| KINDS.iter().position(|known| *known == k))
                .map(|i| w.kinds[i]);
            let mut header = format!("<b>{}</b>", esc(headline));
            if let Some(kind) = kind {
                header.push_str(&format!(" · {}", esc(kind)));
            }
            let moved = first.is_some_and(|c| c.get("sphere_before").is_some());
            if let Some(sphere) = first.and_then(|c| c["sphere"].as_str()).filter(|_| !moved) {
                header.push_str(&format!(" · {}", esc(&spheres.label(sphere, w))));
            }
            lines.push(header);
            for alternative in &alternatives {
                if alternatives.len() > 1 {
                    let name = alternative["label"]
                        .as_str()
                        .or_else(|| alternative["key"].as_str())
                        .unwrap_or("?");
                    lines.push(format!("\n<b>{} «{}»</b>", esc(w.variant), esc(name)));
                }
                for change in alternative["changes"].as_array().into_iter().flatten() {
                    let title = change["title"].as_str().filter(|t| !t.trim().is_empty());
                    if let Some(title) = title {
                        lines.push(format!("<b>{}</b>", esc(&short(title, 120))));
                    }
                    if let Some(from) = change.get("sphere_before") {
                        // Only the sphere moves.
                        let label = |v: &Value| match v.as_str() {
                            Some(id) => spheres.label(id, w),
                            None => w.no_sphere.to_owned(),
                        };
                        lines.push(format!(
                            "<b>{}:</b> {} → {}",
                            esc(w.sphere),
                            esc(&label(from)),
                            esc(&label(&change["sphere"]))
                        ));
                    }
                    match (change["before"].as_str(), change["after"].as_str()) {
                        (Some(before), Some(after)) => {
                            lines.push(format!(
                                "<b>{}:</b> {}",
                                esc(w.before),
                                esc(&short(before, 220))
                            ));
                            lines.push(format!(
                                "<b>{}:</b> {}",
                                esc(w.after),
                                esc(&short(after, 220))
                            ));
                        }
                        (None, Some(after)) if Some(after.trim()) != title.map(str::trim) => {
                            lines.push(esc(&short(after, 300)))
                        }
                        (None, Some(_)) => {}
                        (Some(before), None) => lines.push(format!(
                            "<b>{}:</b> {}",
                            esc(w.closed),
                            esc(&short(before, 220))
                        )),
                        (None, None) => {}
                    }
                }
            }
            if let Some(quote) = content["quote"].as_str() {
                lines.push(format!("\n<i>«{}»</i>", esc(&short(quote, 220))));
            }
            if let Some(rationale) = content["rationale"].as_str() {
                lines.push(format!(
                    "<b>{}:</b> {}",
                    esc(w.why),
                    esc(&short(rationale, 200))
                ));
            }
            // What is attached: «📎 2 — Demo plan, board.jpg».
            let attached: Vec<String> = content["attachments"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| {
                    let name = a["title"]
                        .as_str()
                        .or_else(|| a["file"]["filename"].as_str())
                        .or_else(|| a["uri"].as_str())
                        .unwrap_or("?");
                    short(name, 40)
                })
                .collect();
            if !attached.is_empty() {
                let names: Vec<&str> = attached.iter().take(3).map(String::as_str).collect();
                lines.push(format!(
                    "📎 {} — {}",
                    attached.len(),
                    esc(&names.join(", "))
                ));
            }
            if let Some(job) = content["author"].as_str().and_then(agent_job) {
                lines.push(format!("<i>{}: {}</i>", esc(w.from_agent), esc(job)));
            }
        }
        "question" => {
            lines.push(format!(
                "<b>{} {}</b>",
                esc(w.question),
                esc(content["job"].as_str().unwrap_or("?"))
            ));
            lines.push(esc(content["text"].as_str().unwrap_or_default()));
            if let Some(context) = content["context"].as_str() {
                lines.push(format!("\n<i>{}</i>", esc(&short(context, 400))));
            }
        }
        "reminder" => {
            // Days from the due date: -1 the day before, 0 the day, 1 the day after.
            let days = content["days"]
                .as_i64()
                .unwrap_or(match content["step"].as_str() {
                    Some("before") => -1,
                    Some("today") => 0,
                    _ => 1,
                });
            let step = match days {
                -1 => w.due_before.to_owned(),
                0 => w.due_today.to_owned(),
                1 => w.due_overdue.to_owned(),
                d if d < 0 => w.due_in.replace("{n}", &(-d).to_string()),
                d => w.due_ago.replace("{n}", &d.to_string()),
            };
            lines.push(format!(
                "<b>{} {}</b>: {}",
                esc(w.due),
                esc(content["due"].as_str().unwrap_or("?")),
                esc(&step)
            ));
            lines.push(esc(content["text"].as_str().unwrap_or_default()));
        }
        "alert" if content.get("detector").is_some() => lines.push(esc(&detector_text(content, w))),
        "alert" => {
            lines.push(format!(
                "<b>{}: {}</b>",
                esc(w.failed),
                esc(content["job"].as_str().unwrap_or("?"))
            ));
            if let Some(error) = content["error"].as_str() {
                lines.push(esc(&short(error, 400)));
            }
        }
        "digest" => {
            let written = item
                .pointer("/investigation/report")
                .and_then(Value::as_str);
            lines.push(digest_text(content, w, spheres, written));
            return lines.join("\n");
        }
        "sign_in" => {
            lines.push(format!("<b>{}</b>", esc(w.sign_in_title)));
            lines.push(format!(
                "{}: {}",
                esc(w.sign_in_app),
                esc(content["client"].as_str().unwrap_or("Frood"))
            ));
            lines.push(format!(
                "{}: <b>{}</b>",
                esc(w.sign_in_code),
                esc(content["code"].as_str().unwrap_or("?"))
            ));
            if item["state"] != "resolved" {
                lines.push(format!("\n<i>{}</i>", esc(w.sign_in_hint)));
            }
        }
        _ => lines.push(esc(&short(&content.to_string(), 800))),
    }
    if let Some(report) = item
        .pointer("/investigation/report")
        .and_then(Value::as_str)
    {
        lines.push(format!(
            "\n🔎 <b>{}:</b> {}",
            esc(w.found),
            esc(&short(report, 600))
        ));
    }
    if item["state"] == "resolved" {
        lines.push(format!("\n{}", esc(&resolution(&item["resolution"], w))));
    }
    lines.join("\n")
}

/// A detector's alert: what stalled, in the bot's language.
fn detector_text(c: &Value, w: &Words) -> String {
    let field = |key: &str| c[key].as_str().unwrap_or("?").to_owned();
    let since = c["since"]
        .as_str()
        .map(|s| s.chars().take(16).collect::<String>().replace('T', " "));
    let usd = |key: &str| format!("${:.2}", c[key].as_f64().unwrap_or(0.0));
    let fill = |template: &str| {
        template
            .replace("{module}", &field("module"))
            .replace("{job}", &field("job"))
            .replace("{source}", &field("source"))
            .replace("{account}", &field("account"))
            .replace("{spent}", &usd("spent_usd"))
            .replace("{limit}", &usd("monthly_usd"))
    };
    let text = match c["detector"].as_str().unwrap_or_default() {
        "module_down" => fill(w.module_down),
        "failures_in_a_row" => fill(w.failing),
        "queued" => fill(w.queued_long),
        "overrun" => fill(w.overrun),
        "source_stale" => fill(w.source_stale),
        "budget" => fill(w.budget),
        "dispatcher_lag" => w.lagging.to_owned(),
        _ => field("message"),
    };
    let mut out = format!("🚨 {text}");
    if let Some(since) = since {
        out.push_str(&format!(" ({since} UTC)"));
    }
    if let Some(error) = c["error"].as_str() {
        out.push_str(&format!("\n{}", short(error, 500)));
    }
    out
}

/// A digest (`GET /v1/digest` or a `digest` item) as text, each section by sphere.
/// A label as a link to the original when there is one.
fn labelled(label: Option<&str>, link: Option<&str>) -> Option<String> {
    let label = label?;
    Some(match link {
        Some(link) => format!("<a href=\"{}\">{}</a>", esc(link), esc(label)),
        None => esc(label),
    })
}

/// "AIRC-1808 — title (3)", the label a link to the original.
fn digest_line(entry: &Value) -> String {
    let title = esc(&short(entry["title"].as_str().unwrap_or("?"), DIGEST_CHARS));
    let count = entry["count"].as_u64().unwrap_or(1);
    let more = if count > 1 {
        format!(" ({count})")
    } else {
        String::new()
    };
    match labelled(entry["label"].as_str(), entry["link"].as_str()) {
        Some(label) => format!("{label} — {title}{more}"),
        None => format!("{title}{more}"),
    }
}

/// The digest (docs/31): what needs the person now, what is new, and how many waited longer.
/// When an agent wrote it in plain words (the item's investigation report), that text goes
/// instead, its labels turned into links.
pub(crate) fn digest_text(
    digest: &Value,
    w: &Words,
    spheres: &Spheres,
    written: Option<&str>,
) -> String {
    let mut lines = vec![format!("<b>{}</b>", esc(w.digest))];
    let entries = |key: &str| -> Vec<Value> {
        digest
            .pointer(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    if let Some(written) = written.map(str::trim).filter(|t| !t.is_empty()) {
        // The agent's words, with each label it kept made a link again.
        let mut links: Vec<(String, String)> = Vec::new();
        for key in ["/important", "/accepted", "/waiting/new"] {
            for entry in entries(key) {
                if let (Some(label), Some(link)) = (entry["label"].as_str(), entry["link"].as_str())
                {
                    links.push((
                        esc(label),
                        format!("<a href=\"{}\">{}</a>", esc(link), esc(label)),
                    ));
                }
            }
        }
        for line in written
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(14)
        {
            let mut line = esc(&short(line, 160));
            if let Some((label, link)) = links
                .iter()
                .find(|(label, _)| line.starts_with(label.as_str()))
            {
                line = line.replacen(label.as_str(), link, 1);
            }
            lines.push(line);
        }
        return lines.join("\n");
    }
    let tag = |why: &str| match why {
        "conflict" => w.tag_conflict,
        "question" => w.tag_question,
        "due" => w.tag_due,
        "due_today" => w.tag_today,
        "due_tomorrow" => w.tag_tomorrow,
        "overdue" => w.tag_overdue,
        "failed" => w.tag_failed,
        _ => "",
    };
    let mut important: Vec<String> = entries("/important")
        .iter()
        .map(|i| {
            format!(
                "• <i>{}</i> · {}",
                esc(tag(i["why"].as_str().unwrap_or_default())),
                digest_line(i)
            )
        })
        .collect();
    important.extend(
        entries("/due_soon")
            .iter()
            .filter(|d| d.get("why").is_some())
            .map(|d| {
                format!(
                    "• <i>{}</i> · {}",
                    esc(tag(d["why"].as_str().unwrap_or_default())),
                    digest_line(d)
                )
            }),
    );
    if !important.is_empty() {
        lines.push(format!("\n<b>{}</b>", esc(w.digest_important)));
        lines.extend(important.into_iter().take(DIGEST_ITEMS * 2));
    }
    let sphere = |v: &Value| v["sphere"].as_str().map(str::to_owned);
    let fresh: Vec<(Option<String>, String)> = entries("/accepted")
        .iter()
        .chain(entries("/waiting/new").iter())
        .map(|e| (sphere(e), digest_line(e)))
        .collect();
    if !fresh.is_empty() {
        lines.push(format!("\n<b>{}</b>", esc(w.digest_new)));
        lines.extend(spheres.grouped(fresh.into_iter().take(DIGEST_ITEMS * 2).collect(), w));
    }
    let older = digest["waiting"]["older"]["count"].as_u64().unwrap_or(0);
    if older > 0 {
        let date = digest["waiting"]["older"]["since"]
            .as_str()
            .and_then(|s| s.get(..10))
            .and_then(chrono_like_date)
            .unwrap_or_default();
        lines.push(format!(
            "\n{}",
            esc(&w
                .digest_older
                .replace("{n}", &older.to_string())
                .replace("{date}", &date))
        ));
    }
    if lines.len() == 1 {
        lines.push(esc(w.digest_nothing));
    }
    lines.join("\n")
}

/// "2026-10-20" as "20.10".
fn chrono_like_date(iso: &str) -> Option<String> {
    let mut parts = iso.split('-');
    let (_, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    Some(format!("{day}.{month}"))
}

/// The digest's buttons: go through what waits, accept the findings as not verified, less often.
pub(crate) fn digest_keyboard(shown: &Value, w: &Words) -> Value {
    let id = shown["item"]["id"].as_str().unwrap_or_default();
    let digest = &shown["content"];
    let mut rows = Vec::new();
    let waiting = digest["waiting"]["count"].as_u64().unwrap_or(0);
    if waiting > 0 {
        rows.push(json!([{ "text": w.review.replace("{n}", &waiting.to_string()), "callback_data": format!("g:r:{id}") }]));
    }
    let findings = digest["accept_ids"].as_array().map_or(0, Vec::len);
    if findings > 0 {
        rows.push(json!([{ "text": w.accept_unverified.replace("{n}", &findings.to_string()), "callback_data": format!("g:a:{id}") }]));
    }
    rows.push(json!([{ "text": w.less_often, "callback_data": format!("g:l:{id}") }]));
    json!({ "inline_keyboard": rows })
}

/// How often the digest comes: three buttons in place of the digest's own.
pub(crate) fn every_keyboard(id: &str, w: &Words) -> Value {
    json!({ "inline_keyboard": [
        [{ "text": w.every_daily, "callback_data": format!("g:e:daily:{id}") }],
        [{ "text": w.every_weekly, "callback_data": format!("g:e:weekly:{id}") }],
        [{ "text": w.every_off, "callback_data": format!("g:e:off:{id}") }],
    ] })
}

/// How many lines a digest section shows, and how long each may be.
const DIGEST_ITEMS: usize = 5;
const DIGEST_CHARS: usize = 110;

/// How the situation ended, in a word, and where.
fn resolution(resolution: &Value, w: &Words) -> String {
    let what = match resolution["action"].as_str() {
        Some("accept" | "accept_with_edits") => w.accepted,
        Some("reject") => w.rejected,
        Some("defer") => w.deferred,
        Some("answer" | "answered") => w.answered,
        Some("expired") => w.expired,
        Some("cancelled") => w.cancelled,
        Some("done") => w.marked_done,
        Some("postpone") => w.postponed,
        Some("retry") => w.retried,
        Some("dismiss") => w.dismissed,
        Some("stale") => w.stale_now,
        Some("recovered") => w.recovered,
        Some("approve") => w.signed_in,
        Some("deny") => w.not_signed_in,
        _ => w.resolved,
    };
    let by = resolution["by"].as_str().unwrap_or_default();
    if by.starts_with("policy:") {
        format!("{what} {}", w.by_policy)
    } else if by.ends_with("/via/telegram") || by.is_empty() {
        what.to_owned()
    } else {
        format!("{what} {}", w.elsewhere)
    }
}

/// One row per button; callback data names the item and the action's index. A link to the
/// item in the console comes last when the installation has a public address.
pub(crate) fn keyboard(shown: &Value, w: &Words, console: Option<&str>) -> Value {
    if shown["item"]["kind"] == "digest" {
        return digest_keyboard(shown, w);
    }
    let id = shown["item"]["id"].as_str().unwrap_or_default();
    let several = shown["actions"]
        .as_array()
        .map_or(0, |a| a.iter().filter(|x| x["action"] == "accept").count())
        > 1;
    let rows: Vec<Value> = shown["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, action)| {
            let label = match action["action"].as_str().unwrap_or_default() {
                "accept" if several => format!(
                    "{}: {}",
                    w.accept,
                    action["label"]
                        .as_str()
                        .or_else(|| action["alternative"].as_str())
                        .unwrap_or("?")
                ),
                "accept" => w.accept.to_owned(),
                "reject" => w.reject.to_owned(),
                "defer" => w.defer.to_owned(),
                "snooze" => w.snooze.to_owned(),
                "answer" => action["answer"].as_str().unwrap_or("?").to_owned(),
                "done" => w.done.to_owned(),
                "postpone" => w.postpone.to_owned(),
                "retry" => w.retry.to_owned(),
                "dismiss" => w.dismiss.to_owned(),
                "approve" => w.sign_in.to_owned(),
                "deny" => w.not_me.to_owned(),
                other => other.to_owned(),
            };
            json!([{ "text": short(&label, 60), "callback_data": format!("i:{id}:{i}") }])
        })
        .collect();
    let mut rows = rows;
    // Links: the original the knowledge comes from (the core keeps only web addresses), and
    // the item in the console.
    let mut links = Vec::new();
    if let Some(uri) = shown["content"]["origin"]["uri"].as_str() {
        links.push(json!({ "text": w.original, "url": uri }));
    }
    if let Some(url) = console {
        links.push(json!({ "text": w.details, "url": url }));
    }
    if !links.is_empty() {
        rows.push(Value::Array(links));
    }
    json!({ "inline_keyboard": rows })
}

/// What a message offers: the actions while the item waits; after a decision on a change made
/// here, a way to take it back (docs/25 §4).
pub(crate) fn markup(shown: &Value, w: &Words, console: Option<&str>) -> Value {
    let item = &shown["item"];
    if item["state"] == "open" {
        return keyboard(shown, w, console);
    }
    let here = item["resolution"]["by"]
        .as_str()
        .is_some_and(|by| by.ends_with("/via/telegram"));
    let decided = matches!(
        item["resolution"]["action"].as_str(),
        Some("accept" | "reject")
    );
    if item["kind"] == "proposal" && (item["state"] == "snoozed" || here && decided) {
        let id = item["id"].as_str().unwrap_or_default();
        return json!({ "inline_keyboard": [[{ "text": w.undo, "callback_data": format!("u:{id}") }]] });
    }
    json!({ "inline_keyboard": [] })
}

fn message_ref(chat: &Value, message_id: &Value) -> String {
    format!("{}:{}", chat, message_id)
}

fn parse_ref(message_ref: &str) -> Option<(i64, i64)> {
    let (chat, message) = message_ref.split_once(':')?;
    Some((chat.parse().ok()?, message.parse().ok()?))
}

impl Bot {
    /// `notify.send@1`: shows an item to a person.
    pub(crate) async fn notify_send(&self, body: &Value) -> Result<Value, String> {
        let chat: i64 = body["recipient"]["external_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or("recipient.external_id is not a Telegram account")?;
        let shown = &body["item"];
        let w = self.lang.words();
        let console = body["console_url"].as_str();
        let spheres = self.spheres().await;
        let sent = self
            .telegram(
                "sendMessage",
                json!({ "chat_id": chat, "text": text(shown, w, &spheres), "parse_mode": "HTML", "reply_markup": keyboard(shown, w, console) }),
            )
            .await?;
        Ok(json!({ "message_ref": message_ref(&json!(chat), &sent["message_id"]) }))
    }

    /// `notify.update@1`: the item was resolved; edits its message and drops the buttons.
    pub(crate) async fn notify_update(&self, body: &Value) -> Result<Value, String> {
        let Some((chat, message_id)) = body["message_ref"].as_str().and_then(parse_ref) else {
            return Ok(json!({}));
        };
        self.edit_shown(chat, message_id, &body["item"]).await;
        Ok(json!({}))
    }

    pub(crate) async fn edit_shown(&self, chat: i64, message_id: i64, shown: &Value) {
        let w = self.lang.words();
        let console = self
            .console_url(shown["item"]["id"].as_str().unwrap_or_default())
            .await;
        let result = self
            .telegram(
                "editMessageText",
                json!({ "chat_id": chat, "message_id": message_id, "text": text(shown, w, &self.spheres().await), "parse_mode": "HTML",
                        "reply_markup": markup(shown, w, console.as_deref()) }),
            )
            .await;
        if let Err(err) = result
            && !err.contains("message is not modified")
        {
            tracing::warn!(error = %err, "cannot edit a notification");
        }
    }

    /// A button was pressed: act on the item as the person, then show the outcome.
    pub(crate) async fn on_button(&self, query: &Value) {
        let w = self.lang.words();
        let query_id = query["id"].as_str().unwrap_or_default();
        let from = query["from"]["id"].as_i64().unwrap_or_default().to_string();
        let data = query["data"].as_str().unwrap_or_default();
        let chat = query.pointer("/message/chat/id").and_then(Value::as_i64);
        let message_id = query.pointer("/message/message_id").and_then(Value::as_i64);
        if let Some(rest) = data.strip_prefix("g:") {
            let said = self.digest_button(rest, &from, chat.zip(message_id)).await;
            let _ = self
                .telegram(
                    "answerCallbackQuery",
                    json!({ "callback_query_id": query_id, "text": said }),
                )
                .await;
            return;
        }
        let mut parts = data.split(':');
        let (kind, item_id, index) = (parts.next(), parts.next(), parts.next());
        let answer = |text: String| {
            let bot = self.clone();
            let id = query_id.to_owned();
            async move {
                let _ = bot
                    .telegram(
                        "answerCallbackQuery",
                        json!({ "callback_query_id": id, "text": text }),
                    )
                    .await;
            }
        };
        let (item_id, index) = match (kind, item_id, index) {
            (Some("i"), Some(item_id), Some(index)) => (item_id, index),
            (Some("u"), Some(item_id), None) => {
                // Take the decision back; the message shows the item with its buttons again.
                let undone = self
                    .frood(
                        self.http
                            .post(format!("{}/v1/inbox/{item_id}/undo", self.frood)),
                    )
                    .json(&json!({ "external_id": from }))
                    .send()
                    .await;
                match undone {
                    Ok(r) if r.status().is_success() => {
                        answer(w.undone.to_owned()).await;
                        if let (Some(chat), Some(message_id), Some(shown)) =
                            (chat, message_id, self.inbox_item(item_id).await)
                        {
                            self.edit_shown(chat, message_id, &shown).await;
                        }
                    }
                    Ok(r) => {
                        let body: Value = r.json().await.unwrap_or(Value::Null);
                        answer(short(body["detail"].as_str().unwrap_or("?"), 190)).await;
                    }
                    Err(err) => answer(short(&err.to_string(), 190)).await,
                }
                return;
            }
            (Some("s"), Some(source_id), None) => {
                match self.take_note_back(source_id, &from).await {
                    Ok(text) => {
                        answer(w.undone.to_owned()).await;
                        if let (Some(chat), Some(message_id)) = (chat, message_id) {
                            self.edit(chat, message_id, &text, None).await;
                        }
                    }
                    Err(err) => answer(short(&err, 190)).await,
                }
                return;
            }
            (Some(kind @ ("b" | "B")), Some(key), None) => {
                let at = chat.zip(message_id);
                answer(self.batch(key, &from, at, kind == "B").await).await;
                return;
            }
            _ => return,
        };
        let shown: Value = match self
            .frood(self.http.get(format!("{}/v1/inbox/{item_id}", self.frood)))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
            Ok(r) => {
                let body: Value = r.json().await.unwrap_or(Value::Null);
                answer(body["detail"].as_str().unwrap_or("?").to_owned()).await;
                return;
            }
            Err(err) => {
                answer(err.to_string()).await;
                return;
            }
        };
        let action = index
            .parse::<usize>()
            .ok()
            .and_then(|i| shown["actions"].get(i).cloned());
        let Some(mut action) = action else {
            // Resolved meanwhile: the buttons are gone.
            answer(w.already.to_owned()).await;
            if let (Some(chat), Some(message_id)) = (chat, message_id) {
                self.edit_shown(chat, message_id, &shown).await;
            }
            return;
        };
        action["external_id"] = json!(from);
        let response = self
            .frood(
                self.http
                    .post(format!("{}/v1/inbox/{item_id}/act", self.frood)),
            )
            .json(&action)
            .send()
            .await;
        match response {
            Ok(r) if r.status().is_success() => {
                let outcome: Value = r.json().await.unwrap_or(Value::Null);
                let already = outcome["outcome"] == "already_resolved";
                answer(if already { w.already } else { w.ok }.to_owned()).await;
                let mut shown = shown;
                shown["item"] = outcome["item"].clone();
                if shown["item"]["state"] == "resolved" {
                    shown["actions"] = json!([]);
                }
                if let (Some(chat), Some(message_id)) = (chat, message_id) {
                    self.edit_shown(chat, message_id, &shown).await;
                }
            }
            Ok(r) => {
                let body: Value = r.json().await.unwrap_or(Value::Null);
                answer(short(body["detail"].as_str().unwrap_or("?"), 190)).await;
            }
            Err(err) => answer(short(&err.to_string(), 190)).await,
        }
    }
}

impl Bot {
    /// A digest button (docs/31 D4): `r:<id>` goes through what waits, `a:<id>` accepts the
    /// findings as not verified, `l:<id>` offers how often, `e:<every>:<id>` sets it.
    async fn digest_button(&self, data: &str, from: &str, at: Option<(i64, i64)>) -> String {
        let w = self.lang.words();
        let t = self.lang.texts();
        let parts: Vec<&str> = data.split(':').collect();
        let set_markup = |markup: Value| {
            let bot = self.clone();
            async move {
                if let Some((chat, message_id)) = at {
                    let _ = bot
                        .telegram(
                            "editMessageReplyMarkup",
                            json!({ "chat_id": chat, "message_id": message_id, "reply_markup": markup }),
                        )
                        .await;
                }
            }
        };
        match parts.as_slice() {
            ["r", _] => {
                if let Some((chat, _)) = at {
                    self.inbox(chat).await;
                }
                w.ok.to_owned()
            }
            ["a", id] => {
                let Some(shown) = self.inbox_item(id).await else {
                    return t.batch_gone.to_owned();
                };
                let mut accepted = 0;
                for finding in shown["content"]["accept_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let Some(finding) = finding.as_str() else {
                        continue;
                    };
                    let done = self
                        .frood(
                            self.http
                                .post(format!("{}/v1/inbox/{finding}/act", self.frood)),
                        )
                        .json(
                            &json!({ "action": "accept", "unverified": true, "external_id": from }),
                        )
                        .send()
                        .await
                        .is_ok_and(|r| r.status().is_success());
                    if done {
                        accepted += 1;
                    }
                }
                let mut left = shown.clone();
                left["content"]["accept_ids"] = json!([]);
                set_markup(digest_keyboard(&left, w)).await;
                w.accepted_unverified.replace("{n}", &accepted.to_string())
            }
            ["l", id] => {
                set_markup(every_keyboard(id, w)).await;
                w.less_often.to_owned()
            }
            ["e", every @ ("daily" | "weekly" | "off"), id] => {
                let set = self
                    .frood(self.http.put(format!(
                        "{}/v1/channels/{}/links/{from}/digest",
                        self.frood, self.channel
                    )))
                    .json(&json!({ "every": every }))
                    .send()
                    .await;
                if let Some(shown) = self.inbox_item(id).await {
                    set_markup(digest_keyboard(&shown, w)).await;
                }
                match set {
                    Ok(r) if r.status().is_success() => match *every {
                        "weekly" => w.every_set_weekly,
                        "off" => w.every_set_off,
                        _ => w.every_set_daily,
                    }
                    .to_owned(),
                    Ok(r) => {
                        let body: Value = r.json().await.unwrap_or(Value::Null);
                        short(body["detail"].as_str().unwrap_or("?"), 190)
                    }
                    Err(err) => short(&err.to_string(), 190),
                }
            }
            _ => w.already.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;

    fn conflict() -> Value {
        json!({
            "item": { "id": "0199-item", "kind": "proposal", "state": "open", "investigation": { "state": "done", "report": "Olya confirmed in #release" } },
            "content": {
                "risk": "conflict", "author": "agent:daily-sweep/for/0199-user",
                "rationale": "#release, 18:02", "quote": "релиз переносим на 28.11",
                "alternatives": [{ "key": "default", "label": null, "changes": [{ "kind": "commitment", "before": "Релиз 2.4 — 14 ноября", "after": "Релиз 2.4 — 28 ноября" }] }]
            },
            "actions": [{ "action": "accept", "alternative": "default" }, { "action": "reject" }, { "action": "defer" }]
        })
    }

    #[test]
    fn what_people_wrote_never_reads_as_markup() {
        let mut shown = conflict();
        shown["content"]["quote"] = json!("a <b>bold</b> & <script>");
        assert!(
            text(&shown, Lang::Ru.words(), &Spheres::default())
                .contains("«a &lt;b&gt;bold&lt;/b&gt; &amp; &lt;script&gt;»")
        );
    }

    #[test]
    fn a_conflict_reads_as_was_and_now_with_buttons() {
        let w = Lang::Ru.words();
        let shown = conflict();
        assert_eq!(
            text(&shown, w, &Spheres::default()),
            "<b>⚠️ Противоречит записанному</b> · Обязательство\n<b>Было:</b> Релиз 2.4 — 14 ноября\n<b>Стало:</b> Релиз 2.4 — 28 ноября\n\n<i>«релиз переносим на 28.11»</i>\n<b>Почему:</b> #release, 18:02\n<i>От агента: daily-sweep</i>\n\n🔎 <b>Агент выяснил:</b> Olya confirmed in #release"
        );
        assert_eq!(
            keyboard(&shown, w, None),
            json!({ "inline_keyboard": [
                [{ "text": "Принять", "callback_data": "i:0199-item:0" }],
                [{ "text": "Оставить старое", "callback_data": "i:0199-item:1" }],
                [{ "text": "Отложить", "callback_data": "i:0199-item:2" }]
            ] })
        );
    }

    #[test]
    fn new_knowledge_reads_as_title_and_text_with_a_link_to_the_original() {
        let w = Lang::Ru.words();
        let shown = json!({
            "item": { "id": "0199-item", "kind": "proposal", "state": "open" },
            "content": {
                "risk": "new", "author": "agent:daily-sweep",
                "alternatives": [{ "key": "default", "label": null, "changes": [{ "kind": "fact", "title": "AIRC-1808: сессия грузится 22 минуты", "before": null, "after": "Завершённая сессия с длинной историей показывает только хвост и грузится около 22 минут" }] }],
                "origin": { "uri": "https://youtrack.example/issue/AIRC-1808", "title": "AIRC-1808" }
            },
            "actions": [{ "action": "accept", "alternative": "default" }]
        });
        assert_eq!(
            text(&shown, w, &Spheres::default()),
            "<b>🆕 Новое знание</b> · Факт\n<b>AIRC-1808: сессия грузится 22 минуты</b>\nЗавершённая сессия с длинной историей показывает только хвост и грузится около 22 минут\n<i>От агента: daily-sweep</i>"
        );
        assert_eq!(
            keyboard(
                &shown,
                w,
                Some("https://frood.example/admin/inbox/0199-item")
            ),
            json!({ "inline_keyboard": [
                [{ "text": "Принять", "callback_data": "i:0199-item:0" }],
                [{ "text": "Открыть оригинал", "url": "https://youtrack.example/issue/AIRC-1808" },
                 { "text": "Подробнее", "url": "https://frood.example/admin/inbox/0199-item" }]
            ] })
        );
    }

    #[test]
    fn detector_alerts_say_what_stalled() {
        let alert = |content: Value| json!({ "item": { "id": "a", "kind": "alert", "state": "open" }, "content": content, "actions": [{ "action": "dismiss" }] });
        let shown = alert(
            json!({ "detector": "failures_in_a_row", "job": "transcribe", "error": "ffmpeg missing" }),
        );
        assert_eq!(
            text(&shown, Lang::Ru.words(), &Spheres::default()),
            "🚨 Джоба transcribe падает раз за разом\nffmpeg missing"
        );
        assert_eq!(
            keyboard(&shown, Lang::Ru.words(), None)["inline_keyboard"][0][0]["text"],
            "Скрыть"
        );
        let shown = alert(json!({ "detector": "module_down", "module": "stt" }));
        assert_eq!(
            text(&shown, Lang::En.words(), &Spheres::default()),
            "🚨 Module stt does not answer"
        );
    }

    #[test]
    fn reminders_say_how_far_the_due_date_is() {
        let w = Lang::Ru.words();
        let reminder = |days: i64| {
            json!({ "item": { "id": "r", "kind": "reminder", "state": "open" },
                    "content": { "text": "Сдать отчёт", "due": "2026-11-14", "days": days }, "actions": [] })
        };
        assert_eq!(
            text(&reminder(-3), w, &Spheres::default()),
            "<b>⏰ Срок 2026-11-14</b>: срок через 3 дн.\nСдать отчёт"
        );
        assert_eq!(
            text(&reminder(0), w, &Spheres::default()),
            "<b>⏰ Срок 2026-11-14</b>: срок сегодня\nСдать отчёт"
        );
        assert_eq!(
            text(&reminder(2), w, &Spheres::default()),
            "<b>⏰ Срок 2026-11-14</b>: срок прошёл 2 дн. назад: выполнено или перенести?\nСдать отчёт"
        );
    }

    #[test]
    fn a_digest_says_what_needs_you_what_is_new_and_counts_the_rest() {
        let w = Lang::Ru.words();
        let digest = json!({
            "important": [{ "id": "q1", "kind": "question", "why": "question", "title": "Решение или обсуждение?", "label": null, "link": null }],
            "accepted": [{ "title": "Релиз 2.4 — 28 ноября", "count": 1 }],
            "waiting": {
                "count": 13,
                "new": [{ "title": "сессия с длинной историей грузится 22 минуты", "label": "AIRC-1808", "link": "https://yt.example/issue/AIRC-1808", "count": 3 }],
                "older": { "count": 11, "since": "2026-10-20T08:00:00Z" }
            },
            "due_soon": [{ "title": "Сдать отчёт", "due": "2026-11-07", "why": "due_today" }, { "title": "Позже", "due": "2026-11-12" }],
            "accept_ids": ["p1", "p2"]
        });
        assert_eq!(
            digest_text(&digest, w, &Spheres::default(), None),
            "<b>📋 Что нового</b>\n\n<b>Нужно вам сейчас</b>\n• <i>вопрос</i> · Решение или обсуждение?\n• <i>сегодня</i> · Сдать отчёт\n\n<b>Новое</b>\n• Релиз 2.4 — 28 ноября\n• <a href=\"https://yt.example/issue/AIRC-1808\">AIRC-1808</a> — сессия с длинной историей грузится 22 минуты (3)\n\n⏳ 11 ждут вашего решения с 20.10"
        );
        // Nothing to say: the core sends no digest, and a bare one says so.
        assert_eq!(
            digest_text(
                &json!({ "waiting": { "count": 0 } }),
                w,
                &Spheres::default(),
                None
            ),
            "<b>📋 Что нового</b>\nНичего нового."
        );
        // Written by the agent: its lines, the labels made links again.
        let written = "AIRC-1808 — долгая загрузка старой сессии: три находки\nЕщё 11 ждут решения с 20 октября";
        assert_eq!(
            digest_text(&digest, w, &Spheres::default(), Some(written)),
            "<b>📋 Что нового</b>\n<a href=\"https://yt.example/issue/AIRC-1808\">AIRC-1808</a> — долгая загрузка старой сессии: три находки\nЕщё 11 ждут решения с 20 октября"
        );
        // Its buttons: go through the 13, accept the 2 findings, less often.
        let shown = json!({ "item": { "id": "d1", "kind": "digest" }, "content": digest, "actions": [{ "action": "seen" }] });
        let buttons = keyboard(&shown, w, None);
        let rows: Vec<(&str, &str)> = buttons["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r[0]["text"].as_str().unwrap(),
                    r[0]["callback_data"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Разобрать (13)", "g:r:d1"),
                ("Принять 2 как непроверенные", "g:a:d1"),
                ("Реже", "g:l:d1")
            ]
        );
        assert_eq!(
            every_keyboard("d1", w)["inline_keyboard"][1][0]["callback_data"],
            "g:e:weekly:d1"
        );
    }

    #[test]
    fn cards_and_digests_say_the_sphere() {
        let w = Lang::Ru.words();
        let spheres = Spheres::default();
        let card = |change: Value| {
            json!({ "item": { "id": "s", "kind": "proposal", "state": "open" },
                    "content": { "risk": "new", "author": "user:1",
                                 "alternatives": [{ "key": "default", "changes": [change] }] },
                    "actions": [] })
        };
        let new = card(
            json!({ "kind": "decision", "title": "Поиск на OpenSearch", "sphere": "tech",
                               "before": null, "after": "Поиск на OpenSearch" }),
        );
        assert!(
            text(&new, w, &spheres)
                .starts_with("<b>🆕 Новое знание</b> · Решение · 🛠 Работа · техника\n"),
            "{}",
            text(&new, w, &spheres)
        );
        let mut with_files = new.clone();
        with_files["content"]["attachments"] = json!([
            { "kind": "link", "title": "ADR-12", "uri": "https://git.example/adr/12" },
            { "kind": "photo", "title": null, "file": { "filename": "board.jpg" } }
        ]);
        assert!(text(&with_files, w, &spheres).contains("📎 2 — ADR-12, board.jpg"));
        let moved = card(
            json!({ "kind": "decision", "title": "Поиск на OpenSearch", "sphere": "business",
                                 "sphere_before": "tech", "before": null, "after": "Поиск на OpenSearch" }),
        );
        assert!(
            text(&moved, w, &spheres)
                .contains("<b>Сфера:</b> 🛠 Работа · техника → 💼 Работа · бизнес"),
            "{}",
            text(&moved, w, &spheres)
        );

        let digest = json!({
            "accepted": [{ "title": "Билеты на праздники", "sphere": "personal" },
                         { "title": "Релиз 2.4 — 28 ноября", "sphere": "business" }],
            "waiting": { "count": 0 },
            "due_soon": []
        });
        assert_eq!(
            digest_text(&digest, w, &spheres, None),
            "<b>📋 Что нового</b>\n\n<b>Новое</b>\n<i>💼 Работа · бизнес</i>\n• Релиз 2.4 — 28 ноября\n<i>🏠 Личное</i>\n• Билеты на праздники"
        );
    }

    #[test]
    fn a_sign_in_shows_the_app_and_the_code_with_two_buttons() {
        let w = Lang::Ru.words();
        let mut shown = json!({
            "item": { "id": "s", "kind": "sign_in", "state": "open" },
            "content": { "client": "Claude", "code": "0427", "expires_at": "2026-10-26T06:30:00Z" },
            "actions": [{ "action": "approve" }, { "action": "deny" }]
        });
        let message = text(&shown, w, &Spheres::default());
        assert!(message.contains("Вход во Frood"), "{message}");
        assert!(message.contains("Приложение: Claude"), "{message}");
        assert!(message.contains("Код: <b>0427</b>"), "{message}");
        assert!(message.contains("тот же код"), "{message}");
        let buttons = keyboard(&shown, w, None);
        assert_eq!(buttons["inline_keyboard"][0][0]["text"], "Войти");
        assert_eq!(buttons["inline_keyboard"][1][0]["text"], "Не я");
        shown["item"]["state"] = json!("resolved");
        shown["item"]["resolution"] = json!({ "action": "approve", "by": "user:1/via/telegram" });
        let message = text(&shown, w, &Spheres::default());
        assert!(message.contains("Вход выполнен"), "{message}");
        assert!(!message.contains("тот же код"), "{message}");
    }

    #[test]
    fn a_resolved_item_says_how() {
        let w = Lang::Ru.words();
        let mut shown = conflict();
        shown["item"]["state"] = json!("resolved");
        shown["item"]["resolution"] =
            json!({ "action": "accept", "by": "user:0199/via/claude-code" });
        assert!(text(&shown, w, &Spheres::default()).ends_with("✅ Принято в другом месте"));
        shown["item"]["resolution"] = json!({ "action": "reject", "by": "user:0199/via/telegram" });
        assert!(text(&shown, w, &Spheres::default()).ends_with("↩️ Оставлено старое"));
        assert_eq!(parse_ref("373739001:52"), Some((373_739_001, 52)));
    }
}
