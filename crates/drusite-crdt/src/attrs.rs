// SPDX-License-Identifier: AGPL-3.0-or-later

//! Block attributes in a Loro map, and inline marks in Loro text styles.

use drusite_model::{BlockKind, HeadingLevel, MAX_INDENT, Mark, Marks};
use loro::{
    ExpandType, LoroDoc, LoroMap, LoroResult, LoroValue, StyleConfig, StyleConfigMap,
    ValueOrContainer,
};

/// The style key on a marker character that holds its block's ID.
pub(crate) const BLOCK_STYLE: &str = "block";

/// Every peer must configure the same styles before touching a page's text, or marks merge
/// differently on different devices.
pub(crate) fn configure_styles(doc: &LoroDoc) {
    let mut styles = StyleConfigMap::new();
    for (key, expand) in [
        // Typing at the end of bold text continues it, as in every editor.
        ("bold", ExpandType::After),
        ("italic", ExpandType::After),
        ("underline", ExpandType::After),
        ("strikethrough", ExpandType::After),
        ("code", ExpandType::None),
        ("link", ExpandType::None),
        // The block ID belongs to the marker alone, never to text typed next to it.
        (BLOCK_STYLE, ExpandType::None),
    ] {
        styles.insert(key.into(), StyleConfig { expand });
    }
    doc.config_text_style(styles);
}

pub(crate) fn mark_value(mark: &Mark) -> LoroValue {
    match mark {
        Mark::Link { href } => href.as_str().into(),
        _ => true.into(),
    }
}

pub(crate) fn marks_from<S: std::hash::BuildHasher>(
    attributes: Option<&std::collections::HashMap<String, LoroValue, S>>,
) -> Marks {
    let mut marks = Marks::default();
    let Some(attributes) = attributes else {
        return marks;
    };
    let on = |key: &str| matches!(attributes.get(key), Some(LoroValue::Bool(true)));
    marks.bold = on("bold");
    marks.italic = on("italic");
    marks.underline = on("underline");
    marks.strikethrough = on("strikethrough");
    marks.code = on("code");
    if let Some(LoroValue::String(href)) = attributes.get("link") {
        marks.link = Some(href.to_string());
    }
    marks
}

/// Writes a block's kind. Attributes another kind used are left alone: they cost nothing, and if
/// a concurrent edit turns the block back, they are still there.
pub(crate) fn write_kind(map: &LoroMap, kind: &BlockKind) -> LoroResult<()> {
    map.insert("type", kind.name())?;
    match kind {
        BlockKind::Heading { level } => map.insert("level", i64::from(level.number())),
        BlockKind::Todo { checked } => map.insert("checked", *checked),
        BlockKind::Code { language } => map.insert("language", language.as_str()),
        BlockKind::Callout { icon } => map.insert("icon", icon.as_str()),
        BlockKind::Query { query } => map.insert("query", query.as_str()),
        BlockKind::Paragraph
        | BlockKind::Bulleted
        | BlockKind::Numbered
        | BlockKind::Quote
        | BlockKind::Divider
        | BlockKind::Toggle => Ok(()),
    }
}

/// Reads a block's kind. A missing or unknown type reads as a paragraph, so a block written by a
/// newer build still shows its text.
pub(crate) fn read_kind(map: Option<&LoroMap>) -> BlockKind {
    let Some(map) = map else {
        return BlockKind::Paragraph;
    };
    match get_str(map, "type").as_deref() {
        Some("heading") => BlockKind::Heading {
            level: HeadingLevel::clamped(get_int(map, "level")),
        },
        Some("bulleted") => BlockKind::Bulleted,
        Some("numbered") => BlockKind::Numbered,
        Some("todo") => BlockKind::Todo {
            checked: get_bool(map, "checked"),
        },
        Some("quote") => BlockKind::Quote,
        Some("code") => BlockKind::Code {
            language: get_str(map, "language").unwrap_or_default(),
        },
        Some("divider") => BlockKind::Divider,
        Some("callout") => BlockKind::Callout {
            icon: get_str(map, "icon").unwrap_or_default(),
        },
        Some("toggle") => BlockKind::Toggle,
        Some("query") => BlockKind::Query {
            query: get_str(map, "query").unwrap_or_default(),
        },
        _ => BlockKind::Paragraph,
    }
}

pub(crate) fn read_indent(map: Option<&LoroMap>) -> u8 {
    map.map_or(0, |m| {
        u8::try_from(get_int(m, "indent").clamp(0, i64::from(MAX_INDENT))).unwrap_or(0)
    })
}

pub(crate) fn get_str(map: &LoroMap, key: &str) -> Option<String> {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

pub(crate) fn get_int(map: &LoroMap, key: &str) -> i64 {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::I64(i))) => i,
        _ => 0,
    }
}

pub(crate) fn get_bool(map: &LoroMap, key: &str) -> bool {
    matches!(
        map.get(key),
        Some(ValueOrContainer::Value(LoroValue::Bool(true)))
    )
}
