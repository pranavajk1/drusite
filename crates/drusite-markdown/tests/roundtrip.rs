// SPDX-License-Identifier: AGPL-3.0-or-later

//! Page → Markdown → page loses nothing, for random pages of every block kind, mark and
//! nesting, with text built to trip Markdown up. And Markdown → page → Markdown is a fixed
//! point, so a file the bridge reads back doesn't churn.

#![allow(clippy::unwrap_used)]

use drusite_markdown::{BlockRef, Frontmatter, MdBlock, Resolver, normalise_spans, read, write};
use drusite_model::{Block, BlockId, BlockKind, HeadingLevel, Marks, Span};
use proptest::prelude::*;
use uuid::Uuid;

/// Attachments live at `files/<blob>/<name>`.
struct Files;

impl Resolver for Files {
    fn attachment_out(&self, blob: &str, name: &str) -> String {
        format!("files/{blob}/{name}")
    }

    fn attachment_in(&self, dest: &str, _wiki: bool) -> Option<BlockKind> {
        let rest = dest.strip_prefix("files/")?;
        let (blob, name) = rest.split_once('/')?;
        Some(BlockKind::Attachment {
            blob: blob.to_owned(),
            name: name.to_owned(),
            mime: String::new(),
            size: 0,
        })
    }
}

fn text(hostile: bool) -> BoxedStrategy<String> {
    let word = if hostile {
        prop_oneof![
            4 => "[a-zA-Z]{1,8}",
            2 => "[\\\\`*_\\[\\]<>|~#!&^$(){}+=:.,;'\"/%@-]{1,3}",
            1 => "[0-9]{1,3}[.)]",
            1 => Just("&amp;".to_owned()),
            1 => Just("<b>".to_owned()),
            1 => Just("<!-- x -->".to_owned()),
            1 => Just("^abc123".to_owned()),
            1 => Just("[[Page]]".to_owned()),
            1 => Just("[!note]".to_owned()),
            1 => Just("==hi==".to_owned()),
            1 => Just("http://x.y/a_b".to_owned()),
            1 => "[éü日本🙂ß]{1,3}",
        ]
        .boxed()
    } else {
        "[a-zA-Z0-9]{1,8}".boxed()
    };
    let sep = prop_oneof![
        6 => Just(" "),
        1 => Just("  "),
        1 => Just("\t"),
        1 => Just("\n"),
    ];
    (
        prop::collection::vec((word, sep), 0..6),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(words, lead, trail)| {
            let mut s = String::new();
            for (i, (w, sep)) in words.iter().enumerate() {
                if i > 0 {
                    s.push_str(sep);
                }
                s.push_str(w);
            }
            if lead {
                s.insert(0, ' ');
            }
            if trail && !s.is_empty() {
                s.push(' ');
            }
            s
        })
        .boxed()
}

fn marks() -> impl Strategy<Value = Marks> {
    (
        any::<bool>(),
        any::<bool>(),
        prop::bool::weighted(0.15),
        prop::bool::weighted(0.15),
        prop::bool::weighted(0.15),
        prop::option::weighted(
            0.2,
            prop_oneof![
                Just("https://example.com/a".to_owned()),
                Just("https://example.com/a b?c=(d)".to_owned()),
                Just("Projects/Drusite.md".to_owned()),
            ],
        ),
    )
        .prop_map(
            |(bold, italic, underline, strikethrough, code, link)| Marks {
                bold,
                italic,
                underline,
                strikethrough,
                code,
                link,
            },
        )
}

fn spans() -> impl Strategy<Value = Vec<Span>> {
    prop::collection::vec((text(true), marks()), 0..4).prop_map(|v| {
        v.into_iter()
            .map(|(text, mut marks)| {
                // A code span can't hold a line break, and an empty one isn't there.
                let text = if marks.code {
                    text.replace('\n', " ")
                } else {
                    text
                };
                if marks.code && text.trim().is_empty() {
                    marks.code = false;
                }
                Span { text, marks }
            })
            .filter(|s| !s.text.is_empty())
            .collect()
    })
}

