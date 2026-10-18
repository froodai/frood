//! `embed`: embeddings of new knowledge, so search finds it by meaning and across languages.
//! It catches up on everything without an embedding, not only the version in the event.

use serde_json::{Value, json};

use super::JobError;
use crate::Ctx;

const BATCH: i64 = 64;
const BATCHES: usize = 10;

pub(crate) async fn run(ctx: &Ctx) -> Result<Value, JobError> {
    let mut total = 0;
    for _ in 0..BATCHES {
        let done = ctx.searcher.embed_missing(BATCH).await?;
        total += done;
        if done < BATCH as usize {
            break;
        }
    }
    Ok(json!({ "embedded": total, "model": ctx.searcher.embedding_model() }))
}
