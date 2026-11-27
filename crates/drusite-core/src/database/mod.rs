// SPDX-License-Identifier: AGPL-3.0-or-later

//! Databases through the Core API: making them (from a template, for tasks), changing their
//! schemas and views, setting cells, and showing views ([`compute`]).

pub use drusite_db::{compute, markdown};
pub mod notion;
#[cfg(feature = "net")]
pub mod notion_api;
pub mod zip;

use drusite_model::database::{
    Color, DateTime, Layout, Person, PropertyDef, PropertyKind, Schema, SelectOption, StatusGroup,
    TITLE, Value, View, ViewProperty, short_id,
};
use drusite_model::{Day, PageId};
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::api::{ApiError, ErrorCode, PageInfo};
use crate::engine::Core;
pub use compute::{Cell, RowGroup, RowView, ViewResult};

/// What a new database starts with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum Template {
    /// A title and a table.
    #[default]
    Empty,
    /// Tasks: status, due date, priority and assignee, with a board to work from.
    Tasks,
}

/// The person's clock, which formulas and relative filters ("today") use.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Clock {
    /// The local date and time, `YYYY-MM-DDTHH:MM`.
    pub now: DateTime,
    /// Minutes ahead of UTC.
    pub utc_offset: i32,
}

impl Clock {
    /// UTC, for callers without a person's clock (agents, the Markdown folder).
    pub fn utc() -> Self {
        let now = time::OffsetDateTime::now_utc();
        let day = Day::new(now.year(), now.month().into(), now.day())
            .unwrap_or_else(|| unreachable!("today is a day"));
        Self {
            now: DateTime {
                day,
                minutes: Some(u16::from(now.hour()) * 60 + u16::from(now.minute())),
            },
            utc_offset: 0,
        }
    }
}

/// How a formula checks out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FormulaCheck {
    /// Its type, when Notion would accept it.
    pub result: Option<drusite_formula::Type>,
    pub error: Option<drusite_formula::Error>,
}

fn invalid(m: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Invalid, m)
}

fn option(name: &str, color: Color, group: Option<StatusGroup>, position: f64) -> SelectOption {
    SelectOption {
        id: short_id(),
        name: name.into(),
        color,
        group,
        position,
    }
}

fn def(name: &str, kind: PropertyKind, position: f64) -> PropertyDef {
    PropertyDef {
        id: short_id(),
        name: name.into(),
        kind,
        options: Vec::new(),
        position,
        description: None,
    }
}

/// The default status options Notion gives a new status property.
pub fn default_status_options() -> Vec<SelectOption> {
    vec![
        option("Not started", Color::Default, Some(StatusGroup::ToDo), 0.0),
        option(
            "In progress",
            Color::Blue,
            Some(StatusGroup::InProgress),
            1.0,
        ),
        option("Done", Color::Green, Some(StatusGroup::Complete), 2.0),
    ]
}

impl Core {
    pub(crate) fn now_ms() -> i64 {
        crate::trash::now_ms()
    }

    /// The person this device acts as, for created-by and edited-by: its account, if signed in.
    pub(crate) fn me(&self) -> Option<Person> {
        let email = self.store.account().ok().flatten()?.email?;
        Some(Person {
            id: email.clone(),
            name: email.split('@').next().unwrap_or(&email).to_owned(),
            email: Some(email),
        })
    }

