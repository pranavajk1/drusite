// SPDX-License-Identifier: AGPL-3.0-or-later

//! Importing a folder of Markdown notes, such as an Obsidian vault, as new pages.
//!
//! The folder is only read. Its notes become pages under one new page named after it, its
//! folders become pages holding their notes (a note named like its folder, beside it or inside
//! it, is that folder's page), and:
//! - frontmatter becomes properties;
//! - wikilinks (`[[Note]]`, `[[Folder/Note|text]]`, `[[Note#Heading]]`) and links to other
//!   notes become links to their pages, found as Obsidian finds them: by path, else by name;
//! - embedded files (`![[photo.png]]`, `![](photo.png)`) become attachments;
//! - daily notes (`2026-11-26.md`) become journal entries for their day.
//!
//! Hidden folders (`.obsidian`, `.trash`) and files no note embeds are left out.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use drusite_markdown::Resolver;
use drusite_model::{BlockKind, Day, PageId, PageKind, Property, PropertyValue, journal};
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::api::{ApiError, ErrorCode};
use crate::attachments::{blob_id, guess_mime};
use crate::bridge::PAGE_LINK;
use crate::engine::Core;
use drusite_crdt::merge::{self, Wanted};

/// The largest embedded file imported.
const MAX_FILE: u64 = 100 * 1024 * 1024;

/// What an import made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// The page everything was imported under.
    pub page: Option<PageId>,
    /// Pages made, including those for folders.
    pub pages: u32,
    /// Files attached.
    pub attachments: u32,
    /// Links and embeds that pointed at nothing in the folder, and files that couldn't be read.
    pub unresolved: Vec<String>,
}

fn err(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Storage, e.to_string())
}

/// Every file under `root`, relative with `/` between parts, hidden entries left out.
fn files(root: &Path) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut dirs: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                dirs.push((entry.path(), path));
            } else if kind.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn is_note(path: &str) -> bool {
    path.to_lowercase().ends_with(".md")
}

fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Finds notes and files the way Obsidian does.
struct Vault {
    root: PathBuf,
    /// Note path (lowercase, without `.md`) → its page.
    notes: HashMap<String, PageId>,
    /// Note name (lowercase) → its pages, shortest path first.
    names: HashMap<String, Vec<PageId>>,
    /// Every other file, by lowercase path and by lowercase name.
    files: HashSet<String>,
    file_names: HashMap<String, Vec<String>>,
}

impl Vault {
    fn resolve_note(&self, from: &str, target: &str) -> Option<PageId> {
        let target = target.trim().trim_end_matches(".md").to_lowercase();
        if target.is_empty() {
            return None;
        }
        let here = parent_dir(from).to_lowercase();
        let relative = if here.is_empty() {
            target.clone()
        } else {
            format!("{here}/{target}")
        };
        self.notes
            .get(&relative)
            .or_else(|| self.notes.get(&target))
            .copied()
            .or_else(|| {
                let name = target.rsplit('/').next().unwrap_or(&target);
                self.names.get(name).and_then(|v| v.first().copied())
            })
    }

    fn resolve_file(&self, from: &str, target: &str) -> Option<String> {
        let target = target.trim();
        let lower = target.to_lowercase();
        let here = parent_dir(from).to_lowercase();
        let relative = crate::bridge::paths::resolve(&format!("{here}/x"), &lower);
        if let Some(r) = relative.filter(|r| self.files.contains(r)) {
            return Some(r);
        }
        if self.files.contains(&lower) {
            return Some(lower);
        }
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        self.file_names.get(name).and_then(|v| v.first().cloned())
    }
}

/// Links and embeds for one note.
struct NoteLinks<'a> {
    vault: &'a Vault,
    from: &'a str,
    /// Embedded files read: their ID, name and bytes.
    attached: std::cell::RefCell<Vec<(String, String, Vec<u8>)>>,
    unresolved: std::cell::RefCell<Vec<String>>,
}

