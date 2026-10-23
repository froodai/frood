//! Commands beyond capture (docs/25 §4): the menu, `/now`, `/ask` (or a message starting with
//! `?`) and `/inbox`. They read the person's Frood, so they answer only linked accounts.

use serde_json::{Value, json};

use crate::lang::{Lang, Texts};
use crate::notify::{keyboard, text};
use crate::{Bot, Message};

/// What the menu shows; the descriptions come from the language's texts.
const MENU: [&str; 5] = ["now", "inbox", "ask", "digest", "help"];

/// How many deadlines `/now` lists, and how many items `/inbox` sends at once.
const SHOWN: usize = 5;

impl Bot {
    /// Puts the commands into Telegram's menu, in the bot's language and in each one it speaks.
    pub(crate) async fn set_menu(&self) {
        let each = Lang::ALL.into_iter().map(|lang| (lang, Some(lang.code())));
        for (lang, code) in std::iter::once((self.lang, None)).chain(each) {
            let t = lang.texts();
            let commands: Vec<Value> = MENU
                .iter()
                .zip(t.menu)
                .map(|(command, description)| json!({ "command": command, "description": description }))
                .collect();
            let mut body = json!({ "commands": commands });
            if let Some(code) = code {
                body["language_code"] = json!(code);
            }
            if let Err(err) = self.telegram("setMyCommands", body).await {
                tracing::warn!(error = %err, "cannot set the menu");
            }
        }
    }

    /// The person behind this Telegram account, if it is linked.
    async fn linked(&self, external_id: &str) -> bool {
        let response = self
            .frood(self.http.get(format!(
                "{}/v1/channels/{}/links/{external_id}",
                self.frood, self.channel
            )))
            .send()
            .await;
        match response {
            Ok(r) if r.status().is_success() => {
                r.json::<Value>().await.is_ok_and(|v| v["linked"] == true)
            }
            _ => false,
        }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let response = self
            .frood(self.http.get(format!("{}{path}", self.frood)))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let ok = response.status().is_success();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if ok {
            Ok(body)
        } else {
            Err(body["detail"].as_str().unwrap_or("?").to_owned())
        }
    }

    /// The console's page of an Inbox item, when the installation has a public address.
    pub(crate) async fn console_url(&self, item_id: &str) -> Option<String> {
        let base = self
            .console
            .get_or_init(|| async {
                self.get("/v1/version")
                    .await
                    .ok()
                    .and_then(|v| v["console"].as_str().map(str::to_owned))
            })
            .await;
        base.as_ref().map(|base| format!("{base}inbox/{item_id}"))
    }

    pub(crate) async fn inbox_item(&self, id: &str) -> Option<Value> {
        self.get(&format!("/v1/inbox/{id}")).await.ok()
    }

    /// Commands that read Frood; false when the text is not one of them.
    pub(crate) async fn command(&self, message: &Message, external_id: &str, text: &str) -> bool {
        let t = self.lang.texts();
        let command = text.split_whitespace().next().unwrap_or_default();
        let command = command.split('@').next().unwrap_or_default();
        let question = match (command, text.strip_prefix('?')) {
            (_, Some(rest)) => Some(rest.trim()),
            ("/ask", _) => Some(
                text.split_once(char::is_whitespace)
                    .map_or("", |(_, rest)| rest.trim()),
            ),
            _ => None,
        };
        if !matches!(command, "/now" | "/inbox") && question.is_none() {
            return false;
        }
        if !self.linked(external_id).await {
            self.reply(message, t.not_linked).await;
            return true;
        }
        let answer = match (command, question) {
            (_, Some("")) => Ok(t.ask_usage.to_owned()),
            (_, Some(question)) => self.ask(question).await,
            ("/now", _) => self.now().await,
            _ => {
                self.inbox(message.chat.id).await;
                return true;
            }
        };
        let reply = answer.unwrap_or_else(|err| format!("{} {err}", t.error));
        self.reply(message, &reply).await;
        true
    }