fn kind() -> impl Strategy<Value = BlockKind> {
    prop_oneof![
        4 => Just(BlockKind::Paragraph),
        1 => (1i64..=3).prop_map(|l| BlockKind::Heading { level: HeadingLevel::clamped(l) }),
        2 => Just(BlockKind::Bulleted),
        2 => Just(BlockKind::Numbered),
        2 => any::<bool>().prop_map(|checked| BlockKind::Todo { checked }),
        1 => Just(BlockKind::Quote),
        1 => prop_oneof![Just(""), Just("rust"), Just("js x")]
            .prop_map(|l| BlockKind::Code { language: l.to_owned() }),
        1 => Just(BlockKind::Divider),
        1 => prop_oneof![Just("💡"), Just("⚠️"), Just("🦀"), Just("")]
            .prop_map(|i| BlockKind::Callout { icon: i.to_owned() }),
        1 => Just(BlockKind::Toggle),
        1 => prop_oneof![
            Just(r#"{"filter":[],"sort":null}"#),
            Just("```\nnot json"),
        ]
        .prop_map(|q| BlockKind::Query { query: q.to_owned() }),
        1 => (any::<u128>(), prop::option::of("[a-z0-9]{8}")).prop_map(|(d, view)| {
            BlockKind::Database {
                database: drusite_model::PageId::from_uuid(uuid::Uuid::from_u128(d)),
                view,
            }
        }),
        1 => ("[0-9a-f]{64}", "[a-zA-Z0-9 ._-]{1,12}\\.png").prop_map(|(blob, name)| {
            BlockKind::Attachment { blob, name, mime: String::new(), size: 0 }
        }),
    ]
}

fn block() -> impl Strategy<Value = Block> {
    (any::<u128>(), kind(), spans(), 0u8..4, text(true)).prop_map(
        |(id, kind, spans, indent, code)| {
            let spans = match &kind {
                // What a page holds for these: code as one plain run, no text at all, or a plain
                // one-line caption.
                BlockKind::Code { .. } => {
                    if code.is_empty() {
                        Vec::new()
                    } else {
                        vec![Span::plain(code)]
                    }
                }
                BlockKind::Divider | BlockKind::Query { .. } | BlockKind::Database { .. } => {
                    Vec::new()
                }
                BlockKind::Attachment { .. } => {
                    let caption: String = spans.iter().map(|s| s.text.as_str()).collect();
                    let caption = caption.replace(['\n', '\t'], " ").trim().to_owned();
                    if caption.is_empty() {
                        Vec::new()
                    } else {
                        vec![Span::plain(caption)]
                    }
                }
                _ => spans,
            };
            Block {
                id: BlockId::from_uuid(Uuid::from_u128(id)),
                kind,
                indent,
                spans,
            }
        },
    )
}

/// A page as the editor allows: never more than one level deeper than the block before.
fn page() -> impl Strategy<Value = Vec<Block>> {
    prop::collection::vec(block(), 1..14).prop_map(|mut blocks| {
        let mut prev: Option<u8> = None;
        for (i, b) in blocks.iter_mut().enumerate() {
            b.indent = b.indent.min(prev.map_or(0, |p| p + 1));
            prev = Some(b.indent);
            // Every block's ID is its own.
            let n = b.id.as_uuid().as_u128();
            b.id = BlockId::from_uuid(Uuid::from_u128((n << 8) | i as u128));
        }
        blocks
    })
}

/// What a block means, for comparing.
fn meaning(b: &Block) -> (BlockId, BlockKind, u8, Vec<Span>) {
    (b.id, b.kind.clone(), b.indent, normalise_spans(&b.spans))
}

fn back(md: &MdBlock) -> (BlockId, BlockKind, u8, Vec<Span>) {
    let id = match md.id {
        Some(BlockRef::Block(id)) => id,
        // Compared as the nil ID, so the failure shows the whole page.
        _ => BlockId::from_uuid(Uuid::max()),
    };
    (id, md.kind.clone(), md.indent, normalise_spans(&md.spans))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, ..ProptestConfig::default() })]

    #[test]
    fn pages_survive_markdown(blocks in page()) {
        let md = write(&Frontmatter::default(), &blocks, &Files);
        let read = read(&md, &Files);
        let want: Vec<_> = blocks.iter().map(meaning).collect();
        let got: Vec<_> = read.blocks.iter().map(back).collect();
        prop_assert_eq!(got, want, "\n--- markdown ---\n{}", md);
    }

    #[test]
    fn markdown_written_is_a_fixed_point(blocks in page()) {
        let md = write(&Frontmatter::default(), &blocks, &Files);
        let again: Vec<Block> = read(&md, &Files)
            .blocks
            .iter()
            .map(|b| {
                let (id, kind, indent, _) = back(b);
                Block { id, kind, indent, spans: b.spans.clone() }
            })
            .collect();
        let md2 = write(&Frontmatter::default(), &again, &Files);
        prop_assert_eq!(md2, md);
    }
}

