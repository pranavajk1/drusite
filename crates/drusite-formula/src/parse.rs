// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a formula: tokens, then an expression tree.
//!
//! Precedence, loosest first, as Notion has it: `? :`, `or`/`||`, `and`/`&&`, `==` `!=`,
//! `<` `<=` `>` `>=`, `+` `-`, `*` `/` `%`, `^` (right to left, and looser than a sign, so
//! `-2 ^ 2` is 4), the prefix `-` `+` `not` `!`, then calls and methods (`x.f(y)` is
//! `f(x, y)`).

use crate::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Text(String),
    Bool(bool),
    List(Vec<Expr>),
    /// A name: a `let` variable, `current`, `index`.
    Name(String),
    Call {
        name: String,
        args: Vec<Expr>,
    },
    Unary {
        op: Unary,
        expr: Box<Expr>,
    },
    Binary {
        op: Binary,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        other: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unary {
    Neg,
    Plus,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    End,
}

const OPS: [&str; 25] = [
    "==", "!=", "<=", ">=", "&&", "||", "+", "-", "*", "/", "%", "^", "<", ">", "!", "?", ":", "(",
    ")", "[", "]", ",", ".", "=", "≠",
];

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, Error> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (at, c) = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Comments: /* … */ and // … to the end of the line.
        if c == '/' && chars.get(i + 1).map(|p| p.1) == Some('*') {
            let rest = &src[at + 2..];
            let end = rest
                .find("*/")
                .ok_or_else(|| Error::syntax(at, "a comment that never ends"))?;
            let stop = at + 2 + end + 2;
            while i < chars.len() && chars[i].0 < stop {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1).map(|p| p.1) == Some('/') {
            while i < chars.len() && chars[i].1 != '\n' {
                i += 1;
            }
            continue;
        }
        if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(|p| p.1.is_ascii_digit()))
        {
            let start = i;
            while i < chars.len() && (chars[i].1.is_ascii_digit() || chars[i].1 == '.') {
                i += 1;
            }
            if i < chars.len() && (chars[i].1 == 'e' || chars[i].1 == 'E') {
                let save = i;
                i += 1;
                if i < chars.len() && (chars[i].1 == '+' || chars[i].1 == '-') {
                    i += 1;
                }
                if i < chars.len() && chars[i].1.is_ascii_digit() {
                    while i < chars.len() && chars[i].1.is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            let end = chars.get(i).map_or(src.len(), |p| p.0);
            let text = &src[chars[start].0..end];
            let n = text
                .parse()
                .map_err(|_| Error::syntax(at, format!("not a number: {text}")))?;
            out.push((Tok::Num(n), at));
            continue;
        }
        if c == '"' || c == '\u{201c}' {
            let close = if c == '"' { '"' } else { '\u{201d}' };
            let mut s = String::new();
            i += 1;
            loop {
                let Some(&(_, d)) = chars.get(i) else {
                    return Err(Error::syntax(at, "a text that never ends"));
                };
                i += 1;
                if d == close || (close == '\u{201d}' && d == '"') {
                    break;
                }
                if d == '\\' {
                    let Some(&(_, e)) = chars.get(i) else {
                        return Err(Error::syntax(at, "a text that never ends"));
                    };
                    i += 1;
                    s.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                } else {
                    s.push(d);
                }
            }
            out.push((Tok::Str(s), at));
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].1.is_alphanumeric() || chars[i].1 == '_') {
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |p| p.0);
            out.push((Tok::Ident(src[chars[start].0..end].to_owned()), at));
            continue;
        }
        let rest = &src[at..];
        let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) else {
            return Err(Error::syntax(at, format!("unexpected {c:?}")));
        };
        i += op.chars().count();
        out.push((Tok::Op(op), at));
    }
    out.push((Tok::End, src.len()));
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    at: usize,
}

