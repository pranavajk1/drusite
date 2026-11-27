// SPDX-License-Identifier: AGPL-3.0-or-later

//! YAML frontmatter: a page's properties, and the facts about it a file needs to carry.

use drusite_model::{Day, PageId, PageKind, Property, PropertyValue};
use yaml_rust2::{Yaml, YamlLoader};

/// What a page's frontmatter says.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Frontmatter {
    /// `drusite-id`: which page the file is.
    pub id: Option<PageId>,
    /// `drusite-kind`, when it isn't an ordinary page.
    pub kind: PageKind,
    /// `drusite-title`: written only when the file's name can't carry the title.
    pub title: Option<String>,
    /// Everything else, by name. Lists (Obsidian's `tags`, `aliases`) read as text joined with
    /// `, `, since a property has no list type yet.
    pub properties: Vec<Property>,
    /// YAML written after the rest, for reading only (a database's `drusite-schema`); reading a
    /// file leaves it empty.
    pub extra: String,
}

const ID: &str = "drusite-id";
const KIND: &str = "drusite-kind";
const TITLE: &str = "drusite-title";

/// Splits a file into its frontmatter's YAML (between `---` lines at the very start) and the
/// rest. No frontmatter: none, and the whole file.
pub fn split_frontmatter(file: &str) -> (Option<&str>, &str) {
    let file = file.strip_prefix('\u{feff}').unwrap_or(file);
    let Some(rest) = file
        .strip_prefix("---\n")
        .or_else(|| file.strip_prefix("---\r\n"))
    else {
        return (None, file);
    };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" || trimmed == "..." {
            return (Some(&rest[..at]), &rest[at + line.len()..]);
        }
        at += line.len();
    }
    // Never closed: not frontmatter.
    (None, file)
}

impl Frontmatter {
    /// Reads frontmatter YAML. Anything that isn't a mapping of names to values reads as
    /// nothing: a broken header never loses the page.
    pub fn parse(yaml: &str) -> Self {
        let mut out = Self::default();
        let Ok(docs) = YamlLoader::load_from_str(yaml) else {
            return out;
        };
        let Some(Yaml::Hash(map)) = docs.into_iter().next() else {
            return out;
        };
        for (key, value) in map {
            let Some(key) = scalar_text(&key) else {
                continue;
            };
            match key.as_str() {
                ID => out.id = scalar_text(&value).and_then(|s| s.parse().ok()),
                KIND => {
                    out.kind =
                        scalar_text(&value).map_or(PageKind::Page, |k| PageKind::from_name(&k));
                }
                TITLE => out.title = scalar_text(&value),
                // Written for reading only (see `extra`).
                k if k.starts_with("drusite-") => {}
                _ => {
                    if let Some(value) = property_value(&value) {
                        out.properties.push(Property { name: key, value });
                    }
                }
            }
        }
        out
    }

    /// Whether there is anything to write.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The YAML, without the `---` lines.
    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        if let Some(id) = self.id {
            out.push_str(&format!("{ID}: {id}\n"));
        }
        if self.kind != PageKind::Page {
            out.push_str(&format!("{KIND}: {}\n", self.kind.name()));
        }
        if let Some(title) = &self.title {
            out.push_str(&format!("{TITLE}: {}\n", quote(title)));
        }
        let mut props: Vec<&Property> = self.properties.iter().collect();
        props.sort_by(|a, b| a.name.cmp(&b.name));
        for p in props {
            let value = match &p.value {
                PropertyValue::Text(s) => quote(s),
                PropertyValue::Number(n) => number(*n),
                PropertyValue::Checkbox(b) => b.to_string(),
                PropertyValue::Date(d) => d.to_string(),
            };
            out.push_str(&format!("{}: {value}\n", quote_key(&p.name)));
        }
        out.push_str(&self.extra);
        out
    }
}

fn scalar_text(y: &Yaml) -> Option<String> {
    Some(match y {
        Yaml::String(s) | Yaml::Real(s) => s.clone(),
        Yaml::Integer(i) => i.to_string(),
        Yaml::Boolean(b) => b.to_string(),
        _ => return None,
    })
}