#[test]
fn ordinary_prose_stays_clean() {
    let id = |n: u128| BlockId::from_uuid(Uuid::from_u128(n));
    let blocks = vec![
        Block {
            id: id(1),
            kind: BlockKind::Heading {
                level: HeadingLevel::H1,
            },
            indent: 0,
            spans: vec![Span::plain("Trip to the coast")],
        },
        Block {
            id: id(2),
            kind: BlockKind::Paragraph,
            indent: 0,
            spans: vec![
                Span::plain("We left early, and the "),
                Span {
                    text: "fog".into(),
                    marks: Marks {
                        italic: true,
                        ..Marks::default()
                    },
                },
                Span::plain(" lifted by nine."),
            ],
        },
        Block {
            id: id(3),
            kind: BlockKind::Todo { checked: false },
            indent: 0,
            spans: vec![Span::plain("Book the ferry")],
        },
        Block {
            id: id(4),
            kind: BlockKind::Bulleted,
            indent: 1,
            spans: vec![Span::plain("Ask about bikes")],
        },
        Block {
            id: id(5),
            kind: BlockKind::Callout {
                icon: "💡".into()
            },
            indent: 0,
            spans: vec![Span::plain("Bring a jacket")],
        },
        Block {
            id: id(6),
            kind: BlockKind::Paragraph,
            indent: 1,
            spans: vec![Span::plain("Even in summer.")],
        },
    ];
    let md = write(&Frontmatter::default(), &blocks, &Files);
    let id = |n: u128| drusite_markdown::encode_block_id(id(n));
    assert_eq!(
        md,
        format!(
            "# Trip to the coast ^{}\n\nWe left early, and the *fog* lifted by nine. ^{}\n\n\
             - [ ] Book the ferry ^{}\n\n  - Ask about bikes ^{}\n\n\
             > [!tip] Bring a jacket ^{}\n>\n> Even in summer. ^{}\n",
            id(1),
            id(2),
            id(3),
            id(4),
            id(5),
            id(6)
        )
    );
}

#[test]
fn hand_written_obsidian_reads() {
    let note = "---\ntags: [journal]\nmood: 4\n---\n# 2026-11-25\n\nMet with [[Alex]] about the ==spike==. See [[Projects/Drusite#Roadmap|the roadmap]].\n\n> [!warning] Careful\n> Callouts are quotes with a marker.\n\n- [ ] follow up ^task1\n- [x] ~~old idea~~ dropped\n3. starts at three\n\n![[diagram.png]]\n\n| a | b |\n|:--|--:|\n| 1 | 2 |\n\nText ending in an escaped \\^ref1\n";
    let page = read(note, &Files);
    assert_eq!(page.front.properties.len(), 2);
    let kinds: Vec<&str> = page.blocks.iter().map(|b| b.kind.name()).collect();
    assert_eq!(
        kinds,
        [
            "heading",
            "paragraph",
            "callout",
            "paragraph",
            "todo",
            "todo",
            "numbered",
            "paragraph",
            "code",
            "paragraph"
        ]
    );
    let b = &page.blocks;
    assert_eq!(b[1].spans[1].marks.link.as_deref(), Some("Alex"));
    assert_eq!(b[1].spans[1].text, "Alex");
    assert_eq!(
        b[2].kind,
        BlockKind::Callout {
            icon: "⚠️".into()
        }
    );
    assert_eq!(b[2].text(), "Careful");
    assert_eq!(
        (b[3].indent, b[3].text().as_str()),
        (1, "Callouts are quotes with a marker.")
    );
    assert_eq!(b[4].id, Some(BlockRef::Other("task1".into())));
    assert_eq!(b[4].text(), "follow up");
    assert!(b[5].spans[0].marks.strikethrough);
    // An embed Files doesn't know stays a link.
    assert_eq!(b[7].spans[0].marks.link.as_deref(), Some("diagram.png"));
    assert_eq!(
        b[8].kind,
        BlockKind::Code {
            language: "table".into()
        }
    );
    assert!(b[8].text().starts_with("| a | b |"));
    assert_eq!(b[9].id, None);
    assert_eq!(b[9].text(), "Text ending in an escaped ^ref1");
}
