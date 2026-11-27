// SPDX-License-Identifier: AGPL-3.0-or-later

//! `GET /api/v1/search`: full-text and semantic search over the account's workspaces, fused.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use drusite_crdt::{Page, Workspace};
use drusite_model::BlockId;
use drusite_model::search::{SnippetPart, fold_words, snippet};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{TITLE_BLOCK, vector_literal};
use crate::app::AppState;
use crate::auth::{AuthError, Authenticated};
use crate::sync::store;

/// How far down each ranking reciprocal rank fusion looks, and its constant.
const DEPTH: i64 = 100;
const RRF_K: f64 = 60.0;

#[derive(Deserialize)]
pub(super) struct Params {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

const fn default_limit() -> usize {
    20
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SearchResponse {
    hits: Vec<Hit>,
    /// Whether meaning was searched too (an embedding model is configured and answered).
    semantic: bool,
    /// The embedding model's ID, when it was.
    model: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Hit {
    workspace: Uuid,
    page: Uuid,
    title: String,
    /// The block that matched best; none when it was the title.
    block: Option<BlockId>,
    snippet: Vec<SnippetPart>,
    score: f64,
    /// `"words"`, `"meaning"`, or both: which searches found it.
    matched: Vec<&'static str>,
}

#[derive(Default)]
struct Fused {
    score: f64,
    words: bool,
    meaning: bool,
}

pub(super) async fn search(
    State(state): State<AppState>,
    auth: Authenticated,
    Query(params): Query<Params>,
) -> Result<Json<SearchResponse>, AuthError> {
    let config = &state.config().search;
    if !config.enabled {
        return Err(AuthError::BadRequest(
            "search_disabled",
            "search is turned off on this server".into(),
        ));
    }
    let q = params.q.trim();
    let limit = params.limit.clamp(1, 100);
    if q.is_empty() {
        return Ok(Json(SearchResponse {
            hits: Vec::new(),
            semantic: false,
            model: None,
        }));
    }
    let internal = |e: anyhow::Error| AuthError::internal(e);

    // The query's vector first, outside any transaction.
    let mut query_vector = None;
    if let Some(embedder) = state.embedder() {
        match embedder.embed(&[q.to_owned()]).await {
            Ok(mut v) => {
                query_vector = v
                    .pop()
                    .and_then(|v| vector_literal(&v).map(|l| (l, v.len())))
            }
            Err(error) => tracing::warn!(%error, "embedding a search; searching words only"),
        }
    }

    // Both rankings as the user, so row-level security limits them to their workspaces.
    let mut tx = state
        .db()
        .as_user(auth.user)
        .await
        .map_err(|e| internal(e.into()))?;
    let words: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT workspace_id, page_id, block_id
         FROM search_blocks, websearch_to_tsquery($1::regconfig, $2) q
         WHERE tsv @@ q
         ORDER BY ts_rank_cd(tsv, q) DESC, page_id, block_id LIMIT {DEPTH}"
    )))
    .bind(&config.text_config)
    .bind(q)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| internal(e.into()))?;
    let mut meaning: Vec<(Uuid, Uuid, Uuid)> = Vec::new();
    let model = state.embedder().map(|e| e.model_id().to_owned());
    // Nearest neighbours always exist; only those close enough in meaning count. Cosine
    // distance is 1 − similarity.
    let max_distance = 1.0 - config.embeddings.as_ref().map_or(0.3, |e| e.min_similarity);
    if let (Some((literal, dims)), Some(model)) = (&query_vector, &model) {
        meaning = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT workspace_id, page_id, block_id FROM search_embeddings
             WHERE model = $2 AND vector_dims(embedding) = {dims}
               AND embedding::vector({dims}) <=> $1::vector({dims}) <= $3
             ORDER BY embedding::vector({dims}) <=> $1::vector({dims}) LIMIT {DEPTH}"
        )))
        .bind(literal)
        .bind(model)
        .bind(max_distance)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| internal(e.into()))?;
    }
    tx.commit().await.map_err(|e| internal(e.into()))?;

    // Reciprocal rank fusion, then the best block of each page.
    let mut fused: HashMap<(Uuid, Uuid, Uuid), Fused> = HashMap::new();
    for (rank, key) in words.into_iter().enumerate() {
        let f = fused.entry(key).or_default();
        f.score += 1.0 / (RRF_K + rank as f64 + 1.0);
        f.words = true;
    }
    for (rank, key) in meaning.into_iter().enumerate() {
        let f = fused.entry(key).or_default();
        f.score += 1.0 / (RRF_K + rank as f64 + 1.0);
        f.meaning = true;
    }
    let mut pages: HashMap<(Uuid, Uuid), (Uuid, Fused)> = HashMap::new();
    for ((ws, page, block), f) in fused {
        match pages.get_mut(&(ws, page)) {
            Some((best, have)) => {
                have.words |= f.words;
                have.meaning |= f.meaning;
                if f.score > have.score {
                    *best = block;
                    have.score = f.score;
                }
            }
            None => {
                pages.insert((ws, page), (block, f));
            }
        }
    }
    let mut ranked: Vec<((Uuid, Uuid), (Uuid, Fused))> = pages.into_iter().collect();
    ranked.sort_by(|a, b| b.1.1.score.total_cmp(&a.1.1.score).then(a.0.cmp(&b.0)));

    // Snippets from the documents themselves. The workspaces came through row-level security
    // above, so the user may read them.
    let terms = fold_words(q);
    let mut tx = state
        .db()
        .as_user(auth.user)
        .await
        .map_err(|e| internal(e.into()))?;
    let mut titles: HashMap<Uuid, HashMap<Uuid, String>> = HashMap::new();
    let mut hits = Vec::new();
    for ((ws, page), (block, f)) in ranked {
        if hits.len() == limit {
            break;
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = titles.entry(ws) {
            let map = match store::load(&state, &mut tx, ws).await {
                Ok(Some(doc)) => Workspace::load_all(&doc.parts())
                    .and_then(|w| w.pages())
                    .map(|ps| ps.into_iter().map(|p| (*p.id.as_uuid(), p.title)).collect())
                    .unwrap_or_default(),
                _ => HashMap::new(),
            };
            slot.insert(map);
        }
        // Indexed but no longer in the tree (the indexer hasn't caught up): leave it out.
        let Some(title) = titles.get(&ws).and_then(|t| t.get(&page)).cloned() else {
            continue;
        };
        let (block_id, text) = if block == TITLE_BLOCK {
            (None, title.clone())
        } else {
            let text = match store::load(&state, &mut tx, page).await {
                Ok(Some(doc)) => Page::load_all(&doc.parts())
                    .ok()
                    .and_then(|p| p.block(BlockId::from_uuid(block)).ok().map(|b| b.text())),
                _ => None,
            };
            let Some(text) = text else { continue };
            (Some(BlockId::from_uuid(block)), text)
        };
        let mut matched = Vec::new();
        if f.words {
            matched.push("words");
        }
        if f.meaning {
            matched.push("meaning");
        }
        hits.push(Hit {
            workspace: ws,
            page,
            title,
            block: block_id,
            snippet: snippet(&text, &terms),
            score: f.score,
            matched,
        });
    }
    tx.commit().await.map_err(|e| internal(e.into()))?;
    Ok(Json(SearchResponse {
        hits,
        semantic: query_vector.is_some(),
        model: query_vector.and(model),
    }))
}
