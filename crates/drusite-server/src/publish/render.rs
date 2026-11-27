// SPDX-License-Identifier: AGPL-3.0-or-later

//! Published pages as HTML: every block type, page properties and journal entries, readable
//! without JavaScript, styled by one stylesheet (`assets/publish.css`) in light and dark.
//!
//! Everything from the notes is escaped here, in one place. No ID from the notes reaches the
//! HTML: links to other pages become their public addresses when they're published along with
//! this one and plain text when they aren't, and files are named by their public addresses.
//! Only `http`, `https` and `mailto` links leave the page.

use std::fmt::Write as _;

use drusite_model::journal::{DATE, MOOD};
use drusite_model::{
    Block, BlockKind, HeadingLevel, Marks, Mood, PageId, PageKind, Property, PropertyValue, Span,
};

use super::highlight::highlight;

/// Where the renderer gets the public addresses of what a page links to.
pub trait Links {
    /// A published page's address, if it's part of what's shared with this page.
    fn page(&self, page: PageId) -> Option<String>;
    /// The address of one of this page's files (by its blob), if it may be served.
    fn file(&self, blob: &str) -> Option<String>;
}

/// The prefix of links to other pages in the notes.
pub const PAGE_LINK: &str = "drusite:page/";

/// One step of the way to the page, or a page inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nav {
    pub title: String,
    pub href: String,
}

/// What a published page shows.
#[derive(Debug)]
pub struct PageDoc<'a> {
    pub title: &'a str,
    pub kind: PageKind,
    pub properties: &'a [Property],
    pub blocks: &'a [Block],
    /// The published pages above this one, outermost first (empty for the top page).
    pub trail: Vec<Nav>,
    /// The published pages directly inside this one.
    pub subpages: Vec<Nav>,
    /// The page's full public address.
    pub url: &'a str,
    /// The origin, for Open Graph images.
    pub origin: &'a str,
    pub indexable: bool,
    /// The stylesheet's address.
    pub stylesheet: &'a str,
}

/// Escapes text for HTML element content and quoted attribute values. Control characters
/// HTML doesn't allow become U+FFFD.
pub fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\t' | '\n' => out.push(c),
            '\r' => out.push('\n'),
            c if c.is_control() || is_noncharacter(c) || ('\u{E000}'..='\u{F8FF}').contains(&c) => {
                // Controls aren't allowed; private-use characters (the block marker among them)
                // mean nothing outside the app.
                if !('\u{E000}'..='\u{F8FF}').contains(&c) {
                    out.push('\u{FFFD}');
                }
            }
            c => out.push(c),
        }
    }
}

const fn is_noncharacter(c: char) -> bool {
    let n = c as u32;
    (n >= 0xFDD0 && n <= 0xFDEF) || (n & 0xFFFE) == 0xFFFE
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

/// A link's target as it may appear in the page, or `None` to show only its text.
fn href(link: &str, links: &dyn Links) -> Option<String> {
    let link = link.trim();
    if let Some(rest) = link.strip_prefix(PAGE_LINK) {
        let id = rest.split(['#', '?', '/']).next().unwrap_or_default();
        return id.parse().ok().and_then(|id| links.page(id));
    }
    let lower = link.to_ascii_lowercase();
    let allowed = ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme));
    // A URL with spaces or controls inside isn't one a person meant.
    let clean = !link.chars().any(|c| c.is_whitespace() || c.is_control());
    (allowed && clean && link.len() <= 2048).then(|| link.to_owned())
}

fn inline(out: &mut String, spans: &[Span], links: &dyn Links) {
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        let Marks {
            bold,
            italic,
            underline,
            strikethrough,
            code,
            link,
        } = &span.marks;
        let target = link.as_deref().and_then(|l| href(l, links));
        let mut close = Vec::new();
        if let Some(target) = &target {
            let external = !link.as_deref().is_some_and(|l| l.starts_with(PAGE_LINK));
            out.push_str("<a href=\"");
            escape_into(out, target);
            if external {
                out.push_str("\" rel=\"noopener noreferrer nofollow ugc\">");
            } else {
                out.push_str("\" class=\"page-link\">");
            }
            close.push("</a>");
        }
        for (on, open, end) in [
            (*bold, "<strong>", "</strong>"),
            (*italic, "<em>", "</em>"),
            (*underline, "<u>", "</u>"),
            (*strikethrough, "<s>", "</s>"),
            (*code, "<code>", "</code>"),
        ] {
            if on {
                out.push_str(open);
                close.push(end);
            }
        }
        let mut lines = span.text.split('\n');
        if let Some(first) = lines.next() {
            escape_into(out, first);
        }
        for line in lines {
            out.push_str("<br>");
            escape_into(out, line);
        }
        for end in close.iter().rev() {
            out.push_str(end);
        }
    }
}

