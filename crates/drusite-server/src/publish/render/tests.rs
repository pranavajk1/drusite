// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use drusite_model::{BlockId, Day, Marks};
use proptest::prelude::*;

use super::*;

struct TestLinks {
    pages: HashMap<PageId, String>,
    files: HashMap<String, String>,
}

impl Links for TestLinks {
    fn page(&self, page: PageId) -> Option<String> {
        self.pages.get(&page).cloned()
    }
    fn file(&self, blob: &str) -> Option<String> {
        self.files.get(blob).cloned()
    }
}

fn none() -> TestLinks {
    TestLinks {
        pages: HashMap::new(),
        files: HashMap::new(),
    }
}

fn block(kind: BlockKind, indent: u8, text: &str) -> Block {
    Block {
        id: BlockId::new(),
        kind,
        indent,
        spans: vec![Span::plain(text)],
    }
}

fn doc<'a>(title: &'a str, blocks: &'a [Block], props: &'a [Property]) -> PageDoc<'a> {
    PageDoc {
        title,
        kind: PageKind::Page,
        properties: props,
        blocks,
        trail: Vec::new(),
        subpages: Vec::new(),
        url: "https://pages.example.com/p/tok",
        origin: "https://pages.example.com",
        indexable: false,
        stylesheet: "/assets/publish.css",
    }
}

const VOID: &[&str] = &["meta", "link", "br", "hr", "img", "input"];
const TAGS: &[&str] = &[
    "html",
    "head",
    "meta",
    "title",
    "link",
    "body",
    "header",
    "nav",
    "ol",
    "ul",
    "li",
    "a",
    "main",
    "article",
    "h1",
    "h2",
    "h3",
    "h4",
    "p",
    "div",
    "strong",
    "em",
    "u",
    "s",
    "code",
    "br",
    "blockquote",
    "figure",
    "figcaption",
    "pre",
    "span",
    "hr",
    "aside",
    "details",
    "summary",
    "img",
    "label",
    "input",
    "dl",
    "dt",
    "dd",
    "time",
    "footer",
    "form",
    "button",
];
const ATTRS: &[&str] = &[
    "lang",
    "charset",
    "name",
    "content",
    "property",
    "rel",
    "href",
    "class",
    "aria-label",
    "aria-current",
    "aria-hidden",
    "role",
    "tabindex",
    "src",
    "alt",
    "loading",
    "decoding",
    "type",
    "disabled",
    "checked",
    "download",
    "datetime",
    "method",
    "action",
    "for",
    "id",
    "autocomplete",
    "required",
    "autofocus",
    "aria-invalid",
    "aria-describedby",
];

/// Checks the HTML is what the renderer means to write: only known tags and attributes, every
/// value quoted, nothing unescaped in text, and elements properly nested. Returns the hrefs.
fn check(html: &str) -> Vec<String> {
    let rest = html
        .strip_prefix("<!doctype html>\n")
        .unwrap_or_else(|| panic!("no doctype: {html}"));
    let mut stack: Vec<String> = Vec::new();
    let mut hrefs = Vec::new();
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' => {
                i += 1;
                let closing = b[i] == b'/';
                if closing {
                    i += 1;
                }
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                let name = &rest[start..i];
                assert!(TAGS.contains(&name), "unknown tag <{name}> in {html}");
                if closing {
                    assert_eq!(b[i], b'>', "bad closing tag {name}");
                    i += 1;
                    let open = stack.pop().unwrap_or_else(|| panic!("stray </{name}>"));
                    assert_eq!(open, name, "</{name}> closes <{open}> in {html}");
                    continue;
                }
                loop {
                    while b[i] == b' ' {
                        i += 1;
                    }
                    if b[i] == b'>' {
                        i += 1;
                        break;
                    }
                    let start = i;
                    while b[i].is_ascii_lowercase() || b[i] == b'-' {
                        i += 1;
                    }
                    let attr = &rest[start..i];
                    assert!(
                        ATTRS.contains(&attr),
                        "unknown attribute {attr} on <{name}>"
                    );
                    if b[i] == b'=' {
                        assert_eq!(b[i + 1], b'"', "unquoted {attr}");
                        i += 2;
                        let start = i;
                        while b[i] != b'"' {
                            assert!(b[i] != b'<' && b[i] != b'>', "raw <> in {attr}");
                            i += 1;
                        }
                        if attr == "href" || attr == "src" || attr == "action" {
                            hrefs.push(rest[start..i].to_owned());
                        }
                        i += 1;
                    }
                }
                if !VOID.contains(&name) {
                    stack.push(name.to_owned());
                }
            }
            b'>' | b'"' => panic!("unescaped {} in text: {html}", b[i] as char),
            _ => i += 1,
        }
    }
    assert!(stack.is_empty(), "unclosed {stack:?}");
    hrefs
}

