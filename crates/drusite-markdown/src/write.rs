// SPDX-License-Identifier: AGPL-3.0-or-later

//! Page → Markdown.

use drusite_model::{Block, BlockKind, Marks, Span};

use crate::front::Frontmatter;
use crate::ids::encode_block_id;
use crate::{Plain, Resolver};

/// Writes a page as a Markdown file: frontmatter (when there is any), then its blocks.
pub fn write(front: &Frontmatter, blocks: &[Block], r: &dyn Resolver) -> String {
    let mut out = String::new();
    if !front.is_empty() {
        out.push_str("---\n");
        out.push_str(&front.to_yaml());
        out.push_str("---\n");
    }
    let body = write_blocks(blocks, r);
    match body.strip_prefix("---\n") {
        // A file opening with `---` would read as frontmatter; `***` is a divider too.
        Some(rest) if front.is_empty() => {
            out.push_str("***\n");
            out.push_str(rest);
        }
        _ => out.push_str(&body),
    }
    out
}

/// A container a nested block is written inside: what starts each of its lines.
struct Open {
    indent: u8,
    prefix: &'static str,
}

/// The callout types Obsidian knows, and the icon each stands for.
pub(crate) const CALLOUTS: &[(&str, &str)] = &[
    ("note", "📝"),
    ("abstract", "📄"),
    ("info", "ℹ️"),
    ("todo", "☑️"),
    ("tip", "💡"),
    ("success", "✅"),
    ("question", "❓"),
    ("warning", "⚠️"),
    ("failure", "❌"),
    ("danger", "⚡"),
    ("bug", "🐛"),
    ("example", "📋"),
    ("quote", "💬"),
];

/// The fence language a filter block is written as.
pub(crate) const QUERY_LANG: &str = "drusite-query";