/// A block and the blocks nested under it.
struct Node<'a> {
    block: &'a Block,
    children: Vec<Node<'a>>,
}

/// The flat list as a tree: a block's children are the blocks after it with a deeper indent.
fn tree(blocks: &[Block]) -> Vec<Node<'_>> {
    fn take<'a>(blocks: &'a [Block], at: &mut usize, indent: Option<u8>) -> Vec<Node<'a>> {
        let mut out = Vec::new();
        while let Some(block) = blocks.get(*at) {
            if indent.is_some_and(|i| block.indent <= i) {
                break;
            }
            *at += 1;
            let children = take(blocks, at, Some(block.indent));
            out.push(Node { block, children });
        }
        out
    }
    let mut at = 0;
    take(blocks, &mut at, None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum List {
    Bulleted,
    Numbered,
    Todo,
}

fn list_of(kind: &BlockKind) -> Option<List> {
    match kind {
        BlockKind::Bulleted => Some(List::Bulleted),
        BlockKind::Numbered => Some(List::Numbered),
        BlockKind::Todo { .. } => Some(List::Todo),
        _ => None,
    }
}

fn nodes(out: &mut String, nodes: &[Node<'_>], links: &dyn Links, depth: usize) {
    let mut i = 0;
    while i < nodes.len() {
        let Some(list) = list_of(&nodes[i].block.kind) else {
            node(out, &nodes[i], links, depth);
            i += 1;
            continue;
        };
        out.push_str(match list {
            List::Bulleted => "<ul>",
            List::Numbered => "<ol>",
            List::Todo => "<ul class=\"todo-list\">",
        });
        while let Some(n) = nodes
            .get(i)
            .filter(|n| list_of(&n.block.kind) == Some(list))
        {
            if let BlockKind::Todo { checked } = n.block.kind {
                out.push_str(if checked {
                    "<li class=\"todo done\"><label><input type=\"checkbox\" disabled checked> <span>"
                } else {
                    "<li class=\"todo\"><label><input type=\"checkbox\" disabled> <span>"
                });
                inline(out, &n.block.spans, links);
                out.push_str("</span></label>");
            } else {
                out.push_str("<li>");
                inline(out, &n.block.spans, links);
            }
            children(out, &n.children, links, depth);
            out.push_str("</li>");
            i += 1;
        }
        out.push_str(match list {
            List::Numbered => "</ol>",
            _ => "</ul>",
        });
    }
}

fn children(out: &mut String, kids: &[Node<'_>], links: &dyn Links, depth: usize) {
    if kids.is_empty() {
        return;
    }
    // Past the model's deepest indent there is nothing more to nest; flatten what's left.
    nodes(out, kids, links, depth + 1);
}

fn node(out: &mut String, n: &Node<'_>, links: &dyn Links, depth: usize) {
    let b = n.block;
    match &b.kind {
        BlockKind::Paragraph => {
            if b.spans.iter().all(|s| s.text.is_empty()) {
                // An empty paragraph is a gap the writer left; keep it, quietly.
                out.push_str("<p class=\"empty\" aria-hidden=\"true\"></p>");
            } else {
                out.push_str("<p>");
                inline(out, &b.spans, links);
                out.push_str("</p>");
            }
            nested(out, &n.children, links, depth);
        }
        BlockKind::Heading { level } => {
            // The page title is the h1; the page's headings sit below it.
            let h = match level {
                HeadingLevel::H1 => "h2",
                HeadingLevel::H2 => "h3",
                HeadingLevel::H3 => "h4",
            };
            let _ = write!(out, "<{h}>");
            inline(out, &b.spans, links);
            let _ = write!(out, "</{h}>");
            nested(out, &n.children, links, depth);
        }
        BlockKind::Quote => {
            out.push_str("<blockquote><p>");
            inline(out, &b.spans, links);
            out.push_str("</p>");
            children(out, &n.children, links, depth);
            out.push_str("</blockquote>");
        }
        BlockKind::Code { language } => {
            let code = b.text();
            let lang = language.trim();
            out.push_str("<figure class=\"code\">");
            if !lang.is_empty() && lang != "plain" {
                out.push_str("<figcaption>");
                escape_into(out, lang);
                out.push_str("</figcaption>");
            }
            out.push_str("<pre tabindex=\"0\"><code>");
            out.push_str(&highlight(lang, &code));
            out.push_str("</code></pre></figure>");
            nested(out, &n.children, links, depth);
        }
        BlockKind::Divider => {
            out.push_str("<hr>");
            nested(out, &n.children, links, depth);
        }
        BlockKind::Callout { icon } => {
            out.push_str("<aside class=\"callout\" role=\"note\">");
            let icon = icon.trim();
            if !icon.is_empty() {
                out.push_str("<span class=\"callout-icon\" aria-hidden=\"true\">");
                escape_into(out, &icon.chars().take(8).collect::<String>());
                out.push_str("</span>");
            }
            out.push_str("<div class=\"callout-body\"><p>");
            inline(out, &b.spans, links);
            out.push_str("</p>");
            children(out, &n.children, links, depth);
            out.push_str("</div></aside>");
        }
        BlockKind::Toggle => {
            out.push_str("<details><summary>");
            if b.spans.iter().all(|s| s.text.is_empty()) {
                out.push_str("<span class=\"muted\">Toggle</span>");
            } else {
                inline(out, &b.spans, links);
            }
            out.push_str("</summary><div class=\"toggle-body\">");
            children(out, &n.children, links, depth);
            out.push_str("</div></details>");
        }
        // A filter block lists pages from the whole workspace; published pages never show it.
        // List items are drawn by `nodes`, never here.
        BlockKind::Query { .. }
        | BlockKind::Bulleted
        | BlockKind::Numbered
        | BlockKind::Todo { .. } => nested(out, &n.children, links, depth),
        BlockKind::Attachment {
            blob,
            name,
            mime,
            size,
        } => {
            if let Some(src) = links.file(blob) {
                let caption = b.text();
                if is_image(mime) {
                    out.push_str("<figure class=\"image\"><img src=\"");
                    escape_into(out, &src);
                    out.push_str("\" alt=\"");
                    escape_into(
                        out,
                        if caption.trim().is_empty() {
                            name
                        } else {
                            &caption
                        },
                    );
                    out.push_str("\" loading=\"lazy\" decoding=\"async\">");
                    if !caption.trim().is_empty() {
                        out.push_str("<figcaption>");
                        inline(out, &b.spans, links);
                        out.push_str("</figcaption>");
                    }
                    out.push_str("</figure>");
                } else {
                    out.push_str("<p class=\"file\"><a href=\"");
                    escape_into(out, &src);
                    out.push_str("\" download><span class=\"file-name\">");
                    escape_into(out, name);
                    out.push_str("</span> <span class=\"file-size\">");
                    out.push_str(&size_text(u64::from(*size)));
                    out.push_str("</span></a>");
                    if !caption.trim().is_empty() {
                        out.push_str("<br><span class=\"file-caption\">");
                        inline(out, &b.spans, links);
                        out.push_str("</span>");
                    }
                    out.push_str("</p>");
                }
            }
            nested(out, &n.children, links, depth);
        }
    }
}

/// Children of a block that doesn't hold them itself: indented below it.
fn nested(out: &mut String, kids: &[Node<'_>], links: &dyn Links, depth: usize) {
    if kids.is_empty() {
        return;
    }
    out.push_str("<div class=\"indent\">");
    children(out, kids, links, depth);
    out.push_str("</div>");
}

/// The image types a browser shows inline, safely. SVG can carry script, so it's a download.
pub fn is_image(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/avif"
    )
}

fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1000.0;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.0} {}", UNITS[unit])
    }
}