#[test]
fn every_block_type_renders() {
    let blocks = vec![
        block(
            BlockKind::Heading {
                level: HeadingLevel::H1,
            },
            0,
            "Big",
        ),
        block(
            BlockKind::Heading {
                level: HeadingLevel::H3,
            },
            0,
            "Small",
        ),
        block(BlockKind::Paragraph, 0, "Words\nmore"),
        block(BlockKind::Bulleted, 0, "one"),
        block(BlockKind::Bulleted, 1, "one.a"),
        block(BlockKind::Bulleted, 0, "two"),
        block(BlockKind::Numbered, 0, "first"),
        block(BlockKind::Numbered, 0, "second"),
        block(BlockKind::Todo { checked: true }, 0, "done"),
        block(BlockKind::Todo { checked: false }, 0, "open"),
        block(BlockKind::Quote, 0, "said"),
        block(
            BlockKind::Code {
                language: "rust".into(),
            },
            0,
            "fn x() {}",
        ),
        block(
            BlockKind::Callout {
                icon: "💡".into()
            },
            0,
            "note",
        ),
        block(BlockKind::Toggle, 0, "more"),
        block(BlockKind::Paragraph, 1, "hidden inside"),
        block(BlockKind::Divider, 0, ""),
        block(BlockKind::Query { query: "{}".into() }, 0, ""),
    ];
    let html = page(&doc("A page", &blocks, &[]), &none());
    check(&html);
    for want in [
        "<h1 class=\"title\">A page</h1>",
        "<h2>Big</h2>",
        "<h4>Small</h4>",
        "<p>Words<br>more</p>",
        "<ul><li>one<ul><li>one.a</li></ul></li><li>two</li></ul>",
        "<ol><li>first</li><li>second</li></ol>",
        "<li class=\"todo done\"><label><input type=\"checkbox\" disabled checked> <span>done</span></label></li>",
        "<blockquote><p>said</p></blockquote>",
        "<figcaption>rust</figcaption><pre tabindex=\"0\"><code><span class=\"tok-k\">fn</span>",
        "<aside class=\"callout\" role=\"note\"><span class=\"callout-icon\" aria-hidden=\"true\">💡</span>",
        "<details><summary>more</summary><div class=\"toggle-body\"><p>hidden inside</p></div></details>",
        "<hr>",
        "<meta name=\"robots\" content=\"noindex, nofollow, noarchive\">",
        "<meta property=\"og:title\" content=\"A page\">",
        "<meta property=\"og:description\" content=\"Big Small Words more one",
    ] {
        assert!(html.contains(want), "missing {want} in {html}");
    }
    assert!(!html.contains("query"), "{html}");
}

#[test]
fn a_new_list_starts_after_another_kind_of_block() {
    let blocks = vec![
        block(BlockKind::Numbered, 0, "a"),
        block(BlockKind::Paragraph, 0, "break"),
        block(BlockKind::Numbered, 0, "b"),
    ];
    let html = page(&doc("t", &blocks, &[]), &none());
    assert!(
        html.contains("<ol><li>a</li></ol><p>break</p><ol><li>b</li></ol>"),
        "{html}"
    );
}

