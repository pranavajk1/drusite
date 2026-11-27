// SPDX-License-Identifier: AGPL-3.0-or-later

//! Databases as Markdown, for the Markdown folder and agents.
//!
//! The form (documented in `docs/markdown-folder.md`):
//!
//! - A database is a page file like any other (`Tasks.md`), with `drusite-kind: database` and a
//!   `drusite-schema` block listing its properties (name, type, options) and views. The schema is
//!   written for reading: change it in the app or through an agent's tools.
//! - Each row is a page file in the database's folder (`Tasks/Water the seedlings.md`). Its
//!   values are frontmatter, by property name, as text a person would type: an option's name,
//!   `a, b` for several, `2026-11-27`, `2026-11-27T14:30`, `2026-12-03 → 2026-12-05`, `true`,
//!   `[[Other row]]` for a relation. Editing one in the file sets it in the app.
//! - Computed values (formulas, rollups, created and edited) are written too, so a search or an
//!   agent can read them, and ignored when read back.

use drusite_crdt::Workspace;
use drusite_model::database::{
    DateTime, DateValue, Person, PropertyDef, PropertyKind, Schema, TITLE, Value,
};
use drusite_model::{PageId, Property, PropertyValue};

use super::compute::{Cell, RowView};

/// A row's values as frontmatter properties, in schema order (the title is the file's name).
pub fn row_properties(schema: &Schema, row: &RowView) -> Vec<Property> {
    let mut out = Vec::new();
    for def in &schema.properties {
        if def.id == TITLE {
            continue;
        }
        let Some(cell) = row.cells.iter().find(|c| c.property == def.id) else {
            continue;
        };
        if let Some(value) = cell_property(def, cell) {
            out.push(Property {
                name: def.name.clone(),
                value,
            });
        }
    }
    out
}

