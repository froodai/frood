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

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\"><title>{title}</title><style>\
         :root{{--bg:#f2f5f1;--card:#fbfcfa;--ink:#17231d;--muted:#56675e;--rule:#d3dfd6;--accent:#1d6b4c;--on-accent:#fff}}\
         @media (prefers-color-scheme:dark){{:root{{--bg:#0e1411;--card:#151d19;--ink:#dce6df;--muted:#93a59b;--rule:#26332c;--accent:#5cc495;--on-accent:#0b1a12}}}}\
         *{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--ink);font:16px/1.5 system-ui,sans-serif}}\
         main{{max-width:440px;margin:8vh auto;padding:0 16px}}.card{{background:var(--card);border:1px solid var(--rule);border-radius:10px;padding:24px}}\
         h1{{font-size:20px;margin:0 0 12px}}p{{margin:0 0 12px}}.muted{{color:var(--muted);font-size:14px}}\
         .btn{{display:block;width:100%;text-align:center;background:var(--accent);color:var(--on-accent);border:0;border-radius:8px;padding:10px;font:600 15px system-ui;cursor:pointer;text-decoration:none;margin-top:10px}}\
         .btn.secondary{{background:transparent;color:var(--ink);border:1px solid var(--rule)}}\
         label{{display:flex;gap:10px;align-items:flex-start;padding:8px 0;border-top:1px solid var(--rule)}}\
         input[type=email]{{width:100%;padding:9px;border:1px solid var(--rule);border-radius:8px;background:var(--bg);color:var(--ink);font:inherit}}\
         code{{font-size:13px}}</style></head><body><main><div class=\"card\">{body}</div></main></body></html>"
    ))
}

pub(crate) fn error(message: &str) -> Html<String> {
    page(
        "Sign-in problem",
        &format!("<h1>Sign-in problem</h1><p>{}</p><p class=\"muted\">Start again from the app you were connecting.</p>", escape(message)),
    )
}

pub(crate) fn choose_provider(installation: &str, providers: &[(String, String)], tx: &str) -> Html<String> {
    let buttons: String = providers
        .iter()
        .map(|(id, label)| {
            format!(
                "<a class=\"btn\" href=\"/oauth/login/{}?tx={}\">Continue with {}</a>",
                escape(id),
                escape(tx),
                escape(label)
            )
        })
        .collect();
    page(
        "Sign in",
        &format!("<h1>Sign in to Frood</h1><p class=\"muted\">Installation <code>{}</code></p>{buttons}", escape(installation)),
    )
}

pub(crate) fn dev_form(installation: &str, provider: &str, tx: &str) -> Html<String> {
    page(
        "Sign in (development)",
        &format!(
            "<h1>Development sign-in</h1><p class=\"muted\">Installation <code>{}</code>. This form exists only on a local server.</p>\
             <form method=\"post\" action=\"/oauth/login/{}\"><input type=\"hidden\" name=\"tx\" value=\"{}\">\
             <input type=\"email\" name=\"email\" placeholder=\"you@example.com\" required autofocus>\
             <button class=\"btn\" type=\"submit\">Sign in</button></form>",
            escape(installation),
            escape(provider),
            escape(tx)
        ),
    )
}

pub(crate) struct Consent<'a> {
    pub installation: &'a str,
    pub client: &'a str,
    pub client_id: &'a str,
    pub email: &'a str,
    pub requested: &'a [String],
    pub tx: &'a str,
}

pub(crate) fn consent(consent: &Consent<'_>) -> Html<String> {
    // Deciding on someone's behalf is never pre-ticked (docs/12 §3.3).
    let offered: Vec<&str> = scope::ALL
        .into_iter()
        .filter(|s| *s != scope::ADMIN || consent.requested.iter().any(|r| r == scope::ADMIN))
        .collect();
    let checkboxes: String = offered
        .iter()
        .map(|s| {
            let checked = consent.requested.iter().any(|r| r == s) && *s != scope::DECIDE;
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
             <form method=\"post\" action=\"/oauth/consent\"><input type=\"hidden\" name=\"tx\" value=\"{}\">{checkboxes}\
             <button class=\"btn\" name=\"decision\" value=\"allow\" type=\"submit\">Allow</button>\
             <button class=\"btn secondary\" name=\"decision\" value=\"deny\" type=\"submit\">Deny</button></form>",
            escape(consent.client),
            escape(consent.installation),
            escape(consent.email),
            escape(consent.client_id),
            escape(consent.tx)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup() {
        assert_eq!(escape("<a href=\"x\">'&"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;");
        let html = error("<script>alert(1)</script>").0;
        assert!(!html.contains("<script>alert"));
    }

    #[test]
    fn consent_never_preticks_decide() {
        let requested = vec![scope::READ.to_owned(), scope::DECIDE.to_owned()];
        let html = consent(&Consent {
            installation: "personal",
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
}