#[test]
fn links_go_only_where_they_may() {
    let published = PageId::new();
    let private = PageId::new();
    let mut spans = Vec::new();
    for (text, link) in [
        ("pub", format!("drusite:page/{published}")),
        ("priv", format!("drusite:page/{private}#^block")),
        ("web", "https://example.com/a?b=\"c\"".to_owned()),
        ("mail", "mailto:someone@example.com".to_owned()),
        ("js", "javascript:alert(1)".to_owned()),
        ("data", "data:text/html,<script>".to_owned()),
        ("rel", "/p/other".to_owned()),
    ] {
        spans.push(Span {
            text: text.to_owned(),
            marks: Marks {
                link: Some(link),
                ..Marks::default()
            },
        });
    }
    let blocks = vec![Block {
        id: BlockId::new(),
        kind: BlockKind::Paragraph,
        indent: 0,
        spans,
    }];
    let links = TestLinks {
        pages: HashMap::from([(published, "/p/tok/abc".to_owned())]),
        files: HashMap::new(),
    };
    let html = page(&doc("t", &blocks, &[]), &links);
    let hrefs = check(&html);
    assert!(
        html.contains("<a href=\"/p/tok/abc\" class=\"page-link\">pub</a>"),
        "{html}"
    );
    assert!(html.contains("priv"));
    assert!(!html.contains(&private.to_string()) && !html.contains(&published.to_string()));
    assert!(html.contains(
        "<a href=\"https://example.com/a?b=&quot;c&quot;\" rel=\"noopener noreferrer nofollow ugc\">web</a>"
    ));
    assert!(html.contains("href=\"mailto:someone@example.com\""));
    for bad in ["javascript:", "data:", "/p/other"] {
        assert!(
            !hrefs.iter().any(|h| h.starts_with(bad)),
            "{bad} in {hrefs:?}"
        );
    }
}

#[test]
fn content_is_escaped_everywhere() {
    let evil = "<script>alert('x')</script>\"&";
    let blocks = vec![
        block(BlockKind::Paragraph, 0, evil),
        block(
            BlockKind::Code {
                language: evil.into(),
            },
            0,
            evil,
        ),
        block(BlockKind::Callout { icon: evil.into() }, 0, evil),
        block(
            BlockKind::Attachment {
                blob: "b".repeat(64),
                name: evil.into(),
                mime: "image/png".into(),
                size: 10,
            },
            0,
            evil,
        ),
    ];
    let props = [Property {
        name: evil.into(),
        value: PropertyValue::Text(evil.into()),
    }];
    let links = TestLinks {
        pages: HashMap::new(),
        files: HashMap::from([("b".repeat(64), "/p/tok/f/x".to_owned())]),
    };
    let html = page(&doc(evil, &blocks, &props), &links);
    check(&html);
    assert!(!html.contains("<script"));
    assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&quot;&amp;"));
}

#[test]
fn files_show_only_when_served() {
    let image = block(
        BlockKind::Attachment {
            blob: "a".repeat(64),
            name: "cat.png".into(),
            mime: "image/png".into(),
            size: 2048,
        },
        0,
        "A cat",
    );
    let file = block(
        BlockKind::Attachment {
            blob: "c".repeat(64),
            name: "notes.pdf".into(),
            mime: "application/pdf".into(),
            size: 1_500_000,
        },
        0,
        "",
    );
    let svg = block(
        BlockKind::Attachment {
            blob: "d".repeat(64),
            name: "x.svg".into(),
            mime: "image/svg+xml".into(),
            size: 10,
        },
        0,
        "",
    );
    let blocks = vec![image, file, svg];
    let links = TestLinks {
        pages: HashMap::new(),
        files: HashMap::from([
            ("a".repeat(64), "/p/t/k/file/1".to_owned()),
            ("d".repeat(64), "/p/t/k/file/3".to_owned()),
        ]),
    };
    let html = page(&doc("t", &blocks, &[]), &links);
    check(&html);
    assert!(
        html.contains("<img src=\"/p/t/k/file/1\" alt=\"A cat\""),
        "{html}"
    );
    assert!(html.contains("<figcaption>A cat</figcaption>"));
    assert!(!html.contains("notes.pdf"), "an unserved file is left out");
    assert!(
        html.contains("<a href=\"/p/t/k/file/3\" download>"),
        "SVG is a download"
    );
    assert!(html.contains("og:image\" content=\"https://pages.example.com/p/t/k/file/1\""));
    assert!(!html.contains(&"a".repeat(64)), "blob hashes stay private");
}

