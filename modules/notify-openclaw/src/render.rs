//! Inbox items as plain text, and replies as actions. OpenClaw's conversations carry text only:
//! no buttons, no edits (docs/28 §4). So the actions are a numbered list, a reply with the
//! number takes one, and what was decided elsewhere comes as a new message.

use serde_json::Value;

use crate::words::Words;

fn short(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// What an action is called in the list.
fn label(action: &Value, several: bool, w: &Words) -> String {
    match action["action"].as_str().unwrap_or_default() {
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
    }
}

/// The actions a reply can take: a digest's "seen" asks for no answer.
pub(crate) fn choices(shown: &Value) -> Vec<Value> {
    shown["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["action"] != "seen")
        .cloned()
        .collect()
}

fn proposal(content: &Value, w: &Words, lines: &mut Vec<String>) {
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
            lines.push(format!("{} «{name}»", w.variant));
        }
        for change in alternative["changes"].as_array().into_iter().flatten() {
            let title = change["title"].as_str().filter(|t| !t.trim().is_empty());
            if let Some(title) = title {
                lines.push(short(title, 120));
            }
            match (change["before"].as_str(), change["after"].as_str()) {
                (Some(before), Some(after)) => {
                    lines.push(format!("{}: {}", w.before, short(before, 220)));
                    lines.push(format!("{}: {}", w.after, short(after, 220)));
                }
                (None, Some(after)) if Some(after.trim()) != title.map(str::trim) => {
                    lines.push(short(after, 300));
                }
                (Some(before), None) => lines.push(format!("{}: {}", w.closed, short(before, 220))),
                _ => {}
            }
        }
    }
    if let Some(quote) = content["quote"].as_str() {
        lines.push(format!("«{}»", short(quote, 220)));
    }
    if let Some(rationale) = content["rationale"].as_str() {
        lines.push(format!("{}: {}", w.why, short(rationale, 200)));
    }
}

fn reminder(content: &Value, w: &Words, lines: &mut Vec<String>) {
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
        "{} {}: {step}",
        w.due,
        content["due"].as_str().unwrap_or("?")
    ));
    lines.push(content["text"].as_str().unwrap_or_default().to_owned());
}

/// "AIRC-1808 — title (3) https://…": a chat turns the address into a link.
fn digest_line(entry: &Value) -> String {
    let title = short(entry["title"].as_str().unwrap_or("?"), 110);
    let count = entry["count"].as_u64().unwrap_or(1);
    let mut line = match entry["label"].as_str() {
        Some(label) => format!("{label} — {title}"),
        None => title,
    };
    if count > 1 {
        line.push_str(&format!(" ({count})"));
    }
    if let Some(link) = entry["link"].as_str() {
        line.push_str(&format!(" {link}"));
    }
    line
}

