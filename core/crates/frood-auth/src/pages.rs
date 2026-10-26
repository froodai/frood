//! The few HTML pages of sign-in: choose a provider, the development form, consent, errors.

use axum::response::Html;

use crate::scope;

pub(crate) fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Referrer policy of every page: `same-origin` keeps the `tx` in our URLs away from other
/// sites, while form posts still carry our `Origin`, which the server checks. (With
/// `no-referrer` browsers send `Origin: null` on form posts, and every sign-in is refused.)
const REFERRER_POLICY: &str = "same-origin";

fn page(title: &str, body: &str) -> Html<String> {
    page_with(title, "", body)
}

/// A page with more in its head: a refresh while it waits.
fn page_with(title: &str, head: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"referrer\" content=\"{REFERRER_POLICY}\">{head}<title>{title}</title><style>\
         :root{{--bg:#f2f5f1;--card:#fbfcfa;--ink:#17231d;--muted:#56675e;--rule:#d3dfd6;--accent:#1d6b4c;--on-accent:#fff}}\
         @media (prefers-color-scheme:dark){{:root{{--bg:#0e1411;--card:#151d19;--ink:#dce6df;--muted:#93a59b;--rule:#26332c;--accent:#5cc495;--on-accent:#0b1a12}}}}\
         *{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--ink);font:16px/1.5 system-ui,sans-serif}}\
         main{{max-width:440px;margin:8vh auto;padding:0 16px}}.card{{background:var(--card);border:1px solid var(--rule);border-radius:10px;padding:24px}}\
         h1{{font-size:20px;margin:0 0 12px}}p{{margin:0 0 12px}}.muted{{color:var(--muted);font-size:14px}}\
         .btn{{display:block;width:100%;text-align:center;background:var(--accent);color:var(--on-accent);border:0;border-radius:8px;padding:10px;font:600 15px system-ui;cursor:pointer;text-decoration:none;margin-top:10px}}\
         .btn.secondary{{background:transparent;color:var(--ink);border:1px solid var(--rule)}}\
         label{{display:flex;gap:10px;align-items:flex-start;padding:8px 0;border-top:1px solid var(--rule)}}\
         input[type=email]{{width:100%;padding:9px;border:1px solid var(--rule);border-radius:8px;background:var(--bg);color:var(--ink);font:inherit}}\
         code{{font-size:13px}}.code{{font:600 34px/1.2 ui-monospace,monospace;letter-spacing:.2em;text-align:center;margin:16px 0;font-variant-numeric:tabular-nums}}</style></head><body><main><div class=\"card\">{body}</div></main></body></html>"
    ))
}

pub(crate) fn error(message: &str) -> Html<String> {
    page(
        "Sign-in problem",
        &format!(
            "<h1>Sign-in problem</h1><p>{}</p><p class=\"muted\">Start again from the app you were connecting.</p>",
            escape(message)
        ),
    )
}

/// `base` is the server's path behind the proxy (`/frood`, or empty at the root).
pub(crate) fn choose_provider(
    installation: &str,
    base: &str,
    providers: &[(String, String)],
    tx: &str,
) -> Html<String> {
    let buttons: String = providers
        .iter()
        .map(|(id, label)| {
            format!(
                "<a class=\"btn\" href=\"{}/oauth/login/{}?tx={}\">Continue with {}</a>",
                escape(base),
                escape(id),
                escape(tx),
                escape(label)
            )
        })
        .collect();
    page(
        "Sign in",
        &format!(
            "<h1>Sign in to Frood</h1><p class=\"muted\">Installation <code>{}</code></p>{buttons}",
            escape(installation)
        ),
    )
}