    async fn now(&self) -> Result<String, String> {
        let t = self.lang.texts();
        let now = self.get("/v1/now").await?;
        let inbox = self.get("/v1/inbox?limit=200").await?;
        let waiting = open(&inbox).len();
        let mut out = vec![t.now_title.to_owned()];
        let deadlines = now["deadlines"].as_array().cloned().unwrap_or_default();
        if deadlines.is_empty() {
            out.push(t.nothing_due.to_owned());
        } else {
            out.push(format!("{}:", t.deadlines));
            for d in deadlines.iter().take(SHOWN) {
                let when = if d["overdue"] == true {
                    t.overdue.to_owned()
                } else {
                    day(d["due"].as_str().unwrap_or_default(), t)
                };
                out.push(format!("• {when} — {}", d["text"].as_str().unwrap_or("?")));
            }
        }
        out.push(String::new());
        out.push(t.waiting_n.replace("{n}", &waiting.to_string()));
        let changes = now["changes"].as_array().map_or(0, Vec::len);
        out.push(t.changed_n.replace("{n}", &changes.to_string()));
        Ok(out.join("\n"))
    }

    /// Answers from memory without a model: what holds about it, since when.
    async fn ask(&self, question: &str) -> Result<String, String> {
        let t = self.lang.texts();
        let path = format!("/v1/search?limit=3&q={}", url_escape(question));
        let hits = self.get(&path).await?;
        let hits = hits.as_array().cloned().unwrap_or_default();
        if hits.is_empty() {
            return Ok(t.ask_nothing.to_owned());
        }
        Ok(hits
            .iter()
            .map(|h| {
                let since = t.since.replace(
                    "{date}",
                    &day(h["valid_from"].as_str().unwrap_or_default(), t),
                );
                format!("• {} ({since})", h["text"].as_str().unwrap_or("?"))
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Sends what waits for the person as cards with buttons, the first few at once.
    async fn inbox(&self, chat: i64) {
        let t = self.lang.texts();
        let w = self.lang.words();
        let inbox = match self.get("/v1/inbox?limit=200").await {
            Ok(inbox) => inbox,
            Err(err) => {
                let _ = self
                    .telegram(
                        "sendMessage",
                        json!({ "chat_id": chat, "text": format!("{} {err}", t.error) }),
                    )
                    .await;
                return;
            }
        };
        let open = open(&inbox);
        if open.is_empty() {
            let _ = self
                .telegram(
                    "sendMessage",
                    json!({ "chat_id": chat, "text": t.inbox_empty }),
                )
                .await;
            return;
        }
        // Changes with one option can be accepted together, from one button under the cards.
        let mut batch = Vec::new();
        for shown in open.iter().take(SHOWN) {
            let id = shown["item"]["id"].as_str().unwrap_or_default();
            let console = self.console_url(id).await;
            let sent = self
                .telegram(
                    "sendMessage",
                    json!({ "chat_id": chat, "text": text(shown, w), "reply_markup": keyboard(shown, w, console.as_deref()) }),
                )
                .await;
            let one_option = shown["item"]["kind"] == "proposal"
                && shown["actions"]
                    .as_array()
                    .is_some_and(|a| a.iter().filter(|x| x["action"] == "accept").count() == 1);
            if let (Ok(sent), true) = (sent, one_option)
                && let Some(message_id) = sent["message_id"].as_i64()
            {
                batch.push((id.to_owned(), chat, message_id));
            }
        }
        if batch.len() > 1 {
            let key = batch_key();
            let label = t.accept_all.replace("{n}", &batch.len().to_string());
            self.batches
                .lock()
                .expect("batches")
                .insert(key.clone(), batch);
            let _ = self
                .telegram(
                    "sendMessage",
                    json!({ "chat_id": chat, "text": "⬆️", "reply_markup": { "inline_keyboard": [[{ "text": label, "callback_data": format!("b:{key}") }]] } }),
                )
                .await;
        }
        if open.len() > SHOWN {
            let more = t
                .inbox_more
                .replace("{n}", &(open.len() - SHOWN).to_string());
            let _ = self
                .telegram("sendMessage", json!({ "chat_id": chat, "text": more }))
                .await;
        }
    }
}

impl Bot {
    /// "Undo" under a saved note; the text the reply turns into.
    pub(crate) async fn take_note_back(
        &self,
        source_id: &str,
        from: &str,
    ) -> Result<String, String> {
        let t = self.lang.texts();
        let response = self
            .frood(
                self.http
                    .post(format!("{}/v1/sources/{source_id}/withdraw", self.frood)),
            )
            .json(&json!({ "external_id": from }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let ok = response.status().is_success();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !ok {
            return Err(body["detail"].as_str().unwrap_or("?").to_owned());
        }
        Ok(match body["kept"].as_u64().unwrap_or(0) {
            0 => t.taken_back.to_owned(),
            n => t.taken_back_kept.replace("{n}", &n.to_string()),
        })
    }

    /// "Accept all" (or, with `undo`, taking them all back): acts on each card as the person,
    /// edits the cards and the button's message. Returns the callback's answer.
    pub(crate) async fn batch(
        &self,
        key: &str,
        from: &str,
        at: Option<(i64, i64)>,
        undo: bool,
    ) -> String {
        let t = self.lang.texts();
        let w = self.lang.words();
        let Some(cards) = self.batches.lock().expect("batches").get(key).cloned() else {
            return t.batch_gone.to_owned();
        };
        let mut done = 0;
        for (item, chat, message_id) in &cards {
            let request = if undo {
                self.http
                    .post(format!("{}/v1/inbox/{item}/undo", self.frood))
                    .json(&json!({ "external_id": from }))
            } else {
                self.http
                    .post(format!("{}/v1/inbox/{item}/act", self.frood))
                    .json(&json!({ "action": "accept", "external_id": from }))
            };
            if self
                .frood(request)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                done += 1;
            }
            if let Some(shown) = self.inbox_item(item).await {
                self.edit_shown(*chat, *message_id, &shown).await;
            }
        }
        if let Some((chat, message_id)) = at {
            let (text, button, data) = if undo {
                (
                    w.undone.to_owned(),
                    t.accept_all.replace("{n}", &cards.len().to_string()),
                    format!("b:{key}"),
                )
            } else {
                (
                    t.accepted_all.replace("{n}", &done.to_string()),
                    w.undo.to_owned(),
                    format!("B:{key}"),
                )
            };
            let markup =
                json!({ "inline_keyboard": [[{ "text": button, "callback_data": data }]] });
            self.edit(chat, message_id, &text, Some(&markup)).await;
        }
        if undo { w.undone } else { w.ok }.to_owned()
    }
}

/// A short name for a batch of cards, unique enough within one bot's run.
fn batch_key() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    format!("{:x}{:x}", now, NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Items waiting for a person now.
fn open(inbox: &Value) -> Vec<Value> {
    inbox
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["item"]["state"] == "open")
        .cloned()
        .collect()
}

/// `2026-11-14…` as "14 Nov" in the bot's language.
fn day(iso: &str, t: &Texts) -> String {
    let parts: Vec<&str> = iso.get(..10).unwrap_or_default().split('-').collect();
    match parts.as_slice() {
        [_, month, day] => match (month.parse::<usize>(), day.parse::<u32>()) {
            (Ok(month @ 1..=12), Ok(day)) => t
                .date
                .replace("{day}", &day.to_string())
                .replace("{month}", t.months[month - 1]),
            _ => iso.to_owned(),
        },
        _ => iso.to_owned(),
    }
}

fn url_escape(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_read_as_day_and_month() {
        assert_eq!(day("2026-11-14", Lang::Ru.texts()), "14 ноя");
        assert_eq!(day("2026-01-05T10:00:00Z", Lang::En.texts()), "5 Jan");
        assert_eq!(day("soon", Lang::En.texts()), "soon");
    }

    #[test]
    fn questions_are_escaped_for_the_url() {
        assert_eq!(
            url_escape("когда релиз?"),
            "%D0%BA%D0%BE%D0%B3%D0%B4%D0%B0%20%D1%80%D0%B5%D0%BB%D0%B8%D0%B7%3F"
        );
    }
}
