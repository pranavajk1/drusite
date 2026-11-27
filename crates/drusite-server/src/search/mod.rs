// SPDX-License-Identifier: AGPL-3.0-or-later

//! Search over the live documents: Postgres full-text search and, when an embedding model is
//! configured, pgvector semantic search, combined.
//!
//! The indexer keeps `search_blocks` (each block's tsvector) and `search_embeddings` (its
//! vector, with the model's ID) up to date from the sealed documents; the notes' text itself is
//! never stored. A search runs both, fuses their rankings, and makes snippets by opening the
//! matching pages.

mod index;
pub(crate) mod query;

use std::sync::atomic::AtomicBool;

use axum::Router;
use axum::routing::get;
use drusite_ai::{AiError, EmbeddingModel, Embeddings, Provider};

use crate::app::AppState;
use crate::config::Config;

pub use index::{backfill, spawn_indexer};

/// The block ID a page's title is indexed under.
pub const TITLE_BLOCK: uuid::Uuid = uuid::Uuid::nil();

/// The embedding model, when one is configured.
#[derive(Debug)]
pub struct Embedder {
    client: EmbeddingModel,
    /// Whether this process has made sure the model's vector index exists.
    ann_index: AtomicBool,
}

impl Embedder {
    pub fn from_config(config: &Config) -> Result<Option<Self>, AiError> {
        let Some(e) = &config.search.embeddings else {
            return Ok(None);
        };
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| AiError::Network(e.to_string()))?;
        let provider = Provider::new(
            http,
            e.provider,
            e.base_url.as_ref(),
            e.api_key
                .as_ref()
                .map(|k| zeroize::Zeroizing::new(k.expose().to_owned())),
        )?;
        let client = EmbeddingModel::new(provider, e.model.clone(), e.id.clone(), e.dimensions)?;
        Ok(Some(Self {
            client,
            ann_index: AtomicBool::new(false),
        }))
    }

    pub fn model_id(&self) -> &str {
        self.client.model_id()
    }

    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AiError> {
        self.client.embed(texts).await
    }
}

/// A vector as pgvector's text input, `[1,2,3]`. `None` if it holds a NaN or an infinity.
fn vector_literal(v: &[f32]) -> Option<String> {
    if v.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let mut out = String::with_capacity(v.len() * 10 + 2);
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&x.to_string());
    }
    out.push(']');
    Some(out)
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/search", get(query::search))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_print_for_pgvector() {
        assert_eq!(
            vector_literal(&[1.0, -0.5, 3e-8]).unwrap(),
            "[1,-0.5,0.00000003]"
        );
        assert!(vector_literal(&[f32::NAN]).is_none());
    }
}
