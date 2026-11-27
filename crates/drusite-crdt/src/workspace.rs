// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workspace document: the page tree and each page's title.
//!
//! The tree is a Loro movable tree, so moving pages never conflicts, and a move that would make
//! a cycle is refused. Titles live here rather than in the page documents, so the sidebar can
//! show the whole tree without loading any page.

use std::collections::HashMap;

use drusite_model::{PageId, PageKind, Property, PropertyValue, SCHEMA_VERSION, WorkspaceId};
use loro::{
    ExportMode, LoroDoc, LoroMap, LoroText, LoroTree, LoroValue, TreeID, TreeParentId,
    ValueOrContainer, VersionVector,
};

use crate::guard::Guard;
use crate::page::Page;
use crate::{Error, MARKER, Result, attrs};

/// A page's place in the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct PageNode {
    pub id: PageId,
    pub parent: Option<PageId>,
    pub title: String,
    pub children: Vec<PageId>,
    pub kind: PageKind,
    /// In name order.
    pub properties: Vec<Property>,
}

/// A page in the trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedPage {
    pub id: PageId,
    pub title: String,
    /// The page it was under, which it goes back to when restored.
    pub parent: Option<PageId>,
    /// When it was put in the trash, in Unix milliseconds.
    pub trashed_at: i64,
    /// How many pages went with it.
    pub subpages: usize,
}

/// The tree entry key that marks a page as in the trash; its value is when, in Unix
/// milliseconds.
const TRASHED: &str = "trashed";

/// A setting's value, as the workspace document stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    Text(String),
    Int(i64),
    Bool(bool),
}

#[derive(Debug)]
pub struct Workspace {
    guard: Guard,
    doc: LoroDoc,
    id: WorkspaceId,
    nodes: HashMap<PageId, TreeID>,
}

impl Workspace {
    /// Creates a new, empty workspace. Like a page, a workspace is created once.
    pub fn create() -> Result<Self> {
        let doc = LoroDoc::new();
        let id = WorkspaceId::new();
        let meta = doc.get_map("meta");
        meta.insert("schema", i64::from(SCHEMA_VERSION))?;
        meta.insert("workspace", id.to_string())?;
        doc.get_tree("pages").enable_fractional_index(0);
        doc.commit();
        Ok(Self {
            guard: Guard::default(),
            doc,
            id,
            nodes: HashMap::new(),
        })
    }

    pub fn load(bytes: &[u8]) -> Result<Self> {
        Self::load_all(&[bytes])
    }

    pub fn load_all(parts: &[&[u8]]) -> Result<Self> {
        let guard = Guard::default();
        let (doc, id) = guard.run(|| {
            let doc = LoroDoc::new();
            for part in parts {
                doc.import(part)?;
            }
            let meta = doc.get_map("meta");
            let not_ws = || Error::NotADocument {
                expected: "workspace",
            };
            let schema = u32::try_from(attrs::get_int(&meta, "schema")).map_err(|_| not_ws())?;
            if schema == 0 {
                return Err(not_ws());
            }
            if schema > SCHEMA_VERSION {
                return Err(Error::NewerSchema {
                    found: schema,
                    supported: SCHEMA_VERSION,
                });
            }
            let id = attrs::get_str(&meta, "workspace")
                .and_then(|s| s.parse().ok())
                .ok_or_else(not_ws)?;
            Ok((doc, id))
        })?;
        let mut ws = Self {
            guard,
            doc,
            id,
            nodes: HashMap::new(),
        };
        ws.reindex();
        Ok(ws)
    }

    pub fn id(&self) -> WorkspaceId {
        self.id
    }

    pub fn is_poisoned(&self) -> bool {
        self.guard.is_poisoned()
    }

    fn tree(&self) -> LoroTree {
        self.doc.get_tree("pages")
    }

    fn reindex(&mut self) {
        let tree = self.tree();
        self.nodes = tree
            .nodes()
            .into_iter()
            .filter(|n| !tree.is_node_deleted(n).unwrap_or(true))
            .filter_map(|n| {
                let meta = tree.get_meta(n).ok()?;
                let id = attrs::get_str(&meta, "page")?.parse().ok()?;
                Some((id, n))
            })
            .collect();
    }

