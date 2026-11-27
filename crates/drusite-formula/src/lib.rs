// SPDX-License-Identifier: AGPL-3.0-or-later

//! Notion's formula language, as Formulas 2.0 has it: [`parse`] reads a formula, [`check`]
//! works out its type (refusing what Notion refuses), and [`eval`] computes it for one row.
//!
//! Its values are numbers, text, checkboxes, dates (with an optional time and end), people,
//! pages and lists of those, and *empty* — what an unset property gives. As in Notion, an empty
//! value acts as the zero of whatever it meets (`prop("Num") + 1` is 1 when Num is unset) and
//! shows as nothing.
//!
//! Behaviour follows what Notion itself shows: `tests/notion_oracle.txt` holds results read
//! from Notion for ~120 formulas, and the tests check every one. What this engine doesn't do is
//! listed in [`UNSUPPORTED`].

mod check;
mod date;
mod eval;
mod parse;

use drusite_model::PageId;
use drusite_model::database::{DateValue, Person, js_number};
use serde::{Deserialize, Serialize};
use specta::Type as SpectaType;

pub use check::{Type, TypeEnv, check};
pub use date::show_date;
pub use eval::{Context, eval};
pub use parse::{Binary, Expr, Unary, parse};

/// Notion functions this engine doesn't have, and why.
pub const UNSUPPORTED: &[(&str, &str)] = &[
    (
        "style/unstyle colours",
        "the text is kept; its colour and weight are not shown",
    ),
    ("link", "the label is kept as text, not as a link"),
    (
        "formatDate time zones",
        "times are the person's own; `Z`/`ZZ` tokens print nothing",
    ),
    ("ai, button, place", "Notion AI functions and place values"),
];

/// A formula that can't be read, or that Notion would refuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SpectaType)]
#[serde(rename_all = "camelCase")]
pub struct Error {
    /// Where in the formula, in bytes, when known.
    pub at: Option<u32>,
    pub message: String,
}

impl Error {
    pub(crate) fn syntax(at: usize, message: impl Into<String>) -> Self {
        Self {
            at: u32::try_from(at).ok(),
            message: message.into(),
        }
    }

    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            at: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// A page a formula reaches, through a relation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, SpectaType)]
#[serde(rename_all = "camelCase")]
pub struct PageRef {
    pub id: PageId,
    pub title: String,
}

/// A formula's value, and the value a rollup shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, SpectaType)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Value {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Date(DateValue),
    Person(Person),
    Page(PageRef),
    List(Vec<Value>),
}

impl Value {
    /// Whether `empty()` holds for it: nothing, `0`, `""`, `false` or `[]`.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Number(n) => *n == 0.0 || n.is_nan(),
            Self::Text(s) => s.is_empty(),
            Self::Bool(b) => !b,
            Self::List(l) => l.is_empty(),
            Self::Date(_) | Self::Person(_) | Self::Page(_) => false,
        }
    }

    /// How Notion shows it in a cell: numbers to 12 decimal places, lists with `, ` between
    /// items, dates as `November 27, 2026`, nothing as nothing.
    pub fn show(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Number(n) => show_number(*n),
            Self::Text(s) => s.clone(),
            Self::Bool(b) => b.to_string(),
            Self::Date(d) => show_date(d),
            Self::Person(p) => p.name.clone(),
            Self::Page(p) => p.title.clone(),
            Self::List(items) => items.iter().map(Self::show).collect::<Vec<_>>().join(", "),
        }
    }

    /// What `format()` makes of it: numbers in full, list items joined with `,`.
    pub fn format(&self) -> String {
        match self {
            Self::Number(n) if !n.is_finite() => String::new(),
            Self::Number(n) => js_number(*n),
            Self::List(items) => items.iter().map(Self::format).collect::<Vec<_>>().join(","),
            other => other.show(),
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::Empty => Some(0.0),
            _ => None,
        }
    }
}

/// A number as a cell shows it: rounded to 12 decimal places.
pub fn show_number(n: f64) -> String {
    if !n.is_finite() {
        return String::new();
    }
    let rounded: f64 = format!("{n:.12}").parse().unwrap_or(n);
    js_number(rounded)
}
