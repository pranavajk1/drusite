// SPDX-License-Identifier: AGPL-3.0-or-later

//! Finding the blocks an answer can draw on: full-text and, when an embedding model is
//! configured, semantic search over one workspace, as the asker, fused.
//!
//! A question isn't a search: "what did I decide about the roof?" has words that are on every
//! page. Full-text search takes any of its words rather than all of them, and leaves out words
//! so common in this workspace that they say nothing, which works in any language without a
//! list of stop words.

use std::collections::{HashMap, HashSet};

use drusite_crdt::Page;
use drusite_model::BlockId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::app::AppState;
use crate::search::{TITLE_BLOCK, vector_literal};
use crate::sync::store;

/// How far down each ranking fusion looks, and reciprocal rank fusion's constant.
const DEPTH: i64 = 60;
const RRF_K: f64 = 60.0;
/// The most blocks taken from one page, and the most words of the question searched.
const PER_PAGE: usize = 4;
const MAX_TERMS: usize = 24;
/// A block's text is cut to this many characters.
const MAX_BLOCK_CHARS: usize = 2_000;

/// A block the answer may cite.
#[derive(Debug, Clone)]
pub struct Found {
    pub page: Uuid,
    /// None when the page matched by its title and stands in as its opening blocks.
    pub block: Option<BlockId>,
    pub title: String,
    pub text: String,
}

/// What retrieval needs to know about the workspace.
#[derive(Debug)]
pub struct Scope<'a> {
    pub workspace: Uuid,
    /// Pages that may be used, with their titles: everything in the tree, less the journal
    /// unless the workspace lets Ask read it.
    pub pages: &'a HashMap<Uuid, String>,
    /// Pages to leave out (journal entries, unless allowed).
    pub excluded: &'a [Uuid],
    /// Database rows' values, by row: a row found by them is cited with them.
    pub values: &'a HashMap<Uuid, String>,
}

/// The words of a question, as typed, for `plainto_tsquery` to normalise one by one.
fn words(q: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    q.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|w| seen.insert(w.clone()))
        .take(MAX_TERMS)
        .collect()
}

/// The full-text ranking: blocks with any of the question's telling words.
async fn by_words(
    state: &AppState,
    conn: &mut PgConnection,
    scope: &Scope<'_>,
    q: &str,
) -> anyhow::Result<Vec<(Uuid, Uuid)>> {
    let config = &state.config().search.text_config;
    let terms = words(q);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM search_blocks WHERE workspace_id = $1")
            .bind(scope.workspace)
            .fetch_one(&mut *conn)
            .await?;
    // Each word's normalised query and how many blocks have it.
    let stats: Vec<(String, i64)> = sqlx::query_as(
        "SELECT q::text, (SELECT count(*) FROM search_blocks b
                          WHERE b.workspace_id = $3 AND b.tsv @@ q)
         FROM unnest($2::text[]) AS t(word), plainto_tsquery($1::regconfig, t.word) AS q
         WHERE q::text <> ''",
    )
    .bind(config)
    .bind(&terms)
    .bind(scope.workspace)
    .fetch_all(&mut *conn)
    .await?;
    let mut stats: Vec<(String, i64)> = stats.into_iter().filter(|(_, n)| *n > 0).collect();
    if stats.is_empty() {
        return Ok(Vec::new());
    }
    // A word on more than a quarter of the blocks (and more than a handful) says nothing; if
    // every word is like that, the rarest few still count.
    let common = (total / 4).max(12);
    stats.sort_by_key(|(_, n)| *n);
    let mut telling: Vec<&str> = stats
        .iter()
        .filter(|(_, n)| *n <= common)
        .map(|(q, _)| q.as_str())
        .collect();
    if telling.is_empty() {
        telling = stats.iter().take(3).map(|(q, _)| q.as_str()).collect();
    }
    let tsquery = telling
        .iter()
        .map(|q| format!("({q})"))
        .collect::<Vec<_>>()
        .join(" | ");
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT page_id, block_id FROM search_blocks
         WHERE workspace_id = $1 AND tsv @@ $2::tsquery AND NOT (page_id = ANY($3))
         ORDER BY ts_rank(tsv, $2::tsquery, 1) DESC, page_id, block_id LIMIT {DEPTH}"
    )))
    .bind(scope.workspace)
    .bind(tsquery)
    .bind(scope.excluded)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows)
}

