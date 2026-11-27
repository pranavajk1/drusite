// SPDX-License-Identifier: AGPL-3.0-or-later

//! Page properties, the journal, and queries over pages.
//!
//! Properties are the page's frontmatter: a name and a plain value, the shapes YAML and Notion
//! both have. A journal entry is a page of kind [`PageKind::Journal`] with a [`DATE`] property;
//! its mood is the [`MOOD`] property, a number from 1 to 5.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use specta::Type;

/// The property holding a journal entry's day.
pub const DATE: &str = "Date";
/// The property holding a journal entry's mood, 1 ([`Mood::Awful`]) to 5 ([`Mood::Great`]).
pub const MOOD: &str = "Mood";
/// The workspace setting that lets Ask your notes read journal entries. Off unless set: a
/// journal is more private than the rest of the notes.
pub const ASK_JOURNAL_SETTING: &str = "ask.journal";

/// What a page is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum PageKind {
    #[default]
    Page,
    /// One day of the journal.
    Journal,
    /// A database: its subpages are its rows.
    Database,
}

impl PageKind {
    /// The name stored in documents. Unknown names read as [`PageKind::Page`].
    pub const fn name(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Journal => "journal",
            Self::Database => "database",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "journal" => Self::Journal,
            "database" => Self::Database,
            _ => Self::Page,
        }
    }
}

/// A calendar day, without a time zone: the person's own day, as their device sees it.
/// Serialized as `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(time::Date);

impl Day {
    pub fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        let month = time::Month::try_from(month).ok()?;
        time::Date::from_calendar_date(year, month, day)
            .ok()
            .map(Self)
    }

    pub fn year(self) -> i32 {
        self.0.year()
    }

    pub fn month(self) -> u8 {
        self.0.month().into()
    }

    pub fn day(self) -> u8 {
        self.0.day()
    }

    /// The weekday's English name.
    pub fn weekday(self) -> &'static str {
        use time::Weekday as W;
        match self.0.weekday() {
            W::Monday => "Monday",
            W::Tuesday => "Tuesday",
            W::Wednesday => "Wednesday",
            W::Thursday => "Thursday",
            W::Friday => "Friday",
            W::Saturday => "Saturday",
            W::Sunday => "Sunday",
        }
    }

    /// The month's English name.
    pub fn month_name(self) -> &'static str {
        const NAMES: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        NAMES[usize::from(self.month()) - 1]
    }

    /// The title a new journal entry gets, e.g. "Thursday, 26 November 2026".
    pub fn title(self) -> String {
        format!(
            "{}, {} {} {}",
            self.weekday(),
            self.day(),
            self.month_name(),
            self.year()
        )
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}",
            self.year(),
            self.month(),
            self.day()
        )
    }
}

/// A string that isn't a `YYYY-MM-DD` day.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a day (YYYY-MM-DD): {0:?}")]
pub struct DayError(pub String);

impl FromStr for Day {
    type Err = DayError;

    fn from_str(s: &str) -> Result<Self, DayError> {
        let err = || DayError(s.to_owned());
        let mut parts = s.split('-');
        let (Some(y), Some(m), Some(d), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(err());
        };
        if y.len() != 4 || m.len() != 2 || d.len() != 2 {
            return Err(err());
        }
        let y = y.parse().map_err(|_| err())?;
        let m = m.parse().map_err(|_| err())?;
        let d = d.parse().map_err(|_| err())?;
        Self::new(y, m, d).ok_or_else(err)
    }
}

impl Serialize for Day {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Day {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl Type for Day {
    fn definition(types: &mut specta::Types) -> specta::datatype::DataType {
        <String as Type>::definition(types)
    }
}

/// A property's value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum PropertyValue {
    Text(String),
    Number(f64),
    Checkbox(bool),
    Date(Day),
}

impl PropertyValue {
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_day(&self) -> Option<Day> {
        match self {
            Self::Date(d) => Some(*d),
            _ => None,
        }
    }

    /// Orders values of the same type; values of different types order by type.
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let rank = |v: &Self| match v {
            Self::Checkbox(_) => 0,
            Self::Number(_) => 1,
            Self::Date(_) => 2,
            Self::Text(_) => 3,
        };
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
            (Self::Number(a), Self::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Self::Checkbox(a), Self::Checkbox(b)) => a.cmp(b),
            (Self::Date(a), Self::Date(b)) => a.cmp(b),
            _ => rank(self).cmp(&rank(other)),
        }
    }
}

/// A named property on a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Property {
    pub name: String,
    pub value: PropertyValue,
}

/// The journal's moods, from [`MOOD`]'s 1 to 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum Mood {
    Awful,
    Bad,
    Okay,
    Good,
    Great,
}

