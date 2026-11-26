// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Core API end to end: commands in, responses and events out, everything saved.

// Test helpers panic on failure by design.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::mpsc;

use drusite_core::api::{Command, Edit, ErrorCode, Event, PagePatch, PageView, Response};
use drusite_core::{Core, StoreKey};
use drusite_model::{Block, BlockId, BlockKind, HeadingLevel, Mark, PageId};

fn core(dir: &Path, key: &StoreKey) -> (Core, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let core = Core::open(
        dir,
        key,
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
    )
    .unwrap();
    (core, rx)
}

fn create(core: &mut Core, title: &str) -> PageId {
    match core
        .handle(Command::CreatePage {
            parent: None,
            title: title.into(),
        })
        .unwrap()
    {
        Response::PageCreated { page } => page.id,
        other => panic!("{other:?}"),
    }
}

fn open(core: &mut Core, page: PageId) -> PageView {
    match core.handle(Command::OpenPage { page }).unwrap() {
        Response::Opened { view } => view,
        other => panic!("{other:?}"),
    }
}

fn edit(core: &mut Core, page: PageId, edits: Vec<Edit>) -> drusite_core::api::EditOutcome {
    match core.handle(Command::Edit { page, edits }).unwrap() {
        Response::Edited { outcome } => outcome,
        other => panic!("{other:?}"),
    }
}

fn texts(blocks: &[Block]) -> Vec<String> {
    blocks.iter().map(Block::text).collect()
}

#[test]
fn notes_written_through_the_api_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let key = StoreKey::generate().unwrap();
    let page = {
        let (mut core, events) = core(dir.path(), &key);
        assert_eq!(
            core.handle(Command::ListPages).unwrap(),
            Response::Pages { pages: vec![] }
        );
        let page = create(&mut core, "Journal");
        assert!(matches!(events.try_recv(), Ok(Event::PagesChanged { pages }) if pages.len() == 1));
        let view = open(&mut core, page);
        let first = view.blocks[0].id;
        let out = edit(
            &mut core,
            page,
            vec![
                Edit::InsertText {
                    block: first,
                    pos: 0,
                    text: "Today I wrote notes".into(),
                },
                Edit::Split {
                    block: first,
                    pos: 5,
                    kind: BlockKind::Paragraph,
                },
                Edit::SetKind {
                    block: first,
                    kind: BlockKind::Heading {
                        level: HeadingLevel::H1,
                    },
                },
            ],
        );
        assert_eq!(out.created.len(), 1);
        let second = out.created[0];
        edit(
            &mut core,
            page,
            vec![Edit::Format {
                block: second,
                start: 3,
                end: 5,
                mark: Mark::Bold,
                on: true,
            }],
        );
        core.handle(Command::RenamePage {
            page,
            title: "Journal, day one".into(),
        })
        .unwrap();
        page
    };

    let (mut core, _) = core(dir.path(), &key);
    let Response::Pages { pages } = core.handle(Command::ListPages).unwrap() else {
        panic!()
    };
    assert_eq!(pages[0].title, "Journal, day one");
    let view = open(&mut core, page);
    assert_eq!(texts(&view.blocks), ["Today", " I wrote notes"]);
    assert_eq!(
        view.blocks[0].kind,
        BlockKind::Heading {
            level: HeadingLevel::H1
        }
    );
    assert!(
        view.blocks[1]
            .spans
            .iter()
            .any(|s| s.marks.bold && s.text == "wr")
    );
}