#[test]
fn journal_entries_show_their_day_and_mood() {
    let props = [
        Property {
            name: DATE.into(),
            value: PropertyValue::Date(Day::new(2026, 11, 26).unwrap()),
        },
        Property {
            name: MOOD.into(),
            value: PropertyValue::Number(4.0),
        },
        Property {
            name: "Weather".into(),
            value: PropertyValue::Text("Rain".into()),
        },
        Property {
            name: "Done".into(),
            value: PropertyValue::Checkbox(true),
        },
    ];
    let mut d = doc("Thursday", &[], &props);
    d.kind = PageKind::Journal;
    let html = page(&d, &none());
    check(&html);
    assert!(
        html.contains("<time datetime=\"2026-11-26\">Thursday, 26 November 2026</time>"),
        "{html}"
    );
    assert!(html.contains("mood mood-4"));
    assert!(html.contains("Mood: Good"));
    assert!(html.contains("<dt>Weather</dt><dd>Rain</dd>"));
    assert!(html.contains("<dt>Done</dt><dd>Yes</dd>"));
    assert!(!html.contains("<dt>Mood</dt>"));
}

#[test]
fn indexable_pages_have_a_canonical_link_and_others_say_noindex() {
    let mut d = doc("t", &[], &[]);
    d.indexable = true;
    let html = page(&d, &none());
    assert!(html.contains("<link rel=\"canonical\" href=\"https://pages.example.com/p/tok\">"));
    assert!(!html.contains("noindex"));
}

#[test]
fn trail_and_subpages_make_a_small_site() {
    let mut d = doc("Child", &[], &[]);
    d.trail = vec![Nav {
        title: "Root".into(),
        href: "/p/tok".into(),
    }];
    d.subpages = vec![Nav {
        title: "<b>".into(),
        href: "/p/tok/zz".into(),
    }];
    let html = page(&d, &none());
    check(&html);
    assert!(
        html.contains("<li><a href=\"/p/tok\">Root</a></li><li aria-current=\"page\">Child</li>")
    );
    assert!(html.contains("<li><a href=\"/p/tok/zz\">&lt;b&gt;</a></li>"));
    assert!(html.contains("og:site_name\" content=\"Root\""));
}

#[test]
fn the_gate_and_the_not_found_page_are_well_formed() {
    let form = password_form("/p/tok/unlock", "/assets/publish.css", true, false);
    check(&form);
    assert!(form.contains("That isn't the password."));
    check(&password_form(
        "/p/tok/unlock",
        "/assets/publish.css",
        false,
        true,
    ));
    check(&not_found("/assets/publish.css"));
}