fn decode(s: &str) -> String {
    // `%20` and friends, as Markdown links to files with spaces have them.
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

impl Resolver for NoteLinks<'_> {
    fn link_in(&self, dest: &str, wiki: bool) -> String {
        let target = decode(dest.split('#').next().unwrap_or(dest));
        if dest.contains("://") || dest.starts_with("mailto:") {
            return dest.to_owned();
        }
        if !wiki && !is_note(&target) && !target.is_empty() {
            // A link to some other file: kept as it is.
            return dest.to_owned();
        }
        if target.is_empty() || self.vault.resolve_file(self.from, &target).is_some() {
            // `[[#Heading]]` (this note), or a file rather than a note.
            return dest.to_owned();
        }
        match self.vault.resolve_note(self.from, &target) {
            Some(id) => format!("{PAGE_LINK}{id}"),
            None => {
                self.unresolved
                    .borrow_mut()
                    .push(format!("{}: link to {target}", self.from));
                dest.to_owned()
            }
        }
    }

    fn attachment_in(&self, dest: &str, _wiki: bool) -> Option<BlockKind> {
        let target = decode(dest.split(['#', '|']).next().unwrap_or(dest));
        if target.contains("://") {
            return None;
        }
        let Some(path) = self.vault.resolve_file(self.from, &target) else {
            if !is_note(&target) && self.vault.resolve_note(self.from, &target).is_none() {
                self.unresolved
                    .borrow_mut()
                    .push(format!("{}: embed of {target}", self.from));
            }
            return None;
        };
        // The file's real name, as it is on disk.
        let full = std::fs::read_dir(self.vault.root.join(parent_dir(&path)))
            .ok()?
            .flatten()
            .find(|e| {
                e.file_name().to_string_lossy().to_lowercase()
                    == path.rsplit('/').next().unwrap_or(&path)
            })
            .map(|e| e.path())?;
        if std::fs::metadata(&full).ok()?.len() > MAX_FILE {
            self.unresolved
                .borrow_mut()
                .push(format!("{}: {target} is over 100 MB", self.from));
            return None;
        }
        let bytes = std::fs::read(&full).ok()?;
        let name = full.file_name()?.to_string_lossy().into_owned();
        let blob = blob_id(&bytes);
        let kind = BlockKind::Attachment {
            blob: blob.clone(),
            mime: guess_mime(&name).to_owned(),
            size: u32::try_from(bytes.len()).ok()?,
            name: name.clone(),
        };
        self.attached.borrow_mut().push((blob, name, bytes));
        Some(kind)
    }
}

