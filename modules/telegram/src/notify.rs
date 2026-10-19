//! `notify.send@1` and `notify.update@1` (contracts/notify.send/1): Inbox items as Telegram
//! messages with buttons, edited when the situation is resolved. A pressed button acts through
//! `POST /v1/inbox/{id}/act` for the person whose Telegram account pressed it.

use serde_json::{Value, json};

use crate::{Bot, Lang};

/// Words of notifications.
pub(crate) struct Words {
    conflict: &'static str,
    change: &'static str,
    new: &'static str,
    stale: &'static str,
    merge: &'static str,
    variant: &'static str,
    before: &'static str,
    after: &'static str,
    added: &'static str,
    closed: &'static str,
    why: &'static str,
    from_agent: &'static str,
    found: &'static str,
    question: &'static str,
    due: &'static str,
    due_before: &'static str,
    due_today: &'static str,
    due_overdue: &'static str,
    failed: &'static str,
    accept: &'static str,
    reject: &'static str,
    defer: &'static str,
    snooze: &'static str,
    done: &'static str,
    postpone: &'static str,
    retry: &'static str,
    dismiss: &'static str,
    accepted: &'static str,
    rejected: &'static str,
    deferred: &'static str,
    answered: &'static str,
    expired: &'static str,
    cancelled: &'static str,
    marked_done: &'static str,
    postponed: &'static str,
    retried: &'static str,
    dismissed: &'static str,
    stale_now: &'static str,
    recovered: &'static str,
    resolved: &'static str,
    elsewhere: &'static str,
    by_policy: &'static str,
    already: &'static str,
    ok: &'static str,
    digest: &'static str,
    digest_accepted: &'static str,
    digest_waiting: &'static str,
    digest_due: &'static str,
    digest_failed: &'static str,
    digest_nothing: &'static str,
}

const EN: Words = Words {
    conflict: "⚠️ Conflict with what is recorded",
    change: "✏️ Change to what is recorded",
    new: "🆕 New knowledge",
    stale: "🕰 Knowledge may be stale",
    merge: "🔗 Same entity?",
    variant: "Option",
    before: "Was",
    after: "Now",
    added: "+",
    closed: "Closes",
    why: "Why",
    from_agent: "From agent",
    found: "The agent found",
    question: "❓ A question from agent",
    due: "⏰ Due",
    due_before: "is due tomorrow",
    due_today: "is due today",
    due_overdue: "was due: done, or move it?",
    failed: "🚨 A run failed",
    accept: "Accept",
    reject: "Reject",
    defer: "Later",
    snooze: "Later",
    done: "Done",
    postpone: "Move a week",
    retry: "Retry",
    dismiss: "Hide",
    accepted: "✅ Accepted",
    rejected: "❌ Rejected",
    deferred: "⏸ Put off",
    answered: "💬 Answered",
    expired: "⌛ Expired",
    cancelled: "🚫 Cancelled",
    marked_done: "✅ Done",
    postponed: "📅 Moved",
    retried: "🔁 Retried",
    dismissed: "🙈 Hidden",
    stale_now: "♻️ Outdated: the knowledge changed meanwhile",
    recovered: "✅ The job works again",
    resolved: "✅ Resolved",
    elsewhere: "elsewhere",
    by_policy: "by a policy",
    already: "Already resolved",
    ok: "Done",
    digest: "📋 What is new",
    digest_accepted: "Accepted",
    digest_waiting: "Waiting for you",
    digest_due: "Due soon",
    digest_failed: "Failed runs",
    digest_nothing: "Nothing new.",
};

const RU: Words = Words {
    conflict: "⚠️ Противоречит записанному",
    change: "✏️ Изменение записанного",
    new: "🆕 Новое знание",
    stale: "🕰 Знание, возможно, устарело",
    merge: "🔗 Это одно и то же?",
    variant: "Вариант",
    before: "Было",
    after: "Стало",
    added: "+",
    closed: "Закрыть",
    why: "Почему",
    from_agent: "От агента",
    found: "Агент выяснил",
    question: "❓ Вопрос от агента",
    due: "⏰ Срок",
    due_before: "завтра срок",
    due_today: "срок сегодня",
    due_overdue: "срок прошёл: выполнено или перенести?",
    failed: "🚨 Запуск упал",
    accept: "Принять",
    reject: "Отклонить",
    defer: "Отложить",
    snooze: "Отложить",
    done: "Выполнено",
    postpone: "Перенести на неделю",
    retry: "Повторить",
    dismiss: "Скрыть",
    accepted: "✅ Принято",
    rejected: "❌ Отклонено",
    deferred: "⏸ Отложено",
    answered: "💬 Отвечено",
    expired: "⌛ Время вышло",
    cancelled: "🚫 Отменено",
    marked_done: "✅ Выполнено",
    postponed: "📅 Перенесено",
    retried: "🔁 Запущено снова",
    dismissed: "🙈 Скрыто",
    stale_now: "♻️ Неактуально: знание успело измениться",
    recovered: "✅ Джоба снова работает",
    resolved: "✅ Решено",
    elsewhere: "в другом месте",
    by_policy: "политикой",
    already: "Уже решено",
    ok: "Готово",
    digest: "📋 Что нового",
    digest_accepted: "Принято",
    digest_waiting: "Ждёт вашего решения",
    digest_due: "Скоро сроки",
    digest_failed: "Сбои запусков",
    digest_nothing: "Ничего нового.",
};

