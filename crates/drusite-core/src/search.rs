// SPDX-License-Identifier: AGPL-3.0-or-later

//! Full-text search on the device, with tantivy.
//!
//! The index lives in memory only: written to disk it would be the notes in plain text next to
//! the encrypted store. It is built from the store the first time someone searches, and after
//! that only the pages that changed (here or through sync) are indexed again, just before the
//! next search. One tantivy document per block, plus one per page for its title, so a hit can
//! point at the block it came from.

use std::collections::{HashMap, HashSet};

use drusite_model::{BlockId, BlockKind, PageId};

use drusite_model::search::{fold_words, snippet};

use crate::api::{ApiError, ErrorCode, SearchHit};
use crate::engine::Core;

#[cfg(any(target_family = "wasm", test))]
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
mod memory;
#[cfg(not(target_family = "wasm"))]
mod tantivy_index;

#[cfg(target_family = "wasm")]
use memory::Index;
#[cfg(not(target_family = "wasm"))]
use tantivy_index::Index;

/// One document the index found: a block, a page's title (no block, no text), or a row's values
/// (no block, the values as text).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Found {
    pub(crate) page: PageId,
    pub(crate) block: Option<BlockId>,
    pub(crate) text: Option<String>,
}

/// The search state the core keeps: the index once built, and what changed since.
#[derive(Default)]
pub(crate) struct Search {
    built: Option<Built>,
    dirty: HashSet<PageId>,
    titles_changed: bool,
}

struct Built {
    index: Index,
    /// The titles indexed, to see which changed.
    titles: HashMap<PageId, String>,
}

impl Search {
    /// A page's blocks changed (or it arrived from the server).
    pub(crate) fn page_changed(&mut self, page: PageId) {
        if self.built.is_some() {
            self.dirty.insert(page);
        }
    }

    /// The workspace changed: titles, pages added, moved or deleted.
    pub(crate) fn workspace_changed(&mut self) {
        self.titles_changed = true;
    }

    /// Forget everything (another workspace was adopted).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

fn search_error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, format!("search index: {e}"))
}