#[test]
fn commands_arrive_as_the_generated_typescript_types_describe() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _) = core(dir.path(), &StoreKey::generate().unwrap());
    let page = create(&mut core, "x");
    let view = open(&mut core, page);
    let block = view.blocks[0].id;
    let json = format!(
        r#"{{"type":"edit","page":"{page}","edits":[
            {{"type":"insertText","block":"{block}","pos":0,"text":"hi 🙂"}},
            {{"type":"setKind","block":"{block}","kind":{{"type":"heading","level":"h2"}}}},
            {{"type":"format","block":"{block}","start":0,"end":2,"mark":{{"type":"link","href":"https://example.com"}},"on":true}}
        ]}}"#
    );
    let command: Command = serde_json::from_str(&json).unwrap();
    let response = core.handle(command).unwrap();
    let out = serde_json::to_value(&response).unwrap();
    assert_eq!(out["type"], "edited");
    let placed = &out["outcome"]["patch"]["upserted"][0];
    assert_eq!(placed["after"], serde_json::Value::Null);
    assert_eq!(
        placed["block"]["kind"],
        serde_json::json!({"type": "heading", "level": "h2"})
    );
    assert_eq!(
        placed["block"]["spans"][0]["marks"]["link"],
        "https://example.com"
    );
}

#[test]
fn a_bad_edit_in_a_batch_keeps_the_ones_before_it_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let key = StoreKey::generate().unwrap();
    let (mut core, events) = core(dir.path(), &key);
    let page = create(&mut core, "x");
    let block = open(&mut core, page).blocks[0].id;
    let _ = events.try_iter().count();
    let err = core
        .handle(Command::Edit {
            page,
            edits: vec![
                Edit::InsertText {
                    block,
                    pos: 0,
                    text: "kept".into(),
                },
                Edit::InsertText {
                    block,
                    pos: 99,
                    text: "nope".into(),
                },
            ],
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidPosition);
    let Ok(Event::PageChanged { patch, .. }) = events.try_recv() else {
        panic!("no PageChanged event")
    };
    assert_eq!(patch.upserted[0].block.text(), "kept");
    drop(core);
    let (mut core, _) = self::core(dir.path(), &key);
    assert_eq!(texts(&open(&mut core, page).blocks), ["kept"]);
}

#[test]
fn edits_need_an_open_page_and_a_page_in_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _) = core(dir.path(), &StoreKey::generate().unwrap());
    let page = create(&mut core, "x");
    let err = core
        .handle(Command::Edit {
            page,
            edits: vec![],
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PageNotOpen);
    let err = core
        .handle(Command::OpenPage {
            page: PageId::new(),
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownPage);
    let child = match core
        .handle(Command::CreatePage {
            parent: Some(page),
            title: "child".into(),
        })
        .unwrap()
    {
        Response::PageCreated { page } => page.id,
        other => panic!("{other:?}"),
    };
    let err = core
        .handle(Command::MovePage {
            page,
            parent: Some(child),
            index: 0,
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Cycle);
}

/// What the editor does with patches: remove, then place each upserted block after its `after`.
fn apply_patch(shown: &mut Vec<Block>, patch: &PagePatch) {
    if let Some(order) = &patch.order {
        let mut by_id: std::collections::HashMap<BlockId, Block> =
            shown.drain(..).map(|b| (b.id, b)).collect();
        for p in &patch.upserted {
            by_id.insert(p.block.id, p.block.clone());
        }
        *shown = order.iter().map(|id| by_id.remove(id).unwrap()).collect();
        return;
    }
    shown.retain(|b| !patch.removed.contains(&b.id));
    for placed in &patch.upserted {
        shown.retain(|b| b.id != placed.block.id);
        let at = match placed.after {
            None => 0,
            Some(after) => {
                shown
                    .iter()
                    .position(|b| b.id == after)
                    .expect("`after` block is shown")
                    + 1
            }
        };
        shown.insert(at, placed.block.clone());
    }
}

#[test]
fn patches_keep_the_editor_in_step_with_the_core() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _) = core(dir.path(), &StoreKey::generate().unwrap());
    let page = create(&mut core, "x");
    let mut shown = open(&mut core, page).blocks;
    let mut rng = fastrand::Rng::with_seed(11);
    for step in 0..2000 {
        let ids: Vec<BlockId> = shown.iter().map(|b| b.id).collect();
        let block = ids[rng.usize(..ids.len())];
        let len = shown
            .iter()
            .find(|b| b.id == block)
            .unwrap()
            .text()
            .encode_utf16()
            .count() as u32;
        let pos = rng.u32(..=len);
        let other = ids[rng.usize(..ids.len())];
        let one = match rng.u8(..10) {
            0..=3 => Edit::InsertText {
                block,
                pos,
                text: ["a", "é", "日本", " "][rng.usize(..4)].into(),
            },
            4 => Edit::DeleteText {
                block,
                pos: pos.saturating_sub(1),
                len: u32::from(pos > 0),
            },
            5 => Edit::Split {
                block,
                pos,
                kind: BlockKind::Paragraph,
            },
            6 => Edit::Join { block },
            7 => Edit::MoveBlock {
                block,
                after: if rng.bool() { Some(other) } else { None },
            },
            8 => Edit::SetIndent {
                block,
                indent: rng.u8(..3),
            },
            _ => {
                if ids.len() > 1 {
                    Edit::DeleteBlock { block }
                } else {
                    Edit::InsertBlock {
                        after: Some(block),
                        kind: BlockKind::Todo { checked: false },
                    }
                }
            }
        };
        let Ok(Response::Edited { outcome }) = core.handle(Command::Edit {
            page,
            edits: vec![one.clone()],
        }) else {
            continue; // e.g. a delete that lands inside an emoji: refused, nothing changed.
        };
        apply_patch(&mut shown, &outcome.patch);
        let truth = open(&mut core, page).blocks;
        assert_eq!(
            shown, truth,
            "step {step}: after {one:?} the editor and the core disagree"
        );
    }
}

