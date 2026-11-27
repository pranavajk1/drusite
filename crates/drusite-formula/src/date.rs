// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dates in formulas: arithmetic by unit, the difference between two, and Notion's
//! `formatDate` tokens (Day.js's).

use drusite_model::Day;
use drusite_model::database::{DateTime, DateValue};
use time::{Date, Duration, Month, PrimitiveDateTime, Time};

pub(crate) fn to_primitive(d: DateTime) -> PrimitiveDateTime {
    let date = Date::from_calendar_date(
        d.day.year(),
        Month::try_from(d.day.month()).unwrap_or(Month::January),
        d.day.day(),
    )
    .unwrap_or(Date::MIN);
    let m = d.minutes.unwrap_or(0);
    let time = Time::from_hms(
        u8::try_from(m / 60).unwrap_or(0),
        u8::try_from(m % 60).unwrap_or(0),
        0,
    )
    .unwrap_or(Time::MIDNIGHT);
    PrimitiveDateTime::new(date, time)
}

pub(crate) fn from_primitive(p: PrimitiveDateTime, timed: bool) -> Option<DateTime> {
    let day = Day::new(p.year(), p.month().into(), p.day())?;
    Some(DateTime {
        day,
        minutes: timed.then(|| u16::from(p.hour()) * 60 + u16::from(p.minute())),
    })
}

/// Unix milliseconds of a local time, `offset` minutes ahead of UTC.
pub(crate) fn timestamp(d: DateTime, offset: i32) -> f64 {
    let p = to_primitive(d);
    let secs = p.assume_utc().unix_timestamp() - i64::from(offset) * 60;
    secs as f64 * 1000.0
}

pub(crate) fn from_timestamp(ms: f64, offset: i32) -> Option<DateTime> {
    let secs = (ms / 1000.0).floor() as i64 + i64::from(offset) * 60;
    let utc = time::OffsetDateTime::from_unix_timestamp(secs).ok()?;
    from_primitive(PrimitiveDateTime::new(utc.date(), utc.time()), true)
}

/// A unit's canonical name, from Notion's spellings (`"days"`, `"day"`, `"d"`…).
pub(crate) fn unit(name: &str) -> Option<&'static str> {
    Some(match name.trim().to_lowercase().as_str() {
        "years" | "year" | "y" => "years",
        "quarters" | "quarter" | "q" => "quarters",
        "months" | "month" | "m" | "mo" => "months",
        "weeks" | "week" | "w" => "weeks",
        "days" | "day" | "d" => "days",
        "hours" | "hour" | "h" => "hours",
        "minutes" | "minute" | "min" => "minutes",
        "seconds" | "second" | "s" => "seconds",
        "milliseconds" | "millisecond" | "ms" => "milliseconds",
        _ => return None,
    })
}

fn add_months(p: PrimitiveDateTime, months: i64) -> Option<PrimitiveDateTime> {
    let total = i64::from(p.year()) * 12 + i64::from(u8::from(p.month())) - 1 + months;
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month = Month::try_from(u8::try_from(total.rem_euclid(12) + 1).ok()?).ok()?;
    // The day clamps to the month's end: January 31 plus a month is February 28.
    let last = time::util::days_in_month(month, year);
    let day = p.day().min(last);
    Some(PrimitiveDateTime::new(
        Date::from_calendar_date(year, month, day).ok()?,
        p.time(),
    ))
}

/// `d` moved by `amount` of `unit`. Adding hours or minutes to a date without a time gives it
/// one.
pub(crate) fn add(d: DateTime, amount: f64, unit: &str) -> Option<DateTime> {
    let p = to_primitive(d);
    let n = amount.trunc() as i64;
    let (out, timed) = match unit {
        "years" => (add_months(p, n * 12)?, d.minutes.is_some()),
        "quarters" => (add_months(p, n * 3)?, d.minutes.is_some()),
        "months" => (add_months(p, n)?, d.minutes.is_some()),
        "weeks" => (p.checked_add(Duration::weeks(n))?, d.minutes.is_some()),
        "days" => (p.checked_add(Duration::days(n))?, d.minutes.is_some()),
        "hours" => (p.checked_add(Duration::hours(n))?, true),
        "minutes" => (p.checked_add(Duration::minutes(n))?, true),
        "seconds" => (p.checked_add(Duration::seconds(n))?, true),
        "milliseconds" => (p.checked_add(Duration::milliseconds(n))?, true),
        _ => return None,
    };
    from_primitive(out, timed)
}

/// How many whole `unit`s from `b` to `a` (negative when `a` is earlier), truncated as Day.js
/// does.
pub(crate) fn between(a: DateTime, b: DateTime, unit: &str) -> Option<f64> {
    let (pa, pb) = (to_primitive(a), to_primitive(b));
    let months = |pa: PrimitiveDateTime, pb: PrimitiveDateTime| -> Option<i64> {
        let mut n = (i64::from(pa.year()) - i64::from(pb.year())) * 12
            + i64::from(u8::from(pa.month()))
            - i64::from(u8::from(pb.month()));
        let anchor = add_months(pb, n)?;
        if n > 0 && anchor > pa {
            n -= 1;
        } else if n < 0 && anchor < pa {
            n += 1;
        }
        Some(n)
    };
    let diff = pa - pb;
    let whole = |d: i64| d as f64;
    Some(match unit {
        "years" => whole(months(pa, pb)? / 12),
        "quarters" => whole(months(pa, pb)? / 3),
        "months" => whole(months(pa, pb)?),
        "weeks" => whole(diff.whole_weeks()),
        "days" => whole(diff.whole_days()),
        "hours" => whole(diff.whole_hours()),
        "minutes" => whole(diff.whole_minutes()),
        "seconds" => whole(diff.whole_seconds()),
        "milliseconds" => whole(i64::try_from(diff.whole_milliseconds()).ok()?),
        _ => return None,
    })
}

