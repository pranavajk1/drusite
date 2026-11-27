// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Markdown bridge: every page as a Markdown file in a folder on this device, kept in step
//! both ways. Edits to the files, by a person, an editor or an agent, come back into the pages;
//! edits in the app go out to the files.
//!
//! [`Core::bridge_sync`] does one round: it reads the files that changed since it last wrote
//! them and merges each into its page ([`merge`]), makes a page for each new file, moves to the
//! trash the pages whose files were deleted, then writes the pages that changed in the app.
//! [`watch`] runs a round whenever the folder or the pages change.
//!
//! The folder holds plain text, outside the encrypted store: it is off until someone picks a
//! folder, and only on that device.

pub mod merge;
pub mod paths;
#[cfg(feature = "watch")]
pub mod watch;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use drusite_crdt::{Page, Patch, VersionVector};
use drusite_markdown::{BlockRef, Frontmatter, MdPage, Resolver};
use drusite_model::{Block, BlockKind, PageId, PageKind, Property};

use crate::api::{ApiError, ErrorCode, Event};
use crate::attachments::{blob_id, guess_mime};
use crate::engine::{Core, patch_for};
use crate::store::BridgeFile;
use merge::Wanted;
use paths::{ATTACHMENTS, children_dir, file_name, layout, relative, resolve};

/// How a page links to another in its text.
pub const PAGE_LINK: &str = "drusite:page/";

/// The hidden file that marks a folder as this workspace's bridge folder.
const MARKER: &str = ".drusite";

/// The largest file a page's embed is read from.
const MAX_EMBED: u64 = 100 * 1024 * 1024;

/// What one round did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Files whose edits came back into their pages.
    pub merged: usize,
    /// Pages made for new files.
    pub created: usize,
    /// Pages moved to the trash because their files were deleted.
    pub trashed: usize,
    /// Files written from the pages.
    pub written: usize,
    /// What went wrong with single files; the round carried on past them.
    pub problems: Vec<String>,
}

fn err(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Storage, e.to_string())
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// A file's modification time (Unix nanoseconds) and size.
fn stamp(meta: &std::fs::Metadata) -> (i64, i64) {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX));
    (mtime, i64::try_from(meta.len()).unwrap_or(i64::MAX))
}

