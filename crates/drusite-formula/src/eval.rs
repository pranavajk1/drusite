// SPDX-License-Identifier: AGPL-3.0-or-later

//! Computing a formula for one row.

use std::cmp::Ordering;

use drusite_model::PageId;
use drusite_model::database::{DateTime, DateValue};

use crate::parse::{Binary, Expr, Unary};
use crate::{Error, PageRef, Value, date};

/// What a formula can see: the row's properties, the pages it reaches, and the clock.
pub trait Context {
    /// Property `name` of the row (`page` none) or of a page reached through a relation.
    fn prop(&self, page: Option<PageId>, name: &str) -> Result<Value, Error>;
    /// The row itself, for `id()`.
    fn this_page(&self) -> PageRef;
    /// The person's local date and time, for `now()`.
    fn now(&self) -> DateTime;
    /// How many minutes the person's time is ahead of UTC, for timestamps.
    fn utc_offset(&self) -> i32;
}

/// Computes `expr` for the row `ctx` describes.
pub fn eval(expr: &Expr, ctx: &dyn Context) -> Result<Value, Error> {
    let mut ev = Eval {
        ctx,
        vars: Vec::new(),
        depth: 0,
    };
    let v = ev.eval(expr)?;
    Ok(normalise(v))
}

/// Numbers that aren't (1 / 0) show as nothing, as in Notion.
fn normalise(v: Value) -> Value {
    match v {
        Value::Number(n) if !n.is_finite() => Value::Empty,
        Value::List(items) => Value::List(items.into_iter().map(normalise).collect()),
        other => other,
    }
}

struct Eval<'a> {
    ctx: &'a dyn Context,
    vars: Vec<(String, Value)>,
    depth: u32,
}

fn err<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error::new(message))
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Empty => "empty",
        Value::Number(_) => "number",
        Value::Text(_) => "text",
        Value::Bool(_) => "boolean",
        Value::Date(_) => "date",
        Value::Person(_) => "person",
        Value::Page(_) => "page",
        Value::List(_) => "list",
    }
}

fn num(v: &Value, what: &str) -> Result<f64, Error> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Empty => Ok(0.0),
        other => err(format!("{what} needs a number, not a {}", type_name(other))),
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Empty => String::new(),
        other => other.format(),
    }
}

fn boolean(v: &Value, what: &str) -> Result<bool, Error> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Empty => Ok(false),
        other => err(format!(
            "{what} needs a boolean, not a {}",
            type_name(other)
        )),
    }
}

fn list(v: Value) -> Vec<Value> {
    match v {
        Value::List(l) => l,
        Value::Empty => Vec::new(),
        other => vec![other],
    }
}

fn date_of(v: &Value) -> Option<DateValue> {
    match v {
        Value::Date(d) => Some(*d),
        _ => None,
    }
}

fn int(v: &Value, what: &str) -> Result<i64, Error> {
    Ok(num(v, what)?.trunc() as i64)
}

/// Orders two values of a type that has an order; others are equal.
pub(crate) fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y),
        (Value::Text(x), Value::Text(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Date(x), Value::Date(y)) => Some(x.start.cmp(&y.start)),
        (Value::Empty, Value::Empty) => Some(Ordering::Equal),
        (Value::Empty, Value::Number(_)) => compare(&Value::Number(0.0), b),
        (Value::Number(_), Value::Empty) => compare(a, &Value::Number(0.0)),
        (Value::Empty, Value::Text(_)) => compare(&Value::Text(String::new()), b),
        (Value::Text(_), Value::Empty) => compare(a, &Value::Text(String::new())),
        (Value::Empty, Value::Bool(_)) => compare(&Value::Bool(false), b),
        (Value::Bool(_), Value::Empty) => compare(a, &Value::Bool(false)),
        _ => None,
    }
}

pub(crate) fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| equal(p, q))
        }
        (Value::Date(x), Value::Date(y)) => x == y,
        (Value::Person(x), Value::Person(y)) => x.id == y.id,
        (Value::Page(x), Value::Page(y)) => x.id == y.id,
        _ => compare(a, b) == Some(Ordering::Equal),
    }
}

fn regex(pattern: &str) -> Result<regex::Regex, Error> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| Error::new(format!("not a regular expression: {e}")))
}

