// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pages as agents read and write them: the Markdown bridge's dialect, with attachments named
//! by their content (`drusite:file/<blob>`), so an edit that keeps one keeps the file.

use std::collections::HashMap;

use drusite_crdt::merge::Wanted;
use drusite_markdown::{BlockRef, Frontmatter, MdPage, Resolver};
use drusite_model::{Block, BlockKind, PageId, PageKind, Property};

const FILE: &str = "drusite:file/";

/// Knows the attachments a page has, so the ones an agent's Markdown keeps stay files.
pub(crate) struct AgentResolver {
    files: HashMap<String, BlockKind>,
}

impl AgentResolver {
    pub(crate) fn new(blocks: &[Block]) -> Self {
        let files = blocks
            .iter()
            .filter_map(|b| match &b.kind {
                k @ BlockKind::Attachment { blob, .. } => Some((blob.clone(), k.clone())),
                _ => None,
            })
            .collect();
        Self { files }
    }
}

impl Resolver for AgentResolver {
    fn attachment_out(&self, blob: &str, _name: &str) -> String {
        format!("{FILE}{blob}")
    }

    fn attachment_in(&self, dest: &str, _wiki: bool) -> Option<BlockKind> {
        // Only files the page already has: an agent can't attach one by naming it.
        self.files.get(dest.strip_prefix(FILE)?).cloned()
    }
}

/// A page as Markdown, with its title, kind and properties in the frontmatter.
pub(crate) fn page_to_markdown(
    id: PageId,
    title: &str,
    kind: PageKind,
    properties: &[Property],
    blocks: &[Block],
) -> String {
    let front = Frontmatter {
        id: Some(id),
        kind,
        title: Some(title.to_owned()),
        properties: properties.to_vec(),
    };
    drusite_markdown::write(&front, blocks, &AgentResolver::new(blocks))
}

/// Reads an agent's Markdown for a page that has `blocks` now.
pub(crate) fn read(markdown: &str, blocks: &[Block]) -> MdPage {
    drusite_markdown::read(markdown, &AgentResolver::new(blocks))
}

/// What the Markdown's blocks ask for.
pub(crate) fn wanted(page: &MdPage) -> Vec<Wanted> {
    page.blocks
        .iter()
        .map(|b| Wanted {
            id: match &b.id {
                Some(BlockRef::Block(id)) => Some(*id),
                _ => None,
            },
            kind: b.kind.clone(),
            indent: b.indent,
            spans: b.spans.clone(),
        })
        .collect()
}

/// Whether the Markdown started with frontmatter (so its properties are meant as the page's).
pub(crate) fn has_frontmatter(markdown: &str) -> bool {
    drusite_markdown::split_frontmatter(markdown).0.is_some()
}