pub(crate) fn write_blocks(blocks: &[Block], r: &dyn Resolver) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    for b in blocks {
        while stack.last().is_some_and(|o| o.indent >= b.indent) {
            stack.pop();
        }
        let mut flat_indent = None;
        if b.indent > 0 && stack.last().map(|o| o.indent) != Some(b.indent - 1) {
            // Its parent isn't something Markdown nests under.
            stack.clear();
            flat_indent = Some(b.indent);
        }
        let prefix: String = stack.iter().map(|o| o.prefix).collect();
        if !lines.is_empty() {
            lines.push(prefix.trim_end().to_owned());
        }
        if let Some(n) = flat_indent {
            lines.push(format!("<!-- indent {n} -->"));
        }
        let (own, child_prefix) = block_lines(b, r);
        for line in own {
            if line.is_empty() {
                lines.push(prefix.trim_end().to_owned());
            } else {
                lines.push(format!("{prefix}{line}"));
            }
        }
        if let Some(p) = child_prefix {
            stack.push(Open {
                indent: b.indent,
                prefix: p,
            });
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// A block's own lines, before any container prefix, and the prefix its children's lines get
/// when Markdown can nest under it.
fn block_lines(b: &Block, r: &dyn Resolver) -> (Vec<String>, Option<&'static str>) {
    let id = encode_block_id(b.id);
    let text = block_text(&b.spans, r);
    // An empty block's ID stands where its text would: `- ^id`, never `-  ^id` (the second
    // space would move the item's content column and un-nest its children).
    let with_id = |lead: &str| {
        if text.is_empty() {
            format!("{lead}^{id}")
        } else {
            format!("{lead}{text} ^{id}")
        }
    };
    let id_line = |mut v: Vec<String>| {
        v.push(String::new());
        v.push(format!("^{id}"));
        v
    };
    match &b.kind {
        BlockKind::Paragraph if text.is_empty() => (vec![format!("<!-- ^{id} -->")], None),
        BlockKind::Paragraph => (vec![with_id("")], None),
        BlockKind::Heading { level } => {
            let hashes = "#".repeat(usize::from(level.number()));
            (vec![with_id(&format!("{hashes} "))], None)
        }
        BlockKind::Bulleted => (vec![with_id("- ")], Some("  ")),
        BlockKind::Numbered => (vec![with_id("1. ")], Some("   ")),
        BlockKind::Todo { checked } => (
            vec![with_id(if *checked { "- [x] " } else { "- [ ] " })],
            Some("  "),
        ),
        BlockKind::Toggle => (vec![with_id("> [!toggle]- ")], Some("> ")),
        BlockKind::Quote => (vec![with_id("> ")], Some("> ")),
        BlockKind::Callout { icon } => {
            let marker = match CALLOUTS.iter().find(|(_, i)| i == icon) {
                Some((kind, _)) => format!("> [!{kind}] "),
                None if icon.is_empty() => "> [!note|] ".to_owned(),
                None => format!("> [!note|{}] ", icon.replace([']', '\n'], "")),
            };
            (vec![with_id(&marker)], Some("> "))
        }
        BlockKind::Code { language } => {
            let code: String = b.spans.iter().map(|s| s.text.as_str()).collect();
            (id_line(fenced(language, &code)), None)
        }
        BlockKind::Query { query } => (id_line(fenced(QUERY_LANG, query)), None),
        BlockKind::Divider => (id_line(vec!["---".to_owned()]), None),
        BlockKind::Attachment { blob, name, .. } => {
            let caption: String = b
                .spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
                .replace(['\n', '\r'], " ");
            let alt = escape_text(caption.trim(), false);
            let dest = r.attachment_out(blob, name);
            (
                vec![format!("![{alt}]({}) ^{id}", destination(&dest))],
                None,
            )
        }
    }
}

/// A fenced code block, with a fence the code can't close.
fn fenced(language: &str, code: &str) -> Vec<String> {
    let tick = if language.contains('`') { '~' } else { '`' };
    let mut fence: String = std::iter::repeat_n(tick, 3).collect();
    while code.contains(&fence) {
        fence.push(tick);
    }
    let language = language.replace(['\n', '\r'], " ");
    let mut v = vec![format!("{fence}{}", language.trim())];
    v.extend(code.split('\n').map(str::to_owned));
    v.push(fence);
    v
}

/// A link destination in angle brackets, which may hold spaces and parentheses.
fn destination(dest: &str) -> String {
    format!(
        "<{}>",
        dest.replace('<', "%3C")
            .replace('>', "%3E")
            .replace('\n', "%0A")
    )
}

const ALWAYS_ESCAPE: &[char] = &[
    '\\', '`', '*', '_', '[', ']', '<', '>', '|', '~', '#', '!', '&', '^', '$',
];

fn escape_text(s: &str, at_block_start: bool) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch == '\n' {
            out.push_str("<br>");
            continue;
        }
        if ch == '\t' {
            out.push_str("&#9;");
            continue;
        }
        if ch == '\r' {
            out.push_str("&#13;");
            continue;
        }
        let start = at_block_start && i == 0;
        if ALWAYS_ESCAPE.contains(&ch) || (start && matches!(ch, '-' | '+' | '=' | ':')) {
            out.push('\\');
        }
        out.push(ch);
    }
    // "1." or "1)" at the start of a block would start an ordered list.
    if at_block_start {
        let digits = out.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && matches!(out[digits..].chars().next(), Some('.' | ')')) {
            out.insert(digits, '\\');
        }
    }
    out
}

fn code_span(s: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in s.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    let pad = s.starts_with('`')
        || s.ends_with('`')
        || (s.starts_with(' ') && s.ends_with(' ') && !s.trim().is_empty());
    // Line breaks can't be in a code span; CommonMark would turn them into spaces.
    let s = s.replace('\n', " ");
    if pad {
        format!("{fence} {s} {fence}")
    } else {
        format!("{fence}{s}{fence}")
    }
}

struct Delims {
    bold: (&'static str, &'static str),
    italic: (&'static str, &'static str),
    strike: (&'static str, &'static str),
}

const MARKDOWN: Delims = Delims {
    bold: ("**", "**"),
    italic: ("*", "*"),
    strike: ("~~", "~~"),
};
const HTML: Delims = Delims {
    bold: ("<b>", "</b>"),
    italic: ("<i>", "</i>"),
    strike: ("<s>", "</s>"),
};

