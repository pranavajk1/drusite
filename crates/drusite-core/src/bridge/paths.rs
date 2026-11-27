// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where each page's file goes: the page tree as folders, `Parent.md` beside `Parent/` holding
//! its subpages, names made safe for every file system.

use std::collections::{HashMap, HashSet};

use drusite_crdt::PageNode;
use drusite_model::PageId;

/// The folder attachments are written to, at the top of the bridge folder.
pub const ATTACHMENTS: &str = "_attachments";

/// Characters no file name may hold on some system Drusite runs on.
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];
const MAX_NAME: usize = 100;

/// A title as a file name (without `.md`): what can't be in a name becomes `-`, and an empty
/// title is "Untitled".
pub fn file_name(title: &str) -> String {
    let mut name: String = title
        .chars()
        .map(|c| {
            if FORBIDDEN.contains(&c) || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    // Leading dots hide a file; trailing dots and spaces vanish on Windows.
    name = name
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' '])
        .to_owned();
    if name.chars().count() > MAX_NAME {
        name = name
            .chars()
            .take(MAX_NAME)
            .collect::<String>()
            .trim_end()
            .to_owned();
    }
    if name.is_empty() {
        name = "Untitled".to_owned();
    }
    let lower = name.to_lowercase();
    if RESERVED.contains(&lower.as_str()) || lower == ATTACHMENTS {
        name.push('_');
    }
    name
}

/// Every page's file, relative to the folder: `A/B/Page.md` for a page under B under A.
/// Siblings whose names would clash (even only by case) get ` (2)`, ` (3)`… in tree order.
pub fn layout(pages: &[PageNode]) -> HashMap<PageId, String> {
    let by_id: HashMap<PageId, &PageNode> = pages.iter().map(|p| (p.id, p)).collect();
    let mut stems: HashMap<PageId, String> = HashMap::new();
    let mut taken: HashMap<Option<PageId>, HashSet<String>> = HashMap::new();
    // `pages` lists parents before children and siblings in order.
    for p in pages {
        let base = file_name(&p.title);
        let used = taken.entry(p.parent).or_default();
        let mut stem = base.clone();
        let mut n = 2;
        while used.contains(&stem.to_lowercase()) {
            stem = format!("{base} ({n})");
            n += 1;
        }
        used.insert(stem.to_lowercase());
        stems.insert(p.id, stem);
    }
    let mut out = HashMap::with_capacity(pages.len());
    for p in pages {
        let mut parts = vec![format!("{}.md", stems[&p.id])];
        let mut at = p.parent;
        while let Some(parent) = at {
            let Some(node) = by_id.get(&parent) else {
                break;
            };
            parts.push(stems[&parent].clone());
            at = node.parent;
        }
        parts.reverse();
        out.insert(p.id, parts.join("/"));
    }
    out
}

/// The folder a page's subpages go in: its path without `.md`.
pub fn children_dir(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

/// `to` relative to the folder holding `from` (both relative to the bridge folder).
pub fn relative(from: &str, to: &str) -> String {
    let mut from_dir: Vec<&str> = from.split('/').collect();
    from_dir.pop();
    let to_parts: Vec<&str> = to.split('/').collect();
    let common = from_dir
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<&str> = vec![".."; from_dir.len() - common];
    out.extend(&to_parts[common..]);
    out.join("/")
}

/// A link target in the file at `from`, as a path relative to the folder, if it stays inside it.
pub fn resolve(from: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use drusite_model::PageKind;

    use super::*;

    fn node(id: PageId, parent: Option<PageId>, title: &str) -> PageNode {
        PageNode {
            id,
            parent,
            title: title.into(),
            children: Vec::new(),
            kind: PageKind::Page,
            properties: Vec::new(),
        }
    }

    #[test]
    fn names_are_safe_everywhere() {
        assert_eq!(file_name("a/b: c?"), "a-b- c-");
        assert_eq!(file_name("  ..hidden. "), "hidden");
        assert_eq!(file_name(""), "Untitled");
        assert_eq!(file_name("CON"), "CON_");
        assert_eq!(file_name("_attachments"), "_attachments_");
        assert_eq!(file_name(&"x".repeat(300)).len(), MAX_NAME);
    }

    #[test]
    fn the_tree_is_folders_and_clashes_are_numbered() {
        let (a, b, c, d) = (PageId::new(), PageId::new(), PageId::new(), PageId::new());
        let pages = [
            node(a, None, "Projects"),
            node(b, Some(a), "Drusite"),
            node(c, Some(a), "drusite"),
            node(d, None, "Inbox"),
        ];
        let paths = layout(&pages);
        assert_eq!(paths[&a], "Projects.md");
        assert_eq!(paths[&b], "Projects/Drusite.md");
        assert_eq!(paths[&c], "Projects/drusite (2).md");
        assert_eq!(paths[&d], "Inbox.md");
        assert_eq!(children_dir(&paths[&a]), "Projects");
    }

    #[test]
    fn relative_paths_both_ways() {
        assert_eq!(relative("A/B/p.md", "A/q.md"), "../q.md");
        assert_eq!(relative("p.md", "A/q.md"), "A/q.md");
        assert_eq!(relative("A/p.md", "A/q.md"), "q.md");
        assert_eq!(resolve("A/B/p.md", "../q.md").as_deref(), Some("A/q.md"));
        assert_eq!(resolve("p.md", "../outside.md"), None);
    }
}
