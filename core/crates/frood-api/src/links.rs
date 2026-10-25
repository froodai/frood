//! The title of a web page someone attached (docs/26 §2): fetched once, only from public
//! addresses, a little of the page and a few redirects at most.

use std::net::SocketAddr;
use std::time::Duration;

const MAX_BYTES: usize = 256 * 1024;
const REDIRECTS: usize = 3;

/// The page's title (`og:title`, else `<title>`), or none when it cannot be read.
pub(crate) async fn title(uri: &str) -> Option<String> {
    let mut url = url::Url::parse(uri).ok()?;
    for _ in 0..=REDIRECTS {
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
            return None;
        }
        let host = url.host_str()?.to_owned();
        let port = url.port_or_known_default()?;
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .ok()?
            .collect();
        if addresses.is_empty() || addresses.iter().any(|a| !frood_auth::is_public_ip(a.ip())) {
            return None;
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .resolve(&host, addresses[0])
            .user_agent("Frood (link title)")
            .build()
            .ok()?;
        let mut response = client
            .get(url.clone())
            .header("accept", "text/html,application/xhtml+xml")
            .send()
            .await
            .ok()?;
        if response.status().is_redirection() {
            let next = response.headers().get("location")?.to_str().ok()?;
            url = url.join(next).ok()?;
            continue;
        }
        if !response.status().is_success() {
            return None;
        }
        let mut page = Vec::new();
        while let Ok(Some(chunk)) = response.chunk().await {
            page.extend_from_slice(&chunk);
            if page.len() >= MAX_BYTES || find(&page, b"</head").is_some() {
                break;
            }
        }
        return title_of(&String::from_utf8_lossy(&page));
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

/// `og:title` when the page has one, else its `<title>`; entities decoded, spaces folded.
fn title_of(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let og = lower.match_indices("<meta").find_map(|(start, _)| {
        let end = start + lower[start..].find('>')?;
        let tag = &lower[start..end];
        (tag.contains("property=\"og:title\"") || tag.contains("property='og:title'"))
            .then(|| attribute(&html[start..end], "content"))
            .flatten()
    });
    let title = og.or_else(|| {
        let open = lower.find("<title")?;
        let start = open + lower[open..].find('>')? + 1;
        let end = start + lower[start..].find("</title")?;
        Some(html[start..end].to_owned())
    })?;
    let title = decode(&title)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let title: String = title.chars().take(200).collect();
    (!title.is_empty()).then_some(title)
}

/// The value of an attribute in a tag, quoted with " or '.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let quote = tag[at..]
        .chars()
        .next()
        .filter(|c| *c == '"' || *c == '\'')?;
    let rest = &tag[at + 1..];
    Some(rest[..rest.find(quote)?].to_owned())
}

fn decode(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_graph_title_wins_then_the_page_title() {
        let page = r#"<html><head><title>  Atlas &amp; friends
            — wiki </title><meta property="og:title" content="Atlas: release plan"></head>"#;
        assert_eq!(title_of(page).as_deref(), Some("Atlas: release plan"));
        let plain = "<HTML><HEAD><TITLE>Atlas &amp; friends\n — wiki</TITLE></HEAD>";
        assert_eq!(title_of(plain).as_deref(), Some("Atlas & friends — wiki"));
        assert_eq!(title_of("<html><body>no title</body>"), None);
    }

    #[tokio::test]
    async fn private_addresses_are_not_fetched() {
        assert_eq!(title("http://127.0.0.1:9/").await, None);
        assert_eq!(title("http://localhost/").await, None);
        assert_eq!(title("file:///etc/passwd").await, None);
    }
}