/// Wraps already-escaped text in the delimiters for `marks` (not links).
fn wrap(mut t: String, m: &Marks, d: &Delims) -> String {
    if m.italic {
        t = format!("{}{t}{}", d.italic.0, d.italic.1);
    }
    if m.bold {
        t = format!("{}{t}{}", d.bold.0, d.bold.1);
    }
    if m.strikethrough {
        t = format!("{}{t}{}", d.strike.0, d.strike.1);
    }
    if m.underline {
        t = format!("<u>{t}</u>");
    }
    t
}

/// Inline Markdown for a run of spans. Whitespace at the edges of emphasis moves outside the
/// delimiters: CommonMark won't open `** x` or close `x **`.
fn inline_md(spans: &[Span], r: &dyn Resolver, d: &Delims) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < spans.len() {
        // Consecutive spans with the same link are one link.
        let link = spans[i].marks.link.clone();
        let mut j = i;
        while j < spans.len() && spans[j].marks.link == link {
            j += 1;
        }
        let mut inner = String::new();
        for s in &spans[i..j] {
            if s.text.is_empty() {
                continue;
            }
            let first = out.is_empty() && inner.is_empty() && link.is_none();
            if s.marks.code {
                inner.push_str(&wrap(code_span(&s.text), &s.marks, d));
                continue;
            }
            let lead: String = s.text.chars().take_while(|c| c.is_whitespace()).collect();
            let rest = &s.text[lead.len()..];
            let core = rest.trim_end();
            let trail = &rest[core.len()..];
            let mut t = escape_text(core, first && lead.is_empty());
            if !core.is_empty() {
                t = wrap(t, &s.marks, d);
            }
            inner.push_str(&escape_text(&lead, first));
            inner.push_str(&t);
            inner.push_str(&escape_text(trail, false));
        }
        match link {
            Some(href) => {
                let dest = r.link_out(&href);
                out.push_str(&format!("[{inner}]({})", destination(&dest)));
            }
            None => out.push_str(&inner),
        }
        i = j;
    }
    out
}

/// A block's text as inline Markdown, checked by reading it back; emphasis falls back to HTML
/// tags for a block whose `**`/`*` wouldn't read back the same.
pub(crate) fn block_text(spans: &[Span], r: &dyn Resolver) -> String {
    // Runs with the same marks are one run: `*a**b*` wouldn't read back.
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans.iter().filter(|s| !s.text.is_empty()) {
        match merged.last_mut() {
            Some(last) if last.marks == s.marks => last.text.push_str(&s.text),
            _ => merged.push(s.clone()),
        }
    }
    let spans = merged.as_slice();
    let md = edge_spaces(&inline_md(spans, r, &MARKDOWN));
    // What reading it back should give: the same, with links as the resolver writes them.
    let want = crate::read::normalise(
        &spans
            .iter()
            .map(|s| {
                let mut s = s.clone();
                s.marks.link = s.marks.link.map(|h| r.link_out(&h));
                s
            })
            .collect::<Vec<_>>(),
    );
    let back = crate::read::read_body(&md, &Plain);
    let ok = match back.as_slice() {
        [] => want.is_empty(),
        [b] if b.kind == BlockKind::Paragraph && b.id.is_none() && b.indent == 0 => {
            crate::read::normalise(&b.spans) == want
        }
        _ => false,
    };
    if ok {
        md
    } else {
        edge_spaces(&inline_md(spans, r, &HTML))
    }
}

/// Markdown strips spaces at a block's start and end; those are written as `&#32;`.
fn edge_spaces(md: &str) -> String {
    let core = md.trim_matches(' ');
    if core.len() == md.len() {
        return md.to_owned();
    }
    let lead = md.len() - md.trim_start_matches(' ').len();
    let trail = md.len() - md.trim_end_matches(' ').len();
    if core.is_empty() {
        return "&#32;".repeat(md.len());
    }
    format!("{}{core}{}", "&#32;".repeat(lead), "&#32;".repeat(trail))
}
