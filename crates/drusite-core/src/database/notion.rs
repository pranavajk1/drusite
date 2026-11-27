// SPDX-License-Identifier: AGPL-3.0-or-later

//! Importing a Notion export (Markdown & CSV), offline.
//!
//! What an export holds, as Notion writes it (checked against a real export):
//!
//! - a page is `Title <32 hex>.md`, starting `# Title`; its subpages are in a folder named
//!   `Title` (without the ID) beside it;
//! - a database is `Title <hex>.csv` (and `Title <hex>_all.csv`, the same with every row),
//!   with its rows as pages in the folder `Title`; a row's file lists its properties as
//!   `Name: value` lines under its title;
//! - the CSV has the title column first, then the others; values as Notion shows them
//!   (`$1,200.50`, `November 28, 2026 2:30 PM (CST)`, `Yes`, `backend, docs`), and relations as
//!   `Title (path/to/Row%20<hex>.csv)`;
//! - a page links to a database as a line holding only a link to its CSV.
//!
//! The export has no property types, views, formulas or rollups: only what they showed. So
//! types are read off the values (and names: a column called Status is a status), formulas and
//! rollups come in as the values they had, and each database gets a table and, when it has a
//! status or select, a board. The report says so; importing from the API keeps all of those.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use base64::Engine as _;
use drusite_markdown::Resolver;
use drusite_model::database::{
    Color, DateTime, DateValue, FileRef, Grouping, Layout, NumberFormat, Person, PropertyDef,
    PropertyKind, SelectOption, StatusGroup, TITLE, Value, View, ViewProperty, short_id,
};
use drusite_model::{BlockKind, Day, PageId};
use serde::{Deserialize, Serialize};
use specta::Type;

use super::zip::{Export, Zip, ZipError};
use crate::api::{ApiError, ErrorCode};
use crate::attachments::{blob_id, guess_mime};
use crate::bridge::PAGE_LINK;
use crate::engine::Core;
use drusite_crdt::merge::{self, Wanted};

/// What an import from Notion made, and what it couldn't carry over.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct NotionReport {
    /// The page everything was imported under.
    pub page: Option<PageId>,
    pub pages: u32,
    pub databases: u32,
    pub rows: u32,
    pub attachments: u32,
    /// What couldn't be carried over as it was, one line each.
    pub unmapped: Vec<String>,
}

fn zip_err(e: ZipError) -> ApiError {
    ApiError::new(ErrorCode::Invalid, format!("can't read the export: {e}"))
}

/// A Notion ID at the end of a file's stem: `Title 425211431b888149a5b9fa066dbf6448`.
pub(crate) fn split_id(stem: &str) -> (&str, Option<&str>) {
    if let Some((title, id)) = stem.rsplit_once(' ')
        && id.len() == 32
        && id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return (title, Some(id));
    }
    (stem, None)
}

/// The first Notion ID anywhere in a link: the row a relation or link points at.
pub(crate) fn id_in(link: &str) -> Option<String> {
    let decoded = percent_decode(link);
    let bytes = decoded.as_bytes();
    let mut run = 0;
    let mut found = None;
    for (i, b) in bytes.iter().enumerate() {
        if b.is_ascii_hexdigit() {
            run += 1;
            if run == 32 && !bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit) {
                found = Some(decoded[i + 1 - 32..=i].to_lowercase());
            }
        } else {
            run = 0;
        }
    }
    found.or_else(|| {
        // A UUID with dashes (the API's form).
        let hex: String = decoded.chars().filter(|c| *c != '-').collect();
        (hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_lowercase())
    })
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
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

/// A CSV file's rows, as RFC 4180 has them (quotes, doubled quotes, line breaks in quotes).
pub(crate) fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                any = false;
            }
            _ => field.push(c),
        }
    }
    if any || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// One moment as Notion writes it: `November 28, 2026`, `November 28, 2026 2:30 PM (CST)`,
/// `Nov 28, 2026`, `2026-11-28`, `2026/11/28`, `11/28/2026`, `2026-11-28T14:30:00.000Z`.
pub(crate) fn parse_moment(s: &str) -> Option<DateTime> {
    let s = s.trim();
    let s = match s.rfind(" (") {
        Some(i) if s.ends_with(')') => &s[..i],
        _ => s,
    };
    if let Some(d) = DateTime::parse(s) {
        return Some(d);
    }
    let (date_part, time_part) = match s.find(", ") {
        // "November 28, 2026 2:30 PM": the year is the word after ", ".
        Some(i) => {
            let rest = &s[i + 2..];
            match rest.split_once(' ') {
                Some((year, time)) => (format!("{} {year}", &s[..i]), Some(time.to_owned())),
                None => (format!("{} {rest}", &s[..i]), None),
            }
        }
        None => match s.split_once(' ') {
            Some((d, t)) if d.contains('/') => (d.to_owned(), Some(t.to_owned())),
            _ => (s.to_owned(), None),
        },
    };
    let day = parse_day(&date_part)?;
    let minutes = match time_part {
        None => None,
        Some(t) => Some(parse_time(&t)?),
    };
    Some(DateTime { day, minutes })
}

