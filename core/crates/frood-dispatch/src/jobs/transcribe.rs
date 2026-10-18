//! `transcribe` (docs/14 §3): the media of a source → its text, through the module that gives
//! `media.transcribe@1`. The transcript becomes the source's text and `source.transcribed`
//! starts extraction.

use base64::Engine;
use frood_ledger::model::Actor;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{JobError, JobSpec};
use crate::Ctx;
use crate::runs::Run;

pub(crate) async fn run(ctx: &Ctx, spec: &JobSpec, run: &Run) -> Result<Value, JobError> {
    let source_id: Uuid = run
        .input
        .pointer("/event/data/source_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| JobError::permanent("the event names no source"))?;
    let source = ctx.ledger.source(source_id).await?;
    let (file, data) = ctx.ledger.source_file(source_id).await?;
    let module = ctx
        .modules
        .provider("media.transcribe", spec.with_str("module"))?;
    let languages = spec
        .with
        .get("languages")
        .cloned()
        .or_else(|| source.metadata.get("language").map(|l| json!([l])))
        .unwrap_or(Value::Null);
    let input = json!({
        "audio": {
            "content_type": file.content_type,
            "filename": file.filename,
            "data_base64": base64::engine::general_purpose::STANDARD.encode(&data),
        },
        "languages": languages,
    });
    let out = ctx
        .modules
        .call(&module, "media.transcribe", &input)
        .await?;
    let text = out
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if text.is_empty() {
        return Ok(json!({ "module": module, "skipped": "nothing was heard" }));
    }
    let mut transcript = out.clone();
    if let Some(map) = transcript.as_object_mut() {
        map.remove("text");
        map.insert("module".into(), json!(module));
    }
    let actor = Actor::parse("system:transcribe").expect("valid actor");
    ctx.ledger
        .set_transcript(&actor, source_id, &text, transcript)
        .await?;
    Ok(json!({ "module": module, "chars": text.chars().count(), "language": out.get("language") }))
}
