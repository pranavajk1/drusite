// SPDX-License-Identifier: AGPL-3.0-or-later

//! The indexer: takes documents from `search_queue` a moment after they change and brings their
//! rows in `search_blocks` and `search_embeddings` up to date.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::Duration;

use drusite_crdt::{Page, Workspace};
use drusite_model::{BlockKind, WorkspaceId};
use uuid::Uuid;

use super::{TITLE_BLOCK, vector_literal};
use crate::app::AppState;
use crate::sync::store;

/// How long a document waits after its last change before it's indexed.
const SETTLE: &str = "2 seconds";
/// Documents taken per round.
const BATCH: i64 = 8;
/// A document that keeps failing is given up after this many tries; the next sweep queues it
/// again if it still has no vectors.
const MAX_ATTEMPTS: i32 = 8;
/// How often the indexer looks for documents that aren't indexed (or have no vectors from the
/// current model), say after the embedding model was down for longer than the retries last.
const SWEEP: Duration = Duration::from_secs(10 * 60);

/// Runs the indexer until the server stops. Every node runs one; `SKIP LOCKED` shares the work.
pub fn spawn_indexer(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut swept = tokio::time::Instant::now();
        loop {
            if swept.elapsed() >= SWEEP {
                swept = tokio::time::Instant::now();
                match queue_missing(&state, false).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(documents = n, "queued again for search indexing"),
                    Err(error) => tracing::warn!(%error, "looking for documents to index"),
                }
            }
            match round(&state).await {
                Ok(0) => tokio::time::sleep(Duration::from_secs(1)).await,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "search indexer");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    })
}

/// Queues every document that isn't indexed yet (or has no vector from the current model), so
/// a server that turns search on, or changes model, catches up. Run at startup.
pub async fn backfill(state: &AppState) -> anyhow::Result<u64> {
    queue_missing(state, true).await
}

/// Queues the documents with nothing indexed or no vectors from the current model, and every
/// workspace document if `workspaces` (titles are cheap, and a page's place in the tree may have
/// changed while the server was down).
async fn queue_missing(state: &AppState, workspaces: bool) -> anyhow::Result<u64> {
    if !state.config().search.enabled {
        return Ok(0);
    }
    let model = state.embedder().map(|e| e.model_id().to_owned());
    let mut tx = state.db().as_system().await?;
    let queued = sqlx::query(
        "INSERT INTO search_queue (doc_id, workspace_id)
         SELECT d.id, d.workspace_id FROM documents d
         WHERE ($2 AND d.kind = 'workspace')
            OR NOT EXISTS (SELECT 1 FROM search_blocks b
                           WHERE b.workspace_id = d.workspace_id AND b.page_id = d.id)
            OR ($1::text IS NOT NULL AND NOT EXISTS (
                SELECT 1 FROM search_embeddings e
                WHERE e.workspace_id = d.workspace_id AND e.page_id = d.id AND e.model = $1))
         ON CONFLICT (doc_id) DO NOTHING",
    )
    .bind(model)
    .bind(workspaces)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(queued)
}

/// When the embedding model changed: drops the vectors (and vector indexes) of every other
/// model. They can't be compared with the new model's, so they'd only take room and reveal what
/// the notes were about. The new model's are made in the background ([`backfill`]); until it's
/// done, semantic search finds less and full-text search carries on. Run at startup.
pub async fn forget_other_models(state: &AppState) -> anyhow::Result<u64> {
    let Some(embedder) = state.embedder() else {
        return Ok(0);
    };
    let model = embedder.model_id();
    let mut tx = state.db().as_system().await?;
    let gone = sqlx::query("DELETE FROM search_embeddings WHERE model <> $1")
        .bind(model)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    let indexes: Vec<(String, String)> = sqlx::query_as(
        "SELECT indexname, indexdef FROM pg_indexes
         WHERE tablename = 'search_embeddings' AND indexname LIKE 'search_embeddings_hnsw_%'",
    )
    .fetch_all(state.db().pool())
    .await?;
    let ours = format!("(model = {}::text)", quote_literal(model));
    for (name, definition) in indexes {
        if !definition.contains(&ours)
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP INDEX IF EXISTS {name}")))
                .execute(state.db().pool())
                .await?;
        }
    }
    Ok(gone)
}

