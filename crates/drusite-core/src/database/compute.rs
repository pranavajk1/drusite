// SPDX-License-Identifier: AGPL-3.0-or-later

//! Computing what a database shows: every cell of every row, formulas, rollups and the far
//! side of two-way relations included, then a view's filter, sorts and groups.
//!
//! Nothing computed is stored: it is worked out from the workspace document when a view is
//! shown, so it can never disagree with the values it comes from, on any device.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use drusite_crdt::{RowData, Workspace};
use drusite_formula::{self as formula, PageRef, Value as F};
use drusite_model::database::{
    Color, DateTime, DateUnit, DateValue, Filter, FilterCondition, FilterOp, FilterValue, Grouping,
    Layout, Person, PropertyDef, PropertyKind, RelativeDay, RollupFunction, Schema, TITLE, Value,
    View,
};
use drusite_model::{Day, PageId};
use serde::{Deserialize, Serialize};
use specta::Type;

/// One cell, ready to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    pub property: String,
    /// The stored value, for a property that stores one (and the far side of a relation).
    pub value: Option<Value>,
    /// The computed value, for a formula, rollup, created or edited property.
    pub computed: Option<F>,
    /// The pages a relation links to, with their titles.
    pub pages: Vec<PageRef>,
    /// What the cell shows, as text.
    pub text: String,
    /// Why a formula couldn't be computed, when it couldn't.
    pub error: Option<String>,
}

/// A row, ready to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RowView {
    pub id: PageId,
    pub title: String,
    /// One per property, in schema order.
    pub cells: Vec<Cell>,
}

/// A group of rows: a board's column, a section of a grouped table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RowGroup {
    /// What its rows have: an option's ID, a person's ID, `"true"`; `""` for none.
    pub key: String,
    pub label: String,
    pub color: Color,
    /// The value a row dropped into this group gets; none for the empty group.
    pub value: Option<Value>,
    pub rows: Vec<RowView>,
    /// Hidden by the view (still listed, so the UI can offer to show it).
    pub hidden: bool,
}

/// What a view shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ViewResult {
    pub schema: Schema,
    pub view: View,
    /// One group, keyed `""`, when the view isn't grouped.
    pub groups: Vec<RowGroup>,
    /// Rows the filter let through.
    pub shown: u32,
    /// Rows in the database.
    pub total: u32,
}

struct Loaded {
    schema: Schema,
    rows: Vec<PageId>,
    data: HashMap<PageId, RowData>,
    titles: HashMap<PageId, String>,
}

/// One computation over the workspace: it loads the databases it needs once, and remembers each
/// computed cell.
pub(crate) struct Db<'a> {
    ws: &'a Workspace,
    loaded: RefCell<HashMap<PageId, Rc<Loaded>>>,
    memo: RefCell<HashMap<(PageId, String), Result<F, String>>>,
    busy: RefCell<HashSet<(PageId, String)>>,
    /// `twin` index: (database, property) → row → the rows that link to it.
    back: RefCell<HashMap<(PageId, String), Rc<HashMap<PageId, Vec<PageId>>>>>,
    pub(crate) now: DateTime,
    pub(crate) utc_offset: i32,
}

/// When a page was made, from its ID (a UUIDv7 holds its creation time).
pub(crate) fn id_time(id: PageId) -> i64 {
    id.as_uuid().get_timestamp().map_or(0, |t| {
        let (s, n) = t.to_unix();
        i64::try_from(s).unwrap_or(0) * 1000 + i64::from(n / 1_000_000)
    })
}

fn local(ms: i64, offset: i32) -> DateTime {
    let secs = ms.div_euclid(1000) + i64::from(offset) * 60;
    let t =
        time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let day = Day::new(t.year(), t.month().into(), t.day()).unwrap_or_else(|| {
        Day::new(1970, 1, 1).unwrap_or_else(|| unreachable!("1970-01-01 is a day"))
    });
    DateTime {
        day,
        minutes: Some(u16::from(t.hour()) * 60 + u16::from(t.minute())),
    }
}

impl<'a> Db<'a> {
    pub(crate) fn new(ws: &'a Workspace, now: DateTime, utc_offset: i32) -> Self {
        Self {
            ws,
            loaded: RefCell::default(),
            memo: RefCell::default(),
            busy: RefCell::default(),
            back: RefCell::default(),
            now,
            utc_offset,
        }
    }

    fn load(&self, db: PageId) -> Option<Rc<Loaded>> {
        if let Some(l) = self.loaded.borrow().get(&db) {
            return Some(l.clone());
        }
        let schema = self.ws.schema(db).ok()?;
        let rows = self.ws.rows(db).ok()?;
        let mut data = HashMap::new();
        let mut titles = HashMap::new();
        for &r in &rows {
            data.insert(r, self.ws.row(r, &schema).unwrap_or_default());
            titles.insert(r, self.ws.title(r).unwrap_or_default());
        }
        let l = Rc::new(Loaded {
            schema,
            rows,
            data,
            titles,
        });
        self.loaded.borrow_mut().insert(db, l.clone());
        Some(l)
    }