const MONTHS: [&str; 12] = [
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

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// A date in a Day.js format string, as `formatDate` does: `YYYY`, `MMMM`, `Do`, `dddd`,
/// `h:mm A`, `[literal text]`…
pub(crate) fn format(d: DateTime, pattern: &str) -> String {
    let p = to_primitive(d);
    let month = usize::from(u8::from(p.month()));
    let weekday = usize::from(p.weekday().number_days_from_sunday());
    let hour = p.hour();
    let h12 = if hour.is_multiple_of(12) {
        12
    } else {
        hour % 12
    };
    // Longest tokens first, so `MMMM` isn't read as `MM` twice.
    const TOKENS: [&str; 31] = [
        "YYYY", "MMMM", "dddd", "YY", "MMM", "MM", "Do", "DDDD", "DD", "ddd", "dd", "HH", "hh",
        "mm", "ss", "SSS", "Q", "M", "D", "d", "H", "h", "m", "s", "A", "a", "X", "x", "ZZ", "Z",
        "W",
    ];
    let mut out = String::new();
    let mut rest = pattern;
    while !rest.is_empty() {
        if let Some(inner) = rest.strip_prefix('[')
            && let Some(end) = inner.find(']')
        {
            out.push_str(&inner[..end]);
            rest = &inner[end + 1..];
            continue;
        }
        let Some(tok) = TOKENS.iter().find(|t| rest.starts_with(**t)) else {
            let c = rest.chars().next().unwrap_or(' ');
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        };
        rest = &rest[tok.len()..];
        let piece = match *tok {
            "YYYY" => format!("{:04}", p.year()),
            "YY" => format!("{:02}", p.year().rem_euclid(100)),
            "MMMM" => MONTHS[month - 1].to_owned(),
            "MMM" => MONTHS[month - 1][..3].to_owned(),
            "MM" => format!("{month:02}"),
            "M" => month.to_string(),
            "Q" => (((month - 1) / 3) + 1).to_string(),
            "DDDD" => format!("{:03}", p.ordinal()),
            "DD" => format!("{:02}", p.day()),
            "D" => p.day().to_string(),
            "Do" => ordinal(u32::from(p.day())),
            "dddd" => WEEKDAYS[weekday].to_owned(),
            "ddd" => WEEKDAYS[weekday][..3].to_owned(),
            "dd" => WEEKDAYS[weekday][..2].to_owned(),
            "d" => weekday.to_string(),
            "HH" => format!("{hour:02}"),
            "H" => hour.to_string(),
            "hh" => format!("{h12:02}"),
            "h" => h12.to_string(),
            "mm" => format!("{:02}", p.minute()),
            "m" => p.minute().to_string(),
            "ss" => format!("{:02}", p.second()),
            "s" => p.second().to_string(),
            "SSS" => "000".to_owned(),
            "A" => if hour < 12 { "AM" } else { "PM" }.to_owned(),
            "a" => if hour < 12 { "am" } else { "pm" }.to_owned(),
            "W" => p.iso_week().to_string(),
            "X" => (timestamp(d, 0) / 1000.0).to_string(),
            "x" => timestamp(d, 0).to_string(),
            // Time zones: the person's own time has none to print.
            _ => String::new(),
        };
        out.push_str(&piece);
    }
    out
}

fn show_one(d: DateTime) -> String {
    let base = format(DateTime::day(d.day), "MMMM D, YYYY");
    match d.minutes {
        None => base,
        Some(_) => format!("{base} {}", format(d, "h:mm A")),
    }
}

/// A date as Notion shows it in a cell: `November 27, 2026`, `November 27, 2026 1:05 PM`, or
/// a range with ` → ` between its ends.
pub fn show_date(d: &DateValue) -> String {
    match d.end {
        Some(end) if end != d.start => format!("{} → {}", show_one(d.start), show_one(end)),
        _ => show_one(d.start),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> DateTime {
        DateTime::parse(s).unwrap()
    }

    #[test]
    fn months_clamp_and_differences_truncate() {
        assert_eq!(
            add(dt("2026-01-31"), 1.0, "months").unwrap(),
            dt("2026-02-28")
        );
        assert_eq!(
            between(dt("2026-01-31"), dt("2026-01-01"), "months"),
            Some(0.0)
        );
        assert_eq!(
            between(dt("2026-03-01"), dt("2026-01-01"), "days"),
            Some(59.0)
        );
        assert_eq!(
            between(dt("2026-01-01"), dt("2026-03-01"), "months"),
            Some(-2.0)
        );
        assert_eq!(
            format(dt("2026-11-27T13:05"), "h:mm A [at] Do"),
            "1:05 PM at 27th"
        );
    }
}