/// Takes the documents that are due and indexes them. Returns how many it took.
async fn round(state: &AppState) -> anyhow::Result<usize> {
    // Taken and committed at once, so a push re-queueing a document never waits on indexing.
    let mut tx = state.db().as_system().await?;
    let due: Vec<(Uuid, i32)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "DELETE FROM search_queue WHERE doc_id IN (
             SELECT doc_id FROM search_queue WHERE queued_at <= now() - interval '{SETTLE}'
             ORDER BY queued_at LIMIT {BATCH} FOR UPDATE SKIP LOCKED)
         RETURNING doc_id, attempts"
    )))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    if !state.config().search.enabled {
        return Ok(due.len());
    }
    for (doc, attempts) in &due {
        if let Err(error) = index_doc(state, *doc).await {
            let attempts = attempts + 1;
            if attempts >= MAX_ATTEMPTS {
                tracing::error!(%doc, %error, "giving up indexing a document");
                continue;
            }
            tracing::warn!(%doc, %error, attempts, "indexing a document; will retry");
            let backoff = i64::from(attempts * attempts * 10);
            let mut tx = state.db().as_system().await?;
            // A newer change may have queued it again meanwhile; that one stands.
            sqlx::query(
                "INSERT INTO search_queue (doc_id, workspace_id, queued_at, attempts)
                 SELECT id, workspace_id, now() + make_interval(secs => $2), $3
                 FROM documents WHERE id = $1
                 ON CONFLICT (doc_id) DO NOTHING",
            )
            .bind(doc)
            .bind(backoff as f64)
            .bind(attempts)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
    }
    Ok(due.len())
}

/// The rows a document contributes: (page, block, text).
fn rows_of(doc: &store::OpenedDoc, id: Uuid) -> anyhow::Result<Vec<(Uuid, Uuid, String)>> {
    let parts = doc.parts();
    Ok(match doc.kind.as_str() {
        "workspace" => Workspace::load_all(&parts)?
            .pages()?
            .into_iter()
            .map(|p| (*p.id.as_uuid(), TITLE_BLOCK, p.title))
            .collect(),
        _ => Page::load_all(&parts)?
            .blocks()?
            .into_iter()
            .filter(|b| !matches!(b.kind, BlockKind::Divider | BlockKind::Query { .. }))
            .map(|b| (id, *b.id.as_uuid(), b.text()))
            .filter(|(_, _, t)| !t.trim().is_empty())
            .collect(),
    })
}

type Key = (Uuid, Uuid);

