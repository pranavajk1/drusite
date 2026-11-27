// SPDX-License-Identifier: AGPL-3.0-or-later

//! Databases: a schema of typed properties, the views over it, and rows that are pages.
//!
//! As in Notion, a database is a page (of kind [`crate::PageKind::Database`]) and every row is
//! one of its subpages, with a page's title and content of its own. The schema and the views
//! live with the database's entry in the workspace document, and each row's values with the
//! row's entry, so a view is computed without loading a single page document.
//!
//! Properties are named by ID ([`PropId`]), never by name, so renaming one keeps every value
//! and a rename on one device merges with a value set on another.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{Day, PageId};

/// A property's ID within its database: short, random and never reused.
pub type PropId = String;
/// A select, multi-select or status option's ID within its property.
pub type OptionId = String;
/// A view's ID within its database.
pub type ViewId = String;

/// The ID the title property always has.
pub const TITLE: &str = "title";

/// A property in a database's schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDef {
    pub id: PropId,
    pub name: String,
    pub kind: PropertyKind,
    /// The choices of a select, multi-select or status property, in order.
    pub options: Vec<SelectOption>,
    /// Where it comes in the schema; views have their own order.
    pub position: f64,
    /// A description, shown as the property's tooltip.
    pub description: Option<String>,
}

/// What a property holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PropertyKind {
    /// The row page's title. Every database has exactly one, with ID [`TITLE`].
    Title,
    Text,
    Number {
        format: NumberFormat,
    },
    Select,
    MultiSelect,
    /// A select whose options each belong to one of three groups.
    Status,
    Date,
    Checkbox,
    Url,
    Email,
    Phone,
    Files,
    People,
    CreatedTime,
    CreatedBy,
    EditedTime,
    EditedBy,
    /// Links to rows of `database`. Two-way when `twin` names the property on the other side;
    /// of the two, the one with `mirror` set holds no values of its own and shows the other's
    /// from the other end, so the pair can never disagree.
    Relation {
        database: PageId,
        twin: Option<PropId>,
        mirror: bool,
        /// At most one row (Notion's "limit 1").
        single: bool,
    },
    /// Sums up `target` across the rows `relation` links to.
    Rollup {
        relation: PropId,
        target: PropId,
        function: RollupFunction,
    },
    /// A formula in Notion's formula language.
    Formula {
        expression: String,
    },
    /// A number Drusite gives each new row: 1, 2, 3…, shown with `prefix` (`TASK-12`).
    UniqueId {
        prefix: Option<String>,
    },
}

impl PropertyKind {
    /// The name Notion and the Markdown folder use for it.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Text => "text",
            Self::Number { .. } => "number",
            Self::Select => "select",
            Self::MultiSelect => "multi_select",
            Self::Status => "status",
            Self::Date => "date",
            Self::Checkbox => "checkbox",
            Self::Url => "url",
            Self::Email => "email",
            Self::Phone => "phone_number",
            Self::Files => "files",
            Self::People => "people",
            Self::CreatedTime => "created_time",
            Self::CreatedBy => "created_by",
            Self::EditedTime => "last_edited_time",
            Self::EditedBy => "last_edited_by",
            Self::Relation { .. } => "relation",
            Self::Rollup { .. } => "rollup",
            Self::Formula { .. } => "formula",
            Self::UniqueId { .. } => "unique_id",
        }
    }

    /// Whether a row stores a value for it (rather than it being computed or the title).
    pub const fn is_stored(&self) -> bool {
        match self {
            Self::Relation { mirror, .. } => !*mirror,
            Self::Title
            | Self::CreatedTime
            | Self::CreatedBy
            | Self::EditedTime
            | Self::EditedBy
            | Self::Rollup { .. }
            | Self::Formula { .. } => false,
            _ => true,
        }
    }

    pub const fn has_options(&self) -> bool {
        matches!(self, Self::Select | Self::MultiSelect | Self::Status)
    }
}