    pub(crate) fn schema(&self, db: PageId) -> Option<Schema> {
        self.load(db).map(|l| l.schema.clone())
    }

    fn title_of(&self, page: PageId) -> String {
        if let Some(db) = self.ws.database_of(page)
            && let Some(l) = self.load(db)
            && let Some(t) = l.titles.get(&page)
        {
            return t.clone();
        }
        self.ws.title(page).unwrap_or_default()
    }

    fn page_ref(&self, page: PageId) -> PageRef {
        PageRef {
            id: page,
            title: self.title_of(page),
        }
    }

    /// The rows of `db` whose `prop` links to each row: the far side of a two-way relation.
    fn backlinks(&self, db: PageId, prop: &str) -> Rc<HashMap<PageId, Vec<PageId>>> {
        let key = (db, prop.to_owned());
        if let Some(b) = self.back.borrow().get(&key) {
            return b.clone();
        }
        let mut map: HashMap<PageId, Vec<PageId>> = HashMap::new();
        if let Some(l) = self.load(db) {
            for r in &l.rows {
                if let Some(Value::Relation(targets)) =
                    l.data.get(r).and_then(|d| d.values.get(prop))
                {
                    for t in targets {
                        map.entry(*t).or_default().push(*r);
                    }
                }
            }
        }
        let map = Rc::new(map);
        self.back.borrow_mut().insert(key, map.clone());
        map
    }

    /// The rows of `db` whose `prop` links to `row`.
    pub(crate) fn far_side(&self, row: PageId, db: PageId, prop: &str) -> Vec<PageId> {
        self.backlinks(db, prop)
            .get(&row)
            .cloned()
            .unwrap_or_default()
    }

    /// The stored value of `prop` on `row`, the far side of a mirrored relation included.
    fn stored(&self, row: PageId, def: &PropertyDef, l: &Loaded) -> Option<Value> {
        if let PropertyKind::Relation {
            database,
            twin: Some(twin),
            mirror: true,
            ..
        } = &def.kind
        {
            let links = self.backlinks(*database, twin);
            return links
                .get(&row)
                .filter(|v| !v.is_empty())
                .map(|v| Value::Relation(v.clone()));
        }
        if matches!(def.kind, PropertyKind::Title) {
            return Some(Value::Text(l.titles.get(&row).cloned().unwrap_or_default()));
        }
        l.data
            .get(&row)
            .and_then(|d| d.values.get(&def.id))
            .cloned()
    }

    /// `prop` of `row`, as a formula sees it.
    fn formula_value(&self, row: PageId, def: &PropertyDef, l: &Loaded) -> Result<F, String> {
        let data = l.data.get(&row);
        let option_name = |id: &str| {
            def.options
                .iter()
                .find(|o| o.id == id)
                .map_or_else(String::new, |o| o.name.clone())
        };
        Ok(match &def.kind {
            PropertyKind::Formula { .. } | PropertyKind::Rollup { .. } => {
                return self.computed(row, def, l);
            }
            PropertyKind::CreatedTime => F::Date(DateValue {
                start: local(
                    data.and_then(|d| d.created).unwrap_or_else(|| id_time(row)),
                    self.utc_offset,
                ),
                end: None,
            }),
            PropertyKind::EditedTime => F::Date(DateValue {
                start: local(
                    data.and_then(|d| d.edited.or(d.created))
                        .unwrap_or_else(|| id_time(row)),
                    self.utc_offset,
                ),
                end: None,
            }),
            PropertyKind::CreatedBy => person(data.and_then(|d| d.created_by.clone())),
            PropertyKind::EditedBy => person(data.and_then(|d| d.edited_by.clone())),
            PropertyKind::Checkbox => F::Bool(matches!(
                self.stored(row, def, l),
                Some(Value::Checkbox(true))
            )),
            PropertyKind::MultiSelect
            | PropertyKind::Relation { .. }
            | PropertyKind::People
            | PropertyKind::Files => match self.stored(row, def, l) {
                Some(Value::MultiSelect(ids)) => {
                    F::List(ids.iter().map(|i| F::Text(option_name(i))).collect())
                }
                Some(Value::Relation(pages)) => {
                    F::List(pages.iter().map(|p| F::Page(self.page_ref(*p))).collect())
                }
                Some(Value::People(people)) => F::List(people.into_iter().map(F::Person).collect()),
                Some(Value::Files(files)) => {
                    F::List(files.into_iter().map(|f| F::Text(f.name)).collect())
                }
                _ => F::List(Vec::new()),
            },
            _ => match self.stored(row, def, l) {
                None => F::Empty,
                Some(Value::Text(s) | Value::Url(s) | Value::Email(s) | Value::Phone(s)) => {
                    if s.is_empty() { F::Empty } else { F::Text(s) }
                }
                Some(Value::Number(n)) => F::Number(n),
                Some(Value::Select(id) | Value::Status(id)) => F::Text(option_name(&id)),
                Some(Value::Date(d)) => F::Date(d),
                Some(Value::Checkbox(b)) => F::Bool(b),
                Some(Value::UniqueId(n)) => F::Number(f64::from(n)),
                Some(_) => F::Empty,
            },
        })
    }