fn parse_day(s: &str) -> Option<Day> {
    let s = s.trim();
    if let Ok(d) = s.parse::<Day>() {
        return Some(d);
    }
    let parts: Vec<&str> = s.split(['/', '-']).collect();
    if parts.len() == 3 && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())) {
        let n: Vec<i32> = parts.iter().filter_map(|p| p.parse().ok()).collect();
        return if parts[0].len() == 4 {
            Day::new(n[0], u8::try_from(n[1]).ok()?, u8::try_from(n[2]).ok()?)
        } else {
            // Notion's US form, month first.
            Day::new(n[2], u8::try_from(n[0]).ok()?, u8::try_from(n[1]).ok()?)
        };
    }
    // "November 28 2026" (after the comma went) or "28 November 2026".
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() != 3 {
        return None;
    }
    let month = |w: &str| {
        let w = w.to_lowercase();
        MONTHS
            .iter()
            .position(|m| m.starts_with(&w) && w.len() >= 3)
            .and_then(|i| u8::try_from(i + 1).ok())
    };
    let year: i32 = words[2].parse().ok()?;
    if let Some(m) = month(words[0]) {
        return Day::new(year, m, words[1].parse().ok()?);
    }
    Day::new(year, month(words[1])?, words[0].parse().ok()?)
}

fn parse_time(t: &str) -> Option<u16> {
    let t = t.trim();
    let (clock, ampm) = match t.split_once(' ') {
        Some((c, a)) => (c, Some(a.to_uppercase())),
        None => (t, None),
    };
    let (h, m) = clock.split_once(':')?;
    let mut h: u16 = h.parse().ok()?;
    let m: u16 = m.get(..2)?.parse().ok()?;
    match ampm.as_deref() {
        Some("PM") if h < 12 => h += 12,
        Some("AM") if h == 12 => h = 0,
        _ => {}
    }
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// A date value: one moment, or `start → end`.
pub(crate) fn parse_date(s: &str) -> Option<DateValue> {
    match s.split_once('→') {
        Some((a, b)) => Some(DateValue {
            start: parse_moment(a)?,
            end: Some(parse_moment(b)?),
        }),
        None => Some(DateValue {
            start: parse_moment(s)?,
            end: None,
        }),
    }
}

/// A number as Notion shows it, and the format that shows it so.
fn parse_number(s: &str) -> Option<(f64, NumberFormat)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let mut format = NumberFormat::Number;
    let mut body = body.to_owned();
    if let Some(b) = body.strip_suffix('%') {
        body = b.to_owned();
        format = NumberFormat::Percent;
    } else {
        for f in NumberFormat::ALL {
            if let Some(sym) = f.symbol()
                && let Some(b) = body.strip_prefix(sym.trim_end())
            {
                body = b.trim().to_owned();
                format = f;
                break;
            }
        }
    }
    if body.contains(',') {
        if format == NumberFormat::Number {
            format = NumberFormat::NumberWithCommas;
        }
        body = body.replace(',', "");
    }
    let mut n: f64 = body.parse().ok()?;
    if format == NumberFormat::Percent {
        n /= 100.0;
    }
    Some((if negative { -n } else { n }, format))
}

/// Items of a relation cell: `Title (path), Other, with comma (path)`.
fn relation_items(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        // Each item ends at "(path)" whose path names a page file.
        let Some(open) = rest.find(" (") else {
            break;
        };
        let Some(close_rel) = rest[open..].find(')') else {
            break;
        };
        let title = rest[..open].trim().to_owned();
        let path = rest[open + 2..open + close_rel].to_owned();
        out.push((title, path));
        rest = rest[open + close_rel + 1..]
            .trim_start_matches(',')
            .trim_start();
    }
    out
}

fn is_relation_cell(s: &str) -> bool {
    let items = relation_items(s);
    !items.is_empty() && items.iter().all(|(_, p)| id_in(p).is_some())
}

fn status_group(name: &str) -> StatusGroup {
    let n = name.to_lowercase();
    if [
        "done",
        "complete",
        "completed",
        "shipped",
        "finished",
        "closed",
        "archived",
    ]
    .iter()
    .any(|w| n.contains(w))
    {
        StatusGroup::Complete
    } else if [
        "progress", "doing", "started", "review", "active", "blocked", "testing",
    ]
    .iter()
    .any(|w| n.contains(w) && !n.contains("not started"))
    {
        StatusGroup::InProgress
    } else {
        StatusGroup::ToDo
    }
}

const PALETTE: [Color; 9] = [
    Color::Blue,
    Color::Green,
    Color::Yellow,
    Color::Red,
    Color::Purple,
    Color::Pink,
    Color::Orange,
    Color::Brown,
    Color::Gray,
];

/// A column's type, read off its name and values.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Column {
    pub(crate) name: String,
    pub(crate) kind: PropertyKind,
    pub(crate) options: Vec<SelectOption>,
}

fn options_from(names: &[String], status: bool) -> Vec<SelectOption> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| SelectOption {
            id: short_id(),
            name: n.clone(),
            color: if status {
                match status_group(n) {
                    StatusGroup::ToDo => Color::Default,
                    StatusGroup::InProgress => Color::Blue,
                    StatusGroup::Complete => Color::Green,
                }
            } else {
                PALETTE[i % PALETTE.len()]
            },
            group: status.then(|| status_group(n)),
            position: i as f64,
        })
        .collect()
}