impl Mood {
    pub const ALL: [Mood; 5] = [Self::Awful, Self::Bad, Self::Okay, Self::Good, Self::Great];

    pub const fn score(self) -> u8 {
        self as u8 + 1
    }

    /// The mood a score stands for, rounded and clamped into 1-5.
    pub fn from_score(score: f64) -> Option<Self> {
        if !score.is_finite() {
            return None;
        }
        let i = score.round().clamp(1.0, 5.0) as usize;
        Some(Self::ALL[i - 1])
    }
}

/// How a condition compares a property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum Comparison {
    /// The page has the property, whatever its value.
    Exists,
    Missing,
    Equals,
    NotEquals,
    /// Text containing the value (ignoring case).
    Contains,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

/// One test a page must pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub property: String,
    pub comparison: Comparison,
    /// Unused for [`Comparison::Exists`] and [`Comparison::Missing`].
    pub value: Option<PropertyValue>,
}

impl Condition {
    pub fn matches(&self, value: Option<&PropertyValue>) -> bool {
        use Comparison as C;
        use std::cmp::Ordering;
        match (self.comparison, value, &self.value) {
            (C::Exists, v, _) => v.is_some(),
            (C::Missing, v, _) => v.is_none(),
            (_, None, _) | (_, _, None) => false,
            (C::Contains, Some(PropertyValue::Text(have)), Some(PropertyValue::Text(want))) => {
                have.to_lowercase().contains(&want.to_lowercase())
            }
            (C::Contains, Some(_), Some(_)) => false,
            (cmp, Some(have), Some(want)) => {
                if std::mem::discriminant(have) != std::mem::discriminant(want) {
                    return cmp == C::NotEquals;
                }
                let order = have.compare(want);
                match cmp {
                    C::Equals => order == Ordering::Equal,
                    C::NotEquals => order != Ordering::Equal,
                    C::Less => order == Ordering::Less,
                    C::LessOrEqual => order != Ordering::Greater,
                    C::Greater => order == Ordering::Greater,
                    C::GreaterOrEqual => order != Ordering::Less,
                    C::Exists | C::Missing | C::Contains => unreachable!(),
                }
            }
        }
    }
}

/// How a query orders its pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Sort {
    /// A property name, or `"title"`.
    pub by: String,
    pub descending: bool,
}

/// Which pages to list: every condition must hold.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PageQuery {
    /// Only pages of this kind, when set.
    pub kind: Option<PageKind>,
    pub conditions: Vec<Condition>,
    pub sort: Option<Sort>,
    /// At most this many pages, when set.
    pub limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_parse_and_print() {
        let d: Day = "2026-11-26".parse().unwrap();
        assert_eq!(d.to_string(), "2026-11-26");
        assert_eq!(d.title(), "Thursday, 26 November 2026");
        for bad in ["2026-02-30", "2026-1-01", "26-11-2026", "2026-11-26x", ""] {
            assert!(bad.parse::<Day>().is_err(), "{bad}");
        }
        assert_eq!(
            serde_json::to_string(&PropertyValue::Date(d)).unwrap(),
            r#"{"type":"date","value":"2026-11-26"}"#
        );
    }

    #[test]
    fn conditions() {
        let four = PropertyValue::Number(4.0);
        let cond = |comparison, value| Condition {
            property: MOOD.into(),
            comparison,
            value,
        };
        assert!(
            cond(Comparison::GreaterOrEqual, Some(PropertyValue::Number(4.0))).matches(Some(&four))
        );
        assert!(!cond(Comparison::Less, Some(PropertyValue::Number(4.0))).matches(Some(&four)));
        assert!(cond(Comparison::Exists, None).matches(Some(&four)));
        assert!(cond(Comparison::Missing, None).matches(None));
        assert!(!cond(Comparison::Equals, Some(PropertyValue::Number(4.0))).matches(None));
        assert!(
            cond(Comparison::NotEquals, Some(PropertyValue::Text("4".into()))).matches(Some(&four))
        );
        let text = PropertyValue::Text("Walk in the Park".into());
        assert!(
            cond(
                Comparison::Contains,
                Some(PropertyValue::Text("park".into()))
            )
            .matches(Some(&text))
        );
    }

    #[test]
    fn moods_round_trip_scores() {
        for m in Mood::ALL {
            assert_eq!(Mood::from_score(f64::from(m.score())), Some(m));
        }
        assert_eq!(Mood::from_score(3.4), Some(Mood::Okay));
        assert_eq!(Mood::from_score(9.0), Some(Mood::Great));
        assert_eq!(Mood::from_score(f64::NAN), None);
    }
}