fn mood_name(mood: Mood) -> &'static str {
    match mood {
        Mood::Awful => "Awful",
        Mood::Bad => "Bad",
        Mood::Okay => "Okay",
        Mood::Good => "Good",
        Mood::Great => "Great",
    }
}

fn date_text(day: drusite_model::Day) -> String {
    format!("{} {} {}", day.day(), day.month_name(), day.year())
}

fn value_text(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Text(s) => s.clone(),
        PropertyValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{n:.0}")
            } else {
                n.to_string()
            }
        }
        PropertyValue::Checkbox(true) => "Yes".to_owned(),
        PropertyValue::Checkbox(false) => "No".to_owned(),
        PropertyValue::Date(d) => date_text(*d),
    }
}

fn properties(out: &mut String, doc: &PageDoc<'_>) {
    let journal = doc.kind == PageKind::Journal;
    if journal {
        let day = doc
            .properties
            .iter()
            .find(|p| p.name == DATE)
            .and_then(|p| p.value.as_day());
        let mood = doc
            .properties
            .iter()
            .find(|p| p.name == MOOD)
            .and_then(|p| p.value.as_number())
            .and_then(Mood::from_score);
        if day.is_some() || mood.is_some() {
            out.push_str("<p class=\"journal-meta\">");
            if let Some(day) = day {
                out.push_str("<time datetime=\"");
                out.push_str(&day.to_string());
                out.push_str("\">");
                escape_into(out, &day.title());
                out.push_str("</time>");
            }
            if let Some(mood) = mood {
                let _ = write!(
                    out,
                    "<span class=\"mood mood-{}\"><span class=\"mood-dot\" aria-hidden=\"true\"></span>Mood: {}</span>",
                    mood.score(),
                    mood_name(mood)
                );
            }
            out.push_str("</p>");
        }
    }
    let listed: Vec<&Property> = doc
        .properties
        .iter()
        .filter(|p| !(journal && (p.name == DATE || p.name == MOOD)))
        .collect();
    if listed.is_empty() {
        return;
    }
    out.push_str("<dl class=\"props\">");
    for p in listed {
        out.push_str("<div><dt>");
        escape_into(out, &p.name);
        out.push_str("</dt><dd>");
        if let PropertyValue::Date(d) = &p.value {
            out.push_str("<time datetime=\"");
            out.push_str(&d.to_string());
            out.push_str("\">");
            escape_into(out, &date_text(*d));
            out.push_str("</time>");
        } else {
            escape_into(out, &value_text(&p.value));
        }
        out.push_str("</dd></div>");
    }
    out.push_str("</dl>");
}