fn property_value(y: &Yaml) -> Option<PropertyValue> {
    Some(match y {
        Yaml::String(s) => match s.parse::<Day>() {
            Ok(d) if s.len() == 10 => PropertyValue::Date(d),
            _ => PropertyValue::Text(s.clone()),
        },
        Yaml::Integer(i) => PropertyValue::Number(*i as f64),
        Yaml::Real(r) => PropertyValue::Number(r.parse().ok().filter(|n: &f64| n.is_finite())?),
        Yaml::Boolean(b) => PropertyValue::Checkbox(*b),
        Yaml::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(scalar_text).collect();
            PropertyValue::Text(parts.join(", "))
        }
        _ => return None,
    })
}

fn number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// A string as YAML: plain when that reads back as the same string, double-quoted otherwise.
fn quote(s: &str) -> String {
    let plain = !s.is_empty()
        && !s.starts_with(|c: char| c.is_whitespace() || "-?:,[]{}#&*!|>'\"%@`~=".contains(c))
        && !s.ends_with(char::is_whitespace)
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.ends_with(':')
        && !s.chars().any(char::is_control)
        && matches!(
            YamlLoader::load_from_str(&format!("k: {s}")).ok().as_deref(),
            Some([Yaml::Hash(m)]) if m.values().next() == Some(&Yaml::String(s.to_owned()))
        );
    if plain {
        return s.to_owned();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn quote_key(s: &str) -> String {
    let q = quote(s);
    // A plain key also mustn't contain what ends a key.
    if q.starts_with('"') || s.contains(':') || s.contains('#') {
        if q.starts_with('"') {
            q
        } else {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
    } else {
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_only_at_the_start() {
        assert_eq!(
            split_frontmatter("---\na: 1\n---\nbody"),
            (Some("a: 1\n"), "body")
        );
        assert_eq!(
            split_frontmatter("body\n---\na: 1\n---\n"),
            (None, "body\n---\na: 1\n---\n")
        );
        assert_eq!(
            split_frontmatter("---\nnever closed"),
            (None, "---\nnever closed")
        );
    }

    #[test]
    fn values_keep_their_types_and_awkward_text_survives() {
        let id = PageId::new();
        let f = Frontmatter {
            id: Some(id),
            kind: PageKind::Journal,
            title: Some("a/b: c".into()),
            properties: vec![
                Property {
                    name: "mood".into(),
                    value: PropertyValue::Number(4.0),
                },
                Property {
                    name: "ratio".into(),
                    value: PropertyValue::Number(0.25),
                },
                Property {
                    name: "done".into(),
                    value: PropertyValue::Checkbox(true),
                },
                Property {
                    name: "date".into(),
                    value: PropertyValue::Date("2026-11-26".parse().unwrap()),
                },
                Property {
                    name: "looks like a date".into(),
                    value: PropertyValue::Text("2026-11-26x".into()),
                },
                Property {
                    name: "yes".into(),
                    value: PropertyValue::Text("true".into()),
                },
                Property {
                    name: "n".into(),
                    value: PropertyValue::Text("12".into()),
                },
                Property {
                    name: "odd: key #1".into(),
                    value: PropertyValue::Text("- \"quoted\" \\ #x\nline".into()),
                },
                Property {
                    name: "empty".into(),
                    value: PropertyValue::Text(String::new()),
                },
            ],
            extra: String::new(),
        };
        let back = Frontmatter::parse(&f.to_yaml());
        let mut want = f.clone();
        want.properties.sort_by(|a, b| a.name.cmp(&b.name));
        let mut got = back;
        got.properties.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(got, want, "{}", f.to_yaml());
    }

    #[test]
    fn obsidian_frontmatter_reads() {
        let f = Frontmatter::parse(
            "tags: [journal, daily]\nmood: 🙂\naliases:\n  - Today\nrating: 3\n",
        );
        let get = |n: &str| {
            f.properties
                .iter()
                .find(|p| p.name == n)
                .map(|p| p.value.clone())
        };
        assert_eq!(
            get("tags"),
            Some(PropertyValue::Text("journal, daily".into()))
        );
        assert_eq!(get("mood"), Some(PropertyValue::Text("🙂".into())));
        assert_eq!(get("aliases"), Some(PropertyValue::Text("Today".into())));
        assert_eq!(get("rating"), Some(PropertyValue::Number(3.0)));
        assert_eq!(Frontmatter::parse(": : not yaml ["), Frontmatter::default());
    }
}