/// The `.md` files under `root`, relative, with `/` between parts. Hidden entries and the
/// attachments folder are skipped.
fn scan(root: &Path) -> std::io::Result<Vec<(String, std::fs::Metadata)>> {
    let mut out = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || (rel.is_empty() && name == ATTACHMENTS) {
                continue;
            }
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let meta = entry.metadata()?;
            if meta.is_dir() {
                dirs.push((entry.path(), path));
            } else if meta.is_file() && name.to_lowercase().ends_with(".md") {
                out.push((path, meta));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Writes a file by renaming a finished temporary one over it, so nobody reads half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{name}.drusite-tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len() + 1
            && let Some(h) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

/// Links and attachments for one page's file.
struct Links<'a> {
    root: &'a Path,
    /// The file, relative to the root.
    from: &'a str,
    paths: &'a HashMap<PageId, String>,
    by_path: HashMap<String, PageId>,
    by_title: HashMap<String, PageId>,
    store: &'a crate::store::Store,
    /// Attachments the file names, to write beside it.
    out: RefCell<Vec<(String, String)>>,
    /// Files the file embeds that weren't attachments yet: their ID, name and bytes.
    new_files: RefCell<Vec<(String, String, Vec<u8>)>>,
}

impl<'a> Links<'a> {
    fn new(
        root: &'a Path,
        from: &'a str,
        paths: &'a HashMap<PageId, String>,
        titles: &HashMap<PageId, String>,
        store: &'a crate::store::Store,
    ) -> Self {
        let by_path = paths
            .iter()
            .map(|(id, p)| (p.to_lowercase(), *id))
            .collect();
        let mut by_title = HashMap::new();
        for (id, t) in titles {
            by_title.entry(t.to_lowercase()).or_insert(*id);
        }
        Self {
            root,
            from,
            paths,
            by_path,
            by_title,
            store,
            out: RefCell::new(Vec::new()),
            new_files: RefCell::new(Vec::new()),
        }
    }

    fn page_href(id: PageId) -> String {
        format!("{PAGE_LINK}{id}")
    }

    /// The bridge-relative path an embed points at, trying the file's folder, then the top.
    fn embedded(&self, dest: &str, wiki: bool) -> Option<String> {
        let dest = percent_decode(dest.split('#').next().unwrap_or(dest));
        let mut tries = vec![resolve(self.from, &dest)];
        if wiki {
            tries.push(resolve("x", &dest));
        }
        tries
            .into_iter()
            .flatten()
            .find(|p| self.root.join(p).is_file())
    }
}

impl Resolver for Links<'_> {
    fn link_out(&self, href: &str) -> String {
        if let Some(id) = href
            .strip_prefix(PAGE_LINK)
            .and_then(|s| s.parse::<PageId>().ok())
            && let Some(path) = self.paths.get(&id)
        {
            return relative(self.from, path);
        }
        href.to_owned()
    }

    fn link_in(&self, dest: &str, wiki: bool) -> String {
        let target = percent_decode(dest.split('#').next().unwrap_or(dest));
        if wiki {
            let key = target.trim().to_lowercase();
            let by_path = self
                .by_path
                .get(&format!("{key}.md"))
                .or_else(|| self.by_path.get(&key));
            if let Some(id) = by_path.or_else(|| self.by_title.get(&key)) {
                return Self::page_href(*id);
            }
            return dest.to_owned();
        }
        if !target.contains("://")
            && target.to_lowercase().ends_with(".md")
            && let Some(path) = resolve(self.from, &target)
            && let Some(id) = self.by_path.get(&path.to_lowercase())
        {
            return Self::page_href(*id);
        }
        dest.to_owned()
    }

    fn attachment_out(&self, blob: &str, name: &str) -> String {
        self.out
            .borrow_mut()
            .push((blob.to_owned(), name.to_owned()));
        let path = format!(
            "{ATTACHMENTS}/{}/{}",
            &blob[..16.min(blob.len())],
            file_name(name)
        );
        relative(self.from, &path)
    }

    fn attachment_in(&self, dest: &str, wiki: bool) -> Option<BlockKind> {
        let path = self.embedded(dest, wiki)?;
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        // One already written out: found by its ID's start.
        let mut parts = path.split('/');
        if parts.next() == Some(ATTACHMENTS)
            && let Some(prefix) = parts.next()
            && let Ok(Some(blob)) = self.store.blob_by_prefix(prefix)
            && let Ok(Some(bytes)) = self.store.blob(&blob)
        {
            return Some(BlockKind::Attachment {
                mime: guess_mime(&name).to_owned(),
                size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                blob,
                name,
            });
        }
        // Any other file beside the notes becomes an attachment.
        let full = self.root.join(&path);
        if std::fs::metadata(&full).ok()?.len() > MAX_EMBED {
            return None;
        }
        let bytes = std::fs::read(&full).ok()?;
        let blob = blob_id(&bytes);
        let kind = BlockKind::Attachment {
            blob: blob.clone(),
            mime: guess_mime(&name).to_owned(),
            size: u32::try_from(bytes.len()).ok()?,
            name: name.clone(),
        };
        self.new_files.borrow_mut().push((blob, name, bytes));
        Some(kind)
    }
}

/// The frontmatter a page's file carries.
fn front_of(
    id: PageId,
    title: &str,
    kind: PageKind,
    props: &[Property],
    path: &str,
) -> Frontmatter {
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md");
    Frontmatter {
        id: Some(id),
        kind,
        title: (stem != title).then(|| title.to_owned()),
        properties: props.to_vec(),
    }
}

/// What the record remembers as the file's facts: its path, then its frontmatter.
fn meta_of(path: &str, front: &Frontmatter) -> String {
    format!("{path}\n{}", front.to_yaml())
}

