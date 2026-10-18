//! Questions agents ask people (docs/13 §5). An answer, or an expired question, lets the run
//! continue in the same session.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct Question {
    pub id: Uuid,
    pub run_id: Uuid,
    pub job: String,
    pub asked_for: Option<Uuid>,
    pub text: String,
    pub options: Value,
    pub context: Option<String>,
    pub status: String,
    pub answer: Option<String>,
    pub answered_by: Option<String>,
    pub answered_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub on_expire: String,
    pub created_at: DateTime<Utc>,
}

macro_rules! columns {
    () => {
        "id, run_id, job, asked_for, text, options, context, status, answer, answered_by, answered_at, expires_at, on_expire, created_at"
    };
}

fn question(row: &sqlx::postgres::PgRow) -> sqlx::Result<Question> {
    Ok(Question {
        id: row.try_get("id")?,
        run_id: row.try_get("run_id")?,
        job: row.try_get("job")?,
        asked_for: row.try_get("asked_for")?,
        text: row.try_get("text")?,
        options: row.try_get("options")?,
        context: row.try_get("context")?,
        status: row.try_get("status")?,
        answer: row.try_get("answer")?,
        answered_by: row.try_get("answered_by")?,
        answered_at: row.try_get("answered_at")?,
        expires_at: row.try_get("expires_at")?,
        on_expire: row.try_get("on_expire")?,
        created_at: row.try_get("created_at")?,
    })
}

pub(crate) struct NewQuestion<'a> {
    pub run_id: Uuid,
    pub job: &'a str,
    pub asked_for: Option<Uuid>,
    pub text: &'a str,
    pub options: &'a [String],
    pub context: Option<&'a str>,
    pub expires_at: Option<DateTime<Utc>>,
    pub on_expire: &'a str,
    pub default_answer: Option<&'a str>,
}

pub(crate) async fn ask(pool: &PgPool, new: NewQuestion<'_>) -> sqlx::Result<Question> {
    let row = sqlx::query(concat!(
        "INSERT INTO questions (id, run_id, job, asked_for, text, options, context, status, expires_at, on_expire, default_answer) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'open', $8, $9, $10) RETURNING ",
        columns!()
    ))
    .bind(Uuid::now_v7())
    .bind(new.run_id)
    .bind(new.job)
    .bind(new.asked_for)
    .bind(new.text)
    .bind(serde_json::json!(new.options))
    .bind(new.context)
    .bind(new.expires_at)
    .bind(new.on_expire)
    .bind(new.default_answer)
    .fetch_one(pool)
    .await?;
    question(&row)
}

/// Records an answer. `None` when the question is not open any more.
pub(crate) async fn answer(
    pool: &PgPool,
    id: Uuid,
    answer: &str,
    actor: &str,
) -> sqlx::Result<Option<Question>> {
    let row = sqlx::query(concat!(
        "UPDATE questions SET status = 'answered', answer = $2, answered_by = $3, answered_at = now() \
         WHERE id = $1 AND status = 'open' RETURNING ",
        columns!()
    ))
    .bind(id)
    .bind(answer)
    .bind(actor)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(question).transpose()
}

pub async fn get(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<Question>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM questions WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(question).transpose()
}

/// Questions by status, newest first.
pub async fn list(pool: &PgPool, status: Option<&str>, limit: i64) -> sqlx::Result<Vec<Question>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM questions WHERE ($1::text IS NULL OR status = $1) ORDER BY created_at DESC LIMIT $2"
    ))
    .bind(status)
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await?;
    rows.iter().map(question).collect()
}

pub(crate) async fn for_run(pool: &PgPool, run_id: Uuid) -> sqlx::Result<Vec<Question>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM questions WHERE run_id = $1 ORDER BY created_at"
    ))
    .bind(run_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(question).collect()
}

/// Open questions whose time ran out: (question, default answer).
pub(crate) async fn overdue(pool: &PgPool) -> sqlx::Result<Vec<(Question, Option<String>)>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        ", default_answer FROM questions WHERE status = 'open' AND expires_at IS NOT NULL AND expires_at <= now()"
    ))
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| Ok((question(row)?, row.try_get("default_answer")?)))
        .collect()
}

pub(crate) async fn set_status(pool: &PgPool, id: Uuid, status: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE questions SET status = $2 WHERE id = $1 AND status = 'open'")
        .bind(id)
        .bind(status)
        .execute(pool)
        .await?;
    Ok(())
}

/// The message that resumes a run after its questions were settled.
pub(crate) fn resume_message(questions: &[Question]) -> String {
    let mut lines = vec!["Answers to your questions:".to_owned()];
    for q in questions {
        let answer = match q.status.as_str() {
            "answered" => q.answer.clone().unwrap_or_default(),
            "expired" => "(no answer in time; continue without it)".to_owned(),
            _ => "(not answered)".to_owned(),
        };
        lines.push(format!("- {}\n  {}", q.text, answer));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_become_one_message() {
        let q = |text: &str, status: &str, answer: Option<&str>| Question {
            id: Uuid::nil(),
            run_id: Uuid::nil(),
            job: "j".into(),
            asked_for: None,
            text: text.into(),
            options: Value::Null,
            context: None,
            status: status.into(),
            answer: answer.map(str::to_owned),
            answered_by: None,
            answered_at: None,
            expires_at: None,
            on_expire: "skip".into(),
            created_at: Utc::now(),
        };
        let message = resume_message(&[
            q("Decision or discussion?", "answered", Some("Decision")),
            q("Who owns it?", "expired", None),
        ]);
        assert!(message.contains("Decision or discussion?\n  Decision"));
        assert!(message.contains("no answer in time"));
    }
}
