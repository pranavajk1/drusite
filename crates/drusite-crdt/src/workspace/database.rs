// SPDX-License-Identifier: AGPL-3.0-or-later

//! Databases in the workspace document.
//!
//! A database is a page of kind `database`; its rows are its subpages. Everything is stored as
//! flat keys in tree entries, never in containers of their own (two devices could each make the
//! same container, and one would be lost), so each fact merges on its own:
//!
//! - in the **database's** entry: `db:p:<prop>:name`, `:kind`, `:pos`, `:desc` for each
//!   property; `db:o:<prop>:<option>` for each select option; `db:v:<view>:<field>` for each
//!   field of each view (its name, layout, filter, sorts, grouping…), each as JSON;
//! - in each **row's** entry: `v:<prop>` for a single value, as JSON, and `v:<prop>#<item>` for
//!   each item of a multi-select, relation, people or files value, so two devices adding a tag
//!   each keep theirs; plus `created`, `createdBy`, `edited` and `editedBy`.
//!
//! So a rename on one device and a new value on another both stay; two devices changing the
//! same field of the same thing keep the last writer's.

use std::collections::{BTreeMap, HashMap};

use drusite_model::database::{
    FileRef, OptionId, Person, PropId, PropertyDef, PropertyKind, Schema, SelectOption, TITLE,
    Value, View, ViewId,
};
use drusite_model::{PageId, PageKind};
use loro::{LoroMap, LoroValue, ValueOrContainer};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::Workspace;
use crate::page::Page;
use crate::{Error, Result, attrs};

const INLINE: &str = "db:inline";

/// What a row holds besides its title and content.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RowData {
    pub values: HashMap<PropId, Value>,
    /// Unix milliseconds; from the page's ID unless an import set it.
    pub created: Option<i64>,
    pub created_by: Option<Person>,
    pub edited: Option<i64>,
    pub edited_by: Option<Person>,
}

fn to_json<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| Error::Invalid(e.to_string()))
}

fn from_json<T: DeserializeOwned>(v: &ValueOrContainer) -> Option<T> {
    match v {
        ValueOrContainer::Value(LoroValue::String(s)) => serde_json::from_str(s).ok(),
        _ => None,
    }
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || id.contains(':') || id.contains('#') || id.chars().any(char::is_control) {
        return Err(Error::Invalid(format!("not an ID: {id:?}")));
    }
    Ok(())
}

/// The key a multi-valued item is stored under, and the item's own key.
fn item_key(prop: &str, item: &str) -> String {
    format!("v:{prop}#{item}")
}

/// A stored item: its place among the others, and what it is.
#[derive(serde::Serialize, serde::Deserialize)]
struct Item<T> {
    o: f64,
    v: T,
}

fn file_key(f: &FileRef) -> String {
    let id = f.blob.clone().or_else(|| f.url.clone()).unwrap_or_default();
    blake3_short(&format!("{}\u{0}{id}", f.name))
}