pub(crate) fn infer(name: &str, values: &[&str]) -> Column {
    let filled: Vec<&str> = values
        .iter()
        .copied()
        .filter(|v| !v.trim().is_empty())
        .collect();
    let lower = name.to_lowercase();
    let col = |kind| Column {
        name: name.to_owned(),
        kind,
        options: Vec::new(),
    };
    if filled.is_empty() {
        return col(PropertyKind::Text);
    }
    if filled.iter().all(|v| matches!(*v, "Yes" | "No")) {
        return col(PropertyKind::Checkbox);
    }
    if filled.iter().all(|v| is_relation_cell(v)) {
        // The database is found once every database is known.
        return col(PropertyKind::Relation {
            database: PageId::from_uuid(uuid::Uuid::nil()),
            twin: None,
            mirror: false,
            single: false,
        });
    }
    // A unique ID: PREFIX-12 throughout, one prefix.
    let prefixes: HashSet<&str> = filled
        .iter()
        .filter_map(|v| {
            let (p, n) = v.rsplit_once('-')?;
            (!p.is_empty() && n.parse::<u32>().is_ok() && !p.contains(' ')).then_some(p)
        })
        .collect();
    if prefixes.len() == 1
        && filled.iter().all(|v| {
            v.rsplit_once('-')
                .is_some_and(|(_, n)| n.parse::<u32>().is_ok())
        })
    {
        return col(PropertyKind::UniqueId {
            prefix: prefixes.into_iter().next().map(str::to_owned),
        });
    }
    if let Some(formats) = filled
        .iter()
        .map(|v| parse_number(v).map(|(_, f)| f))
        .collect::<Option<Vec<_>>>()
    {
        let format = formats
            .iter()
            .copied()
            .find(|f| *f != NumberFormat::Number && *f != NumberFormat::NumberWithCommas)
            .or_else(|| {
                formats
                    .iter()
                    .copied()
                    .find(|f| *f == NumberFormat::NumberWithCommas)
            })
            .unwrap_or_default();
        return col(PropertyKind::Number { format });
    }
    if filled.iter().all(|v| parse_date(v).is_some()) {
        let timed = filled
            .iter()
            .all(|v| parse_date(v).is_some_and(|d| d.start.minutes.is_some()));
        if timed && (lower.contains("created") || lower == "created time") {
            return col(PropertyKind::CreatedTime);
        }
        if timed && (lower.contains("edited") || lower.contains("updated")) {
            return col(PropertyKind::EditedTime);
        }
        return col(PropertyKind::Date);
    }
    if filled
        .iter()
        .all(|v| v.starts_with("http://") || v.starts_with("https://"))
    {
        return col(if lower.contains("file") || lower.contains("attach") {
            PropertyKind::Files
        } else {
            PropertyKind::Url
        });
    }
    if filled.iter().all(|v| {
        let v = v.trim();
        v.contains('@') && !v.contains(' ') && v.contains('.') && !v.contains(',')
    }) {
        return col(PropertyKind::Email);
    }
    if filled.iter().all(|v| {
        v.chars().filter(char::is_ascii_digit).count() >= 7
            && v.chars()
                .all(|c| c.is_ascii_digit() || " +-().".contains(c))
    }) {
        return col(PropertyKind::Phone);
    }
    if ["created by", "last edited by", "edited by"].contains(&lower.as_str()) {
        return col(if lower.starts_with("created") {
            PropertyKind::CreatedBy
        } else {
            PropertyKind::EditedBy
        });
    }
    if [
        "assignee",
        "assignees",
        "assigned to",
        "owner",
        "owners",
        "person",
        "people",
        "reviewer",
    ]
    .contains(&lower.as_str())
    {
        return col(PropertyKind::People);
    }
    // Selects: short values that repeat, or that a status column holds.
    let mut names: Vec<String> = Vec::new();
    let multi = filled.iter().any(|v| v.contains(", "));
    for v in &filled {
        let items: Vec<&str> = if multi {
            v.split(", ").collect()
        } else {
            vec![v]
        };
        for i in items {
            let i = i.trim();
            if !i.is_empty() && !names.iter().any(|n| n == i) {
                names.push(i.to_owned());
            }
        }
    }
    let short = names
        .iter()
        .all(|n| n.chars().count() <= 40 && !n.contains('\n'));
    let statusy = ["status", "state", "stage"].contains(&lower.as_str());
    let selecty = [
        "priority", "type", "category", "tag", "tags", "label", "labels", "kind", "level", "area",
        "team",
    ]
    .contains(&lower.as_str());
    // Words, not sentences: a few short words each, and not many different ones.
    let wordy = names
        .iter()
        .all(|n| n.chars().count() <= 25 && n.split_whitespace().count() <= 3)
        && names.len() <= 12;
    if short
        && (statusy
            || selecty
            || names.len() * 2 <= filled.len().max(2)
            || (wordy && names.len() < filled.len())
            || (multi && names.len() < filled.len() * 3))
    {
        if lower == "status" && !multi {
            let mut names = names;
            names.sort_by_key(|n| status_group(n) as u8);
            return Column {
                name: name.to_owned(),
                kind: PropertyKind::Status,
                options: options_from(&names, true),
            };
        }
        return Column {
            name: name.to_owned(),
            kind: if multi {
                PropertyKind::MultiSelect
            } else {
                PropertyKind::Select
            },
            options: options_from(&names, false),
        };
    }
    col(PropertyKind::Text)
}