    pub(crate) fn create_database(
        &mut self,
        parent: Option<PageId>,
        title: &str,
        inline: bool,
        template: Template,
    ) -> Result<(PageInfo, Schema), ApiError> {
        let (page, mut schema) = self.workspace.create_database(parent, title, inline)?;
        let db = page.id();
        self.store.create_page(&page)?;
        if template == Template::Tasks {
            self.tasks_template(db, &mut schema)?;
        }
        self.save_workspace()?;
        let info = self
            .pages()?
            .into_iter()
            .find(|p| p.id == db)
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "database vanished"))?;
        Ok((info, self.workspace.schema(db)?))
    }

    fn tasks_template(&mut self, db: PageId, schema: &mut Schema) -> Result<(), ApiError> {
        let mut title = schema
            .property(TITLE)
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "no title"))?;
        title.name = "Task".into();
        self.workspace.put_property(db, &title)?;
        let mut status = def("Status", PropertyKind::Status, 1.0);
        status.options = default_status_options();
        let due = def("Due", PropertyKind::Date, 2.0);
        let mut priority = def("Priority", PropertyKind::Select, 3.0);
        priority.options = vec![
            option("High", Color::Red, None, 0.0),
            option("Medium", Color::Yellow, None, 1.0),
            option("Low", Color::Gray, None, 2.0),
        ];
        let assignee = def("Assignee", PropertyKind::People, 4.0);
        for p in [&status, &due, &priority, &assignee] {
            self.workspace.put_property(db, p)?;
        }
        let shown = |ids: &[&str]| {
            ids.iter()
                .map(|id| ViewProperty {
                    id: (*id).to_owned(),
                    width: None,
                })
                .collect::<Vec<_>>()
        };
        // The table the database came with becomes "All tasks"; the board comes first.
        let mut table = schema.views[0].clone();
        table.name = "All tasks".into();
        table.position = 1.0;
        table.properties = shown(&[TITLE, &status.id, &due.id, &priority.id, &assignee.id]);
        self.workspace.put_view(db, &table)?;
        let mut board = View::new(short_id(), "Board", Layout::Board);
        board.group = Some(drusite_model::database::Grouping {
            property: status.id.clone(),
            hidden: vec![String::new()],
            hide_empty: false,
            date_unit: None,
        });
        board.properties = shown(&[TITLE, &due.id, &priority.id, &assignee.id]);
        self.workspace.put_view(db, &board)?;
        let mut calendar = View::new(short_id(), "Calendar", Layout::Calendar);
        calendar.position = 2.0;
        calendar.date_property = Some(due.id.clone());
        calendar.properties = shown(&[TITLE]);
        self.workspace.put_view(db, &calendar)?;
        *schema = self.workspace.schema(db)?;
        Ok(())
    }

    pub(crate) fn query_view(
        &self,
        db: PageId,
        view: Option<&str>,
        clock: Clock,
    ) -> Result<ViewResult, ApiError> {
        let schema = self.workspace.schema(db)?;
        let view = match view {
            Some(id) => schema
                .views
                .iter()
                .find(|v| v.id == id)
                .cloned()
                .ok_or_else(|| invalid("no such view"))?,
            None => schema.views.first().cloned().unwrap_or_else(|| {
                let mut v = View::new(short_id(), "Table", Layout::Table);
                v.properties = schema
                    .properties
                    .iter()
                    .map(|p| ViewProperty {
                        id: p.id.clone(),
                        width: None,
                    })
                    .collect();
                v
            }),
        };
        let db_ = compute::Db::new(&self.workspace, clock.now, clock.utc_offset);
        db_.query(db, &view, clock.now.day)
            .ok_or_else(|| invalid("that page isn't a database"))
    }

    pub(crate) fn row_cells(
        &self,
        row: PageId,
        clock: Clock,
    ) -> Result<Option<(Schema, RowView)>, ApiError> {
        let db = compute::Db::new(&self.workspace, clock.now, clock.utc_offset);
        Ok(db.row(row))
    }

    /// Adds or changes a property. A new relation with `twin_name` gets its other side in the
    /// related database too.
    pub(crate) fn put_property(
        &mut self,
        db: PageId,
        mut prop: PropertyDef,
        twin_name: Option<String>,
    ) -> Result<Schema, ApiError> {
        let schema = self.workspace.schema(db)?;
        let old = schema.property(&prop.id).cloned();
        if let PropertyKind::Formula { expression } = &prop.kind {
            // Refused here as Notion's editor refuses it.
            let expr = drusite_formula::parse(expression).map_err(|e| invalid(e.message))?;
            let mut with = schema.clone();
            with.properties.retain(|p| p.id != prop.id);
            with.properties.push(prop.clone());
            drusite_formula::check(&expr, &compute::Types(&with))
                .map_err(|e| invalid(e.message))?;
        }
        if prop.kind.has_options()
            && prop.options.is_empty()
            && old.is_none()
            && matches!(prop.kind, PropertyKind::Status)
        {
            prop.options = default_status_options();
        }
        if let PropertyKind::Relation {
            database: other,
            twin: None,
            mirror: false,
            single,
        } = prop.kind.clone()
            && let Some(name) = twin_name
        {
            if !self.workspace.is_database(other) {
                return Err(invalid("a relation must point at a database"));
            }
            let twin_id = short_id();
            let position = self
                .workspace
                .schema(other)?
                .properties
                .iter()
                .map(|p| p.position)
                .fold(0.0, f64::max)
                + 1.0;
            let twin = PropertyDef {
                id: twin_id.clone(),
                name: if name.trim().is_empty() {
                    format!("Related to {}", schema.title)
                } else {
                    name
                },
                kind: PropertyKind::Relation {
                    database: db,
                    twin: Some(prop.id.clone()),
                    mirror: true,
                    single: false,
                },
                options: Vec::new(),
                position,
                description: None,
            };
            self.workspace.put_property(other, &twin)?;
            prop.kind = PropertyKind::Relation {
                database: other,
                twin: Some(twin_id),
                mirror: false,
                single,
            };
        }
        self.workspace.put_property(db, &prop)?;
        // A new property shows in every view that lists properties.
        if old.is_none() {
            for mut v in self.workspace.schema(db)?.views {
                if !v.properties.iter().any(|p| p.id == prop.id) {
                    v.properties.push(ViewProperty {
                        id: prop.id.clone(),
                        width: None,
                    });
                    self.workspace.put_view(db, &v)?;
                }
            }
        }
        self.save_workspace()?;
        Ok(self.workspace.schema(db)?)
    }

    pub(crate) fn delete_property(&mut self, db: PageId, prop: &str) -> Result<Schema, ApiError> {
        let schema = self.workspace.schema(db)?;
        if let Some(PropertyDef {
            kind:
                PropertyKind::Relation {
                    database,
                    twin: Some(twin),
                    ..
                },
            ..
        }) = schema.property(prop)
            && self.workspace.is_database(*database)
        {
            self.workspace.delete_property(*database, twin)?;
        }
        self.workspace.delete_property(db, prop)?;
        self.save_workspace()?;
        Ok(self.workspace.schema(db)?)
    }

    pub(crate) fn put_view(&mut self, db: PageId, view: &View) -> Result<Schema, ApiError> {
        self.workspace.put_view(db, view)?;
        self.save_workspace()?;
        Ok(self.workspace.schema(db)?)
    }

    pub(crate) fn delete_view(&mut self, db: PageId, view: &str) -> Result<Schema, ApiError> {
        if self.workspace.schema(db)?.views.len() <= 1 {
            return Err(invalid("a database keeps at least one view"));
        }
        self.workspace.delete_view(db, &view.to_owned())?;
        self.save_workspace()?;
        Ok(self.workspace.schema(db)?)
    }

    /// Sets one cell without saving (for imports and batches).
    pub(crate) fn set_cell_unsaved(
        &mut self,
        row: PageId,
        prop: &str,
        value: Option<Value>,
    ) -> Result<(), ApiError> {
        let db = self
            .workspace
            .database_of(row)
            .ok_or_else(|| invalid("that page isn't a row of a database"))?;
        let schema = self.workspace.schema(db)?;
        let def = schema
            .property(prop)
            .ok_or_else(|| invalid("no such property"))?
            .clone();
        let value = value.filter(|v| !v.is_empty());
        match &def.kind {
            PropertyKind::Title => {
                let title = match &value {
                    Some(Value::Text(s)) => s.clone(),
                    _ => String::new(),
                };
                self.workspace.set_title(row, &title)?;
            }
            PropertyKind::Relation {
                database,
                twin: Some(twin),
                mirror: true,
                ..
            } => {
                // The far side: change the rows on the other end.
                let have = compute::Db::new(&self.workspace, Clock::utc().now, 0)
                    .far_side(row, *database, twin);
                let want = match &value {
                    Some(Value::Relation(p)) => p.clone(),
                    _ => Vec::new(),
                };
                let other = self.workspace.schema(*database)?;
                for p in have
                    .iter()
                    .filter(|p| !want.contains(p))
                    .chain(want.iter().filter(|p| !have.contains(p)))
                {
                    let mut links = match self.workspace.row(*p, &other)?.values.remove(twin) {
                        Some(Value::Relation(l)) => l,
                        _ => Vec::new(),
                    };
                    if want.contains(p) {
                        links.push(row);
                    } else {
                        links.retain(|x| *x != row);
                    }
                    self.workspace
                        .set_value(*p, twin, Some(&Value::Relation(links)))?;
                }
            }
            kind if kind.is_stored() => {
                let mut value = value;
                if let (Some(v), false) = (&value, value.as_ref().is_none_or(|v| v.fits(kind))) {
                    return Err(invalid(format!(
                        "a {} property can't hold that value ({v:?})",
                        kind.name()
                    )));
                }
                if let (PropertyKind::Relation { single: true, .. }, Some(Value::Relation(p))) =
                    (kind, &mut value)
                {
                    p.truncate(1);
                }
                if let Some(Value::Select(id) | Value::Status(id)) = &value
                    && !def.options.iter().any(|o| &o.id == id)
                {
                    return Err(invalid("no such option"));
                }
                self.workspace.set_value(row, prop, value.as_ref())?;
            }
            _ => return Err(invalid("that property is computed, not set")),
        }
        let me = self.me();
        self.workspace.touch_row(row, Self::now_ms(), me.as_ref())?;
        Ok(())
    }

    pub(crate) fn set_cell(
        &mut self,
        row: PageId,
        prop: &str,
        value: Option<Value>,
    ) -> Result<(), ApiError> {
        self.set_cell_unsaved(row, prop, value)?;
        self.save_workspace()
    }

    /// Makes a row, after `after` (or last), with `values`. New rows get the next unique ID and
    /// a status property's first to-do option, as in Notion.
    pub(crate) fn create_row(
        &mut self,
        db: PageId,
        title: &str,
        values: Vec<(String, Value)>,
        after: Option<PageId>,
    ) -> Result<PageInfo, ApiError> {
        let schema = self.workspace.schema(db)?;
        let page = self.workspace.create_page(Some(db), title)?;
        let row = page.id();
        self.store.create_page(&page)?;
        if let Some(after) = after
            && let Some(i) = self.workspace.rows(db)?.iter().position(|r| *r == after)
        {
            self.workspace.move_page(row, Some(db), i + 1)?;
        }
        let me = self.me();
        self.workspace.set_row_history(
            row,
            None,
            me.as_ref(),
            Some(Self::now_ms()),
            me.as_ref(),
        )?;
        for p in &schema.properties {
            match &p.kind {
                PropertyKind::UniqueId { .. } => {
                    let next = self
                        .workspace
                        .rows(db)?
                        .iter()
                        .filter_map(|r| {
                            match self.workspace.row(*r, &schema).ok()?.values.get(&p.id) {
                                Some(Value::UniqueId(n)) => Some(*n),
                                _ => None,
                            }
                        })
                        .max()
                        .unwrap_or(0)
                        + 1;
                    self.workspace
                        .set_value(row, &p.id, Some(&Value::UniqueId(next)))?;
                }
                PropertyKind::Status if !values.iter().any(|(id, _)| id == &p.id) => {
                    if let Some(o) = p
                        .options
                        .iter()
                        .find(|o| o.group == Some(StatusGroup::ToDo))
                        .or(p.options.first())
                    {
                        self.workspace
                            .set_value(row, &p.id, Some(&Value::Status(o.id.clone())))?;
                    }
                }
                _ => {}
            }
        }
        for (prop, value) in values {
            self.set_cell_unsaved(row, &prop, Some(value))?;
        }
        self.save_workspace()?;
        self.pages()?
            .into_iter()
            .find(|p| p.id == row)
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "row vanished"))
    }

    pub(crate) fn check_formula(
        &self,
        db: PageId,
        expression: &str,
    ) -> Result<FormulaCheck, ApiError> {
        let schema = self.workspace.schema(db)?;
        Ok(
            match drusite_formula::parse(expression)
                .and_then(|e| drusite_formula::check(&e, &compute::Types(&schema)))
            {
                Ok(t) => FormulaCheck {
                    result: Some(t),
                    error: None,
                },
                Err(e) => FormulaCheck {
                    result: None,
                    error: Some(e),
                },
            },
        )
    }

    /// A row page was edited: note when, and by whom.
    pub(crate) fn row_touched(&mut self, page: PageId) {
        if self.workspace.database_of(page).is_some() {
            let me = self.me();
            let before = self.workspace.version();
            if self
                .workspace
                .touch_row(page, Self::now_ms(), me.as_ref())
                .is_ok()
                && self.workspace.version() != before
            {
                let _ = self.save_workspace();
            }
        }
    }
}