/// Reads a formula.
pub fn parse(src: &str) -> Result<Expr, Error> {
    let mut p = Parser {
        toks: lex(src)?,
        at: 0,
    };
    if p.peek() == &Tok::End {
        return Err(Error::syntax(0, "the formula is empty"));
    }
    let e = p.ternary()?;
    match p.peek() {
        Tok::End => Ok(e),
        _ => Err(Error::syntax(p.pos(), "unexpected text after the formula")),
    }
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.at].0
    }

    fn pos(&self) -> usize {
        self.toks[self.at].1
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.at].0.clone();
        if self.at + 1 < self.toks.len() {
            self.at += 1;
        }
        t
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Tok::Op(o) if *o == op) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Tok::Ident(w) if w == word) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, op: &str) -> Result<(), Error> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(Error::syntax(self.pos(), format!("expected {op}")))
        }
    }

    fn ternary(&mut self) -> Result<Expr, Error> {
        let cond = self.or()?;
        if self.eat_op("?") {
            let then = self.ternary()?;
            self.expect(":")?;
            let other = self.ternary()?;
            return Ok(Expr::If {
                cond: Box::new(cond),
                then: Box::new(then),
                other: Box::new(other),
            });
        }
        Ok(cond)
    }

    fn or(&mut self) -> Result<Expr, Error> {
        let mut left = self.and()?;
        while self.eat_op("||") || self.eat_word("or") {
            let right = self.and()?;
            left = bin(Binary::Or, left, right);
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, Error> {
        let mut left = self.equality()?;
        while self.eat_op("&&") || self.eat_word("and") {
            let right = self.equality()?;
            left = bin(Binary::And, left, right);
        }
        Ok(left)
    }

    fn equality(&mut self) -> Result<Expr, Error> {
        let mut left = self.comparison()?;
        loop {
            let op = if self.eat_op("==") || self.eat_op("=") {
                Binary::Eq
            } else if self.eat_op("!=") || self.eat_op("≠") {
                Binary::Ne
            } else {
                return Ok(left);
            };
            let right = self.comparison()?;
            left = bin(op, left, right);
        }
    }

    fn comparison(&mut self) -> Result<Expr, Error> {
        let mut left = self.additive()?;
        loop {
            let op = if self.eat_op("<=") {
                Binary::Le
            } else if self.eat_op(">=") {
                Binary::Ge
            } else if self.eat_op("<") {
                Binary::Lt
            } else if self.eat_op(">") {
                Binary::Gt
            } else {
                return Ok(left);
            };
            let right = self.additive()?;
            left = bin(op, left, right);
        }
    }

    fn additive(&mut self) -> Result<Expr, Error> {
        let mut left = self.multiplicative()?;
        loop {
            let op = if self.eat_op("+") {
                Binary::Add
            } else if self.eat_op("-") {
                Binary::Sub
            } else {
                return Ok(left);
            };
            let right = self.multiplicative()?;
            left = bin(op, left, right);
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, Error> {
        let mut left = self.power()?;
        loop {
            let op = if self.eat_op("*") {
                Binary::Mul
            } else if self.eat_op("/") {
                Binary::Div
            } else if self.eat_op("%") {
                Binary::Rem
            } else {
                return Ok(left);
            };
            let right = self.power()?;
            left = bin(op, left, right);
        }
    }

    fn power(&mut self) -> Result<Expr, Error> {
        let base = self.unary()?;
        if self.eat_op("^") {
            let exp = self.power()?;
            return Ok(bin(Binary::Pow, base, exp));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<Expr, Error> {
        let op = if self.eat_op("-") {
            Unary::Neg
        } else if self.eat_op("+") {
            Unary::Plus
        } else if self.eat_op("!") || self.eat_word("not") {
            Unary::Not
        } else {
            return self.postfix();
        };
        let expr = self.unary()?;
        Ok(Expr::Unary {
            op,
            expr: Box::new(expr),
        })
    }

    fn postfix(&mut self) -> Result<Expr, Error> {
        let mut e = self.primary()?;
        while self.eat_op(".") {
            let at = self.pos();
            let Tok::Ident(name) = self.bump() else {
                return Err(Error::syntax(at, "expected a function name after ."));
            };
            let mut args = vec![e];
            if self.eat_op("(") {
                args.extend(self.args(")")?);
            }
            e = Expr::Call { name, args };
        }
        Ok(e)
    }

    fn args(&mut self, close: &str) -> Result<Vec<Expr>, Error> {
        let mut args = Vec::new();
        if self.eat_op(close) {
            return Ok(args);
        }
        loop {
            args.push(self.ternary()?);
            if self.eat_op(close) {
                return Ok(args);
            }
            self.expect(",")?;
        }
    }

    fn primary(&mut self) -> Result<Expr, Error> {
        let at = self.pos();
        match self.bump() {
            Tok::Num(n) => Ok(Expr::Number(n)),
            Tok::Str(s) => Ok(Expr::Text(s)),
            Tok::Op("(") => {
                let e = self.ternary()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Op("[") => Ok(Expr::List(self.args("]")?)),
            Tok::Ident(w) => match w.as_str() {
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                _ if self.eat_op("(") => Ok(Expr::Call {
                    name: w,
                    args: self.args(")")?,
                }),
                _ => Ok(Expr::Name(w)),
            },
            Tok::End => Err(Error::syntax(at, "the formula ends too soon")),
            Tok::Op(op) => Err(Error::syntax(at, format!("unexpected {op}"))),
        }
    }
}

fn bin(op: Binary, left: Expr, right: Expr) -> Expr {
    Expr::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence() {
        assert_eq!(
            parse("1 + 2 * 3").unwrap(),
            bin(
                Binary::Add,
                Expr::Number(1.0),
                bin(Binary::Mul, Expr::Number(2.0), Expr::Number(3.0))
            )
        );
        // A sign binds tighter than ^.
        assert!(matches!(
            parse("-2 ^ 2").unwrap(),
            Expr::Binary {
                op: Binary::Pow,
                ..
            }
        ));
        assert!(matches!(
            parse("a.f(b)").unwrap(),
            Expr::Call { name, args } if name == "f" && args.len() == 2
        ));
        assert!(parse("1 +").is_err());
        assert!(parse("").is_err());
        assert!(parse("1 /* note */ + 2").is_ok());
    }
}