/// How a number property shows its numbers (Notion's formats).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum NumberFormat {
    #[default]
    Number,
    NumberWithCommas,
    Percent,
    Dollar,
    CanadianDollar,
    AustralianDollar,
    Euro,
    Pound,
    Yen,
    Ruble,
    Rupee,
    Won,
    Yuan,
    Real,
    Lira,
    Franc,
    HongKongDollar,
    NewZealandDollar,
    Krona,
    NorwegianKrone,
    MexicanPeso,
    Rand,
    NewTaiwanDollar,
    DanishKrone,
    Zloty,
    Baht,
    Forint,
    Koruna,
    Shekel,
    ChileanPeso,
    PhilippinePeso,
    Dirham,
    ColombianPeso,
    Riyal,
    Ringgit,
    Leu,
    ArgentinePeso,
    UruguayanPeso,
    SingaporeDollar,
}

impl NumberFormat {
    /// The symbol before the number, for currencies.
    pub const fn symbol(self) -> Option<&'static str> {
        use NumberFormat as F;
        Some(match self {
            F::Number | F::NumberWithCommas | F::Percent => return None,
            F::Dollar => "$",
            F::CanadianDollar => "CA$",
            F::AustralianDollar => "A$",
            F::Euro => "€",
            F::Pound => "£",
            F::Yen => "¥",
            F::Ruble => "₽",
            F::Rupee => "₹",
            F::Won => "₩",
            F::Yuan => "CN¥",
            F::Real => "R$",
            F::Lira => "₺",
            F::Franc => "CHF ",
            F::HongKongDollar => "HK$",
            F::NewZealandDollar => "NZ$",
            F::Krona => "kr ",
            F::NorwegianKrone => "kr ",
            F::MexicanPeso => "MX$",
            F::Rand => "R ",
            F::NewTaiwanDollar => "NT$",
            F::DanishKrone => "kr ",
            F::Zloty => "zł ",
            F::Baht => "฿",
            F::Forint => "Ft ",
            F::Koruna => "Kč ",
            F::Shekel => "₪",
            F::ChileanPeso => "CLP$",
            F::PhilippinePeso => "₱",
            F::Dirham => "AED ",
            F::ColombianPeso => "COP$",
            F::Riyal => "SAR ",
            F::Ringgit => "RM ",
            F::Leu => "lei ",
            F::ArgentinePeso => "ARS$",
            F::UruguayanPeso => "UYU$",
            F::SingaporeDollar => "S$",
        })
    }

    /// `n` as this format shows it.
    pub fn show(self, n: f64) -> String {
        if !n.is_finite() {
            return if n.is_nan() {
                "NaN".to_owned()
            } else if n > 0.0 {
                "Infinity".to_owned()
            } else {
                "-Infinity".to_owned()
            };
        }
        match self {
            Self::Number => crate::database::js_number(n),
            Self::NumberWithCommas => group_thousands(n, None),
            Self::Percent => format!("{}%", trim_decimals(n * 100.0)),
            other => {
                let symbol = other.symbol().unwrap_or("");
                let body = group_thousands(n.abs(), Some(2));
                if n < 0.0 {
                    format!("-{symbol}{body}")
                } else {
                    format!("{symbol}{body}")
                }
            }
        }
    }

    pub const ALL: [Self; 39] = {
        use NumberFormat as F;
        [
            F::Number,
            F::NumberWithCommas,
            F::Percent,
            F::Dollar,
            F::CanadianDollar,
            F::AustralianDollar,
            F::Euro,
            F::Pound,
            F::Yen,
            F::Ruble,
            F::Rupee,
            F::Won,
            F::Yuan,
            F::Real,
            F::Lira,
            F::Franc,
            F::HongKongDollar,
            F::NewZealandDollar,
            F::Krona,
            F::NorwegianKrone,
            F::MexicanPeso,
            F::Rand,
            F::NewTaiwanDollar,
            F::DanishKrone,
            F::Zloty,
            F::Baht,
            F::Forint,
            F::Koruna,
            F::Shekel,
            F::ChileanPeso,
            F::PhilippinePeso,
            F::Dirham,
            F::ColombianPeso,
            F::Riyal,
            F::Ringgit,
            F::Leu,
            F::ArgentinePeso,
            F::UruguayanPeso,
            F::SingaporeDollar,
        ]
    };

    /// The snake_case name Notion's API uses.
    pub fn api_name(self) -> String {
        serde_json_name(self)
    }
}