impl Lang {
    pub(crate) fn words(self) -> &'static Words {
        match self {
            Self::En => &EN,
            Self::Ru => &RU,
        }
    }
}

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
pub(crate) fn text(shown: &Value, w: &Words) -> String {
    let item = &shown["item"];
    let content = &shown["content"];
    let mut lines: Vec<String> = Vec::new();
    match item["kind"].as_str().unwrap_or_default() {
        "proposal" => {
            lines.push(
                match content["risk"].as_str() {
                    Some("conflict") => w.conflict,
                    Some("change") => w.change,
                    Some("stale_knowledge") => w.stale,
                    Some("entity_merge") => w.merge,
                    _ => w.new,
                }
                .to_owned(),
            );
            let alternatives = content["alternatives"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for alternative in &alternatives {
                if alternatives.len() > 1 {
                    let name = alternative["label"]
                        .as_str()
                        .or_else(|| alternative["key"].as_str())
                        .unwrap_or("?");
                    lines.push(format!("\n{} «{name}»:", w.variant));
                }
                for change in alternative["changes"].as_array().into_iter().flatten() {
                    match (change["before"].as_str(), change["after"].as_str()) {
                        (Some(before), Some(after)) => {
                            lines.push(format!("{}: {}", w.before, short(before, 300)));
                            lines.push(format!("{}: {}", w.after, short(after, 300)));
                        }
                        (None, Some(after)) => {
                            lines.push(format!("{} {}", w.added, short(after, 300)))
                        }
                        (Some(before), None) => {
                            lines.push(format!("{}: {}", w.closed, short(before, 300)))
                        }
                        (None, None) => {}
                    }
                }
            }
            if let Some(quote) = content["quote"].as_str() {
                lines.push(format!("\n«{}»", short(quote, 300)));
            }
            if let Some(rationale) = content["rationale"].as_str() {
                lines.push(format!("{}: {}", w.why, short(rationale, 300)));
            }
            if let Some(job) = content["author"].as_str().and_then(agent_job) {
                lines.push(format!("{}: {job}", w.from_agent));
            }
        }
        "question" => {
            lines.push(format!(
                "{} {}:",
                w.question,
                content["job"].as_str().unwrap_or("?")
            ));
            lines.push(content["text"].as_str().unwrap_or_default().to_owned());
            if let Some(context) = content["context"].as_str() {
                lines.push(format!("\n{}", short(context, 500)));
            }
        }
        "reminder" => {
            let step = match content["step"].as_str() {
                Some("before") => w.due_before,
                Some("today") => w.due_today,
                _ => w.due_overdue,
            };
            lines.push(format!(
                "{} {}: {}",
                w.due,
                content["due"].as_str().unwrap_or("?"),
                step
            ));
            lines.push(content["text"].as_str().unwrap_or_default().to_owned());
        }
        "alert" => {
            lines.push(format!(
                "{}: {}",
                w.failed,
                content["job"].as_str().unwrap_or("?")
            ));
            if let Some(error) = content["error"].as_str() {
                lines.push(short(error, 500));
            }
        }
        "digest" => lines.push(digest_text(content, w)),
        _ => lines.push(short(&content.to_string(), 1000)),
    }
    if let Some(report) = item
        .pointer("/investigation/report")
        .and_then(Value::as_str)
    {
        lines.push(format!("\n🔎 {}: {}", w.found, short(report, 800)));
    }
    if item["state"] == "resolved" {
        lines.push(format!("\n{}", resolution(&item["resolution"], w)));
    }
    lines.join("\n")
}

/// A digest (`GET /v1/digest` or a `digest` item) as text.
pub(crate) fn digest_text(digest: &Value, w: &Words) -> String {
    let mut lines = vec![w.digest.to_owned()];
    let list = |title: &str, items: Vec<String>, lines: &mut Vec<String>| {
        if !items.is_empty() {
            lines.push(format!("\n{title}:"));
            lines.extend(items.into_iter().map(|i| format!("• {}", short(&i, 200))));
        }
    };
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    list(w.digest_accepted, strings(&digest["accepted"]), &mut lines);
    let waiting: Vec<String> = digest["waiting"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["what"].as_str().map(str::to_owned))
        .collect();
    let count = digest["waiting"]["count"].as_u64().unwrap_or(0);
    if count > 0 {
        list(
            &format!("{} ({count})", w.digest_waiting),
            waiting,
            &mut lines,
        );
    }
    let due: Vec<String> = digest["due_soon"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            format!(
                "{} — {}",
                d["due"].as_str().unwrap_or("?"),
                d["text"].as_str().unwrap_or("?")
            )
        })
        .collect();
    list(w.digest_due, due, &mut lines);
    let failed: Vec<String> = digest["failed_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            format!(
                "{}: {}",
                r["job"].as_str().unwrap_or("?"),
                r["error"].as_str().unwrap_or("?")
            )
        })
        .collect();
    list(w.digest_failed, failed, &mut lines);
    if lines.len() == 1 {
        lines.push(w.digest_nothing.to_owned());
    }
    lines.join("\n")
}

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