    fn node(&self, id: PageId) -> Result<TreeID> {
        self.nodes.get(&id).copied().ok_or(Error::UnknownPage(id))
    }

    fn parent_of(&self, parent: Option<PageId>) -> Result<TreeParentId> {
        Ok(match parent {
            Some(p) => TreeParentId::Node(self.node(p)?),
            None => TreeParentId::Root,
        })
    }

    /// Creates a page as the last child of `parent` (or at the top level) and returns its
    /// document, which the caller saves. This is the only place pages come from.
    pub fn create_page(&mut self, parent: Option<PageId>, title: &str) -> Result<Page> {
        self.create_page_of(parent, title, PageKind::Page, &[])
    }

    /// Creates a page of `kind` with `properties`, as [`Self::create_page`] does.
    pub fn create_page_of(
        &mut self,
        parent: Option<PageId>,
        title: &str,
        kind: PageKind,
        properties: &[Property],
    ) -> Result<Page> {
        let page = Page::create()?;
        let id = page.id();
        let node = self.guard.run(|| {
            let tree = self.tree();
            let parent = self.parent_of(parent)?;
            let index = tree.children_num(parent).unwrap_or(0);
            let node = tree.create_at(parent, index)?;
            let meta = tree.get_meta(node)?;
            meta.insert("page", id.to_string())?;
            if kind != PageKind::Page {
                meta.insert("kind", kind.name())?;
            }
            let text = meta.insert_container("title", LoroText::new())?;
            text.insert(0, &clean_title(title))?;
            for p in properties {
                meta.insert(&prop_key(&p.name), encode_value(&p.value))?;
            }
            self.doc.commit();
            Ok(node)
        })?;
        self.nodes.insert(id, node);
        Ok(page)
    }

    /// Moves a page to position `index` among `parent`'s children (or the top-level pages).
    pub fn move_page(&mut self, id: PageId, parent: Option<PageId>, index: usize) -> Result<()> {
        self.guard.run(|| {
            let node = self.node(id)?;
            if let Some(p) = parent
                && (p == id || self.is_descendant(p, id)?)
            {
                return Err(Error::Cycle);
            }
            if let Some(p) = parent
                && self.is_trashed(p)?
            {
                return Err(Error::Invalid("that page is in the trash".into()));
            }
            let tree = self.tree();
            let parent = self.parent_of(parent)?;
            let siblings = tree.children_num(parent).unwrap_or(0);
            let already_there = tree.parent(node) == Some(parent);
            let max = if already_there {
                siblings.saturating_sub(1)
            } else {
                siblings
            };
            tree.mov_to(node, parent, index.min(max))?;
            self.doc.commit();
            Ok(())
        })
    }

    /// Takes a page, and its subpages, out of the tree for good. Returns every page that went,
    /// so the caller can drop their documents. Pages reach this through the trash
    /// ([`Self::trash_page`]); this is "delete forever".
    pub fn delete_page(&mut self, id: PageId) -> Result<Vec<PageId>> {
        let gone = self.guard.run(|| {
            let mut gone = vec![id];
            gone.extend(self.descendants(self.node(id)?));
            self.tree().delete(self.node(id)?)?;
            self.doc.commit();
            Ok(gone)
        })?;
        self.reindex();
        Ok(gone)
    }

    /// Moves a page, and its subpages with it, to the trash, at `now` (Unix milliseconds). It
    /// keeps its place in the tree, hidden, so [`Self::restore_page`] puts it back where it
    /// was, and a concurrent edit or move on another device is kept.
    pub fn trash_page(&mut self, id: PageId, now: i64) -> Result<()> {
        self.guard.run(|| {
            let meta = self.tree().get_meta(self.node(id)?)?;
            if attrs::get_int(&meta, TRASHED) == 0 {
                meta.insert(TRASHED, now)?;
                self.doc.commit();
            }
            Ok(())
        })
    }