fn serde_json_name(f: NumberFormat) -> String {
    // The serde name, without pulling in a JSON crate.
    let debug = format!("{f:?}");
    let mut out = String::new();
    for (i, c) in debug.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn trim_decimals(n: f64) -> String {
    js_number((n * 1e10).round() / 1e10)
}

/// `n` with commas between thousands, and `decimals` places when given.
fn group_thousands(n: f64, decimals: Option<usize>) -> String {
    let text = match decimals {
        Some(d) => format!("{:.*}", d, n.abs()),
        None => js_number(n.abs()),
    };
    let (int, frac) = text
        .split_once('.')
        .map_or((text.as_str(), None), |(i, f)| (i, Some(f)));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let sign = if n < 0.0 { "-" } else { "" };
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// A number as JavaScript prints it, which is how Notion shows a number in a formula:
/// `3`, `0.1`, `1e+21`, `1.5e-7`.
pub fn js_number(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_owned();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if n == 0.0 {
        return "0".to_owned();
    }
    let abs = n.abs();
    if (1e-6..1e21).contains(&abs) {
        // Rust's shortest round-trip form matches JavaScript's in this range.
        let s = format!("{n}");
        return s;
    }
    // Exponent form: shortest digits, then JavaScript's `e+N` / `e-N`.
    let s = format!("{n:e}");
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if exp >= 0 {
        format!("{mantissa}e+{exp}")
    } else {
        format!("{mantissa}e{exp}")
    }
}

/// A choice in a select, multi-select or status property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SelectOption {
    pub id: OptionId,
    pub name: String,
    pub color: Color,
    /// A status option's group; none for select options.
    pub group: Option<StatusGroup>,
    /// Where it comes among the property's options.
    pub position: f64,
}

/// Notion's colours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    #[default]
    Default,
    Gray,
    Brown,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Pink,
    Red,
}

impl Color {
    pub const ALL: [Self; 10] = [
        Self::Default,
        Self::Gray,
        Self::Brown,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
        Self::Pink,
        Self::Red,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Gray => "gray",
            Self::Brown => "brown",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
            Self::Red => "red",
        }
    }

    /// Reads a colour's name; Notion's `_background` variants read as the colour.
    pub fn from_name(name: &str) -> Self {
        let name = name.strip_suffix("_background").unwrap_or(name);
        Self::ALL
            .into_iter()
            .find(|c| c.name() == name)
            .unwrap_or_default()
    }
}

/// The three groups a status option belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum StatusGroup {
    ToDo,
    InProgress,
    Complete,
}

impl StatusGroup {
    pub const ALL: [Self; 3] = [Self::ToDo, Self::InProgress, Self::Complete];

    pub const fn name(self) -> &'static str {
        match self {
            Self::ToDo => "To-do",
            Self::InProgress => "In progress",
            Self::Complete => "Complete",
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::ToDo => "toDo",
            Self::InProgress => "inProgress",
            Self::Complete => "complete",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.key() == key)
    }
}

/// How a rollup sums up the values it reaches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RollupFunction {
    #[default]
    ShowOriginal,
    ShowUnique,
    CountAll,
    CountValues,
    CountUniqueValues,
    CountEmpty,
    CountNotEmpty,
    PercentEmpty,
    PercentNotEmpty,
    Sum,
    Average,
    Median,
    Min,
    Max,
    Range,
    EarliestDate,
    LatestDate,
    DateRange,
    Checked,
    Unchecked,
    PercentChecked,
    PercentUnchecked,
}