/// The digest (docs/31): what needs the person now, what is new, how many waited longer. When
/// an agent wrote it in plain words, that text goes instead.
fn digest(content: &Value, written: Option<&str>, w: &Words, lines: &mut Vec<String>) {
    lines.push(w.digest.to_owned());
    if let Some(written) = written.map(str::trim).filter(|t| !t.is_empty()) {
        lines.extend(
            written
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .take(14)
                .map(|l| short(l, 160)),
        );
        return;
    }
    let entries = |key: &str| -> Vec<Value> {
        content
            .pointer(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
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
    let important: Vec<String> = entries("/important")
        .iter()
        .chain(
            entries("/due_soon")
                .iter()
                .filter(|d| d.get("why").is_some()),
        )
        .map(|i| {
            format!(
                "- {} · {}",
                tag(i["why"].as_str().unwrap_or_default()),
                digest_line(i)
            )
        })
        .collect();
    let fresh: Vec<String> = entries("/accepted")
        .iter()
        .chain(entries("/waiting/new").iter())
        .map(|e| format!("- {}", digest_line(e)))
        .collect();
    let older = content["waiting"]["older"]["count"].as_u64().unwrap_or(0);
    if important.is_empty() && fresh.is_empty() && older == 0 {
        lines.push(w.digest_nothing.to_owned());
        return;
    }
    if !important.is_empty() {
        lines.push(format!("{}:", w.digest_important));
        lines.extend(important.into_iter().take(10));
    }
    if !fresh.is_empty() {
        lines.push(format!("{}:", w.digest_new));
        lines.extend(fresh.into_iter().take(10));
    }
    if older > 0 {
        let date = content["waiting"]["older"]["since"]
            .as_str()
            .and_then(|s| s.get(..10))
            .map(|d| {
                let mut parts = d.split('-').skip(1);
                let (month, day) = (parts.next().unwrap_or("?"), parts.next().unwrap_or("?"));
                format!("{day}.{month}")
            })
            .unwrap_or_default();
        lines.push(
            w.digest_older
                .replace("{n}", &older.to_string())
                .replace("{date}", &date),
        );
    }
}

/// The message of a shown item (`{item, content, actions}`).
pub(crate) fn text(shown: &Value, console: Option<&str>, w: &Words) -> String {
    let item = &shown["item"];
    let content = &shown["content"];
    let mut lines: Vec<String> = Vec::new();
    match item["kind"].as_str().unwrap_or_default() {
        "proposal" => proposal(content, w, &mut lines),
        "question" => {
            lines.push(w.question.to_owned());
            lines.push(content["text"].as_str().unwrap_or_default().to_owned());
            if let Some(context) = content["context"].as_str() {
                lines.push(short(context, 400));
            }
        }
        "reminder" => reminder(content, w, &mut lines),
        "alert" => {
            lines.push(format!(
                "{}: {}",
                w.failed,
                content["job"]
                    .as_str()
                    .or_else(|| content["detector"].as_str())
                    .unwrap_or("?")
            ));
            if let Some(error) = content["error"].as_str() {
                lines.push(short(error, 400));
            }
        }
        "digest" => {
            let written = item
                .pointer("/investigation/report")
                .and_then(Value::as_str);
            digest(content, written, w, &mut lines);
            return lines.join("\n");
        }
        "sign_in" => {
            lines.push(w.sign_in_title.to_owned());
            lines.push(format!(
                "{}: {}",
                w.sign_in_app,
                content["client"].as_str().unwrap_or("Frood")
            ));
            lines.push(format!(
                "{}: {}",
                w.sign_in_code,
                content["code"].as_str().unwrap_or("?")
            ));
        }
        _ => lines.push(short(&content.to_string(), 800)),
    }
    if let Some(report) = item
        .pointer("/investigation/report")
        .and_then(Value::as_str)
    {
        lines.push(format!("{}: {}", w.found, short(report, 600)));
    }
    let actions = choices(shown);
    if !actions.is_empty() {
        let several = actions.iter().filter(|a| a["action"] == "accept").count() > 1;
        lines.push(String::new());
        for (i, action) in actions.iter().enumerate() {
            lines.push(format!("{} — {}", i + 1, label(action, several, w)));
        }
        lines.push(w.reply_hint.to_owned());
    }
    if let Some(url) = console {
        lines.push(format!("{}: {url}", w.details));
    }
    lines.join("\n")
}

/// What the item is about, in a few words, for a message that says how it ended.
pub(crate) fn subject(shown: &Value) -> Option<String> {
    let content = &shown["content"];
    let change = &content["alternatives"][0]["changes"][0];
    [
        &change["title"],
        &change["after"],
        &change["before"],
        &content["text"],
        &content["job"],
    ]
    .into_iter()
    .find_map(|v| v.as_str().filter(|t| !t.trim().is_empty()))
    .map(|t| short(t, 120))
}

/// How the situation ended, as a new message: there is no editing the old one. `channel` is this
/// module's: what was decided through it needs no "elsewhere".
pub(crate) fn outcome(shown: &Value, channel: &str, w: &Words) -> String {
    let resolution = &shown["item"]["resolution"];
    let what = match resolution["action"].as_str() {
        Some("accept" | "accept_with_edits") => w.accepted,
        Some("reject") => w.rejected,
        Some("defer" | "snooze") => w.deferred,
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
    match resolution["by"].as_str() {
        Some(by) if by.starts_with("policy:") => format!("{what} {}", w.by_policy),
        Some(by) if !by.is_empty() && !by.ends_with(&format!("/via/{channel}")) => {
            format!("{what} {}", w.elsewhere)
        }
        _ => what.to_owned(),
    }
}

/// `frood <code>`: the code from `frood link` or the console, sent in the conversation to link.
pub(crate) fn link_code(text: &str) -> Option<String> {
    let mut words = text.split_whitespace();
    let first = words.next()?;
    let code = words.next()?;
    let code_like = (8..=12).contains(&code.len())
        && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    (first.eq_ignore_ascii_case("frood") && words.next().is_none() && code_like)
        .then(|| code.to_owned())
}

/// In any of the eight languages: yes takes the affirmative action, no the negative one.
const YES: [&str; 13] = [
    "да",
    "yes",
    "y",
    "ok",
    "ок",
    "sí",
    "si",
    "oui",
    "sim",
    "是",
    "好",
    "हाँ",
    "نعم",
];
const NO: [&str; 9] = ["нет", "no", "n", "non", "não", "nao", "不", "नहीं", "لا"];

/// The action a reply takes, of the ones offered: a number, yes or no. Anything else is talk for
/// the agent, not an answer.
pub(crate) fn reply(text: &str, actions: &[Value]) -> Option<Value> {
    let answer = text
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation() || c == '¡' || c == '¿')
        .to_lowercase();
    if let Ok(n) = answer.parse::<usize>() {
        return n.checked_sub(1).and_then(|i| actions.get(i)).cloned();
    }
    let named = |wanted: &[&str]| {
        actions
            .iter()
            .find(|a| wanted.contains(&a["action"].as_str().unwrap_or_default()))
            .cloned()
    };
    if YES.contains(&answer.as_str()) {
        // Several variants: only a number says which.
        if actions.iter().filter(|a| a["action"] == "accept").count() > 1 {
            return None;
        }
        return named(&["accept", "approve", "done", "retry"]);
    }
    if NO.contains(&answer.as_str()) {
        return named(&["reject", "deny", "dismiss"]);
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::words::Lang;

    fn conflict() -> Value {
        json!({
            "item": { "id": "i1", "kind": "proposal", "state": "open", "investigation": { "state": "done", "report": "Olya confirmed in #release" } },
            "content": {
                "risk": "conflict",
                "quote": "релиз переносим на 28.11",
                "rationale": "#release, Olya",
                "alternatives": [{ "key": "default", "label": null, "changes": [{ "kind": "commitment", "before": "Релиз 2.4 — 14 ноября", "after": "Релиз 2.4 — 28 ноября" }] }]
            },
            "actions": [{ "action": "accept", "alternative": "default" }, { "action": "reject" }, { "action": "defer" }]
        })
    }

    #[test]
    fn a_conflict_reads_as_was_and_now_with_numbered_choices() {
        let w = Lang::Ru.words();
        let message = text(
            &conflict(),
            Some("https://mem.example.com/admin/inbox/i1"),
            w,
        );
        assert!(message.contains("Было: Релиз 2.4 — 14 ноября"), "{message}");
        assert!(
            message.contains("Стало: Релиз 2.4 — 28 ноября"),
            "{message}"
        );
        assert!(message.contains("«релиз переносим на 28.11»"), "{message}");
        assert!(message.contains("Olya confirmed in #release"), "{message}");
        assert!(
            message.contains("1 — Принять\n2 — Оставить старое\n3 — Отложить\nОтветьте цифрой."),
            "{message}"
        );
        assert!(
            message.ends_with("https://mem.example.com/admin/inbox/i1"),
            "{message}"
        );
    }

    #[test]
    fn a_digest_lists_what_needs_you_and_what_is_new_and_asks_for_nothing() {
        let w = Lang::En.words();
        let shown = json!({
            "item": { "id": "d", "kind": "digest", "state": "open" },
            "content": {
                "important": [{ "why": "question", "title": "A decision or a discussion?" }],
                "accepted": [{ "title": "Release moved to Nov 28", "count": 1 }],
                "waiting": {
                    "count": 12,
                    "new": [{ "title": "old session loads for 22 minutes", "label": "AIRC-1808", "link": "https://yt.example/issue/AIRC-1808", "count": 3 }],
                    "older": { "count": 11, "since": "2026-10-20T08:00:00Z" }
                },
                "due_soon": [{ "title": "Send the report", "due": "2026-11-14" }]
            },
            "actions": [{ "action": "seen" }]
        });
        let message = text(&shown, None, w);
        assert!(
            message.contains("- question · A decision or a discussion?"),
            "{message}"
        );
        assert!(message.contains("- Release moved to Nov 28"), "{message}");
        assert!(message.contains("- AIRC-1808 — old session loads for 22 minutes (3) https://yt.example/issue/AIRC-1808"), "{message}");
        assert!(
            message.contains("11 have waited for your decision since 20.10"),
            "{message}"
        );
        assert!(
            !message.contains("Send the report"),
            "not urgent: {message}"
        );
        assert!(!message.contains(w.reply_hint), "{message}");
        assert!(choices(&shown).is_empty());
        let mut written = shown.clone();
        written["item"]["investigation"] = json!({ "state": "done", "report": "AIRC-1808 — the old session loads slowly\n11 more wait" });
        assert_eq!(
            text(&written, None, w),
            format!(
                "{}\nAIRC-1808 — the old session loads slowly\n11 more wait",
                w.digest
            )
        );
    }

    #[test]
    fn replies_take_a_number_yes_or_no_and_nothing_else() {
        let actions = choices(&conflict());
        assert_eq!(reply("2", &actions).unwrap()["action"], "reject");
        assert_eq!(reply(" 3. ", &actions).unwrap()["action"], "defer");
        assert_eq!(reply("Да!", &actions).unwrap()["action"], "accept");
        assert_eq!(reply("нет", &actions).unwrap()["action"], "reject");
        assert_eq!(reply("Oui", &actions).unwrap()["action"], "accept");
        assert!(reply("4", &actions).is_none());
        assert!(reply("0", &actions).is_none());
        assert!(reply("да, но давай обсудим", &actions).is_none());
        assert!(reply("what about the release?", &actions).is_none());

        // Several variants: yes does not say which; a number does.
        let variants = [
            json!({ "action": "accept", "alternative": "a" }),
            json!({ "action": "accept", "alternative": "b" }),
            json!({ "action": "reject" }),
        ];
        assert!(reply("да", &variants).is_none());
        assert_eq!(reply("2", &variants).unwrap()["alternative"], "b");

        // A question has no yes: its options are the answers.
        let question = [
            json!({ "action": "answer", "answer": "Да" }),
            json!({ "action": "snooze" }),
        ];
        assert!(reply("да", &question).is_none());
        assert_eq!(reply("1", &question).unwrap()["answer"], "Да");
    }

    #[test]
    fn a_link_code_comes_after_the_word_frood() {
        assert_eq!(link_code("frood ABCD2345").as_deref(), Some("ABCD2345"));
        assert_eq!(
            link_code("  Frood abcd-2345 ").as_deref(),
            Some("abcd-2345")
        );
        assert_eq!(link_code("ABCD2345"), None);
        assert_eq!(link_code("frood is great"), None);
        assert_eq!(link_code("frood ABCD2345 please"), None);
    }

    #[test]
    fn what_ended_elsewhere_says_so() {
        let w = Lang::Ru.words();
        let done = |action: &str, by: &str| json!({ "item": { "resolution": { "action": action, "by": by } } });
        assert_eq!(
            outcome(&done("accept", "user:1/via/claude-code"), "openclaw", w),
            format!("{} {}", w.accepted, w.elsewhere)
        );
        assert_eq!(
            outcome(&done("reject", "user:1/via/openclaw"), "openclaw", w),
            w.rejected
        );
        assert_eq!(outcome(&done("reject", ""), "openclaw", w), w.rejected);
        assert_eq!(
            outcome(&done("expired", "policy:ttl"), "openclaw", w),
            format!("{} {}", w.expired, w.by_policy)
        );
    }
}