pub(crate) fn dev_form(installation: &str, base: &str, provider: &str, tx: &str) -> Html<String> {
    page(
        "Sign in (development)",
        &format!(
            "<h1>Development sign-in</h1><p class=\"muted\">Installation <code>{}</code>. This form exists only on a local server.</p>\
             <form method=\"post\" action=\"{}/oauth/login/{}\"><input type=\"hidden\" name=\"tx\" value=\"{}\">\
             <input type=\"email\" name=\"email\" placeholder=\"you@example.com\" required autofocus>\
             <button class=\"btn\" type=\"submit\">Sign in</button></form>",
            escape(installation),
            escape(base),
            escape(provider),
            escape(tx)
        ),
    )
}

/// Sign-in on this computer (docs/27 §1): no password, the address filled in for one person.
pub(crate) fn local_form(
    installation: &str,
    base: &str,
    provider: &str,
    tx: &str,
    email: &str,
) -> Html<String> {
    page(
        "Sign in on this computer",
        &format!(
            "<h1>Sign in on this computer</h1><p class=\"muted\">Installation <code>{}</code> runs on this computer \
             and is reachable only from it, so no password is asked.</p>\
             <form method=\"post\" action=\"{}/oauth/login/{}\"><input type=\"hidden\" name=\"tx\" value=\"{}\">\
             <input type=\"email\" name=\"email\" value=\"{}\" placeholder=\"you@example.com\" required autofocus>\
             <button class=\"btn\" type=\"submit\">Sign in</button></form>",
            escape(installation),
            escape(base),
            escape(provider),
            escape(tx),
            escape(email)
        ),
    )
}

pub(crate) struct ChannelForm<'a> {
    pub installation: &'a str,
    pub base: &'a str,
    pub provider: &'a str,
    /// How the channel is called on the page: "Telegram".
    pub channel: &'a str,
    pub tx: &'a str,
    pub email: &'a str,
}

/// Sign-in through a channel (docs/28 §2): the address, then a button in the channel.
pub(crate) fn channel_form(form: &ChannelForm<'_>) -> Html<String> {
    page(
        &format!("Sign in with {}", escape(form.channel)),
        &format!(
            "<h1>Sign in with {channel}</h1><p class=\"muted\">Installation <code>{}</code>. Frood sends a \
             button to the {channel} account linked to your address.</p>\
             <form method=\"post\" action=\"{}/oauth/login/{}/ask\"><input type=\"hidden\" name=\"tx\" value=\"{}\">\
             <input type=\"email\" name=\"email\" value=\"{}\" placeholder=\"you@example.com\" required autofocus>\
             <button class=\"btn\" type=\"submit\">Send the button to {channel}</button></form>",
            escape(form.installation),
            escape(form.base),
            escape(form.provider),
            escape(form.tx),
            escape(form.email),
            channel = escape(form.channel),
        ),
    )
}

/// Waits for the press, reloading itself; `again` sends a new button.
pub(crate) fn channel_wait(
    installation: &str,
    channel: &str,
    code: &str,
    again: &str,
) -> Html<String> {
    page_with(
        &format!("Check {}", escape(channel)),
        "<meta http-equiv=\"refresh\" content=\"2\">",
        &format!(
            "<h1>Check {channel}</h1><p>Press <b>Sign in</b> in the message from the bot, if it shows this code:</p>\
             <p class=\"code\">{}</p><p class=\"muted\">Installation <code>{}</code>. The button works for {} minutes; \
             this page goes on by itself.</p><a class=\"btn secondary\" href=\"{}\">Send a new button</a>",
            escape(code),
            escape(installation),
            crate::sign_in::ASK_MINUTES,
            escape(again),
            channel = escape(channel),
        ),
    )
}

pub(crate) fn channel_expired(channel: &str, again: &str) -> Html<String> {
    page(
        "The button expired",
        &format!(
            "<h1>The button expired</h1><p>Nobody pressed it in {channel} in time, or a newer one was sent.</p>\
             <a class=\"btn\" href=\"{}\">Send a new button</a>",
            escape(again),
            channel = escape(channel),
        ),
    )
}