impl RollupFunction {
    pub const ALL: [Self; 22] = {
        use RollupFunction as R;
        [
            R::ShowOriginal,
            R::ShowUnique,
            R::CountAll,
            R::CountValues,
            R::CountUniqueValues,
            R::CountEmpty,
            R::CountNotEmpty,
            R::PercentEmpty,
            R::PercentNotEmpty,
            R::Sum,
            R::Average,
            R::Median,
            R::Min,
            R::Max,
            R::Range,
            R::EarliestDate,
            R::LatestDate,
            R::DateRange,
            R::Checked,
            R::Unchecked,
            R::PercentChecked,
            R::PercentUnchecked,
        ]
    };

    /// The name Notion's API uses (`count`, `sum`, `show_original`…).
    pub const fn api_name(self) -> &'static str {
        use RollupFunction as R;
        match self {
            R::ShowOriginal => "show_original",
            R::ShowUnique => "show_unique",
            R::CountAll => "count",
            R::CountValues => "count_values",
            R::CountUniqueValues => "unique",
            R::CountEmpty => "empty",
            R::CountNotEmpty => "not_empty",
            R::PercentEmpty => "percent_empty",
            R::PercentNotEmpty => "percent_not_empty",
            R::Sum => "sum",
            R::Average => "average",
            R::Median => "median",
            R::Min => "min",
            R::Max => "max",
            R::Range => "range",
            R::EarliestDate => "earliest_date",
            R::LatestDate => "latest_date",
            R::DateRange => "date_range",
            R::Checked => "checked",
            R::Unchecked => "unchecked",
            R::PercentChecked => "percent_checked",
            R::PercentUnchecked => "percent_unchecked",
        }
    }

    pub fn from_api_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.api_name() == name)
    }
}

/// A moment: a day, and a time on it when one was given, in the person's own time.
/// Serialized as `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime {
    pub day: Day,
    /// Minutes after midnight.
    pub minutes: Option<u16>,
}

impl DateTime {
    pub const fn day(day: Day) -> Self {
        Self { day, minutes: None }
    }

    /// Reads `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM`, or a longer ISO 8601 time (seconds and any
    /// offset are dropped: the time is kept as written).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (date, time) = match s.split_once(['T', ' ']) {
            Some((d, t)) => (d, Some(t)),
            None => (s, None),
        };
        let day: Day = date.parse().ok()?;
        let minutes = match time {
            None => None,
            Some(t) => {
                let mut parts = t.splitn(3, ':');
                let h: u16 = parts.next()?.get(..2)?.parse().ok()?;
                let m: u16 = parts.next()?.get(..2)?.parse().ok()?;
                if h > 23 || m > 59 {
                    return None;
                }
                Some(h * 60 + m)
            }
        };
        Some(Self { day, minutes })
    }
}

impl std::fmt::Display for DateTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.minutes {
            None => write!(f, "{}", self.day),
            Some(m) => write!(f, "{}T{:02}:{:02}", self.day, m / 60, m % 60),
        }
    }
}

impl Serialize for DateTime {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DateTime {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("not a date: {s:?}")))
    }
}

impl Type for DateTime {
    fn definition(types: &mut specta::Types) -> specta::datatype::DataType {
        <String as Type>::definition(types)
    }
}

/// A date property's value: one moment, or a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DateValue {
    pub start: DateTime,
    pub end: Option<DateTime>,
}

/// A file in a files property: one stored with the workspace, or a link.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FileRef {
    pub name: String,
    /// The attachment's address in the workspace's store, for a stored file.
    pub blob: Option<String>,
    /// Where it is, for a linked file.
    pub url: Option<String>,
}

/// A person in a people property. Drusite's people are accounts on its server, named by email;
/// one imported from Notion keeps the name and email Notion had.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    /// The email, or Notion's user ID when it gave none.
    pub id: String,
    pub name: String,
    pub email: Option<String>,
}