    /// Takes a page out of the trash. If the page it was under is still in the trash, it comes
    /// back at the top level instead.
    pub fn restore_page(&mut self, id: PageId) -> Result<()> {
        self.guard.run(|| {
            let node = self.node(id)?;
            let tree = self.tree();
            tree.get_meta(node)?.delete(TRASHED)?;
            if self.hidden_above(node) {
                let index = tree.children_num(TreeParentId::Root).unwrap_or(0);
                tree.mov_to(node, TreeParentId::Root, index)?;
            }
            self.doc.commit();
            Ok(())
        })
    }

    /// The pages put in the trash, most recent first. Subpages that went with their parent
    /// aren't listed on their own.
    pub fn trash(&self) -> Result<Vec<TrashedPage>> {
        self.guard.run(|| {
            let tree = self.tree();
            let mut out = Vec::new();
            for (&id, &node) in &self.nodes {
                let meta = tree.get_meta(node)?;
                let at = attrs::get_int(&meta, TRASHED);
                if at == 0 {
                    continue;
                }
                let parent = match tree.parent(node) {
                    Some(TreeParentId::Node(p)) => self.page_of(p),
                    _ => None,
                };
                out.push(TrashedPage {
                    id,
                    title: self.title_text(id)?.to_string(),
                    parent,
                    trashed_at: at,
                    subpages: self.descendants(node).len(),
                });
            }
            out.sort_by(|a, b| b.trashed_at.cmp(&a.trashed_at).then(a.id.cmp(&b.id)));
            Ok(out)
        })
    }

    /// Whether a page is in the trash, itself or under a page that is.
    pub fn is_trashed(&self, id: PageId) -> Result<bool> {
        self.guard.run(|| {
            let node = self.node(id)?;
            let meta = self.tree().get_meta(node)?;
            Ok(attrs::get_int(&meta, TRASHED) != 0 || self.hidden_above(node))
        })
    }

    /// Whether some page above `node` is in the trash.
    fn hidden_above(&self, node: TreeID) -> bool {
        let tree = self.tree();
        let mut at = tree.parent(node);
        while let Some(TreeParentId::Node(n)) = at {
            if tree
                .get_meta(n)
                .is_ok_and(|m| attrs::get_int(&m, TRASHED) != 0)
            {
                return true;
            }
            at = tree.parent(n);
        }
        false
    }

    /// Every page below `node`, at any depth.
    fn descendants(&self, node: TreeID) -> Vec<PageId> {
        let tree = self.tree();
        let mut out = Vec::new();
        let mut stack = tree.children(TreeParentId::Node(node)).unwrap_or_default();
        while let Some(n) = stack.pop() {
            out.extend(self.page_of(n));
            stack.extend(tree.children(TreeParentId::Node(n)).unwrap_or_default());
        }
        out
    }

    pub fn title(&self, id: PageId) -> Result<String> {
        self.guard.run(|| Ok(self.title_text(id)?.to_string()))
    }