/// The semantic ranking: blocks close in meaning to the question.
async fn by_meaning(
    state: &AppState,
    conn: &mut PgConnection,
    scope: &Scope<'_>,
    vector: &[f32],
) -> anyhow::Result<Vec<(Uuid, Uuid)>> {
    let (Some(embedder), Some(literal)) = (state.embedder(), vector_literal(vector)) else {
        return Ok(Vec::new());
    };
    let dims = vector.len();
    let max_distance = 1.0
        - state
            .config()
            .search
            .embeddings
            .as_ref()
            .map_or(0.3, |e| e.min_similarity);
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT page_id, block_id FROM search_embeddings
         WHERE workspace_id = $4 AND model = $2 AND vector_dims(embedding) = {dims}
           AND embedding::vector({dims}) <=> $1::vector({dims}) <= $3
           AND NOT (page_id = ANY($5))
         ORDER BY embedding::vector({dims}) <=> $1::vector({dims}) LIMIT {DEPTH}"
    )))
    .bind(literal)
    .bind(embedder.model_id())
    .bind(max_distance)
    .bind(scope.workspace)
    .bind(scope.excluded)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// The blocks to answer from, best first, within `[ask] sources` and `max_context_chars`.
/// `conn` runs as the asker, so row-level security has the last word on every page read.
pub async fn find(
    state: &AppState,
    conn: &mut PgConnection,
    scope: &Scope<'_>,
    q: &str,
    vector: Option<&[f32]>,
) -> anyhow::Result<Vec<Found>> {
    let config = &state.config().ask;
    let words = by_words(state, conn, scope, q).await?;
    let meaning = match vector {
        Some(v) => by_meaning(state, conn, scope, v).await?,
        None => Vec::new(),
    };
    let mut fused: HashMap<(Uuid, Uuid), f64> = HashMap::new();
    for ranking in [words, meaning] {
        for (rank, key) in ranking.into_iter().enumerate() {
            *fused.entry(key).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    let mut ranked: Vec<((Uuid, Uuid), f64)> = fused.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut per_page: HashMap<Uuid, usize> = HashMap::new();
    let mut pages: HashMap<Uuid, Option<Page>> = HashMap::new();
    let mut taken: HashSet<(Uuid, Uuid)> = HashSet::new();
    let mut out = Vec::new();
    let mut chars = 0;
    for ((page, block), _) in ranked {
        if out.len() >= config.sources || chars >= config.max_context_chars {
            break;
        }
        // Indexed but no longer in the tree (or left out): skip.
        let Some(title) = scope.pages.get(&page) else {
            continue;
        };
        let count = per_page.entry(page).or_default();
        if *count >= PER_PAGE {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = pages.entry(page) {
            let doc = match store::load(state, conn, page).await {
                Ok(Some(doc)) if doc.workspace == scope.workspace && doc.kind == "page" => {
                    Page::load_all(&doc.parts()).ok()
                }
                _ => None,
            };
            slot.insert(doc);
        }
        let Some(Some(doc)) = pages.get(&page) else {
            continue;
        };
        let room = config.max_context_chars.saturating_sub(chars);
        if block == TITLE_BLOCK {
            // The page matched by its title: its opening blocks stand in for it, cited as the
            // first of them.
            let mut text = scope.values.get(&page).cloned().unwrap_or_default();
            let mut first = None;
            for b in doc.blocks().unwrap_or_default() {
                let t = b.text();
                if t.trim().is_empty() || !taken.insert((page, *b.id.as_uuid())) {
                    continue;
                }
                first.get_or_insert(b.id);
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t);
                if text.chars().count() >= MAX_BLOCK_CHARS {
                    break;
                }
            }
            // Its blocks are all in already (and it isn't a row with values to tell).
            if first.is_none()
                && (!scope.values.contains_key(&page) || !taken.insert((page, block)))
            {
                continue;
            }
            let text = cut(&text, MAX_BLOCK_CHARS.min(room));
            chars += text.chars().count() + title.chars().count();
            *count += 1;
            out.push(Found {
                page,
                block: first,
                title: title.clone(),
                text,
            });
            continue;
        }
        let id = BlockId::from_uuid(block);
        let Ok(b) = doc.block(id) else { continue };
        let text = b.text();
        if text.trim().is_empty() || !taken.insert((page, block)) {
            continue;
        }
        let text = cut(&text, MAX_BLOCK_CHARS.min(room));
        chars += text.chars().count();
        *count += 1;
        out.push(Found {
            page,
            block: Some(id),
            title: title.clone(),
            text,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_questions_words_in_any_script() {
        assert_eq!(
            words("What did I decide about the roof? The ROOF!"),
            ["what", "did", "decide", "about", "the", "roof"]
        );
        assert_eq!(
            words("Où est le café ? 屋根"),
            ["où", "est", "le", "café", "屋根"]
        );
    }
}
