// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every formula in `notion_oracle.txt` must show what Notion showed, on both rows; and
//! property tests check the operators against JavaScript's arithmetic, which the oracle shows
//! Notion uses, over many more expressions than the oracle could hold.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use drusite_formula::{Context, Error, PageRef, Type, TypeEnv, Value, check, eval, parse};
use drusite_model::PageId;
use drusite_model::database::{DateTime, DateValue};
use proptest::prelude::*;

struct Row {
    props: HashMap<&'static str, Value>,
    id: PageId,
}

impl Context for Row {
    fn prop(&self, _page: Option<PageId>, name: &str) -> Result<Value, Error> {
        self.props.get(name).cloned().ok_or_else(|| Error {
            at: None,
            message: format!("no property {name}"),
        })
    }

    fn this_page(&self) -> PageRef {
        PageRef {
            id: self.id,
            title: String::new(),
        }
    }

    fn now(&self) -> DateTime {
        DateTime::parse("2026-11-27T12:00").unwrap()
    }

    fn utc_offset(&self) -> i32 {
        // The time zone Notion was viewed in.
        -360
    }
}

impl TypeEnv for Row {
    fn prop(&self, name: &str) -> Option<Type> {
        Some(match name {
            "Num" => Type::Number,
            "Txt" | "Pick" | "Stat" | "Name" => Type::Text,
            "When" => Type::Date,
            "Done" => Type::Bool,
            "Tags" => Type::List(Box::new(Type::Text)),
            _ => return None,
        })
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn full() -> Row {
    let date = |s| DateTime::parse(s).unwrap();
    Row {
        id: PageId::new(),
        props: HashMap::from([
            ("Num", Value::Number(4.0)),
            ("Txt", text("Hello")),
            (
                "When",
                Value::Date(DateValue {
                    start: date("2026-11-27"),
                    end: Some(date("2026-11-30")),
                }),
            ),
            ("Done", Value::Bool(true)),
            ("Tags", Value::List(vec![text("x"), text("y")])),
            ("Pick", text("a")),
            ("Stat", text("Doing")),
            ("Name", text("Full")),
        ]),
    }
}

fn empty() -> Row {
    Row {
        id: PageId::new(),
        props: HashMap::from([
            ("Num", Value::Empty),
            ("Txt", Value::Empty),
            ("When", Value::Empty),
            ("Done", Value::Bool(false)),
            ("Tags", Value::List(Vec::new())),
            ("Pick", Value::Empty),
            ("Stat", text("Todo")),
            ("Name", Value::Empty),
        ]),
    }
}

#[test]
fn notion_shows_the_same() {
    let oracle = include_str!("notion_oracle.txt");
    let rows = [full(), empty()];
    let mut checked = 0;
    let mut wrong = Vec::new();
    for line in oracle.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(" ⟶ ").collect();
        let src = parts[0];
        let want_full = parts.get(1).copied().unwrap_or("").trim_end();
        let want_empty = parts.get(2).map_or(want_full, |s| s.trim_end());
        let expr = match parse(src) {
            Ok(e) => e,
            Err(e) => {
                wrong.push(format!("{src}: doesn't parse: {e}"));
                continue;
            }
        };
        if let Err(e) = check(&expr, &rows[0]) {
            wrong.push(format!("{src}: refused: {e}"));
            continue;
        }
        for (row, want) in rows.iter().zip([want_full, want_empty]) {
            let got = eval(&expr, row).map(|v| v.show());
            match got {
                Ok(got) if got == want => {}
                other => wrong.push(format!("{src}: got {other:?}, Notion shows {want:?}")),
            }
        }
        checked += 1;
    }
    assert!(checked > 100, "only {checked} formulas in the oracle");
    assert!(
        wrong.is_empty(),
        "{} differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn refusals() {
    let row = full();
    for bad in [
        "lower(1)",
        "prop(\"Nope\")",
        "nosuch(1)",
        "1 - \"a\"",
        "if(1, 2, 3)",
        "dateAdd(1, 2, \"days\")",
        "x + 1",
        "round()",
    ] {
        let expr = parse(bad).unwrap();
        assert!(check(&expr, &row).is_err(), "{bad} should be refused");
    }
}

/// A random arithmetic expression, and its value by JavaScript's rules.
fn arith() -> impl Strategy<Value = (String, f64)> {
    let leaf = (-50i32..50).prop_map(|n| (format!("({n})"), f64::from(n)));
    leaf.prop_recursive(4, 24, 2, |inner| {
        (inner.clone(), 0usize..5, inner).prop_map(|((a, x), op, (b, y))| match op {
            0 => (format!("({a} + {b})"), x + y),
            1 => (format!("({a} - {b})"), x - y),
            2 => (format!("({a} * {b})"), x * y),
            3 => (format!("({a} / {b})"), x / y),
            _ => (format!("({a} % {b})"), x % y),
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arithmetic_matches_javascript((src, want) in arith()) {
        let got = eval(&parse(&src).unwrap(), &full()).unwrap();
        if want.is_finite() {
            prop_assert_eq!(got, Value::Number(want));
        } else {
            prop_assert_eq!(got, Value::Empty);
        }
    }

    #[test]
    fn format_and_to_number_round_trip(n in -1e15f64..1e15) {
        let src = format!("toNumber(format({n:?}))");
        let got = eval(&parse(&src).unwrap(), &full()).unwrap();
        prop_assert_eq!(got, Value::Number(n));
    }

    #[test]
    fn date_add_and_between_agree(days in -3000i32..3000, months in -120i32..120) {
        let src = format!(
            "dateBetween(dateAdd(parseDate(\"2026-01-15\"), {days}, \"days\"), parseDate(\"2026-01-15\"), \"days\")"
        );
        prop_assert_eq!(eval(&parse(&src).unwrap(), &full()).unwrap(), Value::Number(f64::from(days)));
        // The 15th never clamps, so adding months then measuring gives them back.
        let src = format!(
            "dateBetween(dateAdd(parseDate(\"2026-01-15\"), {months}, \"months\"), parseDate(\"2026-01-15\"), \"months\")"
        );
        prop_assert_eq!(eval(&parse(&src).unwrap(), &full()).unwrap(), Value::Number(f64::from(months)));
    }

    #[test]
    fn whatever_parses_type_checks_or_is_refused_without_panicking(src in "[a-z0-9()+*/,.\"<>=!? -]{0,40}") {
        if let Ok(e) = parse(&src) {
            let row = full();
            if check(&e, &row).is_ok() {
                let _ = eval(&e, &row);
            }
        }
    }

    #[test]
    fn well_typed_lists_keep_their_length(items in prop::collection::vec(-100i32..100, 0..20)) {
        let list = format!("[{}]", items.iter().map(i32::to_string).collect::<Vec<_>>().join(","));
        for f in ["map(current * 2)", "sort()", "reverse()"] {
            let src = format!("{list}.{f}.length()");
            prop_assert_eq!(eval(&parse(&src).unwrap(), &full()).unwrap(), Value::Number(items.len() as f64));
        }
        let src = format!("{list}.filter(current > 0).length() + {list}.filter(current <= 0).length()");
        prop_assert_eq!(eval(&parse(&src).unwrap(), &full()).unwrap(), Value::Number(items.len() as f64));
    }
}