async fn index_doc(state: &AppState, doc: Uuid) -> anyhow::Result<()> {
    let config = &state.config().search;
    let mut tx = state.db().as_system().await?;
    let Some(opened) = store::load(state, &mut tx, doc).await? else {
        return Ok(()); // deleted
    };
    let ws = opened.workspace;
    let titles = opened.kind == "workspace";
    let rows = rows_of(&opened, doc)?;
    let key = state
        .keys()
        .current(&mut tx, WorkspaceId::from_uuid(ws))
        .await?;
    let hashing = key.derive("drusite 2026 search text hash");
    let hash = |text: &str| {
        blake3::keyed_hash(&hashing, text.as_bytes())
            .as_bytes()
            .to_vec()
    };

    // What's indexed now: titles (for the workspace document) or this page's blocks.
    let existing: Vec<(Uuid, Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT page_id, block_id, content_hash FROM search_blocks
         WHERE workspace_id = $1 AND (CASE WHEN $2 THEN block_id = $3
                                          ELSE page_id = $4 AND block_id <> $3 END)",
    )
    .bind(ws)
    .bind(titles)
    .bind(TITLE_BLOCK)
    .bind(doc)
    .fetch_all(&mut *tx)
    .await?;
    let existing: HashMap<Key, Vec<u8>> =
        existing.into_iter().map(|(p, b, h)| ((p, b), h)).collect();
    let mut wanted: HashMap<Key, (String, Vec<u8>)> = HashMap::new();
    for (page, block, text) in rows {
        let h = hash(&text);
        wanted.insert((page, block), (text, h));
    }

    for ((page, block), (text, h)) in &wanted {
        if existing.get(&(*page, *block)) == Some(h) {
            continue;
        }
        sqlx::query(
            "INSERT INTO search_blocks (workspace_id, page_id, block_id, content_hash, config, tsv)
             VALUES ($1, $2, $3, $4, $5::regconfig, to_tsvector($5::regconfig, $6))
             ON CONFLICT (workspace_id, page_id, block_id) DO UPDATE
                 SET content_hash = excluded.content_hash, config = excluded.config,
                     tsv = excluded.tsv, updated_at = now()",
        )
        .bind(ws)
        .bind(page)
        .bind(block)
        .bind(h)
        .bind(&config.text_config)
        .bind(text)
        .execute(&mut *tx)
        .await?;
    }
    let gone: Vec<Key> = existing
        .keys()
        .filter(|k| !wanted.contains_key(k))
        .copied()
        .collect();
    if !gone.is_empty() {
        let (pages, blocks): (Vec<Uuid>, Vec<Uuid>) = gone.iter().copied().unzip();
        for table in ["search_blocks", "search_embeddings"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE workspace_id = $1
                 AND (page_id, block_id) IN (SELECT * FROM unnest($2::uuid[], $3::uuid[]))"
            )))
            .bind(ws)
            .bind(&pages)
            .bind(&blocks)
            .execute(&mut *tx)
            .await?;
        }
    }
    if titles {
        // Pages back in the tree (restored from the trash) need their blocks indexed again.
        let back: Vec<Uuid> = wanted
            .keys()
            .filter(|k| !existing.contains_key(k))
            .map(|(p, _)| *p)
            .collect();
        sqlx::query(
            "INSERT INTO search_queue (doc_id, workspace_id)
             SELECT id, workspace_id FROM documents WHERE id = ANY($1) AND workspace_id = $2
             ON CONFLICT (doc_id) DO UPDATE SET queued_at = now(), attempts = 0",
        )
        .bind(&back)
        .bind(ws)
        .execute(&mut *tx)
        .await?;
        // Pages no longer in the tree (deleted, or in the trash): everything indexed for them
        // goes.
        let live: Vec<Uuid> = wanted.keys().map(|(p, _)| *p).collect();
        for table in ["search_blocks", "search_embeddings"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE workspace_id = $1 AND NOT (page_id = ANY($2))"
            )))
            .bind(ws)
            .bind(&live)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;

    if let Some(embedder) = state.embedder() {
        // Vectors another model made for these blocks (another node, mid-rollout, or before a
        // restart dropped them all) can't be compared with this model's.
        let mut tx = state.db().as_system().await?;
        sqlx::query(
            "DELETE FROM search_embeddings WHERE workspace_id = $1 AND model <> $5
               AND (CASE WHEN $2 THEN block_id = $3 ELSE page_id = $4 AND block_id <> $3 END)",
        )
        .bind(ws)
        .bind(titles)
        .bind(TITLE_BLOCK)
        .bind(doc)
        .bind(embedder.model_id())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        embed(state, embedder, ws, doc, titles, &wanted).await?;
    }
    Ok(())
}