/// A cell's value for a column, or none when it can't be read.
pub(crate) fn cell(col: &Column, s: &str, ids: &HashMap<String, PageId>) -> Option<Value> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let opt = |n: &str| {
        col.options
            .iter()
            .find(|o| o.name == n)
            .map(|o| o.id.clone())
    };
    Some(match &col.kind {
        PropertyKind::Text => Value::Text(s.to_owned()),
        PropertyKind::Number { .. } => Value::Number(parse_number(s)?.0),
        PropertyKind::Select => Value::Select(opt(s)?),
        PropertyKind::Status => Value::Status(opt(s)?),
        PropertyKind::MultiSelect => {
            Value::MultiSelect(s.split(", ").filter_map(|i| opt(i.trim())).collect())
        }
        PropertyKind::Date => Value::Date(parse_date(s)?),
        PropertyKind::Checkbox => Value::Checkbox(s == "Yes"),
        PropertyKind::Url => Value::Url(s.to_owned()),
        PropertyKind::Email => Value::Email(s.to_owned()),
        PropertyKind::Phone => Value::Phone(s.to_owned()),
        PropertyKind::Files => Value::Files(
            s.split(", ")
                .map(|u| FileRef {
                    name: percent_decode(u.rsplit('/').next().unwrap_or(u)),
                    blob: None,
                    url: Some(u.to_owned()),
                })
                .collect(),
        ),
        PropertyKind::People => Value::People(
            s.split(", ")
                .map(|n| Person {
                    id: n.trim().to_owned(),
                    name: n.trim().to_owned(),
                    email: None,
                })
                .collect(),
        ),
        PropertyKind::Relation { .. } => Value::Relation(
            relation_items(s)
                .iter()
                .filter_map(|(_, p)| ids.get(&id_in(p)?).copied())
                .collect(),
        ),
        PropertyKind::UniqueId { .. } => {
            Value::UniqueId(s.rsplit_once('-').map_or(s, |(_, n)| n).parse().ok()?)
        }
        _ => return None,
    })
}

/// One page or database in the export.
#[derive(Debug, Clone)]
struct Node {
    path: String,
    title: String,
    nid: Option<String>,
    dir: String,
    database: bool,
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// Strips a page file's `# Title` and, for a row, its `Name: value` lines.
pub(crate) fn page_body<'a>(
    text: &'a str,
    columns: &[String],
) -> (&'a str, HashMap<String, String>) {
    let mut props = HashMap::new();
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    if rest.starts_with("# ") {
        rest = rest.split_once('\n').map_or("", |(_, r)| r);
    }
    let trimmed = rest.trim_start_matches(['\n', '\r']);
    if columns.is_empty() {
        return (trimmed, props);
    }
    let mut at = 0;
    let mut any = false;
    for line in trimmed.split_inclusive('\n') {
        let l = line.trim_end();
        if l.is_empty() {
            at += line.len();
            break;
        }
        match l.split_once(": ") {
            Some((k, v)) if columns.iter().any(|c| c == k) => {
                props.insert(k.to_owned(), v.to_owned());
                any = true;
                at += line.len();
            }
            _ => break,
        }
    }
    if any {
        (&trimmed[at.min(trimmed.len())..], props)
    } else {
        (trimmed, props)
    }
}

/// Notion's `<aside>` callouts, as the Markdown reader knows callouts.
fn callouts(md: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    let mut first = false;
    for line in md.lines() {
        let t = line.trim();
        if t == "<aside>" {
            inside = true;
            first = true;
            continue;
        }
        if t == "</aside>" {
            inside = false;
            out.push('\n');
            continue;
        }
        if inside {
            if t.is_empty() {
                continue;
            }
            if first {
                // Notion starts a callout with its icon.
                let mut chars = t.chars();
                let icon = chars
                    .next()
                    .filter(|c| !c.is_alphanumeric() && !c.is_ascii());
                match icon {
                    Some(c) => {
                        out.push_str(&format!("> [!note|{c}] "));
                        out.push_str(chars.as_str().trim_start());
                    }
                    None => {
                        out.push_str("> [!note] ");
                        out.push_str(t);
                    }
                }
                first = false;
            } else {
                out.push_str("> ");
                out.push_str(line);
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    // "- [ ]  task": Notion writes two spaces.
    out.replace("- [ ]  ", "- [ ] ")
        .replace("- [x]  ", "- [x] ")
}

struct Links<'a> {
    ids: &'a HashMap<String, PageId>,
    databases: &'a HashSet<PageId>,
    export: &'a Export,
    names: &'a HashSet<String>,
    from: &'a str,
    attached: RefCell<Vec<(String, Vec<u8>)>>,
    missing: RefCell<Vec<String>>,
}

impl Resolver for Links<'_> {
    fn link_in(&self, dest: &str, _wiki: bool) -> String {
        if dest.contains("://") || dest.starts_with("mailto:") {
            return dest.to_owned();
        }
        match id_in(dest).and_then(|i| self.ids.get(&i)) {
            Some(id) => format!("{PAGE_LINK}{id}"),
            None => dest.to_owned(),
        }
    }

    fn attachment_in(&self, dest: &str, _wiki: bool) -> Option<BlockKind> {
        if dest.contains("://") {
            return None;
        }
        let rel = percent_decode(dest);
        let path = crate::bridge::paths::resolve(&format!("{}/x", parent_dir(self.from)), &rel)
            .filter(|p| self.names.contains(p))
            .or_else(|| self.names.contains(&rel).then(|| rel.clone()));
        let Some(path) = path else {
            self.missing
                .borrow_mut()
                .push(format!("{}: file {rel} isn't in the export", self.from));
            return None;
        };
        let bytes = match self.export.read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.missing.borrow_mut().push(format!("{path}: {e}"));
                return None;
            }
        };
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let blob = blob_id(&bytes);
        let kind = BlockKind::Attachment {
            blob: blob.clone(),
            mime: guess_mime(&name).to_owned(),
            size: u32::try_from(bytes.len()).ok()?,
            name,
        };
        self.attached.borrow_mut().push((blob, bytes));
        Some(kind)
    }
}

