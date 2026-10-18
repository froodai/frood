//! Search (docs/14 §8): the Ledger does words and vector distance; this crate supplies the
//! embeddings, for queries and for new knowledge, through the LLM gateway.

use frood_ledger::{Ledger, LedgerError, SearchHit, SearchQuery};
use frood_llm::{Gateway, LlmError};

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Llm(#[from] LlmError),
}

#[derive(Clone)]
struct EmbeddingSettings {
    account: String,
    model: String,
}

#[derive(Clone)]
pub struct Searcher {
    ledger: Ledger,
    gateway: Gateway,
    embeddings: Option<EmbeddingSettings>,
}

impl Searcher {
    pub fn new(ledger: Ledger, gateway: Gateway, search: &frood_config::Search) -> Self {
        let embeddings = search.embeddings.as_ref().map(|e| EmbeddingSettings {
            account: e.account.clone(),
            model: e.model.clone(),
        });
        Self {
            ledger,
            gateway,
            embeddings,
        }
    }

    /// The embedding model, when search by meaning is on.
    pub fn embedding_model(&self) -> Option<&str> {
        self.embeddings.as_ref().map(|e| e.model.as_str())
    }

    /// Hybrid search. If the query cannot be embedded (provider down), it still matches words.
    pub async fn search(&self, mut query: SearchQuery) -> Result<Vec<SearchHit>, SearchError> {
        if query.embedding.is_none()
            && let Some(settings) = &self.embeddings
            && !query.text.trim().is_empty()
        {
            match self
                .gateway
                .embed(
                    &settings.account,
                    &settings.model,
                    std::slice::from_ref(&query.text),
                )
                .await
            {
                Ok(mut out) => query.embedding = out.vectors.pop().map(|v| (out.model, v)),
                Err(err) => {
                    tracing::warn!(error = %err, "query embedding failed; searching words only")
                }
            }
        }
        Ok(self.ledger.search(&query).await?)
    }

    /// Embeds up to `batch` current versions that have no embedding yet. Returns how many.
    pub async fn embed_missing(&self, batch: i64) -> Result<usize, SearchError> {
        let Some(settings) = &self.embeddings else {
            return Ok(0);
        };
        let missing = self
            .ledger
            .missing_embeddings(&settings.model, batch)
            .await?;
        if missing.is_empty() {
            return Ok(0);
        }
        let texts: Vec<String> = missing.iter().map(|(_, _, text)| text.clone()).collect();
        let out = self
            .gateway
            .embed(&settings.account, &settings.model, &texts)
            .await?;
        let items: Vec<_> = missing
            .iter()
            .map(|(id, version, _)| (*id, *version))
            .zip(out.vectors)
            .collect();
        self.ledger
            .store_embeddings(&settings.model, &items)
            .await?;
        Ok(items.len())
    }
}
