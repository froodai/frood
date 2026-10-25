//! `frood import` (docs/27 §5): the memory assistants already keep on this computer — Claude
//! Code, Codex, OpenClaw, Hermes, a folder of Markdown — sent to Frood as notes. Frood reads
//! them like any note, takes what they say as the person's own, marked "from … memory, not
//! verified", and leaves where they disagree with what is known in the Inbox.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::lifecycle::Paths;

/// The largest file sent; Frood reads text files up to this size.
const MAX_FILE: u64 = 200 * 1024;

/// One memory file and where it comes from.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The assistant, as the mark says it: "Claude Code", "Codex", …
    pub from: &'static str,
    /// What it is about, when the assistant keeps memory per project.
    pub project: Option<String>,
    pub path: PathBuf,
    pub bytes: u64,
}

pub struct Options {
    pub server: Option<String>,
    pub token: Option<String>,
    pub email: Option<String>,
    pub folders: Vec<PathBuf>,
    pub yes: bool,
}

/// Memory files in the places assistants keep them, under `home`, and in `folders`.
pub fn find(home: &Path, folders: &[PathBuf]) -> Vec<Found> {
    let mut out = Vec::new();
    // Claude Code: one memory folder per project, named after the project's path.
    if let Ok(projects) = std::fs::read_dir(home.join(".claude/projects")) {
        let mut projects: Vec<PathBuf> = projects.flatten().map(|e| e.path()).collect();
        projects.sort();
        for project in projects {
            let name = project
                .file_name()
                .map(|n| {
                    n.to_string_lossy()
                        .rsplit('-')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .filter(|n| !n.is_empty());
            for path in markdown_in(&project.join("memory"), false) {
                out.extend(file("Claude Code", name.clone(), path));
            }
        }
    }
    for path in markdown_in(&home.join(".codex/memories"), false) {
        out.extend(file("Codex", None, path));
    }
    for (from, folder) in [("OpenClaw", ".openclaw/workspace"), ("Hermes", ".hermes")] {
        for name in ["MEMORY.md", "USER.md"] {
            out.extend(file(from, None, home.join(folder).join(name)));
        }
    }
    for folder in folders {
        let label = folder.file_name().map(|n| n.to_string_lossy().into_owned());
        for path in markdown_in(folder, true) {
            out.extend(file("Markdown", label.clone(), path));
        }
    }
    out
}

fn file(from: &'static str, project: Option<String>, path: PathBuf) -> Option<Found> {
    let bytes = std::fs::metadata(&path).ok().filter(|m| m.is_file())?.len();
    (bytes > 0 && bytes <= MAX_FILE).then_some(Found {
        from,
        project,
        path,
        bytes,
    })
}

fn markdown_in(folder: &Path, deep: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let hidden = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if hidden {
            continue;
        }
        if path.is_dir() {
            if deep {
                out.extend(markdown_in(&path, true));
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// "Claude Code — 9 projects, 41 files", one line for each assistant.
pub fn summary(found: &[Found]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for f in found {
        if !seen.contains(&f.from) {
            seen.push(f.from);
        }
    }
    for from in seen {
        let mine: Vec<&Found> = found.iter().filter(|f| f.from == from).collect();
        let mut projects: Vec<&str> = mine.iter().filter_map(|f| f.project.as_deref()).collect();
        projects.sort_unstable();
        projects.dedup();
        let files = mine.len();
        let files = if files == 1 {
            "1 file".to_owned()
        } else {
            format!("{files} files")
        };
        lines.push(match projects.len() {
            0 => format!("{from} — {files}"),
            1 => format!("{from} — {}, {files}", projects[0]),
            n => format!("{from} — {n} projects, {files}"),
        });
    }
    lines
}

pub async fn run(paths: &Paths, options: &Options) -> anyhow::Result<()> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let found = find(&home, &options.folders);
    if found.is_empty() {
        println!(
            "No assistant memory found on this computer (Claude Code, Codex, OpenClaw, Hermes). \
             A folder of Markdown: frood import --folder <path>."
        );
        return Ok(());
    }
    println!("Found:");
    for line in summary(&found) {
        println!("  {line}");
    }
    let bytes: u64 = found.iter().map(|f| f.bytes).sum();
    // About four characters to a token; reading takes the text once and a short answer.
    println!(
        "Reading it takes about {} thousand tokens of the model that reads notes.",
        (bytes / 4).div_ceil(1000).max(1)
    );
    if !options.yes && !confirm("Send it to Frood?")? {
        return Ok(());
    }

    let (server, token) = connection(paths, options).await?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut sent = 0;
    let mut known = 0;
    for f in &found {
        let text = std::fs::read_to_string(&f.path)
            .with_context(|| format!("cannot read {}", f.path.display()))?;
        let title = match &f.project {
            Some(project) => format!("{} memory · {project} · {}", f.from, name_of(&f.path)),
            None => format!("{} memory · {}", f.from, name_of(&f.path)),
        };
        let body = json!({
            "kind": "memory",
            "origin": "capture",
            "title": title,
            "content": text,
            "metadata": { "imported_from": f.from, "project": f.project, "file": f.path.display().to_string() },
        });
        let response = http
            .post(format!("{server}/v1/sources"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("{server} is not reachable"))?;
        match response.status().as_u16() {
            201 => sent += 1,
            200 => known += 1,
            _ => {
                let detail: Value = response.json().await.unwrap_or(Value::Null);
                bail!(
                    "Frood refused {}: {}",
                    f.path.display(),
                    detail["detail"].as_str().unwrap_or("unknown error")
                );
            }
        }
    }
    println!(
        "Sent {sent} file(s){}. Frood reads them now: the knowledge shows up within minutes, \
         marked \"not verified\"; where the memories disagree with each other or with what is \
         known, it waits in the Inbox.",
        if known > 0 {
            format!(", {known} were sent before")
        } else {
            String::new()
        }
    );
    Ok(())
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    print!("{question} [Y/n]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(!line.trim().to_lowercase().starts_with('n'))
}

/// Where to send, with what token: given, or this installation's (a token made for the import,
/// valid for a day).
async fn connection(paths: &Paths, options: &Options) -> anyhow::Result<(String, String)> {
    if let (Some(server), Some(token)) = (&options.server, &options.token) {
        return Ok((server.trim_end_matches('/').to_owned(), token.clone()));
    }
    if options.server.is_some() {
        bail!("a server needs a token: --token, or FROOD_TOKEN (frood token create on the server)");
    }
    let loaded = paths
        .load()
        .context("run it in the folder of the installation, or pass --server and --token")?;
    let server = match &loaded.config.server.public_url {
        Some(url) => url.trim_end_matches('/').to_owned(),
        None => {
            let port = loaded
                .config
                .server
                .listen
                .rsplit(':')
                .next()
                .unwrap_or("8080");
            format!("http://127.0.0.1:{port}")
        }
    };
    if let Some(token) = &options.token {
        return Ok((server, token.clone()));
    }
    let email = match (&options.email, loaded.config.auth.allow.as_slice()) {
        (Some(email), _) => email.clone(),
        (None, [only]) => only.clone(),
        _ => bail!("whose memory is it? --email you@example.com"),
    };
    let auth = crate::token::auth(paths).await?;
    let user = auth.user_id_by_email(&email, true).await?;
    let (_, token) = auth
        .create_personal_token(user, "import", &["knowledge:capture".to_owned()], Some(1))
        .await?;
    Ok((server, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_what_assistants_remember() {
        let home = tempfile::tempdir().unwrap();
        let put = |path: &str, text: &str| {
            let path = home.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        put(
            ".claude/projects/-Users-me-work-frood/memory/MEMORY.md",
            "- index",
        );
        put(
            ".claude/projects/-Users-me-work-frood/memory/db.md",
            "Postgres 16",
        );
        put(
            ".claude/projects/-Users-me-site/memory/hosting.md",
            "on Hetzner",
        );
        put(".claude/projects/-Users-me-site/notes.md", "not memory");
        put(".codex/memories/MEMORY.md", "Postgres 17");
        put(".openclaw/workspace/USER.md", "likes tea");
        put(
            ".openclaw/workspace/memory/2026-10-01.md",
            "a daily note, left out",
        );
        put("notes/a.md", "one");
        put("notes/deep/b.md", "two");
        put("notes/c.txt", "not markdown");
        put(".codex/memories/empty.md", "");

        let found = find(home.path(), &[home.path().join("notes")]);
        let names: Vec<(&str, Option<&str>, String)> = found
            .iter()
            .map(|f| (f.from, f.project.as_deref(), name_of(&f.path)))
            .collect();
        assert_eq!(
            names,
            [
                ("Claude Code", Some("site"), "hosting.md".to_owned()),
                ("Claude Code", Some("frood"), "MEMORY.md".to_owned()),
                ("Claude Code", Some("frood"), "db.md".to_owned()),
                ("Codex", None, "MEMORY.md".to_owned()),
                ("OpenClaw", None, "USER.md".to_owned()),
                ("Markdown", Some("notes"), "a.md".to_owned()),
                ("Markdown", Some("notes"), "b.md".to_owned()),
            ]
        );
        assert_eq!(
            summary(&found),
            [
                "Claude Code — 2 projects, 3 files",
                "Codex — 1 file",
                "OpenClaw — 1 file",
                "Markdown — notes, 2 files"
            ]
        );
    }
}
