// SPDX-License-Identifier: AGPL-3.0-or-later

//! The index in the browser: a small inverted index in memory, since tantivy's writer needs
//! threads. Same words as tantivy's (lower case, accents folded, split on anything that isn't a
//! letter or digit), same rules (every word must match, the last as a prefix), and BM25 scores,
//! as tantivy gives.

use std::collections::{BTreeMap, HashMap};

use drusite_model::search::fold_words;
use drusite_model::{BlockId, PageId};

use super::Found;
use crate::api::ApiError;

const K1: f32 = 1.2;
const B: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    Title,
    Text,
}

#[derive(Debug)]
struct Doc {
    page: PageId,
    block: Option<BlockId>,
    field: Field,
    /// What was indexed: kept for the hit's snippet, and to rebuild the postings.
    source: String,
    len: u32,
}

/// Word → (document, how often it's there).
type Postings = BTreeMap<String, Vec<(u32, u32)>>;

#[derive(Debug, Default)]
pub(crate) struct Index {
    docs: Vec<Option<Doc>>,
    by_page: HashMap<PageId, Vec<u32>>,
    terms: Postings,
    live: usize,
    /// Per field: live documents and their total length, for BM25.
    count: HashMap<Field, (u32, u64)>,
}

impl Index {
    pub(crate) fn words(text: &str) -> Vec<String> {
        fold_words(text)
    }

    pub(crate) fn new() -> Result<Self, ApiError> {
        Ok(Self::default())
    }

    pub(crate) fn remove_page(&mut self, page: PageId) {
        for id in self.by_page.remove(&page).unwrap_or_default() {
            if let Some(doc) = self.docs.get_mut(id as usize).and_then(Option::take) {
                self.live -= 1;
                let c = self.count.entry(doc.field).or_default();
                c.0 -= 1;
                c.1 -= u64::from(doc.len);
            }
        }
    }

    fn add(&mut self, page: PageId, block: Option<BlockId>, field: Field, source: &str) {
        let words = fold_words(source);
        let id = u32::try_from(self.docs.len()).unwrap_or(u32::MAX);
        let mut tf: HashMap<&str, u32> = HashMap::new();
        for w in &words {
            *tf.entry(w.as_str()).or_default() += 1;
        }
        for (w, n) in tf {
            self.terms.entry(w.to_owned()).or_default().push((id, n));
        }
        let len = u32::try_from(words.len()).unwrap_or(u32::MAX);
        self.docs.push(Some(Doc {
            page,
            block,
            field,
            source: source.to_owned(),
            len,
        }));
        self.by_page.entry(page).or_default().push(id);
        self.live += 1;
        let c = self.count.entry(field).or_default();
        c.0 += 1;
        c.1 += u64::from(len);
    }

    pub(crate) fn add_title(&mut self, page: PageId, title: &str) -> Result<(), ApiError> {
        self.add(page, None, Field::Title, title);
        Ok(())
    }

    pub(crate) fn add_values(&mut self, page: PageId, text: &str) -> Result<(), ApiError> {
        self.add(page, None, Field::Text, text);
        Ok(())
    }

    pub(crate) fn add_block(
        &mut self,
        page: PageId,
        block: BlockId,
        text: &str,
    ) -> Result<(), ApiError> {
        self.add(page, Some(block), Field::Text, text);
        Ok(())
    }

    /// Drops what removed pages left behind once it's most of the index.
    pub(crate) fn commit(&mut self) -> Result<(), ApiError> {
        if self.docs.len() > 1024 && self.docs.len() > self.live * 2 {
            let old = std::mem::take(self);
            for doc in old.docs.into_iter().flatten() {
                self.add(doc.page, doc.block, doc.field, &doc.source);
            }
        }
        Ok(())
    }

