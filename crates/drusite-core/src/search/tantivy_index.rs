// SPDX-License-Identifier: AGPL-3.0-or-later

//! The index on devices: tantivy, in memory.

use drusite_model::{BlockId, PageId};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, OwnedValue, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, SimpleTokenizer, TextAnalyzer};
use tantivy::{IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use super::{Found, search_error};
use crate::api::ApiError;

const TOKENIZER: &str = "drusite";
/// Memory the index writer may use before it flushes; tantivy's minimum.
const WRITER_MEMORY: usize = 15_000_000;

pub(crate) struct Index {
    writer: IndexWriter,
    reader: IndexReader,
    fields: Fields,
}

#[derive(Clone, Copy)]
struct Fields {
    page: Field,
    block: Field,
    title: Field,
    text: Field,
}

fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

impl Index {
    /// The words of `text` as the index sees them: lower case, accents folded.
    pub(crate) fn words(text: &str) -> Vec<String> {
        let mut analyzer = analyzer();
        let mut stream = analyzer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().text.clone());
        }
        out
    }

    pub(crate) fn new() -> Result<Self, ApiError> {
        let mut schema = Schema::builder();
        let indexed = TextOptions::default().set_stored().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        let fields = Fields {
            page: schema.add_text_field("page", STRING | STORED),
            block: schema.add_text_field("block", STRING | STORED),
            title: schema.add_text_field("title", indexed.clone()),
            text: schema.add_text_field("text", indexed),
        };
        let index = tantivy::Index::create_in_ram(schema.build());
        index.tokenizers().register(TOKENIZER, analyzer());
        let writer = index
            .writer_with_num_threads(1, WRITER_MEMORY)
            .map_err(search_error)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(search_error)?;
        Ok(Self {
            writer,
            reader,
            fields,
        })
    }

    pub(crate) fn remove_page(&mut self, page: PageId) {
        self.writer
            .delete_term(Term::from_field_text(self.fields.page, &page.to_string()));
    }

    pub(crate) fn add_title(&mut self, page: PageId, title: &str) -> Result<(), ApiError> {
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.page, page.to_string());
        doc.add_text(self.fields.title, title);
        self.writer.add_document(doc).map_err(search_error)?;
        Ok(())
    }

    /// A database row's values ("Status: Done · Priority: High"), found like its text.
    pub(crate) fn add_values(&mut self, page: PageId, text: &str) -> Result<(), ApiError> {
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.page, page.to_string());
        doc.add_text(self.fields.text, text);
        self.writer.add_document(doc).map_err(search_error)?;
        Ok(())
    }

    pub(crate) fn add_block(
        &mut self,
        page: PageId,
        block: BlockId,
        text: &str,
    ) -> Result<(), ApiError> {
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.page, page.to_string());
        doc.add_text(self.fields.block, block.to_string());
        doc.add_text(self.fields.text, text);
        self.writer.add_document(doc).map_err(search_error)?;
        Ok(())
    }

    pub(crate) fn commit(&mut self) -> Result<(), ApiError> {
        self.writer.commit().map_err(search_error)?;
        self.reader.reload().map_err(search_error)?;
        Ok(())
    }

    /// Documents with every word (the last as a prefix), best first.
    pub(crate) fn search(&self, words: &[String], wanted: usize) -> Result<Vec<Found>, ApiError> {
        let f = self.fields;
        let last = words.len() - 1;
        let clauses: Vec<(Occur, Box<dyn Query>)> = words
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let either: Vec<(Occur, Box<dyn Query>)> = [f.title, f.text]
                    .into_iter()
                    .map(|field| {
                        let term = Term::from_field_text(field, w);
                        let q: Box<dyn Query> = if i == last {
                            Box::new(FuzzyTermQuery::new_prefix(term, 0, true))
                        } else {
                            Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs))
                        };
                        (Occur::Should, q)
                    })
                    .collect();
                (
                    Occur::Must,
                    Box::new(BooleanQuery::new(either)) as Box<dyn Query>,
                )
            })
            .collect();
        let query = BooleanQuery::new(clauses);
        let searcher = self.reader.searcher();
        let top = searcher
            .search(&query, &TopDocs::with_limit(wanted).order_by_score())
            .map_err(search_error)?;
        let mut out = Vec::with_capacity(top.len());
        for (_score, address) in top {
            let doc: TantivyDocument = searcher.doc(address).map_err(search_error)?;
            let text_of = |field: Field| {
                doc.get_first(field)
                    .and_then(|v| match OwnedValue::from(v) {
                        OwnedValue::Str(s) => Some(s),
                        _ => None,
                    })
            };
            let Some(page) = text_of(f.page).and_then(|s| s.parse::<PageId>().ok()) else {
                continue;
            };
            out.push(Found {
                page,
                block: text_of(f.block).and_then(|s| s.parse::<BlockId>().ok()),
                text: text_of(f.text),
            });
        }
        Ok(out)
    }
}