/// JavaScript's `parseFloat`: the number at the start of the text, if there is one.
fn parse_float(s: &str) -> Value {
    let s = s.trim_start();
    let mut end = 0;
    let bytes = s.as_bytes();
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut seen_e = false;
    while end < bytes.len() {
        let c = bytes[end];
        match c {
            b'0'..=b'9' => seen_digit = true,
            b'+' | b'-' if end == 0 || matches!(bytes[end - 1], b'e' | b'E') => {}
            b'.' if !seen_dot && !seen_e => seen_dot = true,
            b'e' | b'E' if seen_digit && !seen_e => seen_e = true,
            _ => break,
        }
        end += 1;
    }
    let mut candidate = &s[..end];
    while !candidate.is_empty() {
        if let Ok(n) = candidate.parse::<f64>() {
            return Value::Number(n);
        }
        candidate = &candidate[..candidate.len() - 1];
    }
    Value::Empty
}

fn chars_slice(s: &str, start: i64, end: Option<i64>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len() as i64;
    let clamp = |i: i64| i.clamp(0, len) as usize;
    let (mut a, mut b) = (clamp(start), clamp(end.unwrap_or(len)));
    // substring() swaps its ends, as JavaScript's does.
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    chars[a..b].iter().collect()
}

fn list_index(len: usize, i: i64) -> Option<usize> {
    let len = len as i64;
    let i = if i < 0 { len + i } else { i };
    (0..len).contains(&i).then_some(i as usize)
}

fn sort_values(items: &mut [Value]) {
    items.sort_by(|a, b| compare(a, b).unwrap_or(Ordering::Equal));
}

fn numbers(args: Vec<Value>, what: &str) -> Result<Vec<f64>, Error> {
    let mut out = Vec::new();
    for a in args {
        match a {
            Value::List(items) => out.extend(numbers(items, what)?),
            Value::Empty => {}
            other => out.push(num(&other, what)?),
        }
    }
    Ok(out)
}