impl Core {
    /// Imports the Markdown notes in `folder` as new pages under a new page named after it, at
    /// the top level. The folder isn't changed.
    #[allow(clippy::too_many_lines)]
    pub fn import_folder(&mut self, folder: &Path) -> Result<ImportReport, ApiError> {
        if !folder.is_dir() {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                format!("{} isn't a folder", folder.display()),
            ));
        }
        let all = files(folder).map_err(err)?;
        let notes: Vec<&String> = all.iter().filter(|p| is_note(p)).collect();
        if notes.is_empty() {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                format!("{} has no Markdown notes", folder.display()),
            ));
        }
        let mut report = ImportReport::default();
        let name = folder.file_name().map_or_else(
            || "Imported notes".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let top = self.workspace.create_page(None, &name)?;
        let top_id = top.id();
        self.store.create_page(&top)?;
        report.page = Some(top_id);
        report.pages += 1;

        // A note named like its folder, beside it (`A.md` and `A/`) or inside it (`A/A.md`),
        // is the folder's page.
        let note_case: HashMap<String, String> = notes
            .iter()
            .map(|p| (p.to_lowercase(), (*p).clone()))
            .collect();
        // The folder's note, as its path is written.
        let folder_note = |dir: &str| -> Option<String> {
            let name = dir.rsplit('/').next().unwrap_or(dir);
            let inside = format!("{dir}/{name}.md").to_lowercase();
            let beside = format!("{dir}.md").to_lowercase();
            [inside, beside]
                .into_iter()
                .find_map(|p| note_case.get(&p).cloned())
        };

        // Pages first, parents before children, so links can point anywhere.
        let mut dir_pages: HashMap<String, PageId> = HashMap::new();
        let mut note_pages: HashMap<String, PageId> = HashMap::new();
        let mut ordered: Vec<&String> = notes.clone();
        ordered.sort_by_key(|p| (p.matches('/').count(), p.to_lowercase()));
        for path in &ordered {
            let lower = path.to_lowercase();
            if note_pages.contains_key(&lower) {
                continue;
            }
            // The folder this note's page goes under (made if need be).
            let mut dir = parent_dir(path).to_owned();
            // A folder's own note goes where the folder goes.
            let is_inside_note = !dir.is_empty()
                && folder_note(&dir).is_some_and(|f| f.to_lowercase() == lower)
                && stem(path).eq_ignore_ascii_case(dir.rsplit('/').next().unwrap_or(&dir));
            if is_inside_note {
                dir = parent_dir(&dir).to_owned();
            }
            let parent = self.import_dir(
                &dir,
                top_id,
                &mut dir_pages,
                &mut note_pages,
                &folder_note,
                &mut report,
            )?;
            let id = self.import_page(path, parent, &mut report)?;
            note_pages.insert(lower.clone(), id);
            // A beside-note (`A.md` next to `A/`) or inside-note stands for its folder too.
            let own_dir = if is_inside_note {
                Some(parent_dir(path).to_owned())
            } else {
                let d = lower.trim_end_matches(".md").to_owned();
                all.iter()
                    .any(|p| p.to_lowercase().starts_with(&format!("{d}/")))
                    .then_some(d)
            };
            if let Some(d) = own_dir {
                dir_pages.insert(d.to_lowercase(), id);
            }
        }

        // How notes and files are found.
        let mut names: HashMap<String, Vec<(usize, PageId)>> = HashMap::new();
        for (path, id) in &note_pages {
            names
                .entry(stem(path).to_lowercase())
                .or_default()
                .push((path.matches('/').count(), *id));
        }
        let mut file_names: HashMap<String, Vec<(usize, String)>> = HashMap::new();
        let mut file_set = HashSet::new();
        for p in all.iter().filter(|p| !is_note(p)) {
            let lower = p.to_lowercase();
            file_names
                .entry(lower.rsplit('/').next().unwrap_or(&lower).to_owned())
                .or_default()
                .push((lower.matches('/').count(), lower.clone()));
            file_set.insert(lower);
        }
        let vault = Vault {
            root: folder.to_path_buf(),
            notes: note_pages
                .iter()
                .map(|(p, id)| (p.trim_end_matches(".md").to_owned(), *id))
                .collect(),
            names: names
                .into_iter()
                .map(|(k, mut v)| {
                    v.sort();
                    (k, v.into_iter().map(|(_, id)| id).collect())
                })
                .collect(),
            files: file_set,
            file_names: file_names
                .into_iter()
                .map(|(k, mut v)| {
                    v.sort();
                    (k, v.into_iter().map(|(_, p)| p).collect())
                })
                .collect(),
        };

        // Then each note's text.
        let mut attached: HashSet<String> = HashSet::new();
        for path in &notes {
            let id = note_pages[&path.to_lowercase()];
            let text = match std::fs::read(folder.join(path)) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(e) => {
                    report.unresolved.push(format!("{path}: {e}"));
                    continue;
                }
            };
            let links = NoteLinks {
                vault: &vault,
                from: path,
                attached: std::cell::RefCell::new(Vec::new()),
                unresolved: std::cell::RefCell::new(Vec::new()),
            };
            let md = drusite_markdown::read(&text, &links);
            for (blob, _, bytes) in links.attached.take() {
                if attached.insert(blob.clone()) {
                    self.store.put_blob(&blob, &bytes, false)?;
                    report.attachments += 1;
                }
            }
            report.unresolved.extend(links.unresolved.take());
            for p in &md.front.properties {
                self.workspace.set_property(id, &p.name, Some(&p.value))?;
            }
            // The file's own block IDs (Obsidian's) mean nothing here.
            let wanted: Vec<Wanted> = md
                .blocks
                .iter()
                .map(|b| Wanted {
                    id: None,
                    kind: b.kind.clone(),
                    indent: b.indent,
                    spans: b.spans.clone(),
                })
                .collect();
            if !wanted.is_empty() {
                let (mut page, mut at) = self.store.load_page(id)?;
                merge::apply(&mut page, &wanted)?;
                self.store.save_page(&page, &mut at)?;
            }
            self.changed(*id.as_uuid());
        }
        if report.attachments > 0
            && let Some(notify) = &self.on_change
        {
            notify();
        }
        self.save_workspace()?;
        Ok(report)
    }

    /// The page for folder `dir` (relative, `""` for the top), made if it isn't there yet.
    fn import_dir(
        &mut self,
        dir: &str,
        top: PageId,
        dirs: &mut HashMap<String, PageId>,
        notes: &mut HashMap<String, PageId>,
        folder_note: &dyn Fn(&str) -> Option<String>,
        report: &mut ImportReport,
    ) -> Result<PageId, ApiError> {
        if dir.is_empty() {
            return Ok(top);
        }
        if let Some(id) = dirs.get(&dir.to_lowercase()) {
            return Ok(*id);
        }
        let parent = self.import_dir(parent_dir(dir), top, dirs, notes, folder_note, report)?;
        let id = match folder_note(dir) {
            // The folder's own note is its page.
            Some(note) if !notes.contains_key(&note.to_lowercase()) => {
                let id = self.import_page(&note, parent, report)?;
                notes.insert(note.to_lowercase(), id);
                id
            }
            Some(note) => notes[&note.to_lowercase()],
            None => {
                let name = dir.rsplit('/').next().unwrap_or(dir);
                let page = self.workspace.create_page(Some(parent), name)?;
                self.store.create_page(&page)?;
                report.pages += 1;
                page.id()
            }
        };
        dirs.insert(dir.to_lowercase(), id);
        Ok(id)
    }

    /// An empty page for the note at `path`: a journal entry if it's named for a day.
    fn import_page(
        &mut self,
        path: &str,
        parent: PageId,
        report: &mut ImportReport,
    ) -> Result<PageId, ApiError> {
        let name = stem(path);
        let page = match name.parse::<Day>() {
            Ok(day) if name.len() == 10 => self.workspace.create_page_of(
                Some(parent),
                &day.title(),
                PageKind::Journal,
                &[Property {
                    name: journal::DATE.to_owned(),
                    value: PropertyValue::Date(day),
                }],
            )?,
            _ => self.workspace.create_page(Some(parent), name)?,
        };
        self.store.create_page(&page)?;
        report.pages += 1;
        Ok(page.id())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use drusite_model::{BlockKind, PageKind};

    use crate::api::{Command, Response};
    use crate::{Core, StoreKey};

    /// A made-up vault with what Obsidian vaults have: frontmatter, wikilinks of every form,
    /// folders with and without folder notes, attachments, a daily note, and the hidden folders
    /// Obsidian keeps.
    fn vault(root: &std::path::Path) {
        let files: &[(&str, &[u8])] = &[
            (
                "Home.md",
                b"---\ntags: [index, start]\nrating: 5\n---\n# Welcome\n\nSee [[Garden]], [[Recipes/Bread|the bread]] and [[Bread#Method]].\n\n> [!tip] Water daily\n> Mornings are best.\n\n- [ ] Plant bulbs ^task1\n- [x] ~~Buy soil~~\n\n![[sunflower.png]]\n\n![The beach](attachments/beach%20day.jpg)\n\nA link to [[Nowhere]].\n",
            ),
            ("Projects/Garden.md", b"Beds by the [[Home|home]] wall.\n"),
            ("Recipes/Recipes.md", b"Everything I bake.\n"),
            ("Recipes/Bread.md", b"## Method\n\nFlour, water, salt.\n"),
            ("Daily/2026-11-20.md", b"Rain all day.\n"),
            ("attachments/sunflower.png", b"png bytes"),
            ("attachments/beach day.jpg", b"jpeg bytes"),
            ("attachments/unused.pdf", b"never embedded"),
            (".obsidian/app.json", b"{}"),
            (".trash/Old.md", b"deleted note"),
            ("Board.canvas", b"{}"),
        ];
        for (path, bytes) in files {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, bytes).unwrap();
        }
    }

    #[test]
    fn an_obsidian_vault_becomes_pages() {
        let data = tempfile::tempdir().unwrap();
        let mut core = Core::open(
            data.path(),
            &StoreKey::generate().unwrap(),
            Box::new(|_| {}),
        )
        .unwrap();
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("My vault");
        vault(&root);
        let report = core.import_folder(&root).unwrap();
        assert_eq!(report.attachments, 2);
        assert_eq!(report.unresolved, ["Home.md: link to Nowhere"]);

        let Response::Pages { pages } = core.handle(Command::ListPages).unwrap() else {
            panic!()
        };
        let by_title = |t: &str| pages.iter().find(|p| p.title == t).unwrap();
        let title_of = |id| pages.iter().find(|p| p.id == id).unwrap().title.clone();
        let top = by_title("My vault");
        assert_eq!(top.parent, None);
        let mut children: Vec<String> = top.children.iter().map(|&c| title_of(c)).collect();
        children.sort();
        // No page for the attachments folder: it holds no notes.
        assert_eq!(children, ["Daily", "Home", "Projects", "Recipes"]);
        // Recipes is its folder note's page, holding Bread.
        let recipes = by_title("Recipes");
        assert_eq!(
            recipes
                .children
                .iter()
                .map(|&c| title_of(c))
                .collect::<Vec<_>>(),
            ["Bread"]
        );
        // The daily note is a journal entry for its day.
        let day = by_title("Friday, 20 November 2026");
        assert_eq!(day.kind, PageKind::Journal);
        assert_eq!(title_of(day.parent.unwrap()), "Daily");
        assert!(pages.iter().all(|p| p.title != "Old"));
        // Properties from frontmatter.
        let home = by_title("Home");
        assert_eq!(home.properties.len(), 2);

        // Links point at pages; embeds are attachments; the callout and tasks came through.
        let Response::Opened { view } = core.handle(Command::OpenPage { page: home.id }).unwrap()
        else {
            panic!()
        };
        let links: Vec<String> = view.blocks[1]
            .spans
            .iter()
            .filter_map(|s| s.marks.link.clone())
            .collect();
        let garden = by_title("Garden").id;
        let bread = by_title("Bread").id;
        assert_eq!(
            links,
            [
                format!("drusite:page/{garden}"),
                format!("drusite:page/{bread}"),
                format!("drusite:page/{bread}"),
            ]
        );
        let kinds: Vec<&str> = view.blocks.iter().map(|b| b.kind.name()).collect();
        assert_eq!(
            kinds,
            [
                "heading",
                "paragraph",
                "callout",
                "paragraph",
                "todo",
                "todo",
                "attachment",
                "attachment",
                "paragraph"
            ]
        );
        assert!(
            matches!(&view.blocks[6].kind, BlockKind::Attachment { name, mime, .. } if name == "sunflower.png" && mime == "image/png")
        );
        assert_eq!(view.blocks[7].text(), "The beach");
        assert!(
            matches!(&view.blocks[7].kind, BlockKind::Attachment { name, .. } if name == "beach day.jpg")
        );
        assert_eq!(view.blocks[4].text(), "Plant bulbs");
        // The vault wasn't touched.
        assert!(root.join(".obsidian/app.json").exists());
        assert!(!root.join(".drusite").exists());
    }
}