/// Pasting over a page's only block, as the editor sends it: the block goes, the pasted ones
/// come in at the top. The page then holds just what was pasted, after a reopen too.
#[test]
fn replacing_the_only_block_leaves_just_the_new_ones() {
    let dir = tempfile::tempdir().unwrap();
    let key = StoreKey::generate().unwrap();
    let (mut core, _) = core(dir.path(), &key);
    let page = create(&mut core, "Paste");
    let only = open(&mut core, page).blocks[0].id;
    let out = edit(
        &mut core,
        page,
        vec![
            Edit::DeleteBlock { block: only },
            Edit::InsertBlock {
                after: None,
                kind: BlockKind::Heading {
                    level: HeadingLevel::H1,
                },
            },
        ],
    );
    let heading = out.created[0];
    let out = edit(
        &mut core,
        page,
        vec![
            Edit::InsertBlock {
                after: Some(heading),
                kind: BlockKind::Paragraph,
            },
            Edit::InsertText {
                block: heading,
                pos: 0,
                text: "Title".into(),
            },
        ],
    );
    edit(
        &mut core,
        page,
        vec![Edit::InsertText {
            block: out.created[0],
            pos: 0,
            text: "Body".into(),
        }],
    );
    let view = open(&mut core, page);
    assert_eq!(texts(&view.blocks), ["Title", "Body"]);
    assert_eq!(
        view.blocks[0].kind,
        BlockKind::Heading {
            level: HeadingLevel::H1
        }
    );
    drop(core);
    let (mut core, _) = self::core(dir.path(), &key);
    assert_eq!(texts(&open(&mut core, page).blocks), ["Title", "Body"]);
}

/// Two views of one page (an editor still closing while a new one opens it): the first close
/// leaves the page open for the other.
#[test]
fn a_page_stays_open_until_every_view_closes_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _) = core(dir.path(), &StoreKey::generate().unwrap());
    let page = create(&mut core, "Twice");
    let block = open(&mut core, page).blocks[0].id;
    open(&mut core, page);
    core.handle(Command::ClosePage { page }).unwrap();
    let typed = vec![Edit::InsertText {
        block,
        pos: 0,
        text: "still open".into(),
    }];
    edit(&mut core, page, typed.clone());
    core.handle(Command::ClosePage { page }).unwrap();
    let err = core
        .handle(Command::Edit { page, edits: typed })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PageNotOpen);
}