/// A stored value in a row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Value {
    Text(String),
    Number(f64),
    Select(OptionId),
    MultiSelect(Vec<OptionId>),
    Status(OptionId),
    Date(DateValue),
    Checkbox(bool),
    Url(String),
    Email(String),
    Phone(String),
    Files(Vec<FileRef>),
    People(Vec<Person>),
    Relation(Vec<PageId>),
    /// A unique ID's number.
    UniqueId(u32),
}

impl Value {
    /// Whether it counts as empty, as Notion's "is empty" filter sees it.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(s) | Self::Url(s) | Self::Email(s) | Self::Phone(s) => s.is_empty(),
            Self::Select(s) | Self::Status(s) => s.is_empty(),
            Self::MultiSelect(v) => v.is_empty(),
            Self::Files(v) => v.is_empty(),
            Self::People(v) => v.is_empty(),
            Self::Relation(v) => v.is_empty(),
            Self::Number(_) | Self::Date(_) | Self::Checkbox(_) | Self::UniqueId(_) => false,
        }
    }

    /// Whether this value fits a property of `kind`.
    pub fn fits(&self, kind: &PropertyKind) -> bool {
        use PropertyKind as K;
        matches!(
            (self, kind),
            (Self::Text(_), K::Text)
                | (Self::Number(_), K::Number { .. })
                | (Self::Select(_), K::Select)
                | (Self::MultiSelect(_), K::MultiSelect)
                | (Self::Status(_), K::Status)
                | (Self::Date(_), K::Date)
                | (Self::Checkbox(_), K::Checkbox)
                | (Self::Url(_), K::Url)
                | (Self::Email(_), K::Email)
                | (Self::Phone(_), K::Phone)
                | (Self::Files(_), K::Files)
                | (Self::People(_), K::People)
                | (Self::Relation(_), K::Relation { .. })
                | (Self::UniqueId(_), K::UniqueId { .. })
        )
    }
}

/// How a view lays its rows out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum Layout {
    #[default]
    Table,
    Board,
    Calendar,
    List,
    Gallery,
}

impl Layout {
    pub const ALL: [Self; 5] = [
        Self::Table,
        Self::Board,
        Self::Calendar,
        Self::List,
        Self::Gallery,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Board => "board",
            Self::Calendar => "calendar",
            Self::List => "list",
            Self::Gallery => "gallery",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.name() == name)
    }
}

/// A view of a database: how it lays the rows out, which it shows, in what order, grouped how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: ViewId,
    pub name: String,
    pub layout: Layout,
    /// Where it comes among the database's views.
    pub position: f64,
    pub filter: Option<Filter>,
    /// The first sort decides, the next breaks ties, and so on. None: the rows' own order.
    pub sorts: Vec<ViewSort>,
    /// Groups (a board's columns): by a select, status, multi-select, checkbox, person,
    /// relation, text or date property.
    pub group: Option<Grouping>,
    /// The properties it shows, in order; any not listed are hidden.
    pub properties: Vec<ViewProperty>,
    /// The date property a calendar places rows by.
    pub date_property: Option<PropId>,
    /// What a board or gallery card shows at its top.
    pub cover: Cover,
    /// Whether a table wraps long cells.
    pub wrap: bool,
}

impl View {
    pub fn new(id: ViewId, name: impl Into<String>, layout: Layout) -> Self {
        Self {
            id,
            name: name.into(),
            layout,
            position: 0.0,
            filter: None,
            sorts: Vec::new(),
            group: None,
            properties: Vec::new(),
            date_property: None,
            cover: Cover::None,
            wrap: false,
        }
    }
}

/// A property a view shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ViewProperty {
    pub id: PropId,
    /// A table column's width in pixels, when it was resized.
    pub width: Option<u16>,
}

