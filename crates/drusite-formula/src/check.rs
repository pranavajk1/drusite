// SPDX-License-Identifier: AGPL-3.0-or-later

//! Working out a formula's type before it runs, and refusing what Notion's editor refuses: a
//! function it doesn't have, the wrong number of arguments, text where a number must go, a
//! property the database doesn't have.

use serde::{Deserialize, Serialize};
use specta::Type as SpectaType;

use crate::Error;
use crate::parse::{Binary, Expr, Unary};

/// A formula value's type. `Any` is a type the checker can't pin down (a rollup, an item of a
/// mixed list), and passes everywhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, SpectaType)]
#[serde(tag = "type", content = "item", rename_all = "camelCase")]
pub enum Type {
    Any,
    Number,
    Text,
    Bool,
    Date,
    Person,
    Page,
    List(Box<Type>),
}

impl Type {
    fn name(&self) -> String {
        match self {
            Self::Any => "anything".into(),
            Self::Number => "number".into(),
            Self::Text => "text".into(),
            Self::Bool => "boolean".into(),
            Self::Date => "date".into(),
            Self::Person => "person".into(),
            Self::Page => "page".into(),
            Self::List(t) => format!("list of {}", t.name()),
        }
    }

    fn fits(&self, want: &Type) -> bool {
        match (self, want) {
            (Self::Any, _) | (_, Self::Any) => true,
            (Self::List(a), Self::List(b)) => a.fits(b),
            (a, b) => a == b,
        }
    }

    fn item(&self) -> Type {
        match self {
            Self::List(t) => (**t).clone(),
            Self::Any => Self::Any,
            other => other.clone(),
        }
    }
}

/// The properties a formula can name, and their types.
pub trait TypeEnv {
    /// The type `prop(name)` has on the formula's own row; none if there's no such property.
    fn prop(&self, name: &str) -> Option<Type>;
}

/// The type a formula gives, or why Notion would refuse it.
pub fn check(expr: &Expr, env: &dyn TypeEnv) -> Result<Type, Error> {
    Checker {
        env,
        vars: Vec::new(),
    }
    .ty(expr)
}

struct Checker<'a> {
    env: &'a dyn TypeEnv,
    vars: Vec<(String, Type)>,
}

fn refuse<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::new(m))
}

fn unify(a: &Type, b: &Type) -> Type {
    if a == b {
        a.clone()
    } else if *a == Type::Any {
        b.clone()
    } else if *b == Type::Any {
        a.clone()
    } else {
        Type::Any
    }
}

fn want(f: &str, got: &Type, want: &Type) -> Result<(), Error> {
    if got.fits(want) {
        Ok(())
    } else {
        refuse(format!(
            "{f}() needs a {} here, not a {}",
            want.name(),
            got.name()
        ))
    }
}

fn want_list(f: &str, got: &Type) -> Result<(), Error> {
    if matches!(got, Type::List(_) | Type::Any) {
        Ok(())
    } else {
        refuse(format!("{f}() needs a list, not a {}", got.name()))
    }
}

const LAMBDAS: [&str; 8] = [
    "map",
    "filter",
    "find",
    "findIndex",
    "some",
    "every",
    "count",
    "sort",
];