impl Core {
    pub(crate) fn import_notion_path(&mut self, path: &Path) -> Result<NotionReport, ApiError> {
        let export = Export::open(path).map_err(zip_err)?;
        let name = path
            .file_stem()
            .map_or_else(|| "Notion".to_owned(), |n| n.to_string_lossy().into_owned());
        self.import_notion_export(&export, &name)
    }

    pub(crate) fn import_notion_data(
        &mut self,
        name: &str,
        data: &str,
    ) -> Result<NotionReport, ApiError> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| ApiError::new(ErrorCode::Invalid, format!("not base64: {e}")))?;
        let export = Export::from_zip(Zip::from_bytes(bytes).map_err(zip_err)?).map_err(zip_err)?;
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
        self.import_notion_export(&export, stem)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn import_notion_export(
        &mut self,
        export: &Export,
        _name: &str,
    ) -> Result<NotionReport, ApiError> {
        let names = export.names();
        let name_set: HashSet<String> = names.iter().cloned().collect();
        let mut report = NotionReport::default();
        // Databases: prefer `_all.csv`, which has every row.
        let mut nodes: Vec<Node> = Vec::new();
        let mut csv_for: HashMap<String, String> = HashMap::new();
        for n in &names {
            let lower = n.to_lowercase();
            if lower.ends_with(".csv") {
                let stem = file_stem(n);
                let (base, all) = match stem.strip_suffix("_all") {
                    Some(b) => (b, true),
                    None => (stem, false),
                };
                let key = format!("{}/{base}", parent_dir(n));
                if all || !csv_for.contains_key(&key) {
                    csv_for.insert(key, n.clone());
                }
            }
        }
        for (key, csv) in &csv_for {
            let base = key.rsplit('/').next().unwrap_or(key);
            let (title, nid) = split_id(base);
            nodes.push(Node {
                path: csv.clone(),
                title: title.to_owned(),
                nid: nid.map(str::to_lowercase),
                dir: parent_dir(csv).to_owned(),
                database: true,
            });
        }
        for n in names.iter().filter(|n| n.to_lowercase().ends_with(".md")) {
            let (title, nid) = split_id(file_stem(n));
            nodes.push(Node {
                path: n.clone(),
                title: title.to_owned(),
                nid: nid.map(str::to_lowercase),
                dir: parent_dir(n).to_owned(),
                database: false,
            });
        }
        if nodes.is_empty() {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "that isn't a Notion export: there are no pages or databases in it",
            ));
        }
        nodes.sort_by(|a, b| {
            (a.dir.matches('/').count(), a.dir.is_empty(), &a.path).cmp(&(
                b.dir.matches('/').count(),
                b.dir.is_empty(),
                &b.path,
            ))
        });
        nodes.sort_by_key(|n| {
            if n.dir.is_empty() {
                0
            } else {
                n.dir.matches('/').count() + 1
            }
        });

        // The node a folder belongs to: the file beside it whose title is the folder's name.
        let owner = |dir: &str| -> Option<usize> {
            let name = dir.rsplit('/').next().unwrap_or(dir);
            let up = parent_dir(dir);
            nodes
                .iter()
                .position(|n| n.dir == up && !n.database && n.title == name)
                .or_else(|| {
                    nodes
                        .iter()
                        .position(|n| n.dir == up && n.database && n.title == name)
                })
                .or_else(|| {
                    // Older exports name folders with the ID too.
                    let (t, id) = split_id(name);
                    nodes.iter().position(|n| {
                        n.dir == up && n.title == t && id.is_some() && n.nid.as_deref() == id
                    })
                })
        };

        let top = self.workspace.create_page(None, "Imported from Notion")?;
        self.store.create_page(&top)?;
        let top_id = top.id();
        report.page = Some(top_id);
        report.pages += 1;

        let mut made: Vec<Option<PageId>> = vec![None; nodes.len()];
        let mut dir_pages: HashMap<String, PageId> = HashMap::new();
        let mut ids: HashMap<String, PageId> = HashMap::new();
        let mut databases: HashSet<PageId> = HashSet::new();
        // Which database each row belongs to (node index).
        let mut row_of: HashMap<usize, usize> = HashMap::new();
        for i in 0..nodes.len() {
            let n = nodes[i].clone();
            let (parent, parent_node) = if n.dir.is_empty() {
                (top_id, None)
            } else if let Some(o) = owner(&n.dir) {
                match made[o] {
                    Some(p) => (p, Some(o)),
                    None => (top_id, None),
                }
            } else {
                // A folder with no page of its own: make one for it.
                let id = match dir_pages.get(&n.dir) {
                    Some(id) => *id,
                    None => {
                        let name = n.dir.rsplit('/').next().unwrap_or(&n.dir);
                        let p = self.workspace.create_page(Some(top_id), name)?;
                        self.store.create_page(&p)?;
                        report.pages += 1;
                        dir_pages.insert(n.dir.clone(), p.id());
                        p.id()
                    }
                };
                (id, None)
            };
            let id = if n.database {
                let (page, _) = self
                    .workspace
                    .create_database(Some(parent), &n.title, false)?;
                self.store.create_page(&page)?;
                databases.insert(page.id());
                report.databases += 1;
                page.id()
            } else {
                let page = self.workspace.create_page(Some(parent), &n.title)?;
                self.store.create_page(&page)?;
                if let Some(p) = parent_node.filter(|p| nodes[*p].database) {
                    row_of.insert(i, p);
                    report.rows += 1;
                } else {
                    report.pages += 1;
                }
                page.id()
            };
            made[i] = Some(id);
            if let Some(nid) = &n.nid {
                ids.insert(nid.clone(), id);
            }
        }

        // Schemas, from each CSV; then the rows' values.
        let mut columns_of: HashMap<usize, (Vec<String>, Vec<Column>)> = HashMap::new();
        let mut relations: Vec<(PageId, String, PageId)> = Vec::new(); // (db, prop, target db)
        for (i, n) in nodes.iter().enumerate().filter(|(_, n)| n.database) {
            let Some(db) = made[i] else { continue };
            let text = match export.read(&n.path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(e) => {
                    report.unmapped.push(format!("{}: {e}", n.path));
                    continue;
                }
            };
            let rows = parse_csv(&text);
            let Some((header, body)) = rows.split_first() else {
                continue;
            };
            let mut cols = Vec::new();
            for (c, name) in header.iter().enumerate() {
                let values: Vec<&str> = body
                    .iter()
                    .map(|r| r.get(c).map_or("", String::as_str))
                    .collect();
                cols.push(if c == 0 {
                    Column {
                        name: name.clone(),
                        kind: PropertyKind::Title,
                        options: Vec::new(),
                    }
                } else {
                    infer(name, &values)
                });
            }
            // Where each relation points: the database holding the rows it links to.
            for (c, col) in cols.iter_mut().enumerate() {
                if let PropertyKind::Relation { database, .. } = &mut col.kind {
                    let target = body
                        .iter()
                        .filter_map(|r| r.get(c))
                        .flat_map(|v| relation_items(v))
                        .filter_map(|(_, p)| id_in(&p))
                        .filter_map(|nid| ids.get(&nid))
                        .find_map(|row| self.workspace.database_of(*row));
                    match target {
                        Some(t) => *database = t,
                        None => {
                            report.unmapped.push(format!(
                                "{} › {}: links to pages outside the export; kept as text",
                                n.title, col.name
                            ));
                            col.kind = PropertyKind::Text;
                        }
                    }
                }
            }
            let mut schema = self.workspace.schema(db)?;
            let mut title = schema
                .property(TITLE)
                .cloned()
                .ok_or_else(|| ApiError::new(ErrorCode::Internal, "no title"))?;
            title.name = cols[0].name.clone();
            self.workspace.put_property(db, &title)?;
            let mut prop_ids = vec![TITLE.to_owned()];
            for (c, col) in cols.iter().enumerate().skip(1) {
                let id = short_id();
                self.workspace.put_property(
                    db,
                    &PropertyDef {
                        id: id.clone(),
                        name: col.name.clone(),
                        kind: col.kind.clone(),
                        options: col.options.clone(),
                        position: c as f64,
                        description: None,
                    },
                )?;
                if let PropertyKind::Relation { database, .. } = col.kind {
                    relations.push((db, id.clone(), database));
                }
                prop_ids.push(id);
            }
            schema = self.workspace.schema(db)?;
            // Views: Notion's export has none. A table of everything, and a board.
            let mut table = schema.views[0].clone();
            table.properties = prop_ids
                .iter()
                .map(|p| ViewProperty {
                    id: p.clone(),
                    width: None,
                })
                .collect();
            self.workspace.put_view(db, &table)?;
            if let Some(c) = cols
                .iter()
                .position(|c| matches!(c.kind, PropertyKind::Status))
                .or_else(|| {
                    cols.iter()
                        .position(|c| matches!(c.kind, PropertyKind::Select))
                })
            {
                let mut board = View::new(short_id(), "Board", Layout::Board);
                board.position = 1.0;
                board.group = Some(Grouping {
                    property: prop_ids[c].clone(),
                    hidden: Vec::new(),
                    hide_empty: false,
                    date_unit: None,
                });
                board.properties = table.properties.clone();
                self.workspace.put_view(db, &board)?;
            }
            report.unmapped.push(format!(
                "{}: Notion's export has no views, formulas or rollups, so it has a table and a board, and computed columns came in as the values they showed (import from Notion's API to keep them)",
                n.title
            ));
            columns_of.insert(i, (header.clone(), cols.clone()));

            // Rows: each CSV line matched to its page file by title, in order.
            let mut row_nodes: Vec<usize> = row_of
                .iter()
                .filter(|(_, d)| **d == i)
                .map(|(r, _)| *r)
                .collect();
            row_nodes.sort();
            let mut used = HashSet::new();
            for line in body {
                let title = line.first().cloned().unwrap_or_default();
                let row = match row_nodes
                    .iter()
                    .find(|r| !used.contains(*r) && nodes[**r].title == title)
                {
                    Some(r) => {
                        used.insert(*r);
                        made[*r]
                    }
                    None => {
                        let p = self.workspace.create_page(Some(db), &title)?;
                        self.store.create_page(&p)?;
                        report.rows += 1;
                        Some(p.id())
                    }
                };
                let Some(row) = row else { continue };
                for (c, col) in cols.iter().enumerate().skip(1) {
                    let raw = line.get(c).map_or("", String::as_str);
                    match &col.kind {
                        PropertyKind::CreatedTime | PropertyKind::EditedTime => {
                            if let Some(d) = parse_moment(raw) {
                                let ms = local_ms(d);
                                if matches!(col.kind, PropertyKind::CreatedTime) {
                                    self.workspace.set_row_history(
                                        row,
                                        Some(ms),
                                        None,
                                        None,
                                        None,
                                    )?;
                                } else {
                                    self.workspace.set_row_history(
                                        row,
                                        None,
                                        None,
                                        Some(ms),
                                        None,
                                    )?;
                                }
                            }
                        }
                        PropertyKind::CreatedBy | PropertyKind::EditedBy if !raw.is_empty() => {
                            let p = Person {
                                id: raw.to_owned(),
                                name: raw.to_owned(),
                                email: None,
                            };
                            if matches!(col.kind, PropertyKind::CreatedBy) {
                                self.workspace
                                    .set_row_history(row, None, Some(&p), None, None)?;
                            } else {
                                self.workspace
                                    .set_row_history(row, None, None, None, Some(&p))?;
                            }
                        }
                        _ => {
                            if let Some(v) = cell(col, raw, &ids) {
                                self.workspace.set_value(row, &prop_ids[c], Some(&v))?;
                            } else if !raw.trim().is_empty() && col.kind.is_stored() {
                                report.unmapped.push(format!(
                                    "{} › {} › {}: couldn't read {raw:?}",
                                    n.title, title, col.name
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Two-way relations: a pair of relations that link the same rows both ways is one.
        let mut paired = HashSet::new();
        for (a_db, a_prop, b_db) in &relations {
            if paired.contains(a_prop) {
                continue;
            }
            for (c_db, c_prop, d_db) in &relations {
                if c_prop == a_prop || paired.contains(c_prop) || c_db != b_db || d_db != a_db {
                    continue;
                }
                if self.relations_mirror(*a_db, a_prop, *b_db, c_prop)? {
                    let schema = self.workspace.schema(*a_db)?;
                    let mut a = schema
                        .property(a_prop)
                        .cloned()
                        .ok_or_else(|| ApiError::new(ErrorCode::Internal, "relation vanished"))?;
                    if let PropertyKind::Relation { twin, .. } = &mut a.kind {
                        *twin = Some(c_prop.clone());
                    }
                    self.workspace.put_property(*a_db, &a)?;
                    let schema = self.workspace.schema(*b_db)?;
                    let mut c = schema
                        .property(c_prop)
                        .cloned()
                        .ok_or_else(|| ApiError::new(ErrorCode::Internal, "relation vanished"))?;
                    c.kind = PropertyKind::Relation {
                        database: *a_db,
                        twin: Some(a_prop.clone()),
                        mirror: true,
                        single: false,
                    };
                    self.workspace.put_property(*b_db, &c)?;
                    paired.insert(a_prop.clone());
                    paired.insert(c_prop.clone());
                    break;
                }
            }
        }

        // Page content, with links to pages and databases made into Drusite's.
        let mut attached: HashSet<String> = HashSet::new();
        for (i, n) in nodes.iter().enumerate().filter(|(_, n)| !n.database) {
            let Some(page) = made[i] else { continue };
            let text = match export.read(&n.path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(e) => {
                    report.unmapped.push(format!("{}: {e}", n.path));
                    continue;
                }
            };
            let columns: Vec<String> = row_of
                .get(&i)
                .and_then(|d| columns_of.get(d))
                .map(|(h, _)| h.clone())
                .unwrap_or_default();
            let (body, _) = page_body(&text, &columns);
            let body = callouts(body);
            let links = Links {
                ids: &ids,
                databases: &databases,
                export,
                names: &name_set,
                from: &n.path,
                attached: RefCell::new(Vec::new()),
                missing: RefCell::new(Vec::new()),
            };
            let md = drusite_markdown::read(&body, &links);
            for (blob, bytes) in links.attached.take() {
                if attached.insert(blob.clone()) {
                    self.store.put_blob(&blob, &bytes, false)?;
                    report.attachments += 1;
                }
            }
            report.unmapped.extend(links.missing.take());
            let wanted: Vec<Wanted> = md
                .blocks
                .iter()
                .map(|b| {
                    // A paragraph that only links to a database is that database, shown inline.
                    let kind = match only_link(&b.spans) {
                        Some(href) => match href
                            .strip_prefix(PAGE_LINK)
                            .and_then(|i| i.parse::<PageId>().ok())
                            .filter(|i| links.databases.contains(i))
                        {
                            Some(database) => BlockKind::Database {
                                database,
                                view: None,
                            },
                            None => b.kind.clone(),
                        },
                        None => b.kind.clone(),
                    };
                    let spans = if matches!(kind, BlockKind::Database { .. }) {
                        Vec::new()
                    } else {
                        b.spans.clone()
                    };
                    Wanted {
                        id: None,
                        kind,
                        indent: b.indent,
                        spans,
                    }
                })
                .collect();
            if !wanted.is_empty() {
                let (mut doc, mut at) = self.store.load_page(page)?;
                merge::apply(&mut doc, &wanted)?;
                self.store.save_page(&doc, &mut at)?;
            }
            self.changed(*page.as_uuid());
        }
        if report.attachments > 0
            && let Some(notify) = &self.on_change
        {
            notify();
        }
        self.save_workspace()?;
        Ok(report)
    }

    /// Whether relation `a` (in `a_db`) and `c` (in `c_db`) link exactly the same pairs of rows.
    fn relations_mirror(
        &self,
        a_db: PageId,
        a: &str,
        c_db: PageId,
        c: &str,
    ) -> Result<bool, ApiError> {
        let pairs =
            |db: PageId, prop: &str, flip: bool| -> Result<HashSet<(PageId, PageId)>, ApiError> {
                let schema = self.workspace.schema(db)?;
                let mut out = HashSet::new();
                for r in self.workspace.rows(db)? {
                    if let Some(Value::Relation(targets)) =
                        self.workspace.row(r, &schema)?.values.get(prop)
                    {
                        for t in targets {
                            out.insert(if flip { (*t, r) } else { (r, *t) });
                        }
                    }
                }
                Ok(out)
            };
        let x = pairs(a_db, a, false)?;
        let y = pairs(c_db, c, true)?;
        Ok(!x.is_empty() && x == y)
    }
}

/// The link a run of text is, when it is nothing but one link.
fn only_link(spans: &[drusite_model::Span]) -> Option<String> {
    let mut href = None;
    for s in spans {
        if s.text.trim().is_empty() {
            continue;
        }
        let l = s.marks.link.clone()?;
        match &href {
            None => href = Some(l),
            Some(h) if *h == l => {}
            _ => return None,
        }
    }
    href
}

/// Unix milliseconds of a time read as UTC (an export gives no zone that can be trusted).
fn local_ms(d: DateTime) -> i64 {
    let date = time::Date::from_calendar_date(
        d.day.year(),
        time::Month::try_from(d.day.month()).unwrap_or(time::Month::January),
        d.day.day(),
    )
    .unwrap_or(time::Date::MIN);
    let m = d.minutes.unwrap_or(0);
    let t = time::Time::from_hms(
        u8::try_from(m / 60).unwrap_or(0),
        u8::try_from(m % 60).unwrap_or(0),
        0,
    )
    .unwrap_or(time::Time::MIDNIGHT);
    time::PrimitiveDateTime::new(date, t)
        .assume_utc()
        .unix_timestamp()
        * 1000
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn notion_values_read() {
        assert_eq!(
            parse_moment("November 28, 2026 2:30 PM (CST)")
                .unwrap()
                .to_string(),
            "2026-11-28T14:30"
        );
        assert_eq!(
            parse_moment("November 20, 2026").unwrap().to_string(),
            "2026-11-20"
        );
        let r = parse_date("December 3, 2026 → December 5, 2026").unwrap();
        assert_eq!(r.end.unwrap().to_string(), "2026-12-05");
        assert_eq!(
            parse_number("$1,200.50"),
            Some((1200.5, NumberFormat::Dollar))
        );
        assert_eq!(parse_number("25%"), Some((0.25, NumberFormat::Percent)));
        assert_eq!(
            relation_items("Read (Tasks/Read%20425211431b888109b409d7f29fbfd9f2.csv), Board view, with drag (Tasks/B%20425211431b8881a18943d93415cb887d.csv)")
                .len(),
            2
        );
        let rows = parse_csv("\u{feff}a,b\n\"x, y\",\"say \"\"hi\"\"\"\n");
        assert_eq!(rows, vec![vec!["a", "b"], vec!["x, y", "say \"hi\""]]);
    }

    #[test]
    fn column_types_are_read_off_the_values() {
        let kind = |name: &str, v: &[&str]| infer(name, v).kind;
        assert_eq!(kind("Done?", &["Yes", "No", ""]), PropertyKind::Checkbox);
        assert!(matches!(
            kind("Budget", &["$300.00", "$1,200.50"]),
            PropertyKind::Number {
                format: NumberFormat::Dollar
            }
        ));
        assert_eq!(
            kind(
                "Due",
                &[
                    "November 20, 2026",
                    "",
                    "December 3, 2026 → December 5, 2026"
                ]
            ),
            PropertyKind::Date
        );
        assert_eq!(
            kind("Created", &["November 27, 2026 1:00 PM"]),
            PropertyKind::CreatedTime
        );
        assert_eq!(
            kind("Status", &["Done", "Not started", "In progress"]),
            PropertyKind::Status
        );
        assert_eq!(
            kind("Tags", &["backend", "backend, docs", "ui", ""]),
            PropertyKind::MultiSelect
        );
        assert_eq!(
            kind("Priority", &["High", "High", "Low", "Medium"]),
            PropertyKind::Select
        );
        assert_eq!(
            kind("ID", &["DRUTEST-5", "DRUTEST-1"]),
            PropertyKind::UniqueId {
                prefix: Some("DRUTEST".into())
            }
        );
        assert_eq!(kind("Site", &["https://example.com"]), PropertyKind::Url);
        assert_eq!(
            kind(
                "Notes",
                &["A long sentence about things", "Another one entirely"]
            ),
            PropertyKind::Text
        );
        assert_eq!(
            kind("Priority", &["Low", "Medium", "High", "High"]),
            PropertyKind::Select
        );
    }
}