/// The first words of the page, for link previews.
fn description(blocks: &[Block]) -> String {
    let mut text = String::new();
    for b in blocks {
        if matches!(
            b.kind,
            BlockKind::Divider
                | BlockKind::Query { .. }
                | BlockKind::Code { .. }
                | BlockKind::Attachment { .. }
        ) {
            continue;
        }
        let t = b.text();
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&t);
        if text.chars().count() > 200 {
            break;
        }
    }
    if text.chars().count() > 200 {
        let cut: String = text.chars().take(197).collect();
        format!("{}…", cut.trim_end())
    } else {
        text
    }
}

fn first_image(blocks: &[Block], links: &dyn Links) -> Option<String> {
    blocks.iter().find_map(|b| match &b.kind {
        BlockKind::Attachment { blob, mime, .. } if is_image(mime) => links.file(blob),
        _ => None,
    })
}

fn head(out: &mut String, title: &str, stylesheet: &str, indexable: bool) {
    out.push_str("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\">");
    out.push_str("<meta name=\"referrer\" content=\"no-referrer\">");
    if !indexable {
        out.push_str("<meta name=\"robots\" content=\"noindex, nofollow, noarchive\">");
    }
    out.push_str("<title>");
    escape_into(out, title);
    out.push_str("</title><link rel=\"stylesheet\" href=\"");
    escape_into(out, stylesheet);
    out.push_str("\">");
}

fn meta(out: &mut String, property: &str, content: &str) {
    out.push_str("<meta property=\"");
    out.push_str(property);
    out.push_str("\" content=\"");
    escape_into(out, content);
    out.push_str("\">");
}

fn page_title(title: &str) -> &str {
    if title.trim().is_empty() {
        "Untitled"
    } else {
        title
    }
}

