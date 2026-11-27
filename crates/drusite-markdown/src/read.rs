// SPDX-License-Identifier: AGPL-3.0-or-later

//! Markdown → page.

use std::ops::Range;

use drusite_model::{BlockKind, HeadingLevel, MAX_INDENT, Marks, Span};
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::front::{Frontmatter, split_frontmatter};
use crate::ids::decode_block_id;
use crate::write::{CALLOUTS, DATABASE_LANG, QUERY_LANG};
use crate::{BlockRef, MdBlock, MdPage, Resolver};

/// Reads a Markdown file: its frontmatter and its blocks.
///
/// Anything a page can't hold stays as text: tables become a code block of their Markdown
/// (language `table`), footnotes and `==highlights==` stay as typed, headings below level 3
/// read as level 3, a list's start number is dropped, and a callout's fold state is dropped
/// (except `[!toggle]`, which is a toggle).
pub fn read(file: &str, r: &dyn Resolver) -> MdPage {
    let (yaml, body) = split_frontmatter(file);
    MdPage {
        front: yaml.map(Frontmatter::parse).unwrap_or_default(),
        blocks: read_body(body, r),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Holder {
    Item,
    Quote,
}

struct Container {
    holder: Holder,
    /// The block the container's first paragraph is the text of.
    block: usize,
    /// That block already has its text (or something else came first).
    taken: bool,
}

/// What reading collects per block besides the block.
#[derive(Default)]
struct Extra {
    /// The source of the last text added, to tell a real `^id` from an escaped `\^id`.
    tail: Option<Range<usize>>,
    /// An embed in the block: its attachment, and which span holds its alt text.
    embed: Option<(BlockKind, usize)>,
    /// The ID an empty paragraph's comment gave it.
    comment_id: Option<BlockRef>,
}

struct Reader<'a> {
    src: &'a str,
    r: &'a dyn Resolver,
    out: Vec<MdBlock>,
    extra: Vec<Extra>,
    /// The block inline content goes to.
    target: Option<usize>,
    containers: Vec<Container>,
    /// The lists being read: whether each is ordered.
    lists: Vec<bool>,
    marks: Vec<Marks>,
    /// The indent the current top-level block was given by `<!-- indent N -->`.
    base: u8,
    pending_indent: Option<u8>,
    /// Text before this offset is a callout's marker, already read.
    skip_until: usize,
    /// Inside a code block (text is taken as it is).
    in_code: bool,
    /// Inside a table: everything is skipped until it ends.
    in_table: bool,
    /// An image being read: where it points, whether it's a wikilink, its alt text so far.
    image: Option<(String, bool, bool, String)>,
    html: Option<String>,
}

pub(crate) fn read_body(md: &str, r: &dyn Resolver) -> Vec<MdBlock> {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_WIKILINKS;
    let mut reader = Reader {
        src: md,
        r,
        out: Vec::new(),
        extra: Vec::new(),
        target: None,
        containers: Vec::new(),
        lists: Vec::new(),
        marks: vec![Marks::default()],
        base: 0,
        pending_indent: None,
        skip_until: 0,
        in_code: false,
        in_table: false,
        image: None,
        html: None,
    };
    for (event, range) in Parser::new_ext(md, opts).into_offset_iter() {
        reader.event(event, range);
    }
    reader.finish()
}

impl Reader<'_> {
    fn depth(&self) -> u8 {
        let d = self.base as usize + self.containers.len();
        u8::try_from(d.min(MAX_INDENT as usize)).unwrap_or(MAX_INDENT)
    }

    /// A new block at the current depth. Inside a container that has no text yet, a block
    /// other than its first paragraph means it never will.
    fn push(&mut self, kind: BlockKind) -> usize {
        if let Some(c) = self.containers.last_mut() {
            c.taken = true;
        }
        if self.containers.is_empty() {
            self.base = self.pending_indent.take().unwrap_or(0);
        }
        self.out.push(MdBlock {
            id: None,
            kind,
            indent: self.depth(),
            spans: Vec::new(),
        });
        self.extra.push(Extra::default());
        self.out.len() - 1
    }

    fn text(&mut self, t: &str, range: Range<usize>, marks: Marks) {
        if let Some((_, _, _, alt)) = &mut self.image {
            alt.push_str(t);
            return;
        }
        let at = match self.target {
            Some(at) => at,
            None => {
                let at = self.push(BlockKind::Paragraph);
                self.target = Some(at);
                at
            }
        };
        if let Some(c) = self.containers.last_mut()
            && c.block == at
        {
            c.taken = true;
        }
        self.extra[at].tail = Some(range);
        let spans = &mut self.out[at].spans;
        match spans.last_mut() {
            Some(s) if s.marks == marks => s.text.push_str(t),
            _ => spans.push(Span {
                text: t.to_owned(),
                marks,
            }),
        }
    }

    fn current_marks(&self) -> Marks {
        self.marks.last().cloned().unwrap_or_default()
    }

    fn push_marks(&mut self, f: impl FnOnce(&mut Marks)) {
        let mut m = self.current_marks();
        f(&mut m);
        self.marks.push(m);
    }

    fn pop_marks(&mut self) {
        if self.marks.len() > 1 {
            self.marks.pop();
        }
    }

    #[allow(clippy::too_many_lines)]
    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        if self.in_table {
            if matches!(event, Event::End(TagEnd::Table)) {
                self.in_table = false;
            }
            return;
        }
        if let Some(html) = &mut self.html {
            match event {
                Event::Html(t) => html.push_str(&t),
                Event::End(TagEnd::HtmlBlock) => {
                    let html = self.html.take().unwrap_or_default();
                    self.html_block(&html, range);
                }
                _ => {}
            }
            return;
        }
        let marks = self.current_marks();
        match event {
            Event::Start(Tag::Paragraph) => {
                self.marks.truncate(1);
                match self.containers.last_mut() {
                    Some(c) if !c.taken => {
                        c.taken = true;
                        self.target = Some(c.block);
                    }
                    _ => {
                        let at = self.push(BlockKind::Paragraph);
                        self.target = Some(at);
                    }
                }
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => self.target = None,
            Event::Start(Tag::Heading { level, .. }) => {
                self.marks.truncate(1);
                let at = self.push(BlockKind::Heading {
                    level: HeadingLevel::clamped(level as i64),
                });
                self.target = Some(at);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                let (kind, marker_end) = self.quote_kind(range.start);
                let at = self.push(kind);
                self.skip_until = marker_end;
                self.target = None;
                self.containers.push(Container {
                    holder: Holder::Quote,
                    block: at,
                    taken: false,
                });
            }
            Event::End(TagEnd::BlockQuote(_)) => self.pop_container(Holder::Quote),
            Event::Start(Tag::List(start)) => {
                if let Some(c) = self.containers.last_mut() {
                    c.taken = true;
                }
                self.target = None;
                self.lists.push(start.is_some());
            }
            Event::End(TagEnd::List(_)) => {
                self.target = None;
                self.lists.pop();
            }
            Event::Start(Tag::Item) => {
                self.marks.truncate(1);
                let ordered = self.lists.last().copied().unwrap_or(false);
                let at = self.push(if ordered {
                    BlockKind::Numbered
                } else {
                    BlockKind::Bulleted
                });
                self.target = Some(at);
                self.containers.push(Container {
                    holder: Holder::Item,
                    block: at,
                    taken: false,
                });
            }
            Event::End(TagEnd::Item) => self.pop_container(Holder::Item),
            Event::TaskListMarker(checked) => {
                if let Some(at) = self.target
                    && matches!(self.out[at].kind, BlockKind::Bulleted | BlockKind::Numbered)
                {
                    self.out[at].kind = BlockKind::Todo { checked };
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match kind {
                    CodeBlockKind::Fenced(l) => l.trim().to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                let kind = if language == QUERY_LANG {
                    BlockKind::Query {
                        query: String::new(),
                    }
                } else {
                    BlockKind::Code { language }
                };
                let at = self.push(kind);
                self.target = Some(at);
                self.in_code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                self.in_code = false;
                if let Some(at) = self.target.take() {
                    let block = &mut self.out[at];
                    let mut code: String = block.text();
                    if code.ends_with('\n') {
                        code.pop();
                    }
                    if let BlockKind::Query { query } = &mut block.kind {
                        *query = code;
                        block.spans.clear();
                    } else if matches!(&block.kind, BlockKind::Code { language } if language == DATABASE_LANG)
                        && let Some(kind) = database_block(&code)
                    {
                        block.kind = kind;
                        block.spans.clear();
                    } else {
                        block.spans = if code.is_empty() {
                            Vec::new()
                        } else {
                            vec![Span::plain(code)]
                        };
                    }
                }
            }
            Event::Rule => {
                self.push(BlockKind::Divider);
                self.target = None;
            }
            Event::Start(Tag::Table(_)) => {
                let table = self.table_source(range.clone());
                let at = self.push(BlockKind::Code {
                    language: "table".into(),
                });
                self.out[at].spans = vec![Span::plain(table)];
                self.target = None;
                self.in_table = true;
            }
            Event::Start(Tag::HtmlBlock) => self.html = Some(String::new()),
            Event::Start(Tag::Strong) => self.push_marks(|m| m.bold = true),
            Event::Start(Tag::Emphasis) => self.push_marks(|m| m.italic = true),
            Event::Start(Tag::Strikethrough) => self.push_marks(|m| m.strikethrough = true),
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                let wiki = matches!(link_type, LinkType::WikiLink { .. });
                let href = self.r.link_in(&dest_url, wiki);
                self.push_marks(|m| m.link = Some(href));
            }
            Event::End(
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link,
            ) => self.pop_marks(),
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                ..
            }) => {
                let (wiki, piped) = match link_type {
                    LinkType::WikiLink { has_pothole } => (true, has_pothole),
                    _ => (false, false),
                };
                self.image = Some((dest_url.to_string(), wiki, piped, String::new()));
            }
            Event::End(TagEnd::Image) => {
                if let Some((dest, wiki, piped, alt)) = self.image.take() {
                    // `![[file]]` has no caption: its text is the target.
                    let alt = if wiki && !piped { String::new() } else { alt };
                    let embed = self.r.attachment_in(&dest, wiki);
                    let href = self.r.link_in(&dest, wiki);
                    let shown = if alt.is_empty() {
                        dest.clone()
                    } else {
                        alt.clone()
                    };
                    let mut m = marks;
                    m.link = Some(href);
                    self.text(&shown, range, m);
                    if let (Some(kind), Some(at)) = (embed, self.target) {
                        let span = self.out[at].spans.len() - 1;
                        // The caption is the alt text alone.
                        self.out[at].spans[span].text = alt;
                        self.extra[at].embed = Some((kind, span));
                    }
                }
            }
            Event::Text(t) => {
                if range.end <= self.skip_until {
                    return;
                }
                let t = if range.start < self.skip_until {
                    // Part of the marker and part text: keep the text.
                    let cut = self.skip_until - range.start;
                    t.get(cut.min(t.len())..).unwrap_or("").to_owned()
                } else {
                    t.to_string()
                };
                if t.is_empty() {
                    return;
                }
                if self.in_code {
                    let at = self
                        .target
                        .unwrap_or_else(|| self.push(BlockKind::Paragraph));
                    self.target = Some(at);
                    match self.out[at].spans.last_mut() {
                        Some(s) => s.text.push_str(&t),
                        None => self.out[at].spans.push(Span::plain(t)),
                    }
                } else {
                    self.text(&t, range, marks);
                }
            }
            Event::Code(t) => {
                let mut m = marks;
                m.code = true;
                self.text(&t, range, m);
            }
            Event::SoftBreak => {
                if range.end <= self.skip_until {
                    return;
                }
                // A callout's title is its first line; what follows is its body.
                if let (Some(at), Some(c)) = (self.target, self.containers.last())
                    && c.block == at
                    && matches!(
                        self.out[at].kind,
                        BlockKind::Callout { .. } | BlockKind::Toggle
                    )
                {
                    let body = self.push(BlockKind::Paragraph);
                    self.target = Some(body);
                    return;
                }
                self.text("\n", range, marks);
            }
            Event::HardBreak => self.text("\n", range, marks),
            Event::InlineHtml(t) => match t.to_ascii_lowercase().as_str() {
                "<b>" | "<strong>" => self.push_marks(|m| m.bold = true),
                "<i>" | "<em>" => self.push_marks(|m| m.italic = true),
                "<s>" | "<del>" => self.push_marks(|m| m.strikethrough = true),
                "<u>" => self.push_marks(|m| m.underline = true),
                "</b>" | "</strong>" | "</i>" | "</em>" | "</s>" | "</del>" | "</u>" => {
                    self.pop_marks();
                }
                h if h.starts_with("<br") => self.text("\n", range, marks),
                _ => self.text(&t, range, marks),
            },
            Event::Html(t) => self.text(&t, range, marks),
            _ => {}
        }
    }

    fn pop_container(&mut self, holder: Holder) {
        if self.containers.last().is_some_and(|c| c.holder == holder) {
            self.containers.pop();
        }
        self.target = None;
    }

    /// What a block quote is, from its first line: a callout (`[!type]`), a toggle
    /// (`[!toggle]`), or a quote. Also where the callout's marker ends.
    fn quote_kind(&self, start: usize) -> (BlockKind, usize) {
        let line_end = self.src[start..]
            .find('\n')
            .map_or(self.src.len(), |i| start + i);
        let line = &self.src[start..line_end];
        let Some(gt) = line.find('>') else {
            return (BlockKind::Quote, 0);
        };
        let after = &line[gt + 1..];
        let trimmed = after.trim_start();
        let Some(rest) = trimmed.strip_prefix("[!") else {
            return (BlockKind::Quote, 0);
        };
        let Some(close) = rest.find(']') else {
            return (BlockKind::Quote, 0);
        };
        let inner = &rest[..close];
        let (kind, meta) = match inner.split_once('|') {
            Some((k, m)) => (k, Some(m)),
            None => (inner, None),
        };
        if kind.is_empty()
            || !kind
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return (BlockKind::Quote, 0);
        }
        let mut end = start + gt + 1 + (after.len() - trimmed.len()) + 2 + close + 1;
        let tail = &self.src[end..line_end];
        let fold = tail.starts_with(['+', '-']);
        if fold {
            end += 1;
        }
        if self.src[end..line_end].starts_with(' ') {
            end += 1;
        }
        let kind = kind.to_ascii_lowercase();
        if kind == "toggle" {
            return (BlockKind::Toggle, end);
        }
        let icon = match meta {
            Some(m) => m.trim().to_owned(),
            None => CALLOUTS
                .iter()
                .find(|(k, _)| *k == kind)
                .map_or("💡", |(_, i)| i)
                .to_owned(),
        };
        (BlockKind::Callout { icon }, end)
    }

    /// A table's Markdown, without the quote markers or indentation it sits in.
    fn table_source(&self, range: Range<usize>) -> String {
        let quotes = self
            .containers
            .iter()
            .filter(|c| c.holder == Holder::Quote)
            .count();
        let src = self.src[range].trim_end();
        src.lines()
            .map(|line| {
                let mut l = line;
                for _ in 0..quotes {
                    let t = l.trim_start();
                    l = t.strip_prefix('>').unwrap_or(t);
                }
                l.trim()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// An HTML block: `<!-- ^id -->` is an empty paragraph, `<!-- indent N -->` sets the
    /// next top-level block's indent; anything else is text.
    fn html_block(&mut self, html: &str, range: Range<usize>) {
        let t = html.trim();
        if let Some(inner) = t
            .strip_prefix("<!--")
            .and_then(|s| s.strip_suffix("-->"))
            .filter(|s| !s.contains("-->"))
        {
            let mut id = None;
            let mut indent = None;
            let mut words = inner.split_whitespace().peekable();
            let mut ours = true;
            while let Some(w) = words.next() {
                if w == "indent" {
                    indent = words.next().and_then(|n| n.parse::<u8>().ok());
                } else if let Some(r) = w.strip_prefix('^').and_then(block_ref) {
                    id = Some(r);
                } else {
                    ours = false;
                }
            }
            if ours {
                if let Some(n) = indent
                    && self.containers.is_empty()
                {
                    self.pending_indent = Some(n.min(MAX_INDENT));
                }
                if id.is_some() || indent.is_none() {
                    let at = self.push(BlockKind::Paragraph);
                    self.extra[at].comment_id = id;
                }
                self.target = None;
                return;
            }
        }
        let at = self.push(BlockKind::Paragraph);
        self.out[at].spans = vec![Span::plain(t)];
        self.extra[at].tail = Some(range);
        self.target = None;
    }

    /// Pulls block references out of the text, and turns lone embeds into attachments.
    fn finish(self) -> Vec<MdBlock> {
        let src = self.src;
        let mut out: Vec<MdBlock> = Vec::with_capacity(self.out.len());
        for (mut b, extra) in self.out.into_iter().zip(self.extra) {
            if let Some(id) = extra.comment_id {
                b.id = Some(id);
                out.push(b);
                continue;
            }
            let holds_text = !matches!(
                b.kind,
                BlockKind::Code { .. }
                    | BlockKind::Query { .. }
                    | BlockKind::Database { .. }
                    | BlockKind::Divider
            );
            if holds_text {
                b.id = take_ref(&mut b.spans, src, extra.tail);
            }
            // A lone `^id` after a block that can't hold one is that block's.
            if b.kind == BlockKind::Paragraph
                && b.id.is_some()
                && b.spans.is_empty()
                && let Some(prev) = out.last_mut()
                && prev.id.is_none()
                && prev.indent >= b.indent
                && matches!(
                    prev.kind,
                    BlockKind::Code { .. }
                        | BlockKind::Query { .. }
                        | BlockKind::Database { .. }
                        | BlockKind::Divider
                )
            {
                prev.id = b.id;
                continue;
            }
            if let Some((kind, span)) = extra.embed
                && b.kind == BlockKind::Paragraph
                && b.spans
                    .iter()
                    .enumerate()
                    .all(|(i, s)| i == span || s.text.trim().is_empty())
            {
                let caption = b
                    .spans
                    .get(span)
                    .map(|s| s.text.clone())
                    .unwrap_or_default();
                b.kind = kind;
                b.spans = if caption.is_empty() {
                    Vec::new()
                } else {
                    vec![Span::plain(caption)]
                };
            }
            out.push(b);
        }
        out
    }
}

fn block_ref(s: &str) -> Option<BlockRef> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    Some(match decode_block_id(s) {
        Some(id) => BlockRef::Block(id),
        None => BlockRef::Other(s.to_owned()),
    })
}

/// Takes a trailing ` ^ref` (or a block's whole text `^ref`) off the spans. `tail` is where in
/// `src` the last text read came from, so an escaped `\^ref` isn't taken for one.
fn take_ref(spans: &mut Vec<Span>, src: &str, tail: Option<Range<usize>>) -> Option<BlockRef> {
    let last = spans.last_mut()?;
    if !last.marks.is_plain() {
        return None;
    }
    let caret = last.text.rfind('^')?;
    let r = block_ref(&last.text[caret + 1..])?;
    if caret > 0 && !last.text[..caret].ends_with(' ') {
        return None;
    }
    let tail = tail?;
    let raw = &src[tail.clone()];
    let raw_caret = raw.rfind('^')?;
    // An escape's text starts after its backslash.
    if src[..tail.start + raw_caret].ends_with('\\')
        || raw[raw_caret + 1..] != last.text[caret + 1..]
    {
        return None;
    }
    let keep = if caret == 0 { 0 } else { caret - 1 };
    last.text.truncate(keep);
    if last.text.is_empty() {
        spans.pop();
    }
    Some(r)
}

/// What a block's spans mean, for comparing: spans merged per character, and emphasis dropped
/// from whitespace (Markdown can't hold it at a mark's edge, and it barely shows).
pub(crate) fn normalise(spans: &[Span]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for s in spans {
        for ch in s.text.chars() {
            let mut m = s.marks.clone();
            if ch.is_whitespace() && !m.code {
                m.bold = false;
                m.italic = false;
                m.strikethrough = false;
                m.underline = false;
            }
            match out.last_mut() {
                Some(l) if l.marks == m => l.text.push(ch),
                _ => out.push(Span {
                    text: ch.to_string(),
                    marks: m,
                }),
            }
        }
    }
    out
}

/// What a block's spans mean once read back, for tests and the bridge's comparisons.
pub fn normalise_spans(spans: &[Span]) -> Vec<Span> {
    normalise(spans)
}

/// An inline database block's fence: `database: <id>` and, optionally, `view: <id>`.
fn database_block(code: &str) -> Option<BlockKind> {
    let mut database = None;
    let mut view = None;
    for line in code.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "database" => database = v.trim().parse().ok(),
                "view" => view = Some(v.trim().to_owned()).filter(|v| !v.is_empty()),
                _ => {}
            }
        }
    }
    Some(BlockKind::Database {
        database: database?,
        view,
    })
}