pub(crate) struct Consent<'a> {
    pub installation: &'a str,
    pub base: &'a str,
    pub client: &'a str,
    pub client_id: &'a str,
    pub email: &'a str,
    pub requested: &'a [String],
    pub tx: &'a str,
}

pub(crate) fn consent(consent: &Consent<'_>) -> Html<String> {
    // Only the everyday permissions are pre-ticked; deciding on someone's behalf, the journal
    // and admin take a deliberate tick (docs/12 §3.3), whatever the client asked for.
    let offered: Vec<&str> = scope::ALL
        .into_iter()
        .filter(|s| *s != scope::ADMIN || consent.requested.iter().any(|r| r == scope::ADMIN))
        .collect();
    let checkboxes: String = offered
        .iter()
        .map(|s| {
            let checked =
                consent.requested.iter().any(|r| r == s) && scope::CLIENT_DEFAULT.contains(s);
            format!(
                "<label><input type=\"checkbox\" name=\"scope\" value=\"{s}\"{}> <span><code>{s}</code><br>\
                 <span class=\"muted\">{}</span></span></label>",
                if checked { " checked" } else { "" },
                scope::describe(s)
            )
        })
        .collect();
    page(
        "Allow access",
        &format!(
            "<h1>Allow {} to use your Frood?</h1>\
             <p class=\"muted\">Installation <code>{}</code>, signed in as {}. Client <code>{}</code>.</p>\
             <form method=\"post\" action=\"{}/oauth/consent\"><input type=\"hidden\" name=\"tx\" value=\"{}\">{checkboxes}\
             <button class=\"btn\" name=\"decision\" value=\"allow\" type=\"submit\">Allow</button>\
             <button class=\"btn secondary\" name=\"decision\" value=\"deny\" type=\"submit\">Deny</button></form>",
            escape(consent.client),
            escape(consent.installation),
            escape(consent.email),
            escape(consent.client_id),
            escape(consent.base),
            escape(consent.tx)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup() {
        assert_eq!(
            escape("<a href=\"x\">'&"),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;"
        );
        let html = error("<script>alert(1)</script>").0;
        assert!(!html.contains("<script>alert"));
    }

    #[test]
    fn form_posts_keep_their_origin() {
        let html = dev_form("test", "", "dev", "tx_1").0;
        assert!(html.contains("<meta name=\"referrer\" content=\"same-origin\">"));
        assert!(!html.contains("no-referrer"));
    }

    #[test]
    fn consent_never_preticks_decide() {
        let requested = vec![scope::READ.to_owned(), scope::DECIDE.to_owned()];
        let html = consent(&Consent {
            installation: "personal",
            base: "",
            client: "Claude",
            client_id: "https://claude.ai/x",
            email: "a@b.c",
            requested: &requested,
            tx: "tx_1",
        })
        .0;
        assert!(html.contains("value=\"knowledge:read\" checked"));
        assert!(html.contains("value=\"knowledge:decide\">"));
        assert!(!html.contains("value=\"admin\""));
    }

    #[test]
    fn consent_preticks_only_everyday_permissions() {
        // What Claude Code asks for: everything the resource metadata lists.
        let requested: Vec<String> = scope::ALL.iter().map(|s| (*s).to_owned()).collect();
        let html = consent(&Consent {
            installation: "personal",
            base: "/frood",
            client: "Claude Code",
            client_id: "https://claude.ai/oauth/claude-code-client-metadata",
            email: "a@b.c",
            requested: &requested,
            tx: "tx_1",
        })
        .0;
        assert!(html.contains("action=\"/frood/oauth/consent\""));
        for s in scope::CLIENT_DEFAULT {
            assert!(html.contains(&format!("value=\"{s}\" checked")), "{s}");
        }
        for s in [scope::DECIDE, scope::JOURNAL, scope::ADMIN] {
            assert!(
                html.contains(&format!("value=\"{s}\">")),
                "{s} is offered unticked"
            );
        }
    }
}