/// A published page.
pub fn page(doc: &PageDoc<'_>, links: &dyn Links) -> String {
    let title = page_title(doc.title);
    let mut out = String::with_capacity(4096 + doc.blocks.len() * 96);
    head(&mut out, title, doc.stylesheet, doc.indexable);
    let desc = description(doc.blocks);
    if doc.indexable {
        out.push_str("<link rel=\"canonical\" href=\"");
        escape_into(&mut out, doc.url);
        out.push_str("\">");
    }
    if !desc.is_empty() {
        out.push_str("<meta name=\"description\" content=\"");
        escape_into(&mut out, &desc);
        out.push_str("\">");
    }
    meta(&mut out, "og:title", title);
    meta(&mut out, "og:type", "article");
    meta(&mut out, "og:url", doc.url);
    if let Some(site) = doc.trail.first() {
        meta(&mut out, "og:site_name", page_title(&site.title));
    }
    if !desc.is_empty() {
        meta(&mut out, "og:description", &desc);
    }
    let image = first_image(doc.blocks, links);
    if let Some(src) = &image {
        let absolute = format!("{}{}", doc.origin.trim_end_matches('/'), src);
        meta(&mut out, "og:image", &absolute);
    }
    out.push_str("<meta name=\"twitter:card\" content=\"");
    out.push_str(if image.is_some() {
        "summary_large_image"
    } else {
        "summary"
    });
    out.push_str("\"></head><body>");

    if !doc.trail.is_empty() {
        out.push_str("<header class=\"bar\"><nav aria-label=\"Breadcrumb\"><ol class=\"crumbs\">");
        for step in &doc.trail {
            out.push_str("<li><a href=\"");
            escape_into(&mut out, &step.href);
            out.push_str("\">");
            escape_into(&mut out, page_title(&step.title));
            out.push_str("</a></li>");
        }
        out.push_str("<li aria-current=\"page\">");
        escape_into(&mut out, title);
        out.push_str("</li></ol></nav></header>");
    }

    out.push_str("<main class=\"page\"><article><h1 class=\"title\">");
    escape_into(&mut out, title);
    out.push_str("</h1>");
    properties(&mut out, doc);
    out.push_str("<div class=\"blocks\">");
    nodes(&mut out, &tree(doc.blocks), links, 0);
    out.push_str("</div></article>");
    if !doc.subpages.is_empty() {
        out.push_str(
            "<nav class=\"subpages\" aria-label=\"Pages inside\"><h2>Pages inside</h2><ul>",
        );
        for sub in &doc.subpages {
            out.push_str("<li><a href=\"");
            escape_into(&mut out, &sub.href);
            out.push_str("\">");
            escape_into(&mut out, page_title(&sub.title));
            out.push_str("</a></li>");
        }
        out.push_str("</ul></nav>");
    }
    out.push_str("</main><footer class=\"foot\">Published with Drusite</footer></body></html>\n");
    out
}

/// The form a password-protected page shows first. Nothing of the page is in it.
pub fn password_form(action: &str, stylesheet: &str, wrong: bool, limited: bool) -> String {
    let mut out = String::with_capacity(2048);
    head(&mut out, "Password needed", stylesheet, false);
    out.push_str("</head><body class=\"gate\"><main class=\"gate-card\">");
    out.push_str("<h1>This page needs a password</h1>");
    out.push_str("<p class=\"muted\">Ask whoever shared the link for it.</p>");
    out.push_str("<form method=\"post\" action=\"");
    escape_into(&mut out, action);
    out.push_str("\"><label for=\"password\">Password</label>");
    out.push_str(
        "<input id=\"password\" name=\"password\" type=\"password\" autocomplete=\"current-password\" required autofocus",
    );
    if wrong || limited {
        out.push_str(" aria-invalid=\"true\" aria-describedby=\"problem\"");
    }
    out.push('>');
    if limited {
        out.push_str("<p id=\"problem\" class=\"problem\" role=\"alert\">Too many tries. Wait a few minutes, then try again.</p>");
    } else if wrong {
        out.push_str(
            "<p id=\"problem\" class=\"problem\" role=\"alert\">That isn't the password.</p>",
        );
    }
    out.push_str("<button type=\"submit\">Open the page</button></form></main></body></html>\n");
    out
}

/// What every link that doesn't work gets: unknown, unpublished, expired or in the trash all
/// look the same.
pub fn not_found(stylesheet: &str) -> String {
    let mut out = String::with_capacity(1024);
    head(&mut out, "Page not found", stylesheet, false);
    out.push_str("</head><body class=\"gate\"><main class=\"gate-card\">");
    out.push_str("<h1>Page not found</h1>");
    out.push_str("<p class=\"muted\">This link doesn't lead to a published page. It may have been unpublished, or never existed.</p>");
    out.push_str("</main></body></html>\n");
    out
}

#[cfg(test)]
mod tests;