fn wanted_of(page: &MdPage) -> Vec<Wanted> {
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

impl Core {
    /// The bridge's folder and its last round.
    pub fn bridge_status(&self) -> Result<crate::api::BridgeStatus, ApiError> {
        Ok(crate::api::BridgeStatus {
            folder: self.store.bridge_folder()?,
            last: self.bridge_last.clone(),
        })
    }

    /// Remembers how a round went, and tells the UI when it did something or failed.
    pub fn bridge_record(&mut self, result: Result<Report, ApiError>) {
        let round = match result {
            Ok(r) => crate::api::BridgeRound {
                merged: u32::try_from(r.merged).unwrap_or(u32::MAX),
                created: u32::try_from(r.created).unwrap_or(u32::MAX),
                trashed: u32::try_from(r.trashed).unwrap_or(u32::MAX),
                written: u32::try_from(r.written).unwrap_or(u32::MAX),
                problems: r.problems,
                error: None,
            },
            Err(e) => crate::api::BridgeRound {
                error: Some(e.message),
                ..crate::api::BridgeRound::default()
            },
        };
        let quiet = round == crate::api::BridgeRound::default();
        if quiet && self.bridge_last.as_ref().is_some_and(|l| l.error.is_none()) {
            return;
        }
        self.bridge_last = Some(round);
        if let Ok(status) = self.bridge_status() {
            (self.events)(Event::BridgeChanged { status });
        }
    }

    /// The bridge's folder, if it's on.
    pub fn bridge_folder(&self) -> Result<Option<PathBuf>, ApiError> {
        Ok(self.store.bridge_folder()?.map(PathBuf::from))
    }

    /// Turns the bridge on with `folder` (made if it isn't there), or off. Changing folders
    /// forgets the old one's files; they stay on disk. Runs the first round at once.
    pub fn set_bridge_folder(&mut self, folder: Option<&Path>) -> Result<Report, ApiError> {
        let folder = match folder {
            Some(f) => {
                if !f.is_absolute() {
                    return Err(ApiError::new(
                        ErrorCode::Invalid,
                        "the Markdown folder must be a full path",
                    ));
                }
                std::fs::create_dir_all(f).map_err(err)?;
                std::fs::write(
                    f.join(MARKER),
                    format!(
                        "This folder holds a Drusite workspace's pages as Markdown.\nworkspace: {}\n",
                        self.workspace.id()
                    ),
                )
                .map_err(err)?;
                Some(f.to_string_lossy().into_owned())
            }
            None => None,
        };
        self.store.set_bridge_folder(folder.as_deref())?;
        if folder.is_some() {
            self.bridge_sync()
        } else {
            Ok(Report::default())
        }
    }

    /// One round of the bridge (see the module docs). Does nothing while it's off.
    pub fn bridge_sync(&mut self) -> Result<Report, ApiError> {
        let Some(root) = self.bridge_folder()? else {
            return Ok(Report::default());
        };
        let mut report = Report::default();
        if !root.is_dir() {
            return Err(ApiError::new(
                ErrorCode::Storage,
                format!("the Markdown folder {} isn't there", root.display()),
            ));
        }
        // Without its marker the folder isn't the one the bridge wrote (a drive not mounted
        // over an empty mount point, a folder replaced): its missing files are not deletions.
        if !root.join(MARKER).is_file() {
            return Err(ApiError::new(
                ErrorCode::Storage,
                format!(
                    "{} isn't the Markdown folder Drusite wrote (its {MARKER} file is gone); \
                     choose the folder again in Settings",
                    root.display()
                ),
            ));
        }
        let files = scan(&root).map_err(err)?;
        let records: HashMap<String, BridgeFile> = self
            .store
            .bridge_files()?
            .into_iter()
            .map(|r| (r.page.clone(), r))
            .collect();
        let by_path: HashMap<&str, &BridgeFile> =
            records.values().map(|r| (r.path.as_str(), r)).collect();
        let mut seen: HashSet<PageId> = HashSet::new();
        let mut workspace_changed = false;
        let on_disk: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();

        // Files: what changed comes in.
        for (path, meta) in &files {
            let (mtime, size) = stamp(meta);
            let record = by_path.get(path.as_str()).copied();
            if let Some(r) = record
                && r.mtime == mtime
                && r.size == size
            {
                if let Ok(id) = r.page.parse() {
                    seen.insert(id);
                }
                continue;
            }
            let bytes = match std::fs::read(root.join(path)) {
                Ok(b) => b,
                Err(e) => {
                    report.problems.push(format!("{path}: {e}"));
                    continue;
                }
            };
            let h = hash(&bytes);
            if let Some(r) = record
                && r.hash == h
            {
                // Touched, not changed.
                let mut r = r.clone();
                r.mtime = mtime;
                r.size = size;
                self.store.put_bridge_file(&r)?;
                if let Ok(id) = r.page.parse() {
                    seen.insert(id);
                }
                continue;
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            match self.bridge_read(&root, path, &text, record, &records, &seen, &on_disk) {
                Ok((id, created)) => {
                    seen.insert(id);
                    // Remembered at once, so this file is this page's from now on; the empty
                    // facts make the next write put the page's IDs and frontmatter in it.
                    let version = record
                        .filter(|r| r.page == id.to_string())
                        .or_else(|| records.get(&id.to_string()))
                        .map(|r| r.version.clone())
                        .unwrap_or_default();
                    self.store.put_bridge_file(&BridgeFile {
                        page: id.to_string(),
                        path: path.clone(),
                        version,
                        hash: h,
                        meta: String::new(),
                        mtime,
                        size,
                    })?;
                    workspace_changed = true;
                    if created {
                        report.created += 1;
                    } else {
                        report.merged += 1;
                    }
                }
                Err(e) => report.problems.push(format!("{path}: {}", e.message)),
            }
        }

        // Pages whose files are gone were deleted there: to the trash.
        for r in records.values() {
            let Ok(id) = r.page.parse::<PageId>() else {
                continue;
            };
            if seen.contains(&id) {
                continue;
            }
            self.store.delete_bridge_file(&r.page)?;
            if self.workspace.is_trashed(id).is_ok_and(|t| !t) {
                self.workspace.trash_page(id, crate::trash::now_ms())?;
                workspace_changed = true;
                report.trashed += 1;
            }
        }
        if workspace_changed {
            self.save_workspace()?;
        }

        // Pages: what changed goes out.
        self.bridge_write_all(&root, &mut report)?;
        Ok(report)
    }

    /// Brings one changed file into its page, or makes a page for it. Returns the page, and
    /// whether it is new.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn bridge_read(
        &mut self,
        root: &Path,
        path: &str,
        text: &str,
        record: Option<&BridgeFile>,
        records: &HashMap<String, BridgeFile>,
        seen: &HashSet<PageId>,
        on_disk: &HashSet<&str>,
    ) -> Result<(PageId, bool), ApiError> {
        let nodes = self.workspace.pages()?;
        let paths = layout(&nodes);
        let titles: HashMap<PageId, String> =
            nodes.iter().map(|n| (n.id, n.title.clone())).collect();
        let links = Links::new(root, path, &paths, &titles, &self.store);
        let md = drusite_markdown::read(text, &links);
        let new_files = links.new_files.take();
        drop(links);
        for (blob, _, bytes) in &new_files {
            self.store.put_blob(blob, bytes, false)?;
        }
        if !new_files.is_empty()
            && let Some(notify) = &self.on_change
        {
            notify();
        }

        // Which page: the one the file names, else the one last written there. Not if another
        // file already is that page (this one's a copy): then it's a new page.
        let known = |id: PageId| {
            self.workspace.title(id).is_ok()
                && !seen.contains(&id)
                && records
                    .get(&id.to_string())
                    .is_none_or(|r| r.path == path || !on_disk.contains(r.path.as_str()))
        };
        let page = md.front.id.filter(|&id| known(id)).or_else(|| {
            record
                .and_then(|r| r.page.parse().ok())
                .filter(|&id| known(id))
        });
        let stem = path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".md")
            .trim_end_matches(".MD")
            .to_owned();
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_owned();

        let Some(id) = page else {
            // A new file: a new page where its folder says.
            let parent = self.bridge_parent(&dir)?;
            let title = md.front.title.clone().unwrap_or(stem);
            let mut page = self.workspace.create_page_of(
                parent,
                &title,
                md.front.kind,
                &md.front.properties,
            )?;
            let base = page.version();
            merge::merge(&mut page, &base, &wanted_of(&md))?;
            self.store.create_page(&page)?;
            self.changed(*page.id().as_uuid());
            return Ok((page.id(), true));
        };

        if self.workspace.is_trashed(id)? {
            // A file brought back for a page in the trash brings the page back too.
            self.workspace.restore_page(id)?;
        }
        let own = records.get(&id.to_string());
        let base_front = own
            .and_then(|r| r.meta.split_once('\n'))
            .map(|(_, yaml)| Frontmatter::parse(yaml));
        let base_path = own.map(|r| r.path.as_str());

        // Where it is and what it's called.
        if base_path != Some(path) {
            // Moved or renamed in the folder.
            let parent = self.bridge_parent(&dir)?;
            if parent
                != self
                    .workspace
                    .pages()?
                    .iter()
                    .find(|n| n.id == id)
                    .and_then(|n| n.parent)
                && parent != Some(id)
            {
                let index = self
                    .workspace
                    .pages()?
                    .iter()
                    .filter(|n| n.parent == parent)
                    .count();
                let _ = self.workspace.move_page(id, parent, index);
            }
            let old_stem = base_path.map(|p| {
                p.rsplit('/')
                    .next()
                    .unwrap_or(p)
                    .trim_end_matches(".md")
                    .to_owned()
            });
            if old_stem.as_deref() != Some(stem.as_str()) {
                self.workspace.set_title(id, &stem)?;
            }
        } else if md.front.title != base_front.as_ref().and_then(|f| f.title.clone())
            && let Some(title) = &md.front.title
        {
            self.workspace.set_title(id, title)?;
        }
        // Properties and kind that changed in the file since it was written.
        let base_props = match &base_front {
            Some(f) => f.properties.clone(),
            None => self.workspace.properties(id)?,
        };
        for p in &md.front.properties {
            if !base_props.contains(p) {
                self.workspace.set_property(id, &p.name, Some(&p.value))?;
            }
        }
        for p in &base_props {
            if !md.front.properties.iter().any(|q| q.name == p.name) {
                self.workspace.set_property(id, &p.name, None)?;
            }
        }

        // The text.
        let base = match own.map(|r| VersionVector::decode(&r.version)) {
            Some(Ok(v)) => Some(v),
            _ => None,
        };
        let wanted = wanted_of(&md);
        self.bridge_merge(id, base, &wanted)?;
        Ok((id, false))
    }

    /// The page a folder (relative to the bridge's) stands for, making pages for folders that
    /// have none yet. `""` is the top level.
    fn bridge_parent(&mut self, dir: &str) -> Result<Option<PageId>, ApiError> {
        if dir.is_empty() {
            return Ok(None);
        }
        let mut parent = None;
        let mut at = String::new();
        for part in dir.split('/') {
            if !at.is_empty() {
                at.push('/');
            }
            at.push_str(part);
            let paths = layout(&self.workspace.pages()?);
            let found = paths
                .iter()
                .find(|(_, p)| children_dir(p).eq_ignore_ascii_case(&at))
                .map(|(id, _)| *id);
            parent = Some(match found {
                Some(id) => id,
                None => {
                    let page = self.workspace.create_page(parent, part)?;
                    self.store.create_page(&page)?;
                    self.changed(*page.id().as_uuid());
                    page.id()
                }
            });
        }
        Ok(parent)
    }

    /// Merges a file's blocks into a page, open or not. With no base (a file never written
    /// from this page), the file's version wins wherever it differs.
    fn bridge_merge(
        &mut self,
        id: PageId,
        base: Option<VersionVector>,
        wanted: &[Wanted],
    ) -> Result<(), ApiError> {
        let run = |page: &mut Page| -> Result<Patch, ApiError> {
            let base = base
                .clone()
                .filter(|b| page.version().includes_vv(b))
                .unwrap_or_else(|| page.version());
            Ok(merge::merge(page, &base, wanted)?)
        };
        if let Some(open) = self.open.get_mut(&id) {
            let patch = run(&mut open.page)?;
            self.store.save_page(&open.page, &mut open.at)?;
            let view = patch_for(&open.page, &patch.updated, &patch.removed)?;
            if !view.is_empty() {
                (self.events)(Event::PageChanged {
                    page: id,
                    patch: view,
                });
            }
        } else {
            let (mut page, mut at) = self.store.load_page(id)?;
            run(&mut page)?;
            self.store.save_page(&page, &mut at)?;
        }
        self.changed(*id.as_uuid());
        Ok(())
    }

    /// A page's blocks and version, from the open copy if there is one.
    fn bridge_page(&self, id: PageId) -> Result<(Vec<Block>, VersionVector), ApiError> {
        if let Some(open) = self.open.get(&id) {
            return Ok((open.page.blocks()?, open.page.version()));
        }
        let (page, _) = self.store.load_page(id)?;
        Ok((page.blocks()?, page.version()))
    }

    /// Writes every page whose file is out of date, and removes the files of pages that went.
    fn bridge_write_all(&mut self, root: &Path, report: &mut Report) -> Result<(), ApiError> {
        let nodes = self.workspace.pages()?;
        let paths = layout(&nodes);
        let titles: HashMap<PageId, String> =
            nodes.iter().map(|n| (n.id, n.title.clone())).collect();
        let records: HashMap<String, BridgeFile> = self
            .store
            .bridge_files()?
            .into_iter()
            .map(|r| (r.page.clone(), r))
            .collect();
        let dirty: HashSet<PageId> = std::mem::take(&mut self.bridge_dirty);

        // Files of pages no longer shown (in the trash, deleted) go.
        let live: HashSet<String> = nodes.iter().map(|n| n.id.to_string()).collect();
        for r in records.values() {
            if !live.contains(&r.page) {
                let _ = std::fs::remove_file(root.join(&r.path));
                self.store.delete_bridge_file(&r.page)?;
            }
        }

        for node in &nodes {
            let path = &paths[&node.id];
            let front = front_of(node.id, &node.title, node.kind, &node.properties, path);
            let meta = meta_of(path, &front);
            let record = records.get(&node.id.to_string());
            let fresh = record.is_some_and(|r| r.meta == meta) && !dirty.contains(&node.id);
            if fresh {
                continue;
            }
            let (blocks, version) = self.bridge_page(node.id)?;
            if let Some(r) = record
                && r.meta == meta
                && VersionVector::decode(&r.version).is_ok_and(|v| v == version)
            {
                continue;
            }
            let links = Links::new(root, path, &paths, &titles, &self.store);
            let text = drusite_markdown::write(&front, &blocks, &links);
            let attachments = links.out.take();
            drop(links);
            for (blob, name) in attachments {
                let file = root.join(format!(
                    "{ATTACHMENTS}/{}/{}",
                    &blob[..16.min(blob.len())],
                    file_name(&name)
                ));
                if !file.exists() {
                    match self.store.blob(&blob)? {
                        Some(bytes) => write_atomic(&file, &bytes).map_err(err)?,
                        // Not here yet: fetched, and written on a later round.
                        None => {
                            self.wanted_blobs.insert(blob);
                            self.bridge_dirty.insert(node.id);
                        }
                    }
                }
            }
            let full = root.join(path);
            // A different file where this one goes, written by someone else, isn't overwritten
            // unread: it comes in first, on the next round.
            if record.is_none_or(|r| r.path != *path)
                && full.exists()
                && !records.values().any(|r| r.path == *path)
            {
                self.bridge_dirty.insert(node.id);
                report
                    .problems
                    .push(format!("{path}: already there; reading it before writing"));
                continue;
            }
            write_atomic(&full, text.as_bytes()).map_err(err)?;
            if let Some(r) = record
                && r.path != *path
            {
                let _ = std::fs::remove_file(root.join(&r.path));
            }
            let (mtime, size) = std::fs::metadata(&full).map(|m| stamp(&m)).map_err(err)?;
            self.store.put_bridge_file(&BridgeFile {
                page: node.id.to_string(),
                path: path.clone(),
                version: version.encode(),
                hash: hash(text.as_bytes()),
                meta,
                mtime,
                size,
            })?;
            report.written += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