/// A short, stable key for an item that has no ID of its own.
fn blake3_short(s: &str) -> String {
    // FNV-1a: stable, tiny and good enough for keys within one row.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

impl Workspace {
    /// Makes a database: a page of kind database with just a title property and one table view.
    /// `inline` marks one shown inside its parent page rather than as a page of its own.
    pub fn create_database(
        &mut self,
        parent: Option<PageId>,
        title: &str,
        inline: bool,
    ) -> Result<(Page, Schema)> {
        let page = self.create_page_of(parent, title, PageKind::Database, &[])?;
        let db = page.id();
        self.put_property(
            db,
            &PropertyDef {
                id: TITLE.into(),
                name: "Name".into(),
                kind: PropertyKind::Title,
                options: Vec::new(),
                position: 0.0,
                description: None,
            },
        )?;
        if inline {
            self.with_meta(db, |m| Ok(m.insert(INLINE, true)?))?;
        }
        let mut view = View::new(
            drusite_model::database::short_id(),
            "Table",
            drusite_model::database::Layout::Table,
        );
        view.properties = vec![drusite_model::database::ViewProperty {
            id: TITLE.into(),
            width: None,
        }];
        self.put_view(db, &view)?;
        let schema = self.schema(db)?;
        Ok((page, schema))
    }

    fn with_meta<T>(&mut self, page: PageId, f: impl FnOnce(&LoroMap) -> Result<T>) -> Result<T> {
        let out = self.guard.run(|| {
            let meta = self.tree().get_meta(self.node(page)?)?;
            let out = f(&meta)?;
            self.doc.commit();
            Ok(out)
        })?;
        Ok(out)
    }

    fn database_meta(&self, db: PageId) -> Result<LoroMap> {
        let meta = self.tree().get_meta(self.node(db)?)?;
        if attrs::get_str(&meta, "kind").as_deref() != Some(PageKind::Database.name()) {
            return Err(Error::Invalid("that page isn't a database".into()));
        }
        Ok(meta)
    }

    /// Whether `page` is a database.
    pub fn is_database(&self, page: PageId) -> bool {
        self.kind(page).is_ok_and(|k| k == PageKind::Database)
    }

    /// The database a row belongs to: its parent, when that is a database.
    pub fn database_of(&self, row: PageId) -> Option<PageId> {
        let tree = self.tree();
        let parent = match tree.parent(self.node(row).ok()?)? {
            loro::TreeParentId::Node(p) => self.page_of(p)?,
            _ => return None,
        };
        self.is_database(parent).then_some(parent)
    }

    /// A database's schema and views.
    pub fn schema(&self, db: PageId) -> Result<Schema> {
        self.guard.run(|| {
            let meta = self.database_meta(db)?;
            let mut props: BTreeMap<String, PropertyDef> = BTreeMap::new();
            let mut options: HashMap<String, Vec<SelectOption>> = HashMap::new();
            let mut views: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
                BTreeMap::new();
            let mut inline = false;
            meta.for_each(|key, value| {
                if key == INLINE {
                    inline = matches!(value, ValueOrContainer::Value(LoroValue::Bool(true)));
                } else if let Some(rest) = key.strip_prefix("db:p:") {
                    let Some((id, field)) = rest.rsplit_once(':') else {
                        return;
                    };
                    let p = props.entry(id.to_owned()).or_insert_with(|| PropertyDef {
                        id: id.to_owned(),
                        name: String::new(),
                        kind: PropertyKind::Text,
                        options: Vec::new(),
                        position: f64::MAX,
                        description: None,
                    });
                    match field {
                        "name" => p.name = from_json(&value).unwrap_or_default(),
                        "kind" => {
                            if let Some(k) = from_json(&value) {
                                p.kind = k;
                            }
                        }
                        "pos" => p.position = from_json(&value).unwrap_or(f64::MAX),
                        "desc" => p.description = from_json(&value),
                        _ => {}
                    }
                } else if let Some(rest) = key.strip_prefix("db:o:") {
                    let Some((prop, _)) = rest.split_once(':') else {
                        return;
                    };
                    if let Some(o) = from_json::<SelectOption>(&value) {
                        options.entry(prop.to_owned()).or_default().push(o);
                    }
                } else if let Some(rest) = key.strip_prefix("db:v:") {
                    let Some((id, field)) = rest.split_once(':') else {
                        return;
                    };
                    if let ValueOrContainer::Value(LoroValue::String(s)) = &value
                        && let Ok(v) = serde_json::from_str::<serde_json::Value>(s)
                    {
                        views
                            .entry(id.to_owned())
                            .or_default()
                            .insert(field.to_owned(), v);
                    }
                }
            });
            // A property needs a name and a kind; one half-deleted on another device is gone.
            let mut properties: Vec<PropertyDef> = props
                .into_values()
                .filter(|p| !p.name.is_empty())
                .map(|mut p| {
                    let mut opts = options.remove(&p.id).unwrap_or_default();
                    opts.sort_by(|a, b| {
                        a.position
                            .partial_cmp(&b.position)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| a.id.cmp(&b.id))
                    });
                    p.options = opts;
                    p
                })
                .collect();
            properties.sort_by(|a, b| {
                (a.id != TITLE)
                    .cmp(&(b.id != TITLE))
                    .then(
                        a.position
                            .partial_cmp(&b.position)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                    .then_with(|| a.id.cmp(&b.id))
            });
            let mut views: Vec<View> = views
                .into_iter()
                .filter_map(|(id, mut fields)| {
                    fields.insert("id".into(), serde_json::Value::String(id));
                    serde_json::from_value(serde_json::Value::Object(fields)).ok()
                })
                .collect();
            views.sort_by(|a, b| {
                a.position
                    .partial_cmp(&b.position)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.id.cmp(&b.id))
            });
            Ok(Schema {
                database: db,
                title: self.title_text(db)?.to_string(),
                properties,
                views,
                inline,
            })
        })
    }

    /// Adds a property, or changes one: only the fields that differ are written. Its options
    /// are written one by one too; options it no longer lists are removed.
    pub fn put_property(&mut self, db: PageId, def: &PropertyDef) -> Result<()> {
        check_id(&def.id)?;
        let name = def.name.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(Error::Invalid(format!("not a property name: {name:?}")));
        }
        if (def.id == TITLE) != matches!(def.kind, PropertyKind::Title) {
            return Err(Error::Invalid(
                "only the title property is a title".to_owned(),
            ));
        }
        let old = self
            .schema(db)
            .ok()
            .and_then(|s| s.property(&def.id).cloned());
        let fields = [
            ("name", to_json(&name)?),
            ("kind", to_json(&def.kind)?),
            ("pos", to_json(&def.position)?),
            ("desc", to_json(&def.description)?),
        ];
        let before = old.as_ref().map(|o| {
            [
                to_json(&o.name.as_str()).unwrap_or_default(),
                to_json(&o.kind).unwrap_or_default(),
                to_json(&o.position).unwrap_or_default(),
                to_json(&o.description).unwrap_or_default(),
            ]
        });
        let id = def.id.clone();
        let options = def.options.clone();
        let has_options = def.kind.has_options();
        let old_options = old.map(|o| o.options).unwrap_or_default();
        self.guard.run(|| {
            let meta = self.database_meta(db)?;
            for (i, (field, value)) in fields.iter().enumerate() {
                if before.as_ref().is_none_or(|b| b[i] != *value) {
                    meta.insert(&format!("db:p:{id}:{field}"), value.as_str())?;
                }
            }
            if has_options {
                for o in &options {
                    check_id(&o.id)?;
                    let same = old_options.iter().any(|x| x == o);
                    if !same {
                        meta.insert(&format!("db:o:{id}:{}", o.id), to_json(o)?.as_str())?;
                    }
                }
                for o in &old_options {
                    if !options.iter().any(|x| x.id == o.id) {
                        meta.delete(&format!("db:o:{id}:{}", o.id))?;
                    }
                }
            }
            self.doc.commit();
            Ok(())
        })
    }

    /// Adds or changes one option of a property, leaving the others alone.
    pub fn put_option(&mut self, db: PageId, prop: &str, option: &SelectOption) -> Result<()> {
        check_id(prop)?;
        check_id(&option.id)?;
        let json = to_json(option)?;
        self.guard.run(|| {
            let meta = self.database_meta(db)?;
            meta.insert(&format!("db:o:{prop}:{}", option.id), json.as_str())?;
            self.doc.commit();
            Ok(())
        })
    }

    pub fn delete_option(&mut self, db: PageId, prop: &str, option: &OptionId) -> Result<()> {
        self.guard.run(|| {
            let meta = self.database_meta(db)?;
            meta.delete(&format!("db:o:{prop}:{option}"))?;
            self.doc.commit();
            Ok(())
        })
    }

    /// Removes a property from the schema. Rows keep their values under its ID, unseen, so
    /// taking the removal back (undo) brings them back.
    pub fn delete_property(&mut self, db: PageId, prop: &str) -> Result<()> {
        if prop == TITLE {
            return Err(Error::Invalid("the title property can't be removed".into()));
        }
        self.guard.run(|| {
            let meta = self.database_meta(db)?;
            let mut gone = Vec::new();
            meta.for_each(|key, _| {
                if key.starts_with(&format!("db:p:{prop}:"))
                    || key.starts_with(&format!("db:o:{prop}:"))
                {
                    gone.push(key.to_owned());
                }
            });
            for k in gone {
                meta.delete(&k)?;
            }
            self.doc.commit();
            Ok(())
        })
    }

    /// Adds a view, or changes one: only the fields that differ are written, so two devices
    /// changing one view's filter and its sort both keep theirs.
    pub fn put_view(&mut self, db: PageId, view: &View) -> Result<()> {
        check_id(&view.id)?;
        let serde_json::Value::Object(mut fields) =
            serde_json::to_value(view).map_err(|e| Error::Invalid(e.to_string()))?
        else {
            return Err(Error::Invalid("a view must be an object".into()));
        };
        fields.remove("id");
        let old: Option<serde_json::Map<String, serde_json::Value>> = self
            .schema(db)
            .ok()
            .and_then(|s| s.views.into_iter().find(|v| v.id == view.id))
            .and_then(|v| match serde_json::to_value(v) {
                Ok(serde_json::Value::Object(m)) => Some(m),
                _ => None,
            });
        self.guard.run(|| {
            let meta = self.database_meta(db)?;
            for (field, value) in &fields {
                if old.as_ref().and_then(|o| o.get(field)) != Some(value) {
                    meta.insert(&format!("db:v:{}:{field}", view.id), value.to_string())?;
                }
            }
            self.doc.commit();
            Ok(())
        })
    }

    pub fn delete_view(&mut self, db: PageId, view: &ViewId) -> Result<()> {
        self.guard.run(|| {
            let meta = self.database_meta(db)?;
            let prefix = format!("db:v:{view}:");
            let mut gone = Vec::new();
            meta.for_each(|key, _| {
                if key.starts_with(&prefix) {
                    gone.push(key.to_owned());
                }
            });
            for k in gone {
                meta.delete(&k)?;
            }
            self.doc.commit();
            Ok(())
        })
    }

    /// A database's rows, in their own order, leaving out those in the trash.
    pub fn rows(&self, db: PageId) -> Result<Vec<PageId>> {
        Ok(self.children_of(loro::TreeParentId::Node(self.node(db)?)))
    }

    /// A row's values, read against its database's `schema` (values whose kind no longer
    /// matches its property are left out).
    pub fn row(&self, row: PageId, schema: &Schema) -> Result<RowData> {
        self.guard.run(|| {
            let meta = self.tree().get_meta(self.node(row)?)?;
            let mut singles: HashMap<String, Value> = HashMap::new();
            let mut items: HashMap<String, Vec<(f64, String, serde_json::Value)>> = HashMap::new();
            let mut out = RowData::default();
            meta.for_each(|key, value| {
                if let Some(rest) = key.strip_prefix("v:") {
                    if let Some((prop, item)) = rest.split_once('#') {
                        if let Some(it) = from_json::<Item<serde_json::Value>>(&value) {
                            items.entry(prop.to_owned()).or_default().push((
                                it.o,
                                item.to_owned(),
                                it.v,
                            ));
                        }
                    } else if let Some(v) = from_json::<Value>(&value) {
                        singles.insert(rest.to_owned(), v);
                    }
                    return;
                }
                match key {
                    "created" => {
                        if let ValueOrContainer::Value(LoroValue::I64(t)) = value {
                            out.created = Some(t);
                        }
                    }
                    "edited" => {
                        if let ValueOrContainer::Value(LoroValue::I64(t)) = value {
                            out.edited = Some(t);
                        }
                    }
                    "createdBy" => out.created_by = from_json(&value),
                    "editedBy" => out.edited_by = from_json(&value),
                    _ => {}
                }
            });
            for p in &schema.properties {
                let value = match &p.kind {
                    PropertyKind::MultiSelect => Some(Value::MultiSelect(
                        ordered(items.remove(&p.id))
                            .into_iter()
                            .map(|(k, _)| k)
                            .collect(),
                    )),
                    PropertyKind::Relation { mirror: false, .. } => Some(Value::Relation(
                        ordered(items.remove(&p.id))
                            .into_iter()
                            .filter_map(|(k, _)| k.parse().ok())
                            .collect(),
                    )),
                    PropertyKind::People => Some(Value::People(
                        ordered(items.remove(&p.id))
                            .into_iter()
                            .filter_map(|(_, v)| serde_json::from_value(v).ok())
                            .collect(),
                    )),
                    PropertyKind::Files => Some(Value::Files(
                        ordered(items.remove(&p.id))
                            .into_iter()
                            .filter_map(|(_, v)| serde_json::from_value(v).ok())
                            .collect(),
                    )),
                    kind if kind.is_stored() => singles.remove(&p.id).filter(|v| v.fits(kind)),
                    _ => None,
                };
                if let Some(v) = value
                    && !v.is_empty()
                {
                    out.values.insert(p.id.clone(), v);
                }
            }
            Ok(out)
        })
    }

    /// Sets a row's value for `prop`, or clears it (`None`). A multi-valued one is written item
    /// by item: what was added is added and what was removed is removed, so another device's
    /// concurrent changes to other items stay.
    pub fn set_value(&mut self, row: PageId, prop: &str, value: Option<&Value>) -> Result<()> {
        check_id(prop)?;
        if prop == TITLE {
            return Err(Error::Invalid("the title is the page's title".into()));
        }
        let wanted: Option<Vec<(String, serde_json::Value)>> = match value {
            Some(Value::MultiSelect(ids)) => Some(
                ids.iter()
                    .map(|i| (i.clone(), serde_json::Value::Bool(true)))
                    .collect(),
            ),
            Some(Value::Relation(pages)) => Some(
                pages
                    .iter()
                    .map(|p| (p.to_string(), serde_json::Value::Bool(true)))
                    .collect(),
            ),
            Some(Value::People(people)) => Some(
                people
                    .iter()
                    .map(|p| {
                        (
                            blake3_short(&p.id),
                            serde_json::to_value(p).unwrap_or_default(),
                        )
                    })
                    .collect(),
            ),
            Some(Value::Files(files)) => Some(
                files
                    .iter()
                    .map(|f| (file_key(f), serde_json::to_value(f).unwrap_or_default()))
                    .collect(),
            ),
            _ => None,
        };
        let single = match value {
            Some(v) if wanted.is_none() => Some(to_json(v)?),
            _ => None,
        };
        self.guard.run(|| {
            let meta = self.tree().get_meta(self.node(row)?)?;
            let prefix = format!("v:{prop}#");
            let mut have: HashMap<String, f64> = HashMap::new();
            meta.for_each(|key, value| {
                if let Some(item) = key.strip_prefix(&prefix)
                    && let Some(it) = from_json::<Item<serde_json::Value>>(&value)
                {
                    have.insert(item.to_owned(), it.o);
                }
            });
            match &wanted {
                Some(items) => {
                    meta.delete(&format!("v:{prop}"))?;
                    let keep: std::collections::HashSet<&str> =
                        items.iter().map(|(k, _)| k.as_str()).collect();
                    for k in have.keys() {
                        if !keep.contains(k.as_str()) {
                            meta.delete(&item_key(prop, k))?;
                        }
                    }
                    let mut last = have.values().copied().fold(0.0, f64::max);
                    for (k, v) in items {
                        if !have.contains_key(k) {
                            last += 1.0;
                            let it = Item { o: last, v };
                            meta.insert(&item_key(prop, k), to_json(&it)?.as_str())?;
                        }
                    }
                }
                None => {
                    for k in have.keys() {
                        meta.delete(&item_key(prop, k))?;
                    }
                    match &single {
                        Some(json) => meta.insert(&format!("v:{prop}"), json.as_str())?,
                        None => meta.delete(&format!("v:{prop}"))?,
                    }
                }
            }
            self.doc.commit();
            Ok(())
        })
    }

    /// Records that `row` was edited at `now` (Unix milliseconds) by `by`; also sets who made it
    /// when that isn't known yet. Edits within the same minute aren't recorded again, as Notion
    /// shows edited times to the minute.
    pub fn touch_row(&mut self, row: PageId, now: i64, by: Option<&Person>) -> Result<()> {
        let by_json = by.map(to_json).transpose()?;
        self.guard.run(|| {
            let meta = self.tree().get_meta(self.node(row)?)?;
            let last = attrs::get_int(&meta, "edited");
            let same_by = match (&by_json, meta.get("editedBy")) {
                (Some(j), Some(ValueOrContainer::Value(LoroValue::String(s)))) => **s == *j,
                (None, None) => true,
                _ => false,
            };
            if now - last < 60_000 && same_by {
                return Ok(());
            }
            meta.insert("edited", now)?;
            if let Some(j) = &by_json {
                meta.insert("editedBy", j.as_str())?;
                if meta.get("createdBy").is_none() {
                    meta.insert("createdBy", j.as_str())?;
                }
            }
            self.doc.commit();
            Ok(())
        })
    }

    /// Sets a row's created and edited facts as an import found them.
    pub fn set_row_history(
        &mut self,
        row: PageId,
        created: Option<i64>,
        created_by: Option<&Person>,
        edited: Option<i64>,
        edited_by: Option<&Person>,
    ) -> Result<()> {
        let cb = created_by.map(to_json).transpose()?;
        let eb = edited_by.map(to_json).transpose()?;
        self.with_meta(row, |meta| {
            if let Some(t) = created {
                meta.insert("created", t)?;
            }
            if let Some(t) = edited {
                meta.insert("edited", t)?;
            }
            if let Some(j) = &cb {
                meta.insert("createdBy", j.as_str())?;
            }
            if let Some(j) = &eb {
                meta.insert("editedBy", j.as_str())?;
            }
            Ok(())
        })
    }
}

/// Items in the order they were added (ties by key, the same on every device).
fn ordered(
    items: Option<Vec<(f64, String, serde_json::Value)>>,
) -> Vec<(String, serde_json::Value)> {
    let mut items = items.unwrap_or_default();
    items.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    items.into_iter().map(|(_, k, v)| (k, v)).collect()
}