    /// Sets a page's title, changing only the part that differs so a concurrent edit to the
    /// rest of it survives.
    pub fn set_title(&mut self, id: PageId, title: &str) -> Result<()> {
        self.guard.run(|| {
            let text = self.title_text(id)?;
            let old: Vec<char> = text.to_string().chars().collect();
            let new: Vec<char> = clean_title(title).chars().collect();
            let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(new[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            let removed = old.len() - prefix - suffix;
            if removed > 0 {
                text.delete(prefix, removed)?;
            }
            let inserted: String = new[prefix..new.len() - suffix].iter().collect();
            if !inserted.is_empty() {
                text.insert(prefix, &inserted)?;
            }
            self.doc.commit();
            Ok(())
        })
    }

    pub fn kind(&self, id: PageId) -> Result<PageKind> {
        self.guard.run(|| {
            let meta = self.tree().get_meta(self.node(id)?)?;
            Ok(attrs::get_str(&meta, "kind").map_or(PageKind::Page, |k| PageKind::from_name(&k)))
        })
    }

    /// A page's properties, in name order. Values this build can't read are left out.
    pub fn properties(&self, id: PageId) -> Result<Vec<Property>> {
        self.guard.run(|| {
            let meta = self.tree().get_meta(self.node(id)?)?;
            Ok(read_props(&meta))
        })
    }

    /// Sets property `name` on a page, or removes it (`None`). Each property is its own key in
    /// the page's tree entry (never a map of their own, which two devices could each create and
    /// one lose), so concurrent changes to different properties both stay; to the same one, the
    /// last writer wins.
    pub fn set_property(
        &mut self,
        id: PageId,
        name: &str,
        value: Option<&PropertyValue>,
    ) -> Result<()> {
        let name = name.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(Error::Invalid(format!("not a property name: {name:?}")));
        }
        self.guard.run(|| {
            let meta = self.tree().get_meta(self.node(id)?)?;
            match value {
                Some(v) => meta.insert(&prop_key(name), encode_value(v))?,
                None => meta.delete(&prop_key(name))?,
            }
            self.doc.commit();
            Ok(())
        })
    }

    fn settings_map(&self) -> LoroMap {
        self.doc.get_map("settings")
    }

    /// Every setting stored in the workspace, which syncs with it.
    pub fn settings(&self) -> Result<Vec<(String, SettingValue)>> {
        self.guard.run(|| {
            let map = self.settings_map();
            let mut out = Vec::new();
            map.for_each(|key, value| {
                let value = match value {
                    ValueOrContainer::Value(LoroValue::String(s)) => {
                        SettingValue::Text(s.to_string())
                    }
                    ValueOrContainer::Value(LoroValue::I64(i)) => SettingValue::Int(i),
                    ValueOrContainer::Value(LoroValue::Bool(b)) => SettingValue::Bool(b),
                    _ => return,
                };
                out.push((key.to_owned(), value));
            });
            out.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(out)
        })
    }

    /// Sets a setting, or removes it (`None`). Last writer wins, per setting.
    pub fn set_setting(&mut self, key: &str, value: Option<SettingValue>) -> Result<()> {
        self.guard.run(|| {
            let map = self.settings_map();
            match value {
                Some(SettingValue::Text(s)) => map.insert(key, s)?,
                Some(SettingValue::Int(i)) => map.insert(key, i)?,
                Some(SettingValue::Bool(b)) => map.insert(key, b)?,
                None => map.delete(key)?,
            }
            self.doc.commit();
            Ok(())
        })
    }

    fn title_text(&self, id: PageId) -> Result<LoroText> {
        let meta = self.tree().get_meta(self.node(id)?)?;
        match meta.get("title") {
            Some(ValueOrContainer::Container(c)) => c
                .into_text()
                .map_err(|_| Error::Loro("page title isn't text".into())),
            _ => Ok(meta.insert_container("title", LoroText::new())?),
        }
    }

    fn page_of(&self, node: TreeID) -> Option<PageId> {
        attrs::get_str(&self.tree().get_meta(node).ok()?, "page")?
            .parse()
            .ok()
    }

    fn is_descendant(&self, maybe_child: PageId, ancestor: PageId) -> Result<bool> {
        let tree = self.tree();
        let target = self.node(ancestor)?;
        let mut at = tree.parent(self.node(maybe_child)?);
        while let Some(TreeParentId::Node(n)) = at {
            if n == target {
                return Ok(true);
            }
            at = tree.parent(n);
        }
        Ok(false)
    }

    /// Every page the workspace holds, the ones in the trash included (they still sync, so
    /// they can be restored), in no particular order.
    pub fn all_pages(&self) -> Vec<PageId> {
        self.nodes.keys().copied().collect()
    }

    /// The top-level pages, in order.
    pub fn roots(&self) -> Vec<PageId> {
        self.children_of(TreeParentId::Root)
    }

    /// The pages directly under `parent`, in order, leaving out those in the trash.
    fn children_of(&self, parent: TreeParentId) -> Vec<PageId> {
        let tree = self.tree();
        tree.children(parent)
            .unwrap_or_default()
            .into_iter()
            .filter(|&n| {
                tree.get_meta(n)
                    .is_ok_and(|m| attrs::get_int(&m, TRASHED) == 0)
            })
            .filter_map(|n| self.page_of(n))
            .collect()
    }

    /// Every page in the tree, parents before children, siblings in order.
    pub fn pages(&self) -> Result<Vec<PageNode>> {
        self.guard.run(|| {
            let mut out = Vec::with_capacity(self.nodes.len());
            let mut stack: Vec<(Option<PageId>, PageId)> =
                self.roots().into_iter().rev().map(|r| (None, r)).collect();
            while let Some((parent, id)) = stack.pop() {
                let node = self.node(id)?;
                let meta = self.tree().get_meta(node)?;
                // A page in the trash is hidden, and its subpages with it.
                if attrs::get_int(&meta, TRASHED) != 0 {
                    continue;
                }
                let children = self.children_of(TreeParentId::Node(node));
                stack.extend(children.iter().rev().map(|&c| (Some(id), c)));
                out.push(PageNode {
                    id,
                    parent,
                    title: self.title_text(id)?.to_string(),
                    children,
                    kind: attrs::get_str(&meta, "kind")
                        .map_or(PageKind::Page, |k| PageKind::from_name(&k)),
                    properties: read_props(&meta),
                });
            }
            Ok(out)
        })
    }

    pub fn import(&mut self, bytes: &[u8]) -> Result<()> {
        let result = self.guard.run(|| {
            crate::guard::import_checked(&self.doc, bytes)?;
            Ok(())
        });
        if matches!(result, Err(Error::WrongDocument)) {
            self.guard.poison();
        }
        result?;
        self.reindex();
        Ok(())
    }

    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        self.guard
            .run(|| Ok(self.doc.export(ExportMode::Snapshot)?))
    }

