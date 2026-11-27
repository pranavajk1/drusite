// SPDX-License-Identifier: AGPL-3.0-or-later

//! Databases for agents: reading one through a view, adding and changing rows, making one.
//! Values go in and out as text a person would type, the Markdown folder's form
//! (`drusite_db::markdown`).

use std::collections::BTreeMap;

use drusite_crdt::Workspace;
use drusite_db::compute::{Db, utc_now};
use drusite_db::markdown::{row_properties, value_from_property};
use drusite_model::database::{
    Layout, PropertyDef, PropertyKind, StatusGroup, TITLE, View, ViewProperty, short_id,
};
use drusite_model::{PageId, Property, PropertyValue};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::markdown;
use crate::tools::{Tools, page_id, quote};
use crate::{Activity, Change, Effect, Notes, ToolError, ToolResult};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct QueryDatabase {
    /// The database's page ID.
    pub(crate) database: String,
    /// A view's name or ID; the database's first view if left out.
    pub(crate) view: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AddRow {
    /// The database's page ID.
    pub(crate) database: String,
    pub(crate) title: String,
    /// Values by property name, as text.
    pub(crate) values: Option<BTreeMap<String, Option<String>>>,
    /// The row page's content, as Markdown.
    pub(crate) markdown: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct UpdateRow {
    /// The row's page ID.
    pub(crate) row: String,
    /// Values by property name, as text; null clears one.
    pub(crate) values: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct CreateDatabase {
    pub(crate) title: String,
    /// "tasks" for a task database with a board; leave it out for an empty one.
    pub(crate) template: Option<String>,
    /// The page to put it under; leave it out for the top level.
    pub(crate) parent: Option<String>,
    /// The workspace's ID, for a top-level database; leave it out when there is one.
    pub(crate) workspace: Option<String>,
}

/// A row's values, as frontmatter properties (none if the page isn't a row).
pub(crate) fn row_values(ws: &Workspace, page: PageId) -> Vec<Property> {
    let db = Db::new(ws, utc_now(), 0);
    match db.row(page) {
        Some((schema, row)) => row_properties(&schema, &row),
        None => Vec::new(),
    }
}

/// Splits frontmatter into a row's values (properties its database has) and the page's own.
pub(crate) fn split_row_values(
    ws: &Workspace,
    page: PageId,
    props: &[Property],
) -> (Vec<Property>, Vec<Property>) {
    let Some(schema) = ws.database_of(page).and_then(|d| ws.schema(d).ok()) else {
        return (Vec::new(), props.to_vec());
    };
    props
        .iter()
        .cloned()
        .partition(|p| schema.property_named(&p.name).is_some())
}

/// Sets a row's values from text, by property name. Fails on a name the database doesn't have
/// or a value that can't be read, before changing anything.
pub(crate) fn set_row_values(
    ws: &mut Workspace,
    row: PageId,
    values: &[Property],
) -> ToolResult<()> {
    if values.is_empty() {
        return Ok(());
    }
    let db = ws
        .database_of(row)
        .ok_or_else(|| ToolError::Invalid("that page isn't a row of a database".to_owned()))?;
    let schema = ws.schema(db)?;
    let current = row_values(ws, row);
    let mut writes = Vec::new();
    for p in values {
        let def = schema.property_named(&p.name).ok_or_else(|| {
            ToolError::Invalid(format!("the database has no property {}", quote(&p.name)))
        })?;
        if current
            .iter()
            .any(|c| c.name == p.name && c.value == p.value)
        {
            continue;
        }
        if def.id == TITLE || !def.kind.is_stored() {
            if def.id == TITLE {
                continue;
            }
            return Err(ToolError::Invalid(format!(
                "{} is computed ({}); it can't be set",
                quote(&p.name),
                def.kind.name()
            )));
        }
        let value = value_from_property(def, &p.value, ws).ok_or_else(|| {
            ToolError::Invalid(format!(
                "{} can't be read as {}",
                quote(&text(&p.value)),
                describe(def)
            ))
        })?;
        writes.push((def.id.clone(), value));
    }
    for (id, value) in writes {
        // The far side of a two-way relation lives on the other rows.
        ws.set_value(row, &id, value.as_ref())?;
    }
    Ok(())
}

fn text(v: &PropertyValue) -> String {
    match v {
        PropertyValue::Text(s) => s.clone(),
        PropertyValue::Number(n) => n.to_string(),
        PropertyValue::Checkbox(b) => b.to_string(),
        PropertyValue::Date(d) => d.to_string(),
    }
}

fn describe(def: &PropertyDef) -> String {
    let opts = def
        .options
        .iter()
        .map(|o| o.name.as_str())
        .collect::<Vec<_>>();
    match &def.kind {
        PropertyKind::Select | PropertyKind::Status => format!("one of: {}", opts.join(", ")),
        PropertyKind::MultiSelect => format!("some of: {}", opts.join(", ")),
        PropertyKind::Date => "a date (YYYY-MM-DD, YYYY-MM-DDTHH:MM, or start → end)".to_owned(),
        PropertyKind::Relation { .. } => "[[Row title]]s of the related database".to_owned(),
        other => format!("a {}", other.name()),
    }
}

fn values_of(map: &BTreeMap<String, Option<String>>) -> Vec<Property> {
    map.iter()
        .map(|(name, v)| Property {
            name: name.clone(),
            value: PropertyValue::Text(v.clone().unwrap_or_default()),
        })
        .collect()
}

fn cell_text(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

impl<N: Notes> Tools<'_, N> {
    pub(crate) async fn query_database(&self, a: QueryDatabase) -> ToolResult<String> {
        let at = self.locate(&a.database).await?;
        if !at.ws.is_database(at.node.id) {
            return Err(ToolError::Invalid(
                "that page isn't a database (read it with read_page)".to_owned(),
            ));
        }
        let schema = at.ws.schema(at.node.id)?;
        let view = match &a.view {
            Some(v) => schema
                .views
                .iter()
                .find(|x| x.id == *v || x.name.eq_ignore_ascii_case(v))
                .cloned()
                .ok_or_else(|| ToolError::NotFound(format!("no view {}", quote(v))))?,
            None => schema
                .views
                .first()
                .cloned()
                .unwrap_or_else(|| View::new(short_id(), "Table", Layout::Table)),
        };
        let now = utc_now();
        let db = Db::new(&at.ws, now, 0);
        let result = db
            .query(at.node.id, &view, now.day)
            .ok_or_else(|| ToolError::Server("the view couldn't be computed".to_owned()))?;
        let mut out = format!(
            "# {} (database, page {})\n\n",
            if schema.title.is_empty() {
                "Untitled"
            } else {
                &schema.title
            },
            at.node.id
        );
        out.push_str("Properties:\n");
        for p in &schema.properties {
            let extra = match &p.kind {
                PropertyKind::Formula { expression } => format!(" = {expression}"),
                _ if !p.options.is_empty() => format!(
                    ": {}",
                    p.options
                        .iter()
                        .map(|o| o.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
                _ => String::new(),
            };
            out.push_str(&format!("- {} ({}){extra}\n", p.name, p.kind.name()));
        }
        let views = schema
            .views
            .iter()
            .map(|v| format!("{} ({})", v.name, v.layout.name()))
            .collect::<Vec<_>>();
        out.push_str(&format!(
            "\nViews: {}. Showing {}: {} of {} rows.\n",
            views.join(", "),
            quote(&view.name),
            result.shown,
            result.total
        ));
        // The view's properties, else all.
        let shown: Vec<&PropertyDef> = {
            let list: Vec<&PropertyDef> = view
                .properties
                .iter()
                .filter_map(|vp| schema.property(&vp.id))
                .collect();
            if list.len() > 1 {
                list
            } else {
                schema.properties.iter().collect()
            }
        };
        for g in &result.groups {
            if g.hidden {
                continue;
            }
            if result.groups.len() > 1 || !g.key.is_empty() {
                out.push_str(&format!(
                    "\n## {} ({})\n",
                    if g.label.is_empty() {
                        "No value"
                    } else {
                        &g.label
                    },
                    g.rows.len()
                ));
            }
            if g.rows.is_empty() {
                continue;
            }
            out.push('\n');
            out.push_str(&format!(
                "| {} | row |\n",
                shown
                    .iter()
                    .map(|p| cell_text(&p.name))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
            out.push_str(&format!("|{}---|\n", " --- |".repeat(shown.len())));
            for r in &g.rows {
                let cells: Vec<String> = shown
                    .iter()
                    .map(|p| {
                        if p.id == TITLE {
                            return cell_text(&r.title);
                        }
                        r.cells
                            .iter()
                            .find(|c| c.property == p.id)
                            .map(|c| {
                                cell_text(c.error.as_deref().map_or(c.text.as_str(), |_| "#error"))
                            })
                            .unwrap_or_default()
                    })
                    .collect();
                out.push_str(&format!("| {} | {} |\n", cells.join(" | "), r.id));
            }
        }
        Ok(out)
    }

    pub(crate) async fn add_row(&self, a: AddRow) -> ToolResult<String> {
        let mut at = self.locate(&a.database).await?;
        self.may_write(&at.info, Some(&at.node))?;
        let db = at.node.id;
        if !at.ws.is_database(db) {
            return Err(ToolError::Invalid("that page isn't a database".to_owned()));
        }
        let ws = &mut at.ws;
        let before = ws.version();
        let mut page = ws.create_page(Some(db), a.title.trim())?;
        let id = page.id();
        // A new row starts in its status's first to-do option, as in the app.
        let schema = ws.schema(db)?;
        for p in &schema.properties {
            if matches!(p.kind, PropertyKind::Status)
                && let Some(o) = p
                    .options
                    .iter()
                    .find(|o| o.group == Some(StatusGroup::ToDo))
                    .or(p.options.first())
            {
                ws.set_value(
                    id,
                    &p.id,
                    Some(&drusite_model::database::Value::Status(o.id.clone())),
                )?;
            }
        }
        if let Some(v) = &a.values {
            set_row_values(ws, id, &values_of(v))?;
        }
        if let Some(md) = &a.markdown {
            let read = markdown::read(md, &[]);
            let mut wanted = markdown::wanted(&read);
            for w in &mut wanted {
                w.id = None;
            }
            if !wanted.is_empty() {
                drusite_crdt::merge::apply(&mut page, &wanted)?;
            }
        }
        let change = Change {
            created: vec![(id, page.export_snapshot()?)],
            pages: Vec::new(),
            workspace: Some(ws.export_updates(&before)?),
        };
        let name = quote(a.title.trim());
        let activity = Activity {
            tool: "add_row".to_owned(),
            summary: format!("Added {name} to {}", quote(&at.node.title)),
            effects: vec![Effect::Created { page: id }],
        };
        self.notes
            .commit(self.agent, at.info.id, change, activity)
            .await?;
        Ok(format!("Added {name} (page {id})."))
    }

    pub(crate) async fn update_row(&self, a: UpdateRow) -> ToolResult<String> {
        let mut at = self.locate(&a.row).await?;
        self.may_write(&at.info, Some(&at.node))?;
        let id = at.node.id;
        let ws = &mut at.ws;
        let before = ws.version();
        set_row_values(ws, id, &values_of(&a.values))?;
        let after = ws.version();
        if after == before {
            return Ok("Nothing changed: the row already has those values.".to_owned());
        }
        let change = Change {
            created: Vec::new(),
            pages: Vec::new(),
            workspace: Some(ws.export_updates(&before)?),
        };
        let summary = format!(
            "Set {} on {}",
            a.values
                .keys()
                .map(|k| quote(k))
                .collect::<Vec<_>>()
                .join(", "),
            quote(&at.node.title)
        );
        let activity = Activity {
            tool: "update_row".to_owned(),
            summary: summary.clone(),
            effects: vec![Effect::Workspace {
                before: before.encode(),
                after: after.encode(),
            }],
        };
        self.notes
            .commit(self.agent, at.info.id, change, activity)
            .await?;
        Ok(format!("{summary}."))
    }

    pub(crate) async fn create_database(&self, a: CreateDatabase) -> ToolResult<String> {
        let (info, mut ws, parent) = match &a.parent {
            Some(p) => {
                let at = self.locate(p).await?;
                self.may_write(&at.info, Some(&at.node))?;
                (at.info, at.ws, Some(at.node.id))
            }
            None => {
                let info = self.pick(a.workspace.as_deref()).await?;
                self.may_write(&info, None)?;
                let ws = self.notes.workspace(self.agent, info.id).await?;
                (info, ws, None)
            }
        };
        let tasks = match a.template.as_deref().map(str::trim) {
            None | Some("" | "empty") => false,
            Some("tasks") => true,
            Some(other) => {
                return Err(ToolError::Invalid(format!(
                    "no template {}: use \"tasks\" or leave it out",
                    quote(other)
                )));
            }
        };
        let before = ws.version();
        let (page, schema) = ws.create_database(parent, a.title.trim(), false)?;
        let id = page.id();
        if tasks {
            tasks_template(&mut ws, id, &schema)?;
        }
        let change = Change {
            created: vec![(id, page.export_snapshot()?)],
            pages: Vec::new(),
            workspace: Some(ws.export_updates(&before)?),
        };
        let name = quote(a.title.trim());
        let activity = Activity {
            tool: "create_database".to_owned(),
            summary: format!("Created the database {name}"),
            effects: vec![Effect::Created { page: id }],
        };
        self.notes
            .commit(self.agent, info.id, change, activity)
            .await?;
        Ok(format!(
            "Created the database {name} (page {id}). Add rows with add_row."
        ))
    }
}

/// The tasks template, as the app makes it (`drusite_core::database`).
fn tasks_template(
    ws: &mut Workspace,
    db: PageId,
    schema: &drusite_model::database::Schema,
) -> ToolResult<()> {
    use drusite_model::database::{Color, Grouping, SelectOption};
    let opt = |name: &str, color, group, position| SelectOption {
        id: short_id(),
        name: name.to_owned(),
        color,
        group,
        position,
    };
    let prop = |name: &str, kind, options, position| PropertyDef {
        id: short_id(),
        name: name.to_owned(),
        kind,
        options,
        position,
        description: None,
    };
    let mut title = schema
        .property(TITLE)
        .cloned()
        .ok_or_else(|| ToolError::Server("no title".to_owned()))?;
    title.name = "Task".to_owned();
    ws.put_property(db, &title)?;
    let status = prop(
        "Status",
        PropertyKind::Status,
        vec![
            opt("Not started", Color::Default, Some(StatusGroup::ToDo), 0.0),
            opt(
                "In progress",
                Color::Blue,
                Some(StatusGroup::InProgress),
                1.0,
            ),
            opt("Done", Color::Green, Some(StatusGroup::Complete), 2.0),
        ],
        1.0,
    );
    let due = prop("Due", PropertyKind::Date, Vec::new(), 2.0);
    let priority = prop(
        "Priority",
        PropertyKind::Select,
        vec![
            opt("High", Color::Red, None, 0.0),
            opt("Medium", Color::Yellow, None, 1.0),
            opt("Low", Color::Gray, None, 2.0),
        ],
        3.0,
    );
    let assignee = prop("Assignee", PropertyKind::People, Vec::new(), 4.0);
    for p in [&status, &due, &priority, &assignee] {
        ws.put_property(db, p)?;
    }
    let shown = |ids: &[&str]| {
        ids.iter()
            .map(|id| ViewProperty {
                id: (*id).to_owned(),
                width: None,
            })
            .collect::<Vec<_>>()
    };
    let mut table = schema.views[0].clone();
    table.name = "All tasks".to_owned();
    table.position = 1.0;
    table.properties = shown(&[TITLE, &status.id, &due.id, &priority.id, &assignee.id]);
    ws.put_view(db, &table)?;
    let mut board = View::new(short_id(), "Board", Layout::Board);
    board.group = Some(Grouping {
        property: status.id.clone(),
        hidden: vec![String::new()],
        hide_empty: false,
        date_unit: None,
    });
    board.properties = shown(&[TITLE, &due.id, &priority.id, &assignee.id]);
    ws.put_view(db, &board)?;
    let mut calendar = View::new(short_id(), "Calendar", Layout::Calendar);
    calendar.position = 2.0;
    calendar.date_property = Some(due.id.clone());
    calendar.properties = shown(&[TITLE]);
    ws.put_view(db, &calendar)?;
    let _ = page_id;
    Ok(())
}
