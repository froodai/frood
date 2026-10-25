//! `frood export <dir>` (docs/16 §8): the installation's knowledge in a form that does not depend
//! on the database — one JSONL file per kind of record, attached files as files, the format
//! version, and a readable Markdown page per entity.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use frood_store::sqlx::{self, PgPool, Row};
#[cfg(test)]
use serde_json::Value;
use serde_json::json;

/// Version of the export format; a breaking change makes it 2.
const FORMAT_VERSION: u32 = 1;

/// One table as JSON lines, in a stable order. The SQL is written out per table: no SQL is
/// built from strings at run time.
macro_rules! rows_of {
    ($table:literal, $order:literal) => {
        concat!(
            "SELECT row_to_json(t)::text FROM ",
            $table,
            " t ORDER BY ",
            $order
        )
    };
}

struct Table {
    file: &'static str,
    query: &'static str,
}

const TABLES: &[Table] = &[
    Table {
        file: "journal.jsonl",
        query: rows_of!("journal", "position"),
    },
    Table {
        file: "sources.jsonl",
        query: rows_of!("sources", "captured_at, id"),
    },
    Table {
        file: "evidence.jsonl",
        query: rows_of!("evidence", "created_at, id"),
    },
    Table {
        file: "observations.jsonl",
        query: rows_of!("observations", "created_at, id"),
    },
    Table {
        file: "entities.jsonl",
        query: rows_of!("entities", "created_at, id"),
    },
    Table {
        file: "proposals.jsonl",
        query: rows_of!("proposals", "created_at, id"),
    },
    Table {
        file: "verdicts.jsonl",
        query: rows_of!("verdicts", "created_at, id"),
    },
    Table {
        file: "assertions.jsonl",
        query: rows_of!("assertions", "id"),
    },
    Table {
        file: "assertion_versions.jsonl",
        query: rows_of!("assertion_versions", "assertion_id, version"),
    },
    Table {
        file: "assertion_links.jsonl",
        query: rows_of!("assertion_links", "id"),
    },
    Table {
        file: "assertion_flags.jsonl",
        query: rows_of!("assertion_flags", "assertion_id, flag"),
    },
    Table {
        file: "config_revisions.jsonl",
        query: rows_of!("config_revisions", "id"),
    },
    Table {
        file: "attachments.jsonl",
        query: rows_of!("attachments", "added_at, id"),
    },
    // Sources connected from the console; their tokens stay sealed in the database.
    Table {
        file: "config_overlay.jsonl",
        query: rows_of!("config_overlay", "revision"),
    },
];

/// What an export holds, by file.
pub struct Exported {
    pub counts: BTreeMap<&'static str, usize>,
    pub files: usize,
    pub entities: usize,
}

/// `objects` is where files are when the database does not keep them.
pub async fn export(
    pool: &PgPool,
    objects: &frood_store::objects::Objects,
    installation: &str,
    out: &Path,
) -> anyhow::Result<Exported> {
    if out.exists() && out.read_dir()?.next().is_some() {
        anyhow::bail!("{} is not empty; export into a new folder", out.display());
    }
    std::fs::create_dir_all(out.join("files"))?;
    std::fs::create_dir_all(out.join("entities"))?;
    let mut counts = BTreeMap::new();
    for table in TABLES {
        let rows: Vec<String> = sqlx::query_scalar(table.query).fetch_all(pool).await?;
        let mut file = std::io::BufWriter::new(std::fs::File::create(out.join(table.file))?);
        for row in &rows {
            writeln!(file, "{row}")?;
        }
        file.flush()?;
        counts.insert(table.file, rows.len());
    }

    // Attached files: their bytes as files, their metadata as lines.
    let blobs = sqlx::query(
        "SELECT id, content_type, filename, size_bytes, sha256, created_at, data FROM blobs ORDER BY created_at, id",
    )
    .fetch_all(pool)
    .await?;
    let mut index = std::io::BufWriter::new(std::fs::File::create(out.join("files.jsonl"))?);
    for blob in &blobs {
        let id: uuid::Uuid = blob.try_get("id")?;
        let sha: String = blob.try_get("sha256")?;
        let data = match blob.try_get::<Option<Vec<u8>>, _>("data")? {
            Some(data) => data,
            None => objects
                .get(&sha)
                .await
                .map_err(|e| anyhow::anyhow!("file {id}: {e}"))?,
        };
        let filename: Option<String> = blob.try_get("filename")?;
        let path = format!("files/{id}");
        std::fs::write(out.join(&path), &data)?;
        let line = json!({
            "id": id,
            "path": path,
            "content_type": blob.try_get::<String, _>("content_type")?,
            "filename": filename,
            "size_bytes": blob.try_get::<i32, _>("size_bytes")?,
            "sha256": sha,
            "created_at": blob.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?,
        });
        writeln!(index, "{line}")?;
    }
    index.flush()?;

    let entities = entity_pages(pool, &out.join("entities")).await?;
    let migration: Option<i64> =
        sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success")
            .fetch_one(pool)
            .await?;
    let manifest = json!({
        "format": "frood-export",
        "version": FORMAT_VERSION,
        "installation": installation,
        "exported_at": chrono::Utc::now(),
        "schema_migration": migration,
        "frood": env!("CARGO_PKG_VERSION"),
        "files": counts,
        "attached_files": blobs.len(),
    });
    std::fs::write(
        out.join("schema-version.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    Ok(Exported {
        counts,
        files: blobs.len(),
        entities,
    })
}