fn median(mut v: Vec<f64>) -> Value {
    if v.is_empty() {
        return Value::Empty;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let mid = v.len() / 2;
    Value::Number(if v.len().is_multiple_of(2) {
        f64::midpoint(v[mid - 1], v[mid])
    } else {
        v[mid]
    })
}

/// JavaScript's `Math.round`: halves go up.
fn js_round(n: f64) -> f64 {
    (n + 0.5).floor()
}

fn date_value(d: DateTime) -> Value {
    Value::Date(DateValue {
        start: d,
        end: None,
    })
}

impl Eval<'_> {
    fn eval(&mut self, e: &Expr) -> Result<Value, Error> {
        self.depth += 1;
        if self.depth > 200 {
            return err("the formula is nested too deeply");
        }
        let out = self.eval_inner(e);
        self.depth -= 1;
        out
    }

    fn eval_inner(&mut self, e: &Expr) -> Result<Value, Error> {
        match e {
            Expr::Number(n) => Ok(Value::Number(*n)),
            Expr::Text(s) => Ok(Value::Text(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::List(items) => Ok(Value::List(
                items
                    .iter()
                    .map(|i| self.eval(i))
                    .collect::<Result<_, _>>()?,
            )),
            Expr::Name(n) => match self.vars.iter().rev().find(|(name, _)| name == n) {
                Some((_, v)) => Ok(v.clone()),
                None => err(format!("{n} isn't defined here")),
            },
            Expr::Unary { op, expr } => {
                let v = self.eval(expr)?;
                Ok(match op {
                    Unary::Neg => Value::Number(-num(&v, "-")?),
                    Unary::Plus => Value::Number(num(&v, "+")?),
                    Unary::Not => Value::Bool(!boolean(&v, "not")?),
                })
            }
            Expr::If { cond, then, other } => {
                if boolean(&self.eval(cond)?, "if")? {
                    self.eval(then)
                } else {
                    self.eval(other)
                }
            }
            Expr::Binary { op, left, right } => match op {
                Binary::And => Ok(Value::Bool(
                    boolean(&self.eval(left)?, "and")? && boolean(&self.eval(right)?, "and")?,
                )),
                Binary::Or => Ok(Value::Bool(
                    boolean(&self.eval(left)?, "or")? || boolean(&self.eval(right)?, "or")?,
                )),
                _ => {
                    let a = self.eval(left)?;
                    let b = self.eval(right)?;
                    binary(*op, &a, &b)
                }
            },
            Expr::Call { name, args } => self.call(name, args),
        }
    }

    fn args(&mut self, args: &[Expr]) -> Result<Vec<Value>, Error> {
        args.iter().map(|a| self.eval(a)).collect()
    }

    /// Runs `body` once per item of the list `args[0]`, with `current` and `index` set.
    fn each(
        &mut self,
        args: &[Expr],
        what: &str,
        mut step: impl FnMut(usize, Value, Value) -> Option<Value>,
    ) -> Result<Option<Value>, Error> {
        let [items, body, ..] = args else {
            return err(format!("{what} needs a list and an expression"));
        };
        let items = list(self.eval(items)?);
        for (i, item) in items.into_iter().enumerate() {
            self.vars.push(("current".into(), item.clone()));
            self.vars.push(("index".into(), Value::Number(i as f64)));
            let out = self.eval(body);
            self.vars.truncate(self.vars.len() - 2);
            if let Some(done) = step(i, item, out?) {
                return Ok(Some(done));
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_lines)]
    fn call(&mut self, name: &str, args: &[Expr]) -> Result<Value, Error> {
        // The functions that decide which of their arguments to compute.
        match name {
            "if" => {
                let [c, t, o] = args else {
                    return err("if() needs a condition and two values");
                };
                return if boolean(&self.eval(c)?, "if")? {
                    self.eval(t)
                } else {
                    self.eval(o)
                };
            }
            "ifs" => {
                let (pairs, rest) = args.as_chunks::<2>();
                for [cond, value] in pairs {
                    if boolean(&self.eval(cond)?, "ifs")? {
                        return self.eval(value);
                    }
                }
                return match rest {
                    [other] => self.eval(other),
                    _ => Ok(Value::Empty),
                };
            }
            "let" | "lets" => {
                let Some((body, binds)) = args.split_last() else {
                    return err(format!("{name}() needs a name, a value and an expression"));
                };
                if binds.len() % 2 != 0 || binds.is_empty() {
                    return err(format!("{name}() needs a name, a value and an expression"));
                }
                let before = self.vars.len();
                for pair in binds.as_chunks::<2>().0 {
                    let Expr::Name(var) = &pair[0] else {
                        self.vars.truncate(before);
                        return err(format!("{name}() names its variables with plain words"));
                    };
                    let v = self.eval(&pair[1]);
                    match v {
                        Ok(v) => self.vars.push((var.clone(), v)),
                        Err(e) => {
                            self.vars.truncate(before);
                            return Err(e);
                        }
                    }
                }
                let out = self.eval(body);
                self.vars.truncate(before);
                return out;
            }
            "map" => {
                let mut out = Vec::new();
                self.each(args, "map()", |_, _, v| {
                    out.push(v);
                    None
                })?;
                return Ok(Value::List(out));
            }
            "filter" => {
                let mut out = Vec::new();
                let mut bad = None;
                self.each(args, "filter()", |_, item, keep| {
                    match boolean(&keep, "filter()") {
                        Ok(true) => out.push(item),
                        Ok(false) => {}
                        Err(e) => bad = Some(e),
                    }
                    None
                })?;
                return match bad {
                    Some(e) => Err(e),
                    None => Ok(Value::List(out)),
                };
            }
            "find" => {
                return Ok(self
                    .each(args, "find()", |_, item, hit| {
                        matches!(hit, Value::Bool(true)).then_some(item)
                    })?
                    .unwrap_or(Value::Empty));
            }
            "findIndex" => {
                return Ok(self
                    .each(args, "findIndex()", |i, _, hit| {
                        matches!(hit, Value::Bool(true)).then_some(Value::Number(i as f64))
                    })?
                    .unwrap_or(Value::Number(-1.0)));
            }
            "some" => {
                return Ok(Value::Bool(
                    self.each(args, "some()", |_, _, hit| {
                        matches!(hit, Value::Bool(true)).then_some(Value::Bool(true))
                    })?
                    .is_some(),
                ));
            }
            "every" => {
                return Ok(Value::Bool(
                    self.each(args, "every()", |_, _, hit| {
                        (!matches!(hit, Value::Bool(true))).then_some(Value::Bool(false))
                    })?
                    .is_none(),
                ));
            }
            "count" if args.len() == 2 => {
                let mut n = 0.0;
                self.each(args, "count()", |_, _, hit| {
                    if matches!(hit, Value::Bool(true)) {
                        n += 1.0;
                    }
                    None
                })?;
                return Ok(Value::Number(n));
            }
            "sort" if args.len() == 2 => {
                let mut keyed = Vec::new();
                self.each(args, "sort()", |_, item, key| {
                    keyed.push((key, item));
                    None
                })?;
                keyed.sort_by(|a, b| compare(&a.0, &b.0).unwrap_or(Ordering::Equal));
                return Ok(Value::List(keyed.into_iter().map(|(_, v)| v).collect()));
            }
            _ => {}
        }

        let a = self.args(args)?;
        let arity = |min: usize, max: usize| -> Result<(), Error> {
            if a.len() < min || a.len() > max {
                let want = if min == max {
                    format!("{min}")
                } else {
                    format!("{min} to {max}")
                };
                return err(format!("{name}() takes {want} arguments, not {}", a.len()));
            }
            Ok(())
        };
        let n = |i: usize| num(&a[i], name);
        let t = |i: usize| text(&a[i]);
        let offset = self.ctx.utc_offset();
        Ok(match name {
            "prop" => {
                arity(1, 2)?;
                return match a.as_slice() {
                    [Value::Text(p)] => self.ctx.prop(None, p),
                    [Value::Page(page), Value::Text(p)] => self.ctx.prop(Some(page.id), p),
                    [Value::Empty, Value::Text(_)] => Ok(Value::Empty),
                    _ => err("prop() needs a property's name, in quotes"),
                };
            }
            "empty" => {
                arity(0, 1)?;
                match a.first() {
                    None => Value::Empty,
                    Some(v) => Value::Bool(v.is_empty()),
                }
            }
            "not" => {
                arity(1, 1)?;
                Value::Bool(!boolean(&a[0], name)?)
            }
            "and" => Value::Bool(
                a.iter()
                    .map(|v| boolean(v, name))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .all(|b| b),
            ),
            "or" => Value::Bool(
                a.iter()
                    .map(|v| boolean(v, name))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .any(|b| b),
            ),
            "equal" => {
                arity(2, 2)?;
                Value::Bool(equal(&a[0], &a[1]))
            }
            "unequal" => {
                arity(2, 2)?;
                Value::Bool(!equal(&a[0], &a[1]))
            }
            "add" => {
                arity(2, 2)?;
                return binary(Binary::Add, &a[0], &a[1]);
            }
            "subtract" => {
                arity(2, 2)?;
                return binary(Binary::Sub, &a[0], &a[1]);
            }
            "multiply" => {
                arity(2, 2)?;
                return binary(Binary::Mul, &a[0], &a[1]);
            }
            "divide" => {
                arity(2, 2)?;
                return binary(Binary::Div, &a[0], &a[1]);
            }
            "mod" => {
                arity(2, 2)?;
                return binary(Binary::Rem, &a[0], &a[1]);
            }
            "pow" => {
                arity(2, 2)?;
                return binary(Binary::Pow, &a[0], &a[1]);
            }
            "abs" | "ceil" | "floor" | "sqrt" | "cbrt" | "exp" | "ln" | "log10" | "log2"
            | "sign" => {
                arity(1, 1)?;
                let x = n(0)?;
                Value::Number(match name {
                    "abs" => x.abs(),
                    "ceil" => x.ceil(),
                    "floor" => x.floor(),
                    "sqrt" => x.sqrt(),
                    "cbrt" => x.cbrt(),
                    "exp" => x.exp(),
                    "ln" => x.ln(),
                    "log10" => x.log10(),
                    "log2" => x.log2(),
                    _ => {
                        if x > 0.0 {
                            1.0
                        } else if x < 0.0 {
                            -1.0
                        } else {
                            x
                        }
                    }
                })
            }
            "round" => {
                arity(1, 2)?;
                let x = n(0)?;
                let places = if a.len() == 2 { n(1)?.trunc() } else { 0.0 };
                let scale = 10f64.powf(places);
                let r = js_round(x * scale) / scale;
                // -0 shows as 0.
                Value::Number(if r == 0.0 { 0.0 } else { r })
            }
            "pi" => Value::Number(std::f64::consts::PI),
            "e" => Value::Number(std::f64::consts::E),
            "max" | "min" | "sum" | "mean" | "average" | "median" => {
                let v = numbers(a, name)?;
                match name {
                    "sum" => Value::Number(v.iter().sum()),
                    "median" => median(v),
                    _ if v.is_empty() => Value::Empty,
                    "max" => Value::Number(v.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
                    "min" => Value::Number(v.iter().copied().fold(f64::INFINITY, f64::min)),
                    _ => Value::Number(v.iter().sum::<f64>() / v.len() as f64),
                }
            }
            "toNumber" => {
                arity(1, 1)?;
                match &a[0] {
                    Value::Number(x) => Value::Number(*x),
                    Value::Bool(b) => Value::Number(if *b { 1.0 } else { 0.0 }),
                    Value::Text(s) => parse_float(s),
                    Value::Date(d) => Value::Number(date::timestamp(d.start, offset)),
                    Value::Empty => Value::Empty,
                    other => return err(format!("toNumber() can't read a {}", type_name(other))),
                }
            }
            "format" => {
                arity(1, 1)?;
                Value::Text(a[0].format())
            }
            "length" => {
                arity(1, 1)?;
                match &a[0] {
                    Value::List(l) => Value::Number(l.len() as f64),
                    Value::Text(s) => Value::Number(s.chars().count() as f64),
                    Value::Empty => Value::Number(0.0),
                    other => {
                        return err(format!(
                            "length() needs text or a list, not a {}",
                            type_name(other)
                        ));
                    }
                }
            }
            "substring" => {
                arity(2, 3)?;
                let end = if a.len() == 3 {
                    Some(int(&a[2], name)?)
                } else {
                    None
                };
                Value::Text(chars_slice(&t(0), int(&a[1], name)?, end))
            }
            "contains" => {
                arity(2, 2)?;
                match &a[0] {
                    Value::List(l) => Value::Bool(l.iter().any(|v| equal(v, &a[1]))),
                    _ => Value::Bool(t(0).contains(&t(1))),
                }
            }
            "test" => {
                arity(2, 2)?;
                Value::Bool(regex(&t(1))?.is_match(&t(0)))
            }
            "match" => {
                arity(2, 2)?;
                let s = t(0);
                Value::List(
                    regex(&t(1))?
                        .find_iter(&s)
                        .map(|m| Value::Text(m.as_str().to_owned()))
                        .collect(),
                )
            }
            "replace" | "replaceAll" => {
                arity(3, 3)?;
                let re = regex(&t(1))?;
                let s = t(0);
                let with = t(2);
                Value::Text(if name == "replace" {
                    re.replace(&s, with.as_str()).into_owned()
                } else {
                    re.replace_all(&s, with.as_str()).into_owned()
                })
            }
            "lower" => {
                arity(1, 1)?;
                Value::Text(t(0).to_lowercase())
            }
            "upper" => {
                arity(1, 1)?;
                Value::Text(t(0).to_uppercase())
            }
            "trim" => {
                arity(1, 1)?;
                Value::Text(t(0).trim().to_owned())
            }
            "repeat" => {
                arity(2, 2)?;
                let times = usize::try_from(int(&a[1], name)?.clamp(0, 10_000)).unwrap_or(0);
                let s = t(0);
                if s.len().saturating_mul(times) > 1_000_000 {
                    return err("repeat() would make too long a text");
                }
                Value::Text(s.repeat(times))
            }
            "padStart" | "padEnd" => {
                arity(2, 3)?;
                let s = t(0);
                let target = usize::try_from(int(&a[1], name)?.clamp(0, 100_000)).unwrap_or(0);
                let pad = if a.len() == 3 { t(2) } else { " ".to_owned() };
                let have = s.chars().count();
                if have >= target || pad.is_empty() {
                    Value::Text(s)
                } else {
                    let fill: String = pad.chars().cycle().take(target - have).collect();
                    Value::Text(if name == "padStart" {
                        fill + &s
                    } else {
                        s + &fill
                    })
                }
            }
            "link" => {
                arity(2, 2)?;
                Value::Text(t(0))
            }
            "style" | "unstyle" => {
                if a.is_empty() {
                    return err(format!("{name}() needs text"));
                }
                Value::Text(t(0))
            }
            "split" => {
                arity(1, 2)?;
                let s = t(0);
                let sep = if a.len() == 2 { t(1) } else { String::new() };
                Value::List(if sep.is_empty() {
                    s.chars().map(|c| Value::Text(c.to_string())).collect()
                } else {
                    s.split(sep.as_str())
                        .map(|p| Value::Text(p.to_owned()))
                        .collect()
                })
            }
            "join" => {
                arity(1, 2)?;
                let sep = if a.len() == 2 { t(1) } else { String::new() };
                Value::Text(
                    list(a[0].clone())
                        .iter()
                        .map(text)
                        .collect::<Vec<_>>()
                        .join(&sep),
                )
            }
            "concat" => Value::List(a.into_iter().flat_map(list).collect()),
            "at" => {
                arity(2, 2)?;
                let l = list(a[0].clone());
                list_index(l.len(), int(&a[1], name)?).map_or(Value::Empty, |i| l[i].clone())
            }
            "first" | "last" => {
                arity(1, 1)?;
                let l = list(a[0].clone());
                let item = if name == "first" { l.first() } else { l.last() };
                item.cloned().unwrap_or(Value::Empty)
            }
            "slice" => {
                arity(2, 3)?;
                match &a[0] {
                    Value::Text(s) => {
                        let len = s.chars().count();
                        let start = int(&a[1], name)?;
                        let end = if a.len() == 3 {
                            int(&a[2], name)?
                        } else {
                            len as i64
                        };
                        let fix = |i: i64| if i < 0 { (len as i64 + i).max(0) } else { i };
                        let (s0, e0) = (fix(start), fix(end));
                        if s0 >= e0 {
                            Value::Text(String::new())
                        } else {
                            Value::Text(chars_slice(s, s0, Some(e0)))
                        }
                    }
                    other => {
                        let l = list(other.clone());
                        let len = l.len() as i64;
                        let fix = |i: i64| (if i < 0 { len + i } else { i }).clamp(0, len) as usize;
                        let start = fix(int(&a[1], name)?);
                        let end = if a.len() == 3 {
                            fix(int(&a[2], name)?)
                        } else {
                            l.len()
                        };
                        Value::List(if start < end {
                            l[start..end].to_vec()
                        } else {
                            Vec::new()
                        })
                    }
                }
            }
            "splice" => {
                if a.len() < 3 {
                    return err("splice() needs a list, a start and a count");
                }
                let mut l = list(a[0].clone());
                let len = l.len() as i64;
                let s = int(&a[1], name)?;
                let start = (if s < 0 { len + s } else { s }).clamp(0, len) as usize;
                let count = usize::try_from(int(&a[2], name)?.max(0)).unwrap_or(0);
                let end = (start + count).min(l.len());
                l.splice(start..end, a[3..].iter().cloned());
                Value::List(l)
            }
            "sort" => {
                arity(1, 1)?;
                let mut l = list(a[0].clone());
                sort_values(&mut l);
                Value::List(l)
            }
            "reverse" => {
                arity(1, 1)?;
                let mut l = list(a[0].clone());
                l.reverse();
                Value::List(l)
            }
            "unique" => {
                arity(1, 1)?;
                let mut out: Vec<Value> = Vec::new();
                for v in list(a[0].clone()) {
                    if !out.iter().any(|o| equal(o, &v)) {
                        out.push(v);
                    }
                }
                Value::List(out)
            }
            "includes" => {
                arity(2, 2)?;
                Value::Bool(list(a[0].clone()).iter().any(|v| equal(v, &a[1])))
            }
            "flat" => {
                arity(1, 1)?;
                Value::List(list(a[0].clone()).into_iter().flat_map(list).collect())
            }
            "count" => {
                arity(1, 1)?;
                Value::Number(list(a[0].clone()).len() as f64)
            }
            "now" => date_value(self.ctx.now()),
            "today" => date_value(DateTime::day(self.ctx.now().day)),
            "minute" | "hour" | "day" | "date" | "week" | "month" | "year" => {
                arity(1, 1)?;
                let Some(d) = date_of(&a[0]) else {
                    return Ok(Value::Empty);
                };
                let p = date::to_primitive(d.start);
                Value::Number(f64::from(match name {
                    "minute" => i32::from(p.minute()),
                    "hour" => i32::from(p.hour()),
                    // 1 is Monday, 7 Sunday.
                    "day" => i32::from(p.weekday().number_from_monday()),
                    "date" => i32::from(p.day()),
                    "week" => i32::from(p.iso_week()),
                    "month" => i32::from(u8::from(p.month())),
                    _ => p.year(),
                }))
            }
            "dateAdd" | "dateSubtract" => {
                arity(3, 3)?;
                let Some(d) = date_of(&a[0]) else {
                    return Ok(Value::Empty);
                };
                let Some(unit) = date::unit(&t(2)) else {
                    return err(format!("{name}() doesn't know the unit {:?}", t(2)));
                };
                let amount = if name == "dateAdd" { n(1)? } else { -n(1)? };
                date::add(d.start, amount, unit).map_or(Value::Empty, date_value)
            }
            "dateBetween" => {
                arity(3, 3)?;
                let (Some(x), Some(y)) = (date_of(&a[0]), date_of(&a[1])) else {
                    return Ok(Value::Empty);
                };
                let Some(unit) = date::unit(&t(2)) else {
                    return err(format!("dateBetween() doesn't know the unit {:?}", t(2)));
                };
                date::between(x.start, y.start, unit).map_or(Value::Empty, Value::Number)
            }
            "dateRange" => {
                arity(2, 2)?;
                match (date_of(&a[0]), date_of(&a[1])) {
                    (Some(x), Some(y)) => Value::Date(DateValue {
                        start: x.start,
                        end: Some(y.end.unwrap_or(y.start)),
                    }),
                    (Some(x), None) => Value::Date(x),
                    _ => Value::Empty,
                }
            }
            "dateStart" | "dateEnd" => {
                arity(1, 1)?;
                match date_of(&a[0]) {
                    Some(d) if name == "dateEnd" => date_value(d.end.unwrap_or(d.start)),
                    Some(d) => date_value(d.start),
                    None => Value::Empty,
                }
            }
            "timestamp" => {
                arity(1, 1)?;
                match date_of(&a[0]) {
                    Some(d) => Value::Number(date::timestamp(d.start, offset)),
                    None => Value::Empty,
                }
            }
            "fromTimestamp" => {
                arity(1, 1)?;
                date::from_timestamp(n(0)?, offset).map_or(Value::Empty, date_value)
            }
            "formatDate" => {
                arity(2, 3)?;
                match date_of(&a[0]) {
                    Some(d) => Value::Text(date::format(d.start, &t(1))),
                    None => Value::Empty,
                }
            }
            "parseDate" => {
                arity(1, 1)?;
                match DateTime::parse(&t(0)) {
                    Some(d) => date_value(d),
                    None => return err(format!("parseDate() can't read {:?}", t(0))),
                }
            }
            "name" | "email" => {
                arity(1, 1)?;
                let pick = |v: &Value| match v {
                    Value::Person(p) if name == "name" => Value::Text(p.name.clone()),
                    Value::Person(p) => p.email.clone().map_or(Value::Empty, Value::Text),
                    _ => Value::Empty,
                };
                match &a[0] {
                    Value::List(l) => Value::List(l.iter().map(pick).collect()),
                    other => pick(other),
                }
            }
            "id" => {
                arity(0, 1)?;
                let id = match a.first() {
                    None => self.ctx.this_page().id,
                    Some(Value::Page(p)) => p.id,
                    Some(Value::Person(p)) => return Ok(Value::Text(p.id.clone())),
                    Some(_) => return Ok(Value::Empty),
                };
                Value::Text(id.as_uuid().simple().to_string())
            }
            _ => return err(format!("Notion has no function called {name}()")),
        })
    }
}

fn binary(op: Binary, a: &Value, b: &Value) -> Result<Value, Error> {
    use Binary as B;
    Ok(match op {
        B::Add => match (a, b) {
            (Value::Text(_), _) | (_, Value::Text(_)) => Value::Text(text(a) + &text(b)),
            (Value::List(x), Value::List(y)) => Value::List(x.iter().chain(y).cloned().collect()),
            _ => Value::Number(num(a, "+")? + num(b, "+")?),
        },
        B::Sub => Value::Number(num(a, "-")? - num(b, "-")?),
        B::Mul => Value::Number(num(a, "*")? * num(b, "*")?),
        B::Div => Value::Number(num(a, "/")? / num(b, "/")?),
        B::Rem => Value::Number(num(a, "%")? % num(b, "%")?),
        B::Pow => Value::Number(num(a, "^")?.powf(num(b, "^")?)),
        B::Eq => Value::Bool(equal(a, b)),
        B::Ne => Value::Bool(!equal(a, b)),
        B::Lt | B::Le | B::Gt | B::Ge => {
            let Some(order) = compare(a, b) else {
                return Ok(Value::Bool(false));
            };
            Value::Bool(match op {
                B::Lt => order == Ordering::Less,
                B::Le => order != Ordering::Greater,
                B::Gt => order == Ordering::Greater,
                _ => order != Ordering::Less,
            })
        }
        B::And | B::Or => unreachable!("decided before both sides are computed"),
    })
}