fn cell_property(def: &PropertyDef, cell: &Cell) -> Option<PropertyValue> {
    if cell.error.is_some() {
        return None;
    }
    let text = |s: String| (!s.is_empty()).then_some(PropertyValue::Text(s));
    match (&cell.value, &cell.computed) {
        (Some(Value::Number(n)), _) => Some(PropertyValue::Number(*n)),
        (Some(Value::Checkbox(b)), _) => Some(PropertyValue::Checkbox(*b)),
        (_, _) if matches!(def.kind, PropertyKind::Checkbox) => {
            Some(PropertyValue::Checkbox(false))
        }
        (Some(Value::Date(d)), _) => Some(date_property(d)),
        (Some(Value::Relation(_)), _) => text(
            cell.pages
                .iter()
                .map(|p| format!("[[{}]]", p.title))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (Some(Value::People(people)), _) => text(
            people
                .iter()
                .map(|p| p.email.clone().unwrap_or_else(|| p.name.clone()))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (Some(Value::Files(files)), _) => text(
            files
                .iter()
                .map(|f| f.url.clone().unwrap_or_else(|| f.name.clone()))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (Some(_), _) => text(cell.text.clone()),
        (None, Some(drusite_formula::Value::Number(n))) => Some(PropertyValue::Number(*n)),
        (None, Some(drusite_formula::Value::Bool(b))) => Some(PropertyValue::Checkbox(*b)),
        (None, Some(drusite_formula::Value::Date(d))) => Some(date_property(d)),
        (None, Some(_)) => text(cell.text.clone()),
        (None, None) => None,
    }
}

fn date_property(d: &DateValue) -> PropertyValue {
    match (d.start.minutes, d.end) {
        (None, None) => PropertyValue::Date(d.start.day),
        (_, None) => PropertyValue::Text(d.start.to_string()),
        (_, Some(end)) => PropertyValue::Text(format!("{} → {end}", d.start)),
    }
}

/// A property's value read back from a file, for a property that stores one. None when the
/// text can't be read as that kind (an option that doesn't exist, a date that isn't one).
pub fn value_from_property(
    def: &PropertyDef,
    value: &PropertyValue,
    ws: &Workspace,
) -> Option<Option<Value>> {
    let text = match value {
        PropertyValue::Text(s) => s.trim().to_owned(),
        PropertyValue::Number(n) => drusite_model::database::js_number(*n),
        PropertyValue::Checkbox(b) => b.to_string(),
        PropertyValue::Date(d) => d.to_string(),
    };
    if text.is_empty() {
        return Some(None);
    }
    let option = |name: &str| {
        def.options
            .iter()
            .find(|o| o.name == name)
            .or_else(|| {
                def.options
                    .iter()
                    .find(|o| o.name.eq_ignore_ascii_case(name))
            })
            .map(|o| o.id.clone())
    };
    let parts = || text.split(", ").map(str::trim).filter(|s| !s.is_empty());
    Some(Some(match &def.kind {
        PropertyKind::Text => Value::Text(text),
        PropertyKind::Url => Value::Url(text),
        PropertyKind::Email => Value::Email(text),
        PropertyKind::Phone => Value::Phone(text),
        PropertyKind::Number { .. } => match value {
            PropertyValue::Number(n) => Value::Number(*n),
            _ => Value::Number(text.replace(',', "").parse().ok()?),
        },
        PropertyKind::Checkbox => Value::Checkbox(matches!(
            text.to_lowercase().as_str(),
            "true" | "yes" | "x" | "1"
        )),
        PropertyKind::Select => Value::Select(option(&text)?),
        PropertyKind::Status => Value::Status(option(&text)?),
        PropertyKind::MultiSelect => {
            Value::MultiSelect(parts().map(option).collect::<Option<Vec<_>>>()?)
        }
        PropertyKind::Date => {
            let (a, b) = match text.split_once('→') {
                Some((a, b)) => (a.trim(), Some(b.trim())),
                None => (text.as_str(), None),
            };
            Value::Date(DateValue {
                start: DateTime::parse(a)?,
                end: match b {
                    Some(b) => Some(DateTime::parse(b)?),
                    None => None,
                },
            })
        }
        PropertyKind::People => Value::People(
            parts()
                .map(|p| Person {
                    id: p.to_owned(),
                    name: p.split('@').next().unwrap_or(p).to_owned(),
                    email: p.contains('@').then(|| p.to_owned()),
                })
                .collect(),
        ),
        PropertyKind::Relation { database, .. } => {
            let rows = ws.rows(*database).ok()?;
            let mut ids: Vec<PageId> = Vec::new();
            for p in parts() {
                let title = p.trim_start_matches("[[").trim_end_matches("]]");
                let title = title.split('|').next().unwrap_or(title);
                let id = rows
                    .iter()
                    .find(|r| ws.title(**r).is_ok_and(|t| t == title))?;
                ids.push(*id);
            }
            Value::Relation(ids)
        }
        PropertyKind::Files => Value::Files(
            parts()
                .map(|u| drusite_model::database::FileRef {
                    name: u.rsplit('/').next().unwrap_or(u).to_owned(),
                    blob: None,
                    url: Some(u.to_owned()),
                })
                .collect(),
        ),
        _ => return None,
    }))
}

/// The `drusite-schema` block a database's file carries, for reading.
pub fn schema_yaml(schema: &Schema) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut out = String::from("drusite-schema:\n  properties:\n");
    for p in &schema.properties {
        out.push_str(&format!(
            "    - name: {}\n      type: {}\n",
            q(&p.name),
            p.kind.name()
        ));
        match &p.kind {
            PropertyKind::Formula { expression } => {
                out.push_str(&format!("      formula: {}\n", q(expression)));
            }
            PropertyKind::Number { format } => {
                out.push_str(&format!("      format: {}\n", format.api_name()));
            }
            PropertyKind::Relation { database, .. } => {
                out.push_str(&format!("      database: {database}\n"));
            }
            PropertyKind::Rollup {
                relation,
                target,
                function,
            } => {
                let name = |id: &str| {
                    schema
                        .property(id)
                        .map_or_else(|| id.to_owned(), |p| p.name.clone())
                };
                out.push_str(&format!(
                    "      rollup: {{relation: {}, property: {}, function: {}}}\n",
                    q(&name(relation)),
                    q(target),
                    function.api_name()
                ));
            }
            _ => {}
        }
        if !p.options.is_empty() {
            let names: Vec<String> = p.options.iter().map(|o| q(&o.name)).collect();
            out.push_str(&format!("      options: [{}]\n", names.join(", ")));
        }
    }
    out.push_str("  views:\n");
    for v in &schema.views {
        out.push_str(&format!(
            "    - name: {}\n      layout: {}\n",
            q(&v.name),
            v.layout.name()
        ));
    }
    out
}