    fn bm25(&self, field: Field, tf: u32, len: u32, docs_with: usize) -> f32 {
        let (n, total) = self.count.get(&field).copied().unwrap_or_default();
        let n = n.max(1) as f32;
        let avg = (total as f32 / n).max(1.0);
        let idf = (1.0 + (n - docs_with as f32 + 0.5) / (docs_with as f32 + 0.5)).ln();
        let tf = tf as f32;
        idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len as f32 / avg))
    }

    /// Each live document with `word` (or, as a prefix, a word starting with it), and its score.
    fn matches(&self, word: &str, prefix: bool) -> HashMap<u32, f32> {
        let mut out: HashMap<u32, f32> = HashMap::new();
        let lists: Vec<&Vec<(u32, u32)>> = if prefix {
            self.terms
                .range(word.to_owned()..)
                .take_while(|(t, _)| t.starts_with(word))
                .map(|(_, l)| l)
                .collect()
        } else {
            self.terms.get(word).into_iter().collect()
        };
        for list in lists {
            let live: Vec<(&Doc, u32, u32)> = list
                .iter()
                .filter_map(|&(id, tf)| Some((self.docs.get(id as usize)?.as_ref()?, id, tf)))
                .collect();
            for (doc, id, tf) in &live {
                let score = self.bm25(doc.field, *tf, doc.len, live.len());
                let best = out.entry(*id).or_default();
                *best = best.max(score);
            }
        }
        out
    }

    pub(crate) fn search(&self, words: &[String], wanted: usize) -> Result<Vec<Found>, ApiError> {
        let last = words.len().saturating_sub(1);
        let mut scored: Option<HashMap<u32, f32>> = None;
        for (i, w) in words.iter().enumerate() {
            let found = self.matches(w, i == last);
            scored = Some(match scored {
                None => found,
                Some(so_far) => so_far
                    .into_iter()
                    .filter_map(|(id, s)| found.get(&id).map(|t| (id, s + t)))
                    .collect(),
            });
        }
        let mut ranked: Vec<(u32, f32)> = scored.unwrap_or_default().into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(ranked
            .into_iter()
            .take(wanted)
            .filter_map(|(id, _)| {
                let doc = self.docs.get(id as usize)?.as_ref()?;
                Some(Found {
                    page: doc.page,
                    block: doc.block,
                    text: (doc.field == Field::Text).then(|| doc.source.clone()),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn every_word_the_last_as_a_prefix_accents_folded() {
        let mut index = Index::new().unwrap();
        let (walks, recipes) = (PageId::new(), PageId::new());
        let (b1, b2, b3) = (BlockId::new(), BlockId::new(), BlockId::new());
        index.add_title(walks, "Walks").unwrap();
        index
            .add_block(walks, b1, "A long walk by the river")
            .unwrap();
        index.add_block(walks, b2, "Coffee at Café Noir").unwrap();
        index.add_title(recipes, "Recipes").unwrap();
        index
            .add_block(recipes, b3, "River trout with lemon")
            .unwrap();
        index.add_values(recipes, "Status: Done").unwrap();
        index.commit().unwrap();
        let find = |index: &Index, q: &str| index.search(&Index::words(q), 10).unwrap();
        let hits = find(&index, "walk river");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].block, Some(b1));
        assert_eq!(find(&index, "CAFE no")[0].block, Some(b2));
        assert_eq!(find(&index, "river").len(), 2);
        let title = &find(&index, "recip")[0];
        assert_eq!(
            (title.page, title.block, title.text.as_deref()),
            (recipes, None, None)
        );
        assert_eq!(
            find(&index, "done")[0].text.as_deref(),
            Some("Status: Done")
        );
        assert!(find(&index, "walk lemon").is_empty());

        index.remove_page(recipes);
        assert!(find(&index, "trout").is_empty());
        assert_eq!(find(&index, "river").len(), 1);
    }

    #[test]
    fn rarer_words_and_shorter_blocks_rank_first() {
        let mut index = Index::new().unwrap();
        let page = PageId::new();
        let (short, long) = (BlockId::new(), BlockId::new());
        index
            .add_block(
                page,
                long,
                "notes notes about the heron and many other birds seen",
            )
            .unwrap();
        index.add_block(page, short, "heron").unwrap();
        let hits = index.search(&Index::words("heron"), 10).unwrap();
        assert_eq!(hits[0].block, Some(short));
    }

    #[test]
    fn removed_pages_are_compacted_away() {
        let mut index = Index::new().unwrap();
        let keep = PageId::new();
        index.add_title(keep, "kept").unwrap();
        for _ in 0..3000 {
            let p = PageId::new();
            index.add_title(p, "gone").unwrap();
            index.remove_page(p);
        }
        index.commit().unwrap();
        assert_eq!(index.docs.len(), 1);
        assert_eq!(
            index.search(&Index::words("kept"), 10).unwrap()[0].page,
            keep
        );
    }
}