/// Embeds the rows whose text has no vector from the current model yet.
async fn embed(
    state: &AppState,
    embedder: &super::Embedder,
    ws: Uuid,
    doc: Uuid,
    titles: bool,
    wanted: &HashMap<Key, (String, Vec<u8>)>,
) -> anyhow::Result<()> {
    let model = embedder.model_id().to_owned();
    let mut tx = state.db().as_system().await?;
    let have: Vec<(Uuid, Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT page_id, block_id, content_hash FROM search_embeddings
         WHERE workspace_id = $1 AND model = $5
           AND (CASE WHEN $2 THEN block_id = $3 ELSE page_id = $4 AND block_id <> $3 END)",
    )
    .bind(ws)
    .bind(titles)
    .bind(TITLE_BLOCK)
    .bind(doc)
    .bind(&model)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let have: HashSet<(Uuid, Uuid, Vec<u8>)> = have.into_iter().collect();
    let todo: Vec<(&Key, &(String, Vec<u8>))> = wanted
        .iter()
        .filter(|((p, b), (_, h))| !have.contains(&(*p, *b, h.clone())))
        .collect();
    let batch = state
        .config()
        .search
        .embeddings
        .as_ref()
        .map_or(64, |e| e.batch);
    for chunk in todo.chunks(batch) {
        let texts: Vec<String> = chunk.iter().map(|(_, (t, _))| t.clone()).collect();
        let call = crate::ask::audit::Call {
            workspace: Some(ws),
            user: None,
            feature: "index",
            provider: embedder.provider(),
            model: &model,
            blocks: serde_json::json!([doc]),
            items: texts.len(),
            chars: texts.iter().map(|t| t.chars().count()).sum(),
        };
        let vectors = match embedder.embed(&texts).await {
            Ok(v) => {
                crate::ask::audit::record(state, &call, "ok").await;
                v
            }
            Err(e) => {
                crate::ask::audit::record(state, &call, crate::ask::audit::outcome_of(&e)).await;
                return Err(e.into());
            }
        };
        if let Some(first) = vectors.first() {
            ensure_ann_index(state, embedder, &model, first.len()).await;
        }
        let mut tx = state.db().as_system().await?;
        for (((page, block), (_, h)), v) in chunk.iter().zip(vectors) {
            let Some(literal) = vector_literal(&v) else {
                tracing::warn!(%page, %block, "the embedding model returned a vector that isn't finite");
                continue;
            };
            sqlx::query(
                "INSERT INTO search_embeddings
                     (workspace_id, page_id, block_id, model, content_hash, embedding)
                 VALUES ($1, $2, $3, $4, $5, $6::vector)
                 ON CONFLICT (workspace_id, page_id, block_id, model) DO UPDATE
                     SET content_hash = excluded.content_hash, embedding = excluded.embedding,
                         updated_at = now()",
            )
            .bind(ws)
            .bind(page)
            .bind(block)
            .bind(&model)
            .bind(h)
            .bind(literal)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// An HNSW index for this model's vectors. pgvector indexes vectors of one size, so it's a
/// partial index per model on the vectors cast to that size; searches use the same expression.
async fn ensure_ann_index(state: &AppState, embedder: &super::Embedder, model: &str, dims: usize) {
    if embedder.ann_index.load(Ordering::Relaxed) || dims == 0 || dims > 2000 {
        // pgvector's HNSW takes up to 2,000 dimensions; beyond that, searches scan.
        return;
    }
    let name = format!(
        "search_embeddings_hnsw_{}",
        &blake3::hash(format!("{model}/{dims}").as_bytes()).to_hex()[..12]
    );
    let sql = format!(
        "CREATE INDEX IF NOT EXISTS {name} ON search_embeddings
         USING hnsw ((embedding::vector({dims})) vector_cosine_ops) WHERE model = {}",
        quote_literal(model)
    );
    match sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(state.db().pool())
        .await
    {
        Ok(_) => embedder.ann_index.store(true, Ordering::Relaxed),
        Err(error) => tracing::warn!(%error, model, "making the vector index"),
    }
}

/// A SQL string literal.
pub(super) fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