fn any_kind() -> impl Strategy<Value = BlockKind> {
    prop_oneof![
        Just(BlockKind::Paragraph),
        (1i64..=3).prop_map(|l| BlockKind::Heading {
            level: HeadingLevel::clamped(l)
        }),
        Just(BlockKind::Bulleted),
        Just(BlockKind::Numbered),
        any::<bool>().prop_map(|checked| BlockKind::Todo { checked }),
        Just(BlockKind::Quote),
        prop_oneof![
            Just("rust".to_owned()),
            Just("sql".to_owned()),
            any::<String>()
        ]
        .prop_map(|language| BlockKind::Code { language }),
        Just(BlockKind::Divider),
        any::<String>().prop_map(|icon| BlockKind::Callout { icon }),
        Just(BlockKind::Toggle),
        any::<String>().prop_map(|query| BlockKind::Query { query }),
        (
            any::<String>(),
            prop_oneof![Just("image/png".to_owned()), any::<String>()],
            any::<u32>()
        )
            .prop_map(|(name, mime, size)| BlockKind::Attachment {
                blob: "e".repeat(64),
                name,
                mime,
                size,
            }),
    ]
}

fn any_marks(pages: Vec<PageId>) -> impl Strategy<Value = Marks> {
    let link = prop_oneof![
        Just(None),
        any::<String>().prop_map(Some),
        prop::sample::select(pages).prop_map(|p| Some(format!("drusite:page/{p}"))),
        Just(Some("javascript:alert(1)".to_owned())),
        Just(Some("https://example.com/\"><script>".to_owned())),
    ];
    (any::<[bool; 5]>(), link).prop_map(|(m, link)| Marks {
        bold: m[0],
        italic: m[1],
        underline: m[2],
        strikethrough: m[3],
        code: m[4],
        link,
    })
}

fn any_blocks(pages: Vec<PageId>) -> impl Strategy<Value = Vec<Block>> {
    prop::collection::vec(
        (
            any_kind(),
            0u8..=drusite_model::MAX_INDENT + 2,
            prop::collection::vec((any::<String>(), any_marks(pages)), 0..4),
        )
            .prop_map(|(kind, indent, spans)| Block {
                id: BlockId::new(),
                kind,
                indent,
                spans: spans
                    .into_iter()
                    .map(|(text, marks)| Span { text, marks })
                    .collect(),
            }),
        0..24,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever a page holds, the HTML is well formed, every link is to a published page, the
    /// web or email, and no ID from the notes is in it.
    #[test]
    fn any_page_renders_safely(
        title in any::<String>(),
        blocks in any_blocks(vec![PageId::from_uuid(uuid::Uuid::from_u128(1)), PageId::from_uuid(uuid::Uuid::from_u128(2))]),
        prop_name in any::<String>(),
        prop_text in any::<String>(),
        indexable in any::<bool>(),
    ) {
        let published = PageId::from_uuid(uuid::Uuid::from_u128(1));
        let links = TestLinks {
            pages: HashMap::from([(published, "/p/tok/pub".to_owned())]),
            files: HashMap::from([("e".repeat(64), "/p/tok/pub/file/f".to_owned())]),
        };
        let props = [Property { name: prop_name, value: PropertyValue::Text(prop_text) }];
        let mut d = doc(&title, &blocks, &props);
        d.indexable = indexable;
        let html = page(&d, &links);
        let hrefs = check(&html);
        for h in &hrefs {
            let ok = h.starts_with("/p/tok/") || h.starts_with("/assets/")
                || h.starts_with("https://") || h.starts_with("http://") || h.starts_with("mailto:");
            prop_assert!(ok, "href {h:?}");
        }
        for b in &blocks {
            prop_assert!(!html.contains(&b.id.to_string()));
        }
        prop_assert!(!html.contains("00000000-0000-0000-0000-000000000002"));
        prop_assert!(!html.contains("00000000-0000-0000-0000-000000000001"));
        prop_assert!(!html.contains(&"e".repeat(64)));
        prop_assert_eq!(html.contains("noindex"), !indexable);
    }

    #[test]
    fn highlighting_any_code_is_escaped(lang in prop_oneof![Just("rust"), Just("js"), Just("python"), Just("sql"), Just("haskell"), Just("css"), Just("none")], code in any::<String>()) {
        let html = super::highlight(lang, &code);
        let wrapped = format!("<!doctype html>\n<code>{html}</code>");
        check(&wrapped);
    }
}