    /// A formula's or rollup's value on `row`, computed once.
    fn computed(&self, row: PageId, def: &PropertyDef, l: &Loaded) -> Result<F, String> {
        let key = (row, def.id.clone());
        if let Some(v) = self.memo.borrow().get(&key) {
            return v.clone();
        }
        if !self.busy.borrow_mut().insert(key.clone()) {
            return Err(format!("“{}” depends on itself", def.name));
        }
        let out = match &def.kind {
            PropertyKind::Formula { expression } => formula::parse(expression)
                .and_then(|e| formula::eval(&e, &RowCtx { db: self, row, l }))
                .map_err(|e| e.message),
            PropertyKind::Rollup {
                relation,
                target,
                function,
            } => self.rollup(row, l, relation, target, *function),
            _ => Ok(F::Empty),
        };
        self.busy.borrow_mut().remove(&key);
        self.memo.borrow_mut().insert(key, out.clone());
        out
    }

    fn rollup(
        &self,
        row: PageId,
        l: &Loaded,
        relation: &str,
        target: &str,
        function: RollupFunction,
    ) -> Result<F, String> {
        let Some(rel) = l.schema.property(relation) else {
            return Err("the rollup's relation is gone".into());
        };
        let PropertyKind::Relation { database, .. } = &rel.kind else {
            return Err("a rollup needs a relation".into());
        };
        let pages = match self.stored(row, rel, l) {
            Some(Value::Relation(p)) => p,
            _ => Vec::new(),
        };
        let Some(other) = self.load(*database) else {
            return Err("the related database is gone".into());
        };
        let Some(tdef) = other.schema.property(target).cloned() else {
            return Err("the rolled-up property is gone".into());
        };
        let mut values = Vec::new();
        for p in &pages {
            values.push(self.formula_value(*p, &tdef, &other).unwrap_or(F::Empty));
        }
        Ok(roll(function, &values))
    }

    /// Every cell of `row`.
    fn row_view(&self, row: PageId, l: &Loaded) -> RowView {
        let cells = l
            .schema
            .properties
            .iter()
            .map(|def| self.cell(row, def, l))
            .collect();
        RowView {
            id: row,
            title: l.titles.get(&row).cloned().unwrap_or_default(),
            cells,
        }
    }

    fn cell(&self, row: PageId, def: &PropertyDef, l: &Loaded) -> Cell {
        let mut cell = Cell {
            property: def.id.clone(),
            value: None,
            computed: None,
            pages: Vec::new(),
            text: String::new(),
            error: None,
        };
        match &def.kind {
            PropertyKind::Formula { .. }
            | PropertyKind::Rollup { .. }
            | PropertyKind::CreatedTime
            | PropertyKind::EditedTime
            | PropertyKind::CreatedBy
            | PropertyKind::EditedBy => match self.formula_value(row, def, l) {
                Ok(v) => {
                    cell.text = match (&def.kind, &v) {
                        (PropertyKind::Rollup { function, .. }, F::Number(n))
                            if is_percent(*function) =>
                        {
                            format!("{}%", formula::show_number((n * 1000.0).round() / 10.0))
                        }
                        _ => v.show(),
                    };
                    cell.computed = Some(v);
                }
                Err(e) => {
                    cell.error = Some(e);
                }
            },
            _ => {
                let v = self.stored(row, def, l);
                cell.text = match &v {
                    Some(v) => show_value(v, def, self),
                    None => String::new(),
                };
                if let Some(Value::Relation(pages)) = &v {
                    cell.pages = pages.iter().map(|p| self.page_ref(*p)).collect();
                }
                cell.value = v;
            }
        }
        cell
    }

    /// Whether `row` passes `filter`.
    fn passes(&self, row: PageId, filter: &Filter, l: &Loaded, today: Day) -> bool {
        match filter {
            Filter::And { filters } => filters.iter().all(|f| self.passes(row, f, l, today)),
            Filter::Or { filters } => {
                filters.is_empty() || filters.iter().any(|f| self.passes(row, f, l, today))
            }
            Filter::Condition(c) => {
                let Some(def) = l.schema.property(&c.property) else {
                    // A condition on a property that's gone lets everything through.
                    return true;
                };
                let stored = if def.kind.is_stored()
                    || matches!(
                        def.kind,
                        PropertyKind::Title | PropertyKind::Relation { .. }
                    ) {
                    self.stored(row, def, l)
                } else {
                    None
                };
                let fv = self.formula_value(row, def, l).unwrap_or(F::Empty);
                condition(c, def, stored.as_ref(), &fv, today)
            }
        }
    }