impl Core {
    /// Brings the index up to date: built the first time, then only what changed.
    fn refresh_index(&mut self) -> Result<(), ApiError> {
        let pages = self.workspace.pages()?;
        // A row's values count as part of its title here: when they change, it's indexed again.
        let values = self.row_values_text(&pages);
        let titles: HashMap<PageId, String> = pages
            .iter()
            .map(|p| {
                let v = values.get(&p.id).map_or("", String::as_str);
                (p.id, format!("{}\u{1}{v}", p.title))
            })
            .collect();
        let (mut built, mut todo) = match self.search.built.take() {
            Some(built) => {
                let mut todo = std::mem::take(&mut self.search.dirty);
                if std::mem::take(&mut self.search.titles_changed) {
                    // Pages added, renamed or gone.
                    for (id, title) in &titles {
                        if built.titles.get(id) != Some(title) {
                            todo.insert(*id);
                        }
                    }
                    todo.extend(built.titles.keys().filter(|id| !titles.contains_key(id)));
                }
                (built, todo)
            }
            None => {
                self.search.dirty.clear();
                self.search.titles_changed = false;
                (
                    Built {
                        index: Index::new()?,
                        titles: HashMap::new(),
                    },
                    titles.keys().copied().collect(),
                )
            }
        };
        if todo.is_empty() {
            self.search.built = Some(built);
            return Ok(());
        }
        let mut retry = HashSet::new();
        for page in todo.drain() {
            built.index.remove_page(page);
            built.titles.remove(&page);
            let Some(title) = titles.get(&page) else {
                continue; // deleted
            };
            let (shown, vals) = title.split_once('\u{1}').unwrap_or((title.as_str(), ""));
            built.index.add_title(page, shown)?;
            if !vals.is_empty() {
                built.index.add_values(page, vals)?;
            }
            built.titles.insert(page, title.clone());
            let blocks = match self.open.get(&page) {
                Some(open) => open.page.blocks(),
                None => match self.store.load_page(page) {
                    Ok((p, _)) => p.blocks(),
                    // Not here yet (still coming from the server): try again next time.
                    Err(crate::Error::UnknownDoc(_)) => {
                        retry.insert(page);
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                },
            }?;
            for block in blocks {
                if matches!(block.kind, BlockKind::Divider | BlockKind::Query { .. }) {
                    continue;
                }
                let text = block.text();
                if !text.trim().is_empty() {
                    built.index.add_block(page, block.id, &text)?;
                }
            }
        }
        built.index.commit()?;
        self.search.dirty.extend(retry);
        self.search.built = Some(built);
        Ok(())
    }

    /// Each database row's values as one line of text.
    fn row_values_text(&self, pages: &[drusite_crdt::PageNode]) -> HashMap<PageId, String> {
        let mut out = HashMap::new();
        let clock = crate::database::Clock::utc();
        let db = crate::database::compute::Db::new(&self.workspace, clock.now, clock.utc_offset);
        for p in pages
            .iter()
            .filter(|p| p.kind == drusite_model::PageKind::Database)
        {
            let Some(schema) = db.schema(p.id) else {
                continue;
            };
            for row in db.all_rows(p.id) {
                let text: Vec<String> = row
                    .cells
                    .iter()
                    .filter(|c| c.property != drusite_model::database::TITLE && !c.text.is_empty())
                    .filter_map(|c| {
                        let name = &schema.property(&c.property)?.name;
                        Some(format!("{name}: {}", c.text))
                    })
                    .collect();
                if !text.is_empty() {
                    out.insert(row.id, text.join(" · "));
                }
            }
        }
        out
    }

    /// Pages matching every word of `query`, the last word as a prefix (so results follow the
    /// typing), best first: at most one hit per page, at the block that matched best.
    pub(crate) fn search(&mut self, query: &str, limit: u32) -> Result<Vec<SearchHit>, ApiError> {
        let marked = fold_words(query);
        let words = Index::words(query);
        if words.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        self.refresh_index()?;
        let built = self
            .search
            .built
            .as_ref()
            .ok_or_else(|| search_error("not built"))?;
        // Several blocks of one page can match; look further than `limit` to fill it with pages.
        let wanted = (limit as usize).saturating_mul(8).max(64);
        let top = built.index.search(&words, wanted)?;

        let titles: HashMap<PageId, String> = self
            .workspace
            .pages()?
            .into_iter()
            .map(|p| (p.id, p.title))
            .collect();
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for found in top {
            let page = found.page;
            let Some(title) = titles.get(&page) else {
                continue; // deleted since it was indexed
            };
            if !seen.insert(page) {
                continue;
            }
            let block = found.block;
            let snippet = match &block {
                Some(_) => snippet(found.text.as_deref().unwrap_or_default(), &marked),
                // A row's values matched (they have no block), or else the title.
                None => match found.text.as_deref() {
                    Some(values) => snippet(values, &marked),
                    None => snippet(title, &marked),
                },
            };
            hits.push(SearchHit {
                page,
                title: title.clone(),
                block,
                snippet,
            });
            if hits.len() == limit as usize {
                break;
            }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::StoreKey;
    use crate::api::{Command, Edit, Response};

    fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let core =
            Core::open(dir.path(), &StoreKey::generate().unwrap(), Box::new(|_| {})).unwrap();
        (core, dir)
    }

    fn page(core: &mut Core, title: &str, lines: &[&str]) -> (PageId, Vec<BlockId>) {
        let Response::PageCreated { page } = core
            .handle(Command::CreatePage {
                parent: None,
                title: title.into(),
            })
            .unwrap()
        else {
            panic!()
        };
        let Response::Opened { view } = core.handle(Command::OpenPage { page: page.id }).unwrap()
        else {
            panic!()
        };
        let mut ids = vec![view.blocks[0].id];
        for (i, line) in lines.iter().enumerate() {
            let block = ids[i];
            let Response::Edited { outcome } = core
                .handle(Command::Edit {
                    page: page.id,
                    edits: vec![
                        Edit::InsertText {
                            block,
                            pos: 0,
                            text: (*line).into(),
                        },
                        Edit::Split {
                            block,
                            pos: line.encode_utf16().count() as u32,
                            kind: BlockKind::Paragraph,
                        },
                    ],
                })
                .unwrap()
            else {
                panic!()
            };
            ids.push(outcome.created[0]);
        }
        core.handle(Command::ClosePage { page: page.id }).unwrap();
        (page.id, ids)
    }

    fn search(core: &mut Core, q: &str) -> Vec<SearchHit> {
        match core
            .handle(Command::Search {
                query: q.into(),
                limit: 10,
            })
            .unwrap()
        {
            Response::SearchResults { hits } => hits,
            other => panic!("{other:?}"),
        }
    }

    fn shown(hit: &SearchHit) -> String {
        hit.snippet
            .iter()
            .map(|p| {
                if p.highlight {
                    format!("[{}]", p.text)
                } else {
                    p.text.clone()
                }
            })
            .collect()
    }

    #[test]
    fn finds_blocks_and_titles_and_follows_changes() {
        let (mut core, _dir) = core();
        let (walks, blocks) = page(
            &mut core,
            "Walks",
            &["A long walk by the river", "Coffee at Café Noir"],
        );
        let (recipes, _) = page(&mut core, "Recipes", &["River trout with lemon"]);

        let hits = search(&mut core, "walk river");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page, walks);
        assert_eq!(hits[0].block, Some(blocks[0]));
        assert_eq!(shown(&hits[0]), "A long [walk] by the [river]");

        // Accents fold, case doesn't matter, and the last word is a prefix.
        let hits = search(&mut core, "cafe no");
        assert_eq!(hits[0].block, Some(blocks[1]));
        assert_eq!(shown(&hits[0]), "Coffee at [Café] [Noir]");

        // Both pages mention the river.
        let pages: HashSet<_> = search(&mut core, "river").iter().map(|h| h.page).collect();
        assert_eq!(pages, HashSet::from([walks, recipes]));

        // A title match points at the page, not a block.
        let hits = search(&mut core, "recip");
        assert_eq!((hits[0].page, hits[0].block), (recipes, None));

        // Edits, renames and deletes show up in the next search.
        core.handle(Command::OpenPage { page: walks }).unwrap();
        core.handle(Command::Edit {
            page: walks,
            edits: vec![Edit::InsertText {
                block: blocks[2],
                pos: 0,
                text: "Saw a heron".into(),
            }],
        })
        .unwrap();
        assert_eq!(search(&mut core, "heron")[0].block, Some(blocks[2]));
        core.handle(Command::RenamePage {
            page: recipes,
            title: "Cooking".into(),
        })
        .unwrap();
        assert!(search(&mut core, "recipes").is_empty());
        assert_eq!(search(&mut core, "cooking")[0].page, recipes);
        core.handle(Command::DeletePage { page: recipes }).unwrap();
        assert!(search(&mut core, "trout").is_empty());
        assert!(search(&mut core, "  ").is_empty());
    }
}