/// A Markdown page per entity: what is known about it now, and what was known before.
async fn entity_pages(pool: &PgPool, dir: &Path) -> anyhow::Result<usize> {
    let entities = sqlx::query(
        "SELECT id, type, name, aliases FROM entities WHERE merged_into IS NULL ORDER BY name, id",
    )
    .fetch_all(pool)
    .await?;
    let mut written = 0;
    for entity in &entities {
        let id: uuid::Uuid = entity.try_get("id")?;
        let name: String = entity.try_get("name")?;
        let kind: String = entity.try_get("type")?;
        let aliases: Vec<String> = entity.try_get("aliases")?;
        let versions = sqlx::query(
            "SELECT v.assertion_id, v.version, v.kind, v.text, v.status, v.valid_from, v.valid_to, v.recorded_to \
             FROM assertion_versions v WHERE $1 = ANY(v.subjects) ORDER BY v.assertion_id, v.version",
        )
        .bind(id)
        .fetch_all(pool)
        .await?;
        let mut current = Vec::new();
        let mut earlier = Vec::new();
        for v in &versions {
            let text: String = v.try_get("text")?;
            let kind: String = v.try_get("kind")?;
            let status: String = v.try_get("status")?;
            let from: chrono::DateTime<chrono::Utc> = v.try_get("valid_from")?;
            let recorded_to: Option<chrono::DateTime<chrono::Utc>> = v.try_get("recorded_to")?;
            let line = format!(
                "- **{kind}**: {text} (since {}, {status})",
                from.format("%Y-%m-%d")
            );
            if recorded_to.is_none() && status == "active" {
                current.push(line);
            } else {
                earlier.push(line);
            }
        }
        let mut page = format!("# {name}\n\n{kind}, id `{id}`\n");
        if !aliases.is_empty() {
            page.push_str(&format!("\nAlso called: {}\n", aliases.join(", ")));
        }
        page.push_str("\n## Now\n\n");
        page.push_str(&if current.is_empty() {
            "Nothing current.".to_owned()
        } else {
            current.join("\n")
        });
        if !earlier.is_empty() {
            page.push_str("\n\n## Before\n\n");
            page.push_str(&earlier.join("\n"));
        }
        page.push('\n');
        std::fs::write(dir.join(format!("{}-{id}.md", file_name(&name))), page)
            .with_context(|| format!("cannot write the page of {name}"))?;
        written += 1;
    }
    Ok(written)
}

/// A name safe as a file name: letters, digits and dashes.
fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    let short: String = trimmed.chars().take(40).collect();
    if short.is_empty() {
        "entity".to_owned()
    } else {
        short
    }
}