    /// The order key of a cell for sorting: options sort by their place, others by value.
    fn sort_key(&self, row: PageId, def: &PropertyDef, l: &Loaded) -> F {
        match self.stored(row, def, l) {
            Some(Value::Select(id) | Value::Status(id)) if def.kind.has_options() => def
                .options
                .iter()
                .position(|o| o.id == id)
                .map_or(F::Empty, |i| F::Number(i as f64)),
            _ => match self.formula_value(row, def, l).unwrap_or(F::Empty) {
                F::Text(s) => F::Text(s.to_lowercase()),
                F::List(items) => items.into_iter().next().unwrap_or(F::Empty),
                F::Bool(false) => F::Empty,
                other => other,
            },
        }
    }

    /// What `view` of `db` shows, on `today`.
    pub(crate) fn query(&self, db: PageId, view: &View, today: Day) -> Option<ViewResult> {
        let l = self.load(db)?;
        let total = l.rows.len();
        let mut rows: Vec<PageId> = l
            .rows
            .iter()
            .copied()
            .filter(|r| {
                view.filter
                    .as_ref()
                    .is_none_or(|f| self.passes(*r, f, &l, today))
            })
            .collect();
        if !view.sorts.is_empty() {
            let keys: HashMap<PageId, Vec<F>> = rows
                .iter()
                .map(|r| {
                    let k = view
                        .sorts
                        .iter()
                        .map(|s| {
                            l.schema
                                .property(&s.property)
                                .map_or(F::Empty, |d| self.sort_key(*r, d, &l))
                        })
                        .collect();
                    (*r, k)
                })
                .collect();
            rows.sort_by(|a, b| {
                for (i, s) in view.sorts.iter().enumerate() {
                    let (x, y) = (&keys[a][i], &keys[b][i]);
                    // Empty values go last, whichever way the sort runs.
                    let o = match (matches!(x, F::Empty), matches!(y, F::Empty)) {
                        (true, true) => Ordering::Equal,
                        (true, false) => return Ordering::Greater,
                        (false, true) => return Ordering::Less,
                        _ => compare(x, y),
                    };
                    let o = if s.descending { o.reverse() } else { o };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            });
        }
        let shown = rows.len();
        let group_by = match (&view.group, view.layout) {
            (Some(g), _) => Some(g.clone()),
            // A board with no grouping groups by its first status or select property.
            (None, Layout::Board) => l
                .schema
                .properties
                .iter()
                .find(|p| matches!(p.kind, PropertyKind::Status))
                .or_else(|| {
                    l.schema
                        .properties
                        .iter()
                        .find(|p| matches!(p.kind, PropertyKind::Select))
                })
                .map(|p| Grouping {
                    property: p.id.clone(),
                    hidden: Vec::new(),
                    hide_empty: false,
                    date_unit: None,
                }),
            _ => None,
        };
        let groups =
            match group_by.and_then(|g| l.schema.property(&g.property).cloned().map(|d| (g, d))) {
                None => vec![RowGroup {
                    key: String::new(),
                    label: String::new(),
                    color: Color::Default,
                    value: None,
                    rows: rows.iter().map(|r| self.row_view(*r, &l)).collect(),
                    hidden: false,
                }],
                Some((g, def)) => self.group(&rows, &g, &def, &l),
            };
        Some(ViewResult {
            schema: l.schema.clone(),
            view: view.clone(),
            groups,
            shown: u32::try_from(shown).unwrap_or(u32::MAX),
            total: u32::try_from(total).unwrap_or(u32::MAX),
        })
    }

    fn group(&self, rows: &[PageId], g: &Grouping, def: &PropertyDef, l: &Loaded) -> Vec<RowGroup> {
        let mut groups: Vec<RowGroup> = Vec::new();
        let hidden = g.hidden.clone();
        let add = |groups: &mut Vec<RowGroup>,
                   key: String,
                   label: String,
                   color: Color,
                   value: Option<Value>| {
            if !groups.iter().any(|x| x.key == key) {
                groups.push(RowGroup {
                    hidden: hidden.contains(&key),
                    key,
                    label,
                    color,
                    value,
                    rows: Vec::new(),
                });
            }
        };
        let no = format!("No {}", def.name);
        // The groups a property's options make come first, in the options' order.
        match &def.kind {
            PropertyKind::Select | PropertyKind::Status | PropertyKind::MultiSelect => {
                add(&mut groups, String::new(), no.clone(), Color::Default, None);
                for o in &def.options {
                    let value = match def.kind {
                        PropertyKind::Status => Value::Status(o.id.clone()),
                        PropertyKind::MultiSelect => Value::MultiSelect(vec![o.id.clone()]),
                        _ => Value::Select(o.id.clone()),
                    };
                    add(
                        &mut groups,
                        o.id.clone(),
                        o.name.clone(),
                        o.color,
                        Some(value),
                    );
                }
            }
            PropertyKind::Checkbox => {
                add(
                    &mut groups,
                    "true".into(),
                    "Checked".into(),
                    Color::Default,
                    Some(Value::Checkbox(true)),
                );
                add(
                    &mut groups,
                    "false".into(),
                    "Unchecked".into(),
                    Color::Default,
                    Some(Value::Checkbox(false)),
                );
            }
            _ => add(&mut groups, String::new(), no.clone(), Color::Default, None),
        }
        for r in rows {
            let keys = self.group_keys(*r, def, g, l);
            for (key, label, value) in keys {
                add(&mut groups, key.clone(), label, Color::Default, value);
                if let Some(grp) = groups.iter_mut().find(|x| x.key == key) {
                    grp.rows.push(self.row_view(*r, l));
                }
            }
        }
        // The empty group comes last on a board, as in Notion, unless it was first.
        if g.hide_empty {
            groups.retain(|x| !x.rows.is_empty());
        }
        if !def.kind.has_options() && !matches!(def.kind, PropertyKind::Checkbox) {
            groups.sort_by(|a, b| match (a.key.is_empty(), b.key.is_empty()) {
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                _ => a.key.cmp(&b.key),
            });
        }
        groups
    }

    /// The groups a row falls in: one, or one per item of a multi-valued property.
    fn group_keys(
        &self,
        row: PageId,
        def: &PropertyDef,
        g: &Grouping,
        l: &Loaded,
    ) -> Vec<(String, String, Option<Value>)> {
        let none = || vec![(String::new(), String::new(), None)];
        match self.stored(row, def, l) {
            Some(Value::Select(id)) => vec![(id.clone(), String::new(), Some(Value::Select(id)))],
            Some(Value::Status(id)) => vec![(id.clone(), String::new(), Some(Value::Status(id)))],
            Some(Value::MultiSelect(ids)) if !ids.is_empty() => ids
                .into_iter()
                .map(|i| (i.clone(), String::new(), Some(Value::MultiSelect(vec![i]))))
                .collect(),
            Some(Value::Checkbox(b)) => {
                vec![(b.to_string(), String::new(), Some(Value::Checkbox(b)))]
            }
            None if matches!(def.kind, PropertyKind::Checkbox) => {
                vec![("false".into(), String::new(), Some(Value::Checkbox(false)))]
            }
            Some(Value::People(people)) if !people.is_empty() => people
                .into_iter()
                .map(|p| (p.id.clone(), p.name.clone(), Some(Value::People(vec![p]))))
                .collect(),
            Some(Value::Relation(pages)) if !pages.is_empty() => pages
                .into_iter()
                .map(|p| {
                    (
                        p.to_string(),
                        self.title_of(p),
                        Some(Value::Relation(vec![p])),
                    )
                })
                .collect(),
            Some(Value::Date(d)) => {
                let (key, label) = date_group(d.start.day, g.date_unit.unwrap_or(DateUnit::Month));
                vec![(key, label, None)]
            }
            Some(v) if !v.is_empty() && def.kind.is_stored() => {
                let text = show_value(&v, def, self);
                vec![(text.clone(), text, Some(v))]
            }
            _ => match self.formula_value(row, def, l) {
                Ok(v) if !v.is_empty() && !def.kind.is_stored() => {
                    let text = v.show();
                    vec![(text.clone(), text, None)]
                }
                _ => none(),
            },
        }
    }

    /// One row's cells, for its page.
    pub(crate) fn row(&self, row: PageId) -> Option<(Schema, RowView)> {
        let db = self.ws.database_of(row)?;
        let l = self.load(db)?;
        Some((l.schema.clone(), self.row_view(row, &l)))
    }

    /// Every row's cells, for the search index and the Markdown folder.
    pub(crate) fn all_rows(&self, db: PageId) -> Vec<RowView> {
        let Some(l) = self.load(db) else {
            return Vec::new();
        };
        l.rows.iter().map(|r| self.row_view(*r, &l)).collect()
    }
}

fn person(p: Option<Person>) -> F {
    p.map_or(F::Empty, F::Person)
}

fn compare(a: &F, b: &F) -> Ordering {
    match (a, b) {
        (F::Number(x), F::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (F::Text(x), F::Text(y)) => x.cmp(y),
        (F::Bool(x), F::Bool(y)) => x.cmp(y),
        (F::Date(x), F::Date(y)) => x.start.cmp(&y.start),
        (F::Person(x), F::Person(y)) => x.name.cmp(&y.name),
        (F::Page(x), F::Page(y)) => x.title.cmp(&y.title),
        _ => Ordering::Equal,
    }
}

fn is_percent(f: RollupFunction) -> bool {
    use RollupFunction as R;
    matches!(
        f,
        R::PercentEmpty | R::PercentNotEmpty | R::PercentChecked | R::PercentUnchecked
    )
}

/// A rollup's function over the values it reached.
pub(crate) fn roll(function: RollupFunction, values: &[F]) -> F {
    use RollupFunction as R;
    let flat: Vec<F> = values
        .iter()
        .flat_map(|v| match v {
            F::List(items) => items.clone(),
            other => vec![other.clone()],
        })
        .collect();
    let filled: Vec<&F> = flat
        .iter()
        .filter(|v| !matches!(v, F::Empty) && !matches!(v, F::Text(s) if s.is_empty()))
        .collect();
    let nums: Vec<f64> = flat
        .iter()
        .filter_map(|v| match v {
            F::Number(n) => Some(*n),
            _ => None,
        })
        .collect();
    let dates: Vec<DateValue> = flat
        .iter()
        .filter_map(|v| match v {
            F::Date(d) => Some(*d),
            _ => None,
        })
        .collect();
    let count = |n: usize| F::Number(n as f64);
    let empties = values
        .iter()
        .filter(|v| v.is_empty() && !matches!(v, F::Bool(false)))
        .count();
    let checked = flat.iter().filter(|v| matches!(v, F::Bool(true))).count();
    let frac = |n: usize| {
        if values.is_empty() {
            F::Number(0.0)
        } else {
            F::Number(n as f64 / values.len() as f64)
        }
    };
    let unique = || {
        let mut out: Vec<F> = Vec::new();
        for v in &filled {
            if !out.contains(v) {
                out.push((*v).clone());
            }
        }
        out
    };
    match function {
        R::ShowOriginal => F::List(filled.into_iter().cloned().collect()),
        R::ShowUnique => F::List(unique()),
        R::CountAll => count(values.len()),
        R::CountValues => count(filled.len()),
        R::CountUniqueValues => count(unique().len()),
        R::CountEmpty => count(empties),
        R::CountNotEmpty => count(values.len() - empties),
        R::PercentEmpty => frac(empties),
        R::PercentNotEmpty => frac(values.len() - empties),
        R::Sum => F::Number(nums.iter().sum()),
        R::Average if nums.is_empty() => F::Empty,
        R::Average => F::Number(nums.iter().sum::<f64>() / nums.len() as f64),
        R::Median | R::Min | R::Max | R::Range if nums.is_empty() => F::Empty,
        R::Median => {
            let mut v = nums.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
            let m = v.len() / 2;
            F::Number(if v.len() % 2 == 0 {
                f64::midpoint(v[m - 1], v[m])
            } else {
                v[m]
            })
        }
        R::Min => F::Number(nums.iter().copied().fold(f64::INFINITY, f64::min)),
        R::Max => F::Number(nums.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
        R::Range => F::Number(
            nums.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                - nums.iter().copied().fold(f64::INFINITY, f64::min),
        ),
        R::EarliestDate | R::LatestDate | R::DateRange if dates.is_empty() => F::Empty,
        R::EarliestDate => F::Date(DateValue {
            start: dates
                .iter()
                .map(|d| d.start)
                .min()
                .unwrap_or(dates[0].start),
            end: None,
        }),
        R::LatestDate => F::Date(DateValue {
            start: dates
                .iter()
                .map(|d| d.end.unwrap_or(d.start))
                .max()
                .unwrap_or(dates[0].start),
            end: None,
        }),
        R::DateRange => F::Date(DateValue {
            start: dates
                .iter()
                .map(|d| d.start)
                .min()
                .unwrap_or(dates[0].start),
            end: dates.iter().map(|d| d.end.unwrap_or(d.start)).max(),
        }),
        R::Checked => count(checked),
        R::Unchecked => count(values.len().saturating_sub(checked)),
        R::PercentChecked => frac(checked),
        R::PercentUnchecked => frac(values.len().saturating_sub(checked)),
    }
}

/// A stored value as its cell shows it.
pub(crate) fn show_value(v: &Value, def: &PropertyDef, db: &Db<'_>) -> String {
    let opt = |id: &str| {
        def.options
            .iter()
            .find(|o| o.id == id)
            .map_or_else(String::new, |o| o.name.clone())
    };
    match v {
        Value::Text(s) | Value::Url(s) | Value::Email(s) | Value::Phone(s) => s.clone(),
        Value::Number(n) => match def.kind {
            PropertyKind::Number { format } => format.show(*n),
            _ => formula::show_number(*n),
        },
        Value::Select(id) | Value::Status(id) => opt(id),
        Value::MultiSelect(ids) => ids.iter().map(|i| opt(i)).collect::<Vec<_>>().join(", "),
        Value::Date(d) => formula::show_date(d),
        Value::Checkbox(b) => if *b { "Yes" } else { "" }.to_owned(),
        Value::Files(f) => f
            .iter()
            .map(|f| f.name.clone())
            .collect::<Vec<_>>()
            .join(", "),
        Value::People(p) => p
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>()
            .join(", "),
        Value::Relation(pages) => pages
            .iter()
            .map(|p| db.title_of(*p))
            .collect::<Vec<_>>()
            .join(", "),
        Value::UniqueId(n) => match &def.kind {
            PropertyKind::UniqueId { prefix: Some(p) } => format!("{p}-{n}"),
            _ => n.to_string(),
        },
    }
}

fn date_group(day: Day, unit: DateUnit) -> (String, String) {
    match unit {
        DateUnit::Day => (day.to_string(), day.title()),
        DateUnit::Week => {
            // Weeks start on Monday.
            let mut d = day;
            for _ in 0..7 {
                if d.weekday() == "Monday" {
                    break;
                }
                d = shift(d, -1);
            }
            (
                d.to_string(),
                format!("Week of {} {} {}", d.day(), d.month_name(), d.year()),
            )
        }
        DateUnit::Month => (
            format!("{:04}-{:02}", day.year(), day.month()),
            format!("{} {}", day.month_name(), day.year()),
        ),
        DateUnit::Year => (day.year().to_string(), day.year().to_string()),
    }
}

pub(crate) fn shift(day: Day, days: i64) -> Day {
    let d = DateTime::day(day);
    drusite_formula::parse(&format!("dateAdd(parseDate(\"{d}\"), {days}, \"days\")"))
        .ok()
        .and_then(|e| formula::eval(&e, &NoCtx).ok())
        .and_then(|v| match v {
            F::Date(d) => Some(d.start.day),
            _ => None,
        })
        .unwrap_or(day)
}

fn relative(r: RelativeDay, today: Day) -> Day {
    match r {
        RelativeDay::Today => today,
        RelativeDay::Tomorrow => shift(today, 1),
        RelativeDay::Yesterday => shift(today, -1),
        RelativeDay::OneWeekAgo => shift(today, -7),
        RelativeDay::OneWeekFromNow => shift(today, 7),
        RelativeDay::OneMonthAgo => shift(today, -30),
        RelativeDay::OneMonthFromNow => shift(today, 30),
    }
}

/// Whether one condition holds: `stored` is the stored value (with option IDs), `fv` the
/// value as a formula sees it (with names).
fn condition(
    c: &FilterCondition,
    def: &PropertyDef,
    stored: Option<&Value>,
    fv: &F,
    today: Day,
) -> bool {
    use FilterOp as O;
    let empty = match stored {
        Some(v) => v.is_empty(),
        None => match fv {
            F::Bool(_) => false,
            other => other.is_empty() && !matches!(other, F::Number(_)),
        },
    };
    match c.op {
        O::IsEmpty => return empty,
        O::IsNotEmpty => return !empty,
        _ => {}
    }
    // Options, people and rows, by ID.
    let ids: Option<Vec<String>> = match stored {
        Some(Value::Select(i) | Value::Status(i)) => Some(vec![i.clone()]),
        Some(Value::MultiSelect(i)) => Some(i.clone()),
        Some(Value::People(p)) => Some(p.iter().map(|p| p.id.clone()).collect()),
        Some(Value::Relation(p)) => Some(p.iter().map(ToString::to_string).collect()),
        None if def.kind.has_options()
            || matches!(
                def.kind,
                PropertyKind::People | PropertyKind::Relation { .. }
            ) =>
        {
            Some(Vec::new())
        }
        _ => None,
    };
    if let (Some(ids), Some(FilterValue::Id(want))) = (&ids, &c.value) {
        let has = ids.contains(want) || {
            // A status filter may name a whole group.
            def.options
                .iter()
                .any(|o| ids.contains(&o.id) && o.group.is_some_and(|g| g.key() == want))
        };
        return match c.op {
            O::Equals | O::Contains => has,
            O::NotEquals | O::NotContains => !has,
            _ => false,
        };
    }
    let as_day = |v: &F| match v {
        F::Date(d) => Some(d.start.day),
        _ => None,
    };
    match (&c.value, fv) {
        (_, F::Date(_)) | (Some(FilterValue::Date(_) | FilterValue::Relative(_)), _)
            if matches!(
                c.op,
                O::Equals
                    | O::Before
                    | O::After
                    | O::OnOrBefore
                    | O::OnOrAfter
                    | O::PastWeek
                    | O::PastMonth
                    | O::PastYear
                    | O::NextWeek
                    | O::NextMonth
                    | O::NextYear
                    | O::ThisWeek
                    | O::NotEquals
            ) =>
        {
            let Some(day) = as_day(fv) else {
                return false;
            };
            let want = match &c.value {
                Some(FilterValue::Date(d)) => Some(*d),
                Some(FilterValue::Relative(r)) => Some(relative(*r, today)),
                _ => None,
            };
            let between = |a: i64, b: i64| {
                let lo = shift(today, a);
                let hi = shift(today, b);
                day >= lo && day <= hi
            };
            match c.op {
                O::Equals => want == Some(day),
                O::NotEquals => want != Some(day),
                O::Before => want.is_some_and(|w| day < w),
                O::After => want.is_some_and(|w| day > w),
                O::OnOrBefore => want.is_some_and(|w| day <= w),
                O::OnOrAfter => want.is_some_and(|w| day >= w),
                O::PastWeek => between(-7, 0),
                O::PastMonth => between(-30, 0),
                O::PastYear => between(-365, 0),
                O::NextWeek => between(0, 7),
                O::NextMonth => between(0, 30),
                O::NextYear => between(0, 365),
                O::ThisWeek => {
                    let mut monday = today;
                    for _ in 0..7 {
                        if monday.weekday() == "Monday" {
                            break;
                        }
                        monday = shift(monday, -1);
                    }
                    day >= monday && day <= shift(monday, 6)
                }
                _ => false,
            }
        }
        (Some(FilterValue::Number(want)), _) => {
            let have = match fv {
                F::Number(n) => *n,
                _ => return matches!(c.op, O::NotEquals),
            };
            match c.op {
                O::Equals => have == *want,
                O::NotEquals => have != *want,
                O::Greater => have > *want,
                O::Less => have < *want,
                O::GreaterOrEqual => have >= *want,
                O::LessOrEqual => have <= *want,
                _ => false,
            }
        }
        (Some(FilterValue::Checkbox(want)), _) => {
            let have = matches!(fv, F::Bool(true));
            match c.op {
                O::NotEquals => have != *want,
                _ => have == *want,
            }
        }
        (Some(FilterValue::Text(want) | FilterValue::Id(want)), _) => {
            let have = match fv {
                F::List(items) => items.iter().map(F::show).collect::<Vec<_>>().join(", "),
                other => other.show(),
            };
            let (h, w) = (have.to_lowercase(), want.to_lowercase());
            match c.op {
                O::Equals => h == w,
                O::NotEquals => h != w,
                O::Contains => h.contains(&w),
                O::NotContains => !h.contains(&w),
                O::StartsWith => h.starts_with(&w),
                O::EndsWith => h.ends_with(&w),
                _ => false,
            }
        }
        _ => true,
    }
}

struct RowCtx<'b, 'a> {
    db: &'b Db<'a>,
    row: PageId,
    l: &'b Loaded,
}

impl formula::Context for RowCtx<'_, '_> {
    fn prop(&self, page: Option<PageId>, name: &str) -> Result<F, formula::Error> {
        let fail = |m: String| formula::Error {
            at: None,
            message: m,
        };
        let (row, l) = match page {
            None => (self.row, None),
            Some(p) => {
                let db = self
                    .db
                    .ws
                    .database_of(p)
                    .ok_or_else(|| fail("that page isn't in a database".into()))?;
                (p, self.db.load(db))
            }
        };
        let l = l.as_deref().unwrap_or(self.l);
        let def = l
            .schema
            .property_named(name)
            .ok_or_else(|| fail(format!("there's no property called “{name}”")))?;
        self.db.formula_value(row, def, l).map_err(fail)
    }

    fn this_page(&self) -> PageRef {
        self.db.page_ref(self.row)
    }

    fn now(&self) -> DateTime {
        self.db.now
    }

    fn utc_offset(&self) -> i32 {
        self.db.utc_offset
    }
}

struct NoCtx;

impl formula::Context for NoCtx {
    fn prop(&self, _: Option<PageId>, name: &str) -> Result<F, formula::Error> {
        Err(formula::Error {
            at: None,
            message: format!("no property {name}"),
        })
    }

    fn this_page(&self) -> PageRef {
        PageRef {
            id: PageId::from_uuid(uuid::Uuid::nil()),
            title: String::new(),
        }
    }

    fn now(&self) -> DateTime {
        DateTime::day(Day::new(2000, 1, 1).unwrap_or_else(|| unreachable!("a real day")))
    }

    fn utc_offset(&self) -> i32 {
        0
    }
}

/// The formula types of a database's properties, for the checker.
pub(crate) struct Types<'a>(pub(crate) &'a Schema);

impl formula::TypeEnv for Types<'_> {
    fn prop(&self, name: &str) -> Option<formula::Type> {
        use formula::Type as T;
        let def = self.0.property_named(name)?;
        Some(match &def.kind {
            PropertyKind::Title
            | PropertyKind::Text
            | PropertyKind::Select
            | PropertyKind::Status
            | PropertyKind::Url
            | PropertyKind::Email
            | PropertyKind::Phone => T::Text,
            PropertyKind::Number { .. } | PropertyKind::UniqueId { .. } => T::Number,
            PropertyKind::MultiSelect | PropertyKind::Files => T::List(Box::new(T::Text)),
            PropertyKind::Date | PropertyKind::CreatedTime | PropertyKind::EditedTime => T::Date,
            PropertyKind::Checkbox => T::Bool,
            PropertyKind::People => T::List(Box::new(T::Person)),
            PropertyKind::CreatedBy | PropertyKind::EditedBy => T::Person,
            PropertyKind::Relation { .. } => T::List(Box::new(T::Page)),
            PropertyKind::Rollup { function, .. } => match function {
                RollupFunction::ShowOriginal | RollupFunction::ShowUnique => {
                    T::List(Box::new(T::Any))
                }
                RollupFunction::EarliestDate
                | RollupFunction::LatestDate
                | RollupFunction::DateRange => T::Date,
                _ => T::Number,
            },
            // A formula's type is its own formula's, as far as it can be told without looping.
            PropertyKind::Formula { expression } => formula::parse(expression)
                .ok()
                .and_then(|e| formula::check(&e, &NoFormulas(self.0)).ok())
                .unwrap_or(T::Any),
        })
    }
}

/// Like [`Types`], but formulas are `Any`, so formulas naming each other can't loop.
struct NoFormulas<'a>(&'a Schema);

impl formula::TypeEnv for NoFormulas<'_> {
    fn prop(&self, name: &str) -> Option<formula::Type> {
        let def = self.0.property_named(name)?;
        match def.kind {
            PropertyKind::Formula { .. } => Some(formula::Type::Any),
            _ => Types(self.0).prop(name),
        }
    }
}
