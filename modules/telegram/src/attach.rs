//! Attachments from Telegram (docs/26 §2): a photo, a video, a document or a link sent in reply
//! to a card is kept with the knowledge on that card, or with the change that waits.

use serde_json::{Value, json};

use crate::{Bot, Message, Replied};

impl Bot {
    /// A reply to a card that carries a file or a link: attaches it and says so. False when the
    /// message is not such a reply, so it is captured as usual.
    pub(crate) async fn attach_reply(&self, message: &Message, external_id: &str) -> bool {
        let Some(replied) = message.reply_to_message.as_deref() else {
            return false;
        };
        let words = message.text.as_deref().or(message.caption.as_deref());
        let link = words.and_then(first_link);
        if message.file().is_none() && link.is_none() {
            return false;
        }
        let Some(item_id) = self.replied_item(message.chat.id, replied).await else {
            return false;
        };
        let Some((path, title)) = self.attach_path(&item_id).await else {
            return false;
        };
        let t = self.lang.texts();
        let url = format!("{}{path}", self.frood);
        let sent = match message.file() {
            Some((file, kind)) => match self.file_part(file, kind).await {
                Ok(part) => {
                    let mut form = reqwest::multipart::Form::new()
                        .text("external_id", external_id.to_owned())
                        .part("file", part);
                    if let Some(caption) =
                        message.caption.as_deref().filter(|c| !c.trim().is_empty())
                    {
                        form = form.text("text", caption.to_owned());
                    }
                    self.frood(self.http.post(url)).multipart(form).send().await
                }
                Err(err) => {
                    self.reply(message, &format!("{} {err}", t.error)).await;
                    return true;
                }
            },
            None => {
                let body = json!({ "uri": link, "external_id": external_id });
                self.frood(self.http.post(url)).json(&body).send().await
            }
        };
        let answer = match sent {
            Ok(r) if r.status().is_success() => t.attached.replace("{title}", &title),
            Ok(r) => {
                let body: Value = r.json().await.unwrap_or(Value::Null);
                format!("{} {}", t.error, body["detail"].as_str().unwrap_or("?"))
            }
            Err(err) => format!("{} {err}", t.error),
        };
        self.reply(message, &answer).await;
        true
    }

    /// The Inbox item a card shows: from its buttons, else as Frood remembers the message.
    async fn replied_item(&self, chat: i64, replied: &Replied) -> Option<String> {
        if let Some(id) = item_in_buttons(replied.reply_markup.as_ref()) {
            return Some(id);
        }
        let message_ref = format!("{chat}:{}", replied.message_id);
        let body = self
            .get(&format!(
                "/v1/channels/{}/messages/{message_ref}",
                self.channel
            ))
            .await
            .ok()?;
        body["item"]["id"].as_str().map(str::to_owned)
    }

    /// Where the item keeps attachments — its change, or its knowledge — and its title.
    async fn attach_path(&self, item_id: &str) -> Option<(String, String)> {
        let shown = self.inbox_item(item_id).await?;
        let item = &shown["item"];
        let content = &shown["content"];
        let change = &content["alternatives"][0]["changes"][0];
        let title = change["title"]
            .as_str()
            .or_else(|| change["after"].as_str())
            .or_else(|| change["before"].as_str())
            .or_else(|| content["text"].as_str())
            .or_else(|| item["data"]["text"].as_str())
            .unwrap_or("?");
        let title: String = title.chars().take(80).collect();
        if item["kind"] == "proposal" {
            let proposal = content["proposal_id"].as_str()?;
            return Some((format!("/v1/proposals/{proposal}/attachments"), title));
        }
        let assertion = item["data"]["assertion_id"].as_str()?;
        Some((format!("/v1/assertions/{assertion}/attachments"), title))
    }
}

/// The item id in a card's buttons: `i:<item>:<n>`, `u:<item>`.
fn item_in_buttons(markup: Option<&Value>) -> Option<String> {
    let rows = markup?["inline_keyboard"].as_array()?;
    rows.iter()
        .flat_map(|row| row.as_array().into_iter().flatten())
        .filter_map(|button| button["callback_data"].as_str())
        .find_map(|data| {
            let mut parts = data.split(':');
            match (parts.next(), parts.next()) {
                (Some("i" | "u"), Some(id)) if !id.is_empty() => Some(id.to_owned()),
                _ => None,
            }
        })
}

/// The first web address in a text.
fn first_link(text: &str) -> Option<&str> {
    text.split_whitespace()
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))
        .map(|w| w.trim_end_matches(['.', ',', ')', '»', '"', '\'']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_names_its_item_in_its_buttons() {
        let card = json!({ "inline_keyboard": [[{ "text": "Принять", "callback_data": "i:0199-item:0" }],
                                                [{ "text": "Подробнее", "url": "https://frood.example/admin/inbox/0199-item" }]] });
        assert_eq!(item_in_buttons(Some(&card)).as_deref(), Some("0199-item"));
        let decided = json!({ "inline_keyboard": [[{ "text": "Отменить", "callback_data": "u:0199-item" }]] });
        assert_eq!(
            item_in_buttons(Some(&decided)).as_deref(),
            Some("0199-item")
        );
        assert_eq!(
            item_in_buttons(Some(&json!({ "inline_keyboard": [] }))),
            None
        );
        assert_eq!(item_in_buttons(None), None);
    }

    #[test]
    fn links_are_found_in_words() {
        assert_eq!(
            first_link("макет тут: https://figma.example/file/42."),
            Some("https://figma.example/file/42")
        );
        assert_eq!(first_link("без ссылки"), None);
    }
}