impl Core {
    /// A database row's frontmatter (its values), or a database's schema block, for its file.
    pub(crate) fn database_front(
        &self,
        page: PageId,
        db: &compute::Db<'_>,
    ) -> (Vec<drusite_model::Property>, String) {
        if self.workspace.is_database(page) {
            return match self.workspace.schema(page) {
                Ok(s) => (Vec::new(), markdown::schema_yaml(&s)),
                Err(_) => (Vec::new(), String::new()),
            };
        }
        match db.row(page) {
            Some((schema, row)) => (markdown::row_properties(&schema, &row), String::new()),
            None => (Vec::new(), String::new()),
        }
    }

    /// A property read from a row's file: its value when the database has a property of that
    /// name (true when it was taken as one), else nothing (it's the page's own property).
    pub(crate) fn row_property_from_file(
        &mut self,
        row: PageId,
        name: &str,
        value: Option<&drusite_model::PropertyValue>,
    ) -> Result<bool, ApiError> {
        let Some(db) = self.workspace.database_of(row) else {
            return Ok(false);
        };
        let schema = self.workspace.schema(db)?;
        let Some(def) = schema.property_named(name).cloned() else {
            return Ok(false);
        };
        if !def.kind.is_stored() || def.id == TITLE {
            // Computed values in the file are for reading; the title is the file's name.
            return Ok(true);
        }
        let parsed = match value {
            None => Some(None),
            Some(v) => markdown::value_from_property(&def, v, &self.workspace),
        };
        // A value that can't be read (an option that doesn't exist) is left as it was.
        if let Some(v) = parsed {
            self.workspace.set_value(row, &def.id, v.as_ref())?;
        }
        Ok(true)
    }
}