/// Reads a JSONL file of an export back.
#[cfg(test)]
fn read_jsonl(path: &Path) -> anyhow::Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(Into::into))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exports_knowledge_with_its_history() {
        use frood_ledger::model::{Actor, NewProposal, NewSource, Operation, Risk};
        let Some(url) = frood_store::testing::database_url() else {
            eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
            return;
        };
        let pool = frood_store::testing::fresh_database(&url, "export")
            .await
            .expect("test database");
        let ledger = frood_ledger::Ledger::new(pool.clone(), Default::default());
        let alice = Actor::parse("user:0199f1c2-0000-7000-8000-00000000a11c").unwrap();
        let captured = ledger
            .capture_source(
                &alice,
                NewSource {
                    kind: "note".into(),
                    origin: "capture".into(),
                    uri: None,
                    title: None,
                    content: "Договорились с Олей: релиз 2.4 — 14 ноября".into(),
                    said_at: None,
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        let evidence = captured.source.evidence[0].id;
        let propose = |ops: Value, base: Value, risk: Risk| {
            let ledger = ledger.clone();
            let alice = alice.clone();
            async move {
                let operations: Vec<Operation> = serde_json::from_value(ops).unwrap();
                let input = NewProposal {
                    idempotency_key: None,
                    rationale: None,
                    risk,
                    confidence: None,
                    base: serde_json::from_value(base).unwrap(),
                    operations: Some(operations),
                    alternatives: None,
                    evidence: vec![evidence],
                };
                let p = ledger.open_proposal(&alice, input).await.unwrap();
                if serde_json::to_value(p.state).unwrap() == "open" {
                    let verdict = serde_json::from_value(json!({ "decision": "accept" })).unwrap();
                    ledger.submit_verdict(&alice, p.id, verdict).await.unwrap();
                }
            }
        };
        propose(
            json!([
                { "op": "create_entity", "ref": "olya", "type": "person", "name": "Оля" },
                { "op": "create", "assertion": { "kind": "commitment", "text": "Релиз 2.4 — 14 ноября", "modality": "agreed", "subjects": ["ref:olya"], "evidence": [evidence] } }
            ]),
            json!([]),
            Risk::New,
        )
        .await;
        let id = sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM assertions LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        let subjects: Vec<uuid::Uuid> =
            sqlx::query_scalar("SELECT subjects FROM assertion_versions WHERE assertion_id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        propose(
            json!([{ "op": "supersede", "assertion_id": id, "assertion": { "kind": "commitment", "text": "Релиз 2.4 — 28 ноября", "modality": "reported", "subjects": subjects, "evidence": [evidence] } }]),
            json!([{ "assertion_id": id, "version": 1 }]),
            Risk::Change,
        )
        .await;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("export");
        let exported = export(
            &pool,
            &frood_store::objects::Objects::Database,
            "test",
            &out,
        )
        .await
        .unwrap();
        // Bitemporal: the superseded record, its validity segment and the new version.
        assert_eq!(exported.counts["assertion_versions.jsonl"], 3);
        assert_eq!(exported.entities, 1);
        let versions = read_jsonl(&out.join("assertion_versions.jsonl")).unwrap();
        let texts: Vec<(&str, &str)> = versions
            .iter()
            .map(|v| (v["text"].as_str().unwrap(), v["status"].as_str().unwrap()))
            .collect();
        assert!(
            texts.contains(&("Релиз 2.4 — 14 ноября", "superseded")),
            "{texts:?}"
        );
        assert!(
            texts.contains(&("Релиз 2.4 — 28 ноября", "active")),
            "{texts:?}"
        );
        let sources = read_jsonl(&out.join("sources.jsonl")).unwrap();
        assert_eq!(
            sources[0]["content"],
            "Договорились с Олей: релиз 2.4 — 14 ноября"
        );
        assert!(!read_jsonl(&out.join("journal.jsonl")).unwrap().is_empty());
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(out.join("schema-version.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["format"], "frood-export");
        assert_eq!(manifest["version"], 1);
        let page = std::fs::read_dir(out.join("entities"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let page = std::fs::read_to_string(page).unwrap();
        assert!(page.starts_with("# Оля"), "{page}");
        assert!(
            page.contains("## Now\n\n- **commitment**: Релиз 2.4 — 28 ноября"),
            "{page}"
        );
        assert!(
            page.contains("## Before\n\n- **commitment**: Релиз 2.4 — 14 ноября"),
            "{page}"
        );
        // Exports go into a new folder only.
        assert!(
            export(
                &pool,
                &frood_store::objects::Objects::Database,
                "test",
                &out
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn entity_names_become_file_names() {
        assert_eq!(file_name("Оля"), "оля");
        assert_eq!(file_name("Release 2.4 / Orion"), "release-2-4---orion");
        assert_eq!(file_name("..."), "entity");
    }
}