/// One row per button; callback data names the item and the action's index.
pub(crate) fn keyboard(shown: &Value, w: &Words) -> Value {
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
                other => other.to_owned(),
            };
            json!([{ "text": short(&label, 60), "callback_data": format!("i:{id}:{i}") }])
        })
        .collect();
    json!({ "inline_keyboard": rows })
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
        let sent = self
            .telegram(
                "sendMessage",
                json!({ "chat_id": chat, "text": text(shown, w), "reply_markup": keyboard(shown, w) }),
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

    async fn edit_shown(&self, chat: i64, message_id: i64, shown: &Value) {
        let w = self.lang.words();
        let result = self
            .telegram(
                "editMessageText",
                json!({ "chat_id": chat, "message_id": message_id, "text": text(shown, w),
                        "reply_markup": { "inline_keyboard": [] } }),
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
        let mut parts = data.split(':');
        let (Some("i"), Some(item_id), Some(index)) = (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
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
        let chat = query.pointer("/message/chat/id").and_then(Value::as_i64);
        let message_id = query.pointer("/message/message_id").and_then(Value::as_i64);
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_conflict_reads_as_was_and_now_with_buttons() {
        let w = Lang::Ru.words();
        let shown = conflict();
        assert_eq!(
            text(&shown, w),
            "⚠️ Противоречит записанному\nБыло: Релиз 2.4 — 14 ноября\nСтало: Релиз 2.4 — 28 ноября\n\n«релиз переносим на 28.11»\nПочему: #release, 18:02\nОт агента: daily-sweep\n\n🔎 Агент выяснил: Olya confirmed in #release"
        );
        assert_eq!(
            keyboard(&shown, w),
            json!({ "inline_keyboard": [
                [{ "text": "Принять", "callback_data": "i:0199-item:0" }],
                [{ "text": "Отклонить", "callback_data": "i:0199-item:1" }],
                [{ "text": "Отложить", "callback_data": "i:0199-item:2" }]
            ] })
        );
    }

    #[test]
    fn a_digest_lists_what_is_new() {
        let w = Lang::Ru.words();
        let digest = json!({
            "accepted": ["Релиз 2.4 — 28 ноября"],
            "waiting": { "count": 1, "items": [{ "kind": "question", "what": "Решение или обсуждение?" }] },
            "due_soon": [{ "text": "Сдать отчёт", "due": "2026-11-14" }],
            "failed_runs": []
        });
        assert_eq!(
            digest_text(&digest, w),
            "📋 Что нового\n\nПринято:\n• Релиз 2.4 — 28 ноября\n\nЖдёт вашего решения (1):\n• Решение или обсуждение?\n\nСкоро сроки:\n• 2026-11-14 — Сдать отчёт"
        );
        assert_eq!(
            digest_text(&json!({ "waiting": { "count": 0 } }), w),
            "📋 Что нового\nНичего нового."
        );
    }

    #[test]
    fn a_resolved_item_says_how() {
        let w = Lang::Ru.words();
        let mut shown = conflict();
        shown["item"]["state"] = json!("resolved");
        shown["item"]["resolution"] =
            json!({ "action": "accept", "by": "user:0199/via/claude-code" });
        assert!(text(&shown, w).ends_with("✅ Принято в другом месте"));
        shown["item"]["resolution"] = json!({ "action": "reject", "by": "user:0199/via/telegram" });
        assert!(text(&shown, w).ends_with("❌ Отклонено"));
        assert_eq!(parse_ref("373739001:52"), Some((373_739_001, 52)));
    }
}