/// What a card shows at its top.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "type", content = "property", rename_all = "camelCase")]
pub enum Cover {
    #[default]
    None,
    /// The first image on the row's page.
    PageContent,
    /// The first image in a files property.
    Property(PropId),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ViewSort {
    pub property: PropId,
    pub descending: bool,
}

/// How a view groups its rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Grouping {
    pub property: PropId,
    /// Groups not shown (by key; `""` is the group without a value).
    pub hidden: Vec<String>,
    /// Whether empty groups are left out.
    pub hide_empty: bool,
    /// For a date property: group by day, week, month or year.
    pub date_unit: Option<DateUnit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum DateUnit {
    Day,
    Week,
    Month,
    Year,
}

/// Which rows a view shows: conditions, joined by `and` or `or`, nested as Notion nests them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Filter {
    And { filters: Vec<Filter> },
    Or { filters: Vec<Filter> },
    Condition(FilterCondition),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FilterCondition {
    pub property: PropId,
    pub op: FilterOp,
    /// What it compares with; none for the operators that take nothing (`isEmpty`, `pastWeek`).
    pub value: Option<FilterValue>,
}

/// How a condition tests a value. Each works on the property types Notion offers it for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    Equals,
    NotEquals,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    IsEmpty,
    IsNotEmpty,
    Greater,
    Less,
    GreaterOrEqual,
    LessOrEqual,
    Before,
    After,
    OnOrBefore,
    OnOrAfter,
    PastWeek,
    PastMonth,
    PastYear,
    NextWeek,
    NextMonth,
    NextYear,
    ThisWeek,
}

/// A value a filter compares with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum FilterValue {
    Text(String),
    Number(f64),
    Checkbox(bool),
    Date(Day),
    /// `today`, `tomorrow`, `yesterday`: relative to the day the view is shown.
    Relative(RelativeDay),
    /// An option, a person or a row, by ID.
    Id(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RelativeDay {
    Today,
    Tomorrow,
    Yesterday,
    OneWeekAgo,
    OneWeekFromNow,
    OneMonthAgo,
    OneMonthFromNow,
}

/// A whole schema, as the Core API hands it out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    pub database: PageId,
    pub title: String,
    /// In schema order, the title first.
    pub properties: Vec<PropertyDef>,
    /// In order.
    pub views: Vec<View>,
    /// Whether it sits inside another page (an inline database) rather than being one.
    pub inline: bool,
}

impl Schema {
    pub fn property(&self, id: &str) -> Option<&PropertyDef> {
        self.properties.iter().find(|p| p.id == id)
    }

    pub fn property_named(&self, name: &str) -> Option<&PropertyDef> {
        self.properties.iter().find(|p| p.name == name).or_else(|| {
            self.properties
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
        })
    }
}

/// A short random ID for a property, option or view: 8 characters of `[a-z0-9]`.
pub fn short_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let uuid = uuid::Uuid::now_v7();
    // The random tail of a v7 UUID.
    let bytes = &uuid.as_bytes()[8..16];
    bytes
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_as_javascript_does() {
        assert_eq!(js_number(3.0), "3");
        assert_eq!(js_number(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(js_number(-2.5), "-2.5");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(js_number(1.5e-7), "1.5e-7");
        assert_eq!(js_number(123_456_789.0), "123456789");
    }

    #[test]
    fn number_formats() {
        assert_eq!(
            NumberFormat::NumberWithCommas.show(1_234_567.5),
            "1,234,567.5"
        );
        assert_eq!(NumberFormat::Dollar.show(-1234.5), "-$1,234.50");
        assert_eq!(NumberFormat::Percent.show(0.256), "25.6%");
        assert_eq!(NumberFormat::Euro.show(3.0), "€3.00");
        assert_eq!(
            NumberFormat::NumberWithCommas.api_name(),
            "number_with_commas"
        );
    }

    #[test]
    fn date_times_parse_and_print() {
        let d = DateTime::parse("2026-11-27T09:05:00.000+01:00").unwrap();
        assert_eq!(d.to_string(), "2026-11-27T09:05");
        assert_eq!(
            DateTime::parse("2026-11-27").unwrap().to_string(),
            "2026-11-27"
        );
        assert!(DateTime::parse("2026-11-27T25:00").is_none());
    }

    #[test]
    fn short_ids_differ() {
        let a = short_id();
        assert_eq!(a.len(), 8);
        assert_ne!(a, short_id());
    }
}