    /// Imported updates still waiting for changes from another writer (see
    /// [`crate::Page::export_pending`]).
    pub fn export_pending(&self) -> Result<Option<Vec<u8>>> {
        self.guard.run(|| Ok(self.doc.export_pending_updates()?))
    }

    pub fn export_updates(&self, since: &VersionVector) -> Result<Vec<u8>> {
        self.guard
            .run(|| Ok(self.doc.export(ExportMode::updates(since))?))
    }

    pub fn version(&self) -> VersionVector {
        self.doc.oplog_vv()
    }
}

/// A property value as stored: `{t: type, v: value}`, so a date stays a date and a text that
/// looks like one stays text.
fn encode_value(value: &PropertyValue) -> LoroValue {
    let (t, v): (&str, LoroValue) = match value {
        PropertyValue::Text(s) => ("text", s.as_str().into()),
        PropertyValue::Number(n) => ("number", (*n).into()),
        PropertyValue::Checkbox(b) => ("checkbox", (*b).into()),
        PropertyValue::Date(d) => ("date", d.to_string().into()),
    };
    let mut map = std::collections::HashMap::new();
    map.insert("t".to_owned(), t.into());
    map.insert("v".to_owned(), v);
    map.into()
}

fn decode_value(value: &LoroValue) -> Option<PropertyValue> {
    let LoroValue::Map(map) = value else {
        return None;
    };
    let v = map.get("v")?;
    Some(match (map.get("t")?, v) {
        (LoroValue::String(t), LoroValue::String(s)) if t.as_str() == "text" => {
            PropertyValue::Text(s.to_string())
        }
        (LoroValue::String(t), LoroValue::Double(n)) if t.as_str() == "number" => {
            PropertyValue::Number(*n)
        }
        (LoroValue::String(t), LoroValue::I64(n)) if t.as_str() == "number" => {
            PropertyValue::Number(*n as f64)
        }
        (LoroValue::String(t), LoroValue::Bool(b)) if t.as_str() == "checkbox" => {
            PropertyValue::Checkbox(*b)
        }
        (LoroValue::String(t), LoroValue::String(s)) if t.as_str() == "date" => {
            PropertyValue::Date(s.parse().ok()?)
        }
        _ => return None,
    })
}

/// The key a property is stored under in its page's tree entry.
fn prop_key(name: &str) -> String {
    format!("{PROP_PREFIX}{name}")
}

const PROP_PREFIX: &str = "prop:";

fn read_props(meta: &LoroMap) -> Vec<Property> {
    let mut out = Vec::new();
    meta.for_each(|key, value| {
        if let Some(name) = key.strip_prefix(PROP_PREFIX)
            && let ValueOrContainer::Value(v) = value
            && let Some(value) = decode_value(&v)
        {
            out.push(Property {
                name: name.to_owned(),
                value,
            });
        }
    });
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Titles are one line, without the block marker.
fn clean_title(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' {
                ' '
            } else if c == MARKER {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}