impl Checker<'_> {
    fn ty(&mut self, e: &Expr) -> Result<Type, Error> {
        Ok(match e {
            Expr::Number(_) => Type::Number,
            Expr::Text(_) => Type::Text,
            Expr::Bool(_) => Type::Bool,
            Expr::List(items) => {
                let mut t: Option<Type> = None;
                for i in items {
                    let it = self.ty(i)?;
                    t = Some(t.map_or(it.clone(), |t| unify(&t, &it)));
                }
                Type::List(Box::new(t.unwrap_or(Type::Any)))
            }
            Expr::Name(n) => match self.vars.iter().rev().find(|(v, _)| v == n) {
                Some((_, t)) => t.clone(),
                None => return refuse(format!("{n} isn't defined here")),
            },
            Expr::Unary { op, expr } => {
                let t = self.ty(expr)?;
                match op {
                    Unary::Neg | Unary::Plus => {
                        want("-", &t, &Type::Number)?;
                        Type::Number
                    }
                    Unary::Not => {
                        want("not", &t, &Type::Bool)?;
                        Type::Bool
                    }
                }
            }
            Expr::If { cond, then, other } => {
                want("if", &self.ty(cond)?, &Type::Bool)?;
                let (a, b) = (self.ty(then)?, self.ty(other)?);
                unify(&a, &b)
            }
            Expr::Binary { op, left, right } => {
                let (a, b) = (self.ty(left)?, self.ty(right)?);
                use Binary as B;
                match op {
                    B::Add => match (&a, &b) {
                        (Type::Text, _) | (_, Type::Text) => Type::Text,
                        (Type::List(_), _) => a,
                        (Type::Any, _) | (_, Type::Any) => Type::Any,
                        _ => {
                            want("+", &a, &Type::Number)?;
                            want("+", &b, &Type::Number)?;
                            Type::Number
                        }
                    },
                    B::Sub | B::Mul | B::Div | B::Rem | B::Pow => {
                        let sym = match op {
                            B::Sub => "-",
                            B::Mul => "*",
                            B::Div => "/",
                            B::Rem => "%",
                            _ => "^",
                        };
                        want(sym, &a, &Type::Number)?;
                        want(sym, &b, &Type::Number)?;
                        Type::Number
                    }
                    B::And | B::Or => {
                        let sym = if *op == B::And { "and" } else { "or" };
                        want(sym, &a, &Type::Bool)?;
                        want(sym, &b, &Type::Bool)?;
                        Type::Bool
                    }
                    B::Lt | B::Le | B::Gt | B::Ge => {
                        if !a.fits(&b) {
                            return refuse(format!(
                                "can't compare a {} with a {}",
                                a.name(),
                                b.name()
                            ));
                        }
                        Type::Bool
                    }
                    B::Eq | B::Ne => Type::Bool,
                }
            }
            Expr::Call { name, args } => self.call(name, args)?,
        })
    }

    fn with_item(&mut self, list: &Type, body: &Expr) -> Result<Type, Error> {
        self.vars.push(("current".into(), list.item()));
        self.vars.push(("index".into(), Type::Number));
        let t = self.ty(body);
        self.vars.truncate(self.vars.len() - 2);
        t
    }

    #[allow(clippy::too_many_lines)]
    fn call(&mut self, name: &str, args: &[Expr]) -> Result<Type, Error> {
        let arity = |min: usize, max: usize| -> Result<(), Error> {
            if args.len() < min || args.len() > max {
                let w = if min == max {
                    min.to_string()
                } else {
                    format!("{min} to {max}")
                };
                return refuse(format!("{name}() takes {w} arguments, not {}", args.len()));
            }
            Ok(())
        };
        match name {
            "let" | "lets" => {
                if args.len() < 3
                    || args.len().is_multiple_of(2)
                    || (name == "let" && args.len() != 3)
                {
                    return refuse(format!("{name}() needs a name, a value and an expression"));
                }
                let before = self.vars.len();
                for pair in args[..args.len() - 1].as_chunks::<2>().0 {
                    let Expr::Name(v) = &pair[0] else {
                        self.vars.truncate(before);
                        return refuse(format!("{name}() names its variables with plain words"));
                    };
                    let t = self.ty(&pair[1]);
                    match t {
                        Ok(t) => self.vars.push((v.clone(), t)),
                        Err(e) => {
                            self.vars.truncate(before);
                            return Err(e);
                        }
                    }
                }
                let out = self.ty(&args[args.len() - 1]);
                self.vars.truncate(before);
                return out;
            }
            n if LAMBDAS.contains(&n) && args.len() == 2 => {
                let list = self.ty(&args[0])?;
                want_list(name, &list)?;
                let body = self.with_item(&list, &args[1])?;
                return Ok(match name {
                    "map" => Type::List(Box::new(body)),
                    "filter" | "sort" => list,
                    "find" => list.item(),
                    "findIndex" | "count" => Type::Number,
                    _ => Type::Bool,
                });
            }
            "map" | "filter" | "find" | "findIndex" | "some" | "every" => {
                return refuse(format!("{name}() needs a list and an expression"));
            }
            "if" => {
                arity(3, 3)?;
                want("if", &self.ty(&args[0])?, &Type::Bool)?;
                let (a, b) = (self.ty(&args[1])?, self.ty(&args[2])?);
                return Ok(unify(&a, &b));
            }
            "ifs" => {
                if args.len() < 2 {
                    return refuse("ifs() needs a condition and a value");
                }
                let mut out: Option<Type> = None;
                for (i, a) in args.iter().enumerate() {
                    let t = self.ty(a)?;
                    let is_cond = i % 2 == 0 && i + 1 < args.len();
                    if is_cond {
                        want("ifs", &t, &Type::Bool)?;
                    } else {
                        out = Some(out.map_or(t.clone(), |o| unify(&o, &t)));
                    }
                }
                return Ok(out.unwrap_or(Type::Any));
            }
            _ => {}
        }
        let a: Vec<Type> = args.iter().map(|x| self.ty(x)).collect::<Result<_, _>>()?;
        let n = Type::Number;
        let t = Type::Text;
        let d = Type::Date;
        Ok(match name {
            "prop" => {
                arity(1, 2)?;
                let (page, prop) = if args.len() == 2 {
                    (Some(&a[0]), &args[1])
                } else {
                    (None, &args[0])
                };
                let Expr::Text(p) = prop else {
                    return refuse("prop() needs a property's name, in quotes");
                };
                match page {
                    Some(pt) => {
                        want("prop", pt, &Type::Page)?;
                        Type::Any
                    }
                    None => match self.env.prop(p) {
                        Some(ty) => ty,
                        None => return refuse(format!("there's no property called “{p}”")),
                    },
                }
            }
            "empty" => {
                arity(0, 1)?;
                if a.is_empty() { Type::Any } else { Type::Bool }
            }
            "not" => {
                arity(1, 1)?;
                want(name, &a[0], &Type::Bool)?;
                Type::Bool
            }
            "and" | "or" => {
                for x in &a {
                    want(name, x, &Type::Bool)?;
                }
                Type::Bool
            }
            "equal" | "unequal" => {
                arity(2, 2)?;
                Type::Bool
            }
            "add" => {
                arity(2, 2)?;
                if a[0] == t || a[1] == t { t } else { n }
            }
            "subtract" | "multiply" | "divide" | "mod" | "pow" => {
                arity(2, 2)?;
                want(name, &a[0], &n)?;
                want(name, &a[1], &n)?;
                n
            }
            "abs" | "ceil" | "floor" | "sqrt" | "cbrt" | "exp" | "ln" | "log10" | "log2"
            | "sign" => {
                arity(1, 1)?;
                want(name, &a[0], &n)?;
                n
            }
            "round" => {
                arity(1, 2)?;
                for x in &a {
                    want(name, x, &n)?;
                }
                n
            }
            "pi" | "e" => {
                arity(0, 0)?;
                n
            }
            "max" | "min" | "sum" | "mean" | "average" | "median" => {
                for x in &a {
                    if !(x.fits(&n) || x.fits(&Type::List(Box::new(n.clone())))) {
                        return refuse(format!("{name}() needs numbers, not a {}", x.name()));
                    }
                }
                n
            }
            "toNumber" => {
                arity(1, 1)?;
                n
            }
            "format" => {
                arity(1, 1)?;
                t
            }
            "length" => {
                arity(1, 1)?;
                if !(a[0].fits(&t) || matches!(a[0], Type::List(_))) {
                    return refuse(format!(
                        "length() needs text or a list, not a {}",
                        a[0].name()
                    ));
                }
                n
            }
            "substring" => {
                arity(2, 3)?;
                want(name, &a[0], &t)?;
                for x in &a[1..] {
                    want(name, x, &n)?;
                }
                t
            }
            "contains" => {
                arity(2, 2)?;
                Type::Bool
            }
            "test" => {
                arity(2, 2)?;
                Type::Bool
            }
            "match" | "split" => {
                arity(1, 2)?;
                Type::List(Box::new(t))
            }
            "replace" | "replaceAll" => {
                arity(3, 3)?;
                t
            }
            "lower" | "upper" | "trim" => {
                arity(1, 1)?;
                want(name, &a[0], &t)?;
                t
            }
            "repeat" => {
                arity(2, 2)?;
                want(name, &a[1], &n)?;
                t
            }
            "padStart" | "padEnd" => {
                arity(2, 3)?;
                want(name, &a[1], &n)?;
                t
            }
            "link" => {
                arity(2, 2)?;
                t
            }
            "style" | "unstyle" => {
                if a.is_empty() {
                    return refuse(format!("{name}() needs text"));
                }
                t
            }
            "join" => {
                arity(1, 2)?;
                want_list(name, &a[0])?;
                t
            }
            "concat" => {
                for x in &a {
                    want_list(name, x)?;
                }
                a.first()
                    .cloned()
                    .unwrap_or(Type::List(Box::new(Type::Any)))
            }
            "at" => {
                arity(2, 2)?;
                want_list(name, &a[0])?;
                want(name, &a[1], &n)?;
                a[0].item()
            }
            "first" | "last" => {
                arity(1, 1)?;
                want_list(name, &a[0])?;
                a[0].item()
            }
            "slice" => {
                arity(2, 3)?;
                a[0].clone()
            }
            "splice" | "sort" | "reverse" | "unique" => {
                want_list(name, a.first().unwrap_or(&Type::Any))?;
                a.first().cloned().unwrap_or(Type::Any)
            }
            "includes" => {
                arity(2, 2)?;
                want_list(name, &a[0])?;
                Type::Bool
            }
            "flat" => {
                arity(1, 1)?;
                want_list(name, &a[0])?;
                match &a[0] {
                    Type::List(inner) => match &**inner {
                        Type::List(_) => (**inner).clone(),
                        other => Type::List(Box::new(other.clone())),
                    },
                    _ => Type::Any,
                }
            }
            "count" => {
                arity(1, 1)?;
                n
            }
            "now" | "today" => {
                arity(0, 0)?;
                d
            }
            "minute" | "hour" | "day" | "date" | "week" | "month" | "year" | "timestamp" => {
                arity(1, 1)?;
                want(name, &a[0], &d)?;
                n
            }
            "dateAdd" | "dateSubtract" => {
                arity(3, 3)?;
                want(name, &a[0], &d)?;
                want(name, &a[1], &n)?;
                want(name, &a[2], &t)?;
                d
            }
            "dateBetween" => {
                arity(3, 3)?;
                want(name, &a[0], &d)?;
                want(name, &a[1], &d)?;
                n
            }
            "dateRange" => {
                arity(2, 2)?;
                want(name, &a[0], &d)?;
                want(name, &a[1], &d)?;
                d
            }
            "dateStart" | "dateEnd" => {
                arity(1, 1)?;
                want(name, &a[0], &d)?;
                d
            }
            "fromTimestamp" => {
                arity(1, 1)?;
                want(name, &a[0], &n)?;
                d
            }
            "formatDate" => {
                arity(2, 3)?;
                want(name, &a[0], &d)?;
                t
            }
            "parseDate" => {
                arity(1, 1)?;
                want(name, &a[0], &t)?;
                d
            }
            "name" | "email" => {
                arity(1, 1)?;
                match &a[0] {
                    Type::List(_) => Type::List(Box::new(t)),
                    other => {
                        want(name, other, &Type::Person)?;
                        t
                    }
                }
            }
            "id" => {
                arity(0, 1)?;
                t
            }
            _ => return refuse(format!("Notion has no function called {name}()")),
        })
    }
}
