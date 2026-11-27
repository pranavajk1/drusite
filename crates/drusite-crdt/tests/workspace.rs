// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workspace document: the page tree and titles, on one device and merged from two.

// Test helpers panic on failure by design.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use drusite_crdt::{Error, Workspace};

fn fork(ws: &Workspace) -> Workspace {
    Workspace::load(&ws.export_snapshot().unwrap()).unwrap()
}

fn sync(a: &mut Workspace, b: &mut Workspace) {
    b.import(&a.export_updates(&b.version()).unwrap()).unwrap();
    a.import(&b.export_updates(&a.version()).unwrap()).unwrap();
}

/// The tree as `title(child, child) title`, for comparing.
fn outline(ws: &Workspace) -> String {
    fn walk(nodes: &[drusite_crdt::PageNode], id: drusite_model::PageId) -> String {
        let node = nodes.iter().find(|n| n.id == id).unwrap();
        if node.children.is_empty() {
            node.title.clone()
        } else {
            let kids: Vec<String> = node.children.iter().map(|&c| walk(nodes, c)).collect();
            format!("{}({})", node.title, kids.join(", "))
        }
    }
    let nodes = ws.pages().unwrap();
    ws.roots()
        .into_iter()
        .map(|r| walk(&nodes, r))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn pages_nest_move_and_reload() {
    let mut ws = Workspace::create().unwrap();
    let journal = ws.create_page(None, "Journal").unwrap().id();
    let day = ws.create_page(Some(journal), "2026-11-25").unwrap().id();
    let projects = ws.create_page(None, "Projects").unwrap().id();
    let drusite = ws.create_page(Some(projects), "Drusite").unwrap().id();
    assert_eq!(outline(&ws), "Journal(2026-11-25) Projects(Drusite)");

    ws.move_page(drusite, Some(journal), 0).unwrap();
    assert_eq!(outline(&ws), "Journal(Drusite, 2026-11-25) Projects");
    ws.move_page(projects, None, 0).unwrap();
    assert_eq!(outline(&ws), "Projects Journal(Drusite, 2026-11-25)");
    ws.move_page(day, None, 99).unwrap();
    assert_eq!(outline(&ws), "Projects Journal(Drusite) 2026-11-25");

    let loaded = fork(&ws);
    assert_eq!(outline(&loaded), outline(&ws));
    assert_eq!(loaded.id(), ws.id());
}

#[test]
fn a_page_cannot_move_under_itself() {
    let mut ws = Workspace::create().unwrap();
    let a = ws.create_page(None, "A").unwrap().id();
    let b = ws.create_page(Some(a), "B").unwrap().id();
    let c = ws.create_page(Some(b), "C").unwrap().id();
    assert!(matches!(ws.move_page(a, Some(c), 0), Err(Error::Cycle)));
    assert!(matches!(ws.move_page(a, Some(a), 0), Err(Error::Cycle)));
    assert_eq!(outline(&ws), "A(B(C))");
}

#[test]
fn concurrent_moves_that_would_make_a_cycle_still_converge() {
    let mut a = Workspace::create().unwrap();
    let x = a.create_page(None, "X").unwrap().id();
    let y = a.create_page(None, "Y").unwrap().id();
    let mut b = fork(&a);
    a.move_page(x, Some(y), 0).unwrap();
    b.move_page(y, Some(x), 0).unwrap();
    sync(&mut a, &mut b);
    assert_eq!(outline(&a), outline(&b));
    assert_eq!(
        a.pages().unwrap().len(),
        2,
        "both pages are still in the tree"
    );
}

#[test]
fn concurrent_title_edits_merge() {
    let mut a = Workspace::create().unwrap();
    let page = a.create_page(None, "Meeting notes").unwrap().id();
    let mut b = fork(&a);
    a.set_title(page, "Weekly meeting notes").unwrap();
    b.set_title(page, "Meeting notes, Tuesday").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(a.title(page).unwrap(), "Weekly meeting notes, Tuesday");
    assert_eq!(b.title(page).unwrap(), a.title(page).unwrap());
}

#[test]
fn deleting_a_page_forever_takes_its_subpages_with_it() {
    let mut ws = Workspace::create().unwrap();
    let a = ws.create_page(None, "A").unwrap().id();
    let child = ws.create_page(Some(a), "A child").unwrap().id();
    ws.create_page(None, "B").unwrap();
    let mut gone = ws.delete_page(a).unwrap();
    gone.sort();
    let mut expected = vec![a, child];
    expected.sort();
    assert_eq!(gone, expected);
    assert_eq!(outline(&ws), "B");
    assert!(matches!(ws.title(a), Err(Error::UnknownPage(_))));
}

mod trash {
    use super::*;

    #[test]
    fn a_trashed_page_is_hidden_with_its_subpages_and_restores_in_place() {
        let mut ws = Workspace::create().unwrap();
        let a = ws.create_page(None, "A").unwrap().id();
        let child = ws.create_page(Some(a), "A child").unwrap().id();
        ws.create_page(None, "B").unwrap();
        ws.trash_page(a, 1_000).unwrap();
        assert_eq!(outline(&ws), "B");
        assert!(ws.is_trashed(child).unwrap());
        let trash = ws.trash().unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].id, a);
        assert_eq!(trash[0].title, "A");
        assert_eq!(trash[0].trashed_at, 1_000);
        assert_eq!(trash[0].subpages, 1);
        // Its title and document are still there.
        assert_eq!(ws.title(child).unwrap(), "A child");

        ws.restore_page(a).unwrap();
        assert_eq!(outline(&ws), "A(A child) B");
        assert!(ws.trash().unwrap().is_empty());
        assert!(!ws.is_trashed(child).unwrap());
    }

    #[test]
    fn a_subpage_restored_from_a_trashed_parent_comes_back_at_the_top() {
        let mut ws = Workspace::create().unwrap();
        let a = ws.create_page(None, "A").unwrap().id();
        let child = ws.create_page(Some(a), "C").unwrap().id();
        ws.trash_page(child, 1).unwrap();
        ws.trash_page(a, 2).unwrap();
        let listed: Vec<_> = ws.trash().unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(listed, [a, child], "most recent first");
        ws.restore_page(child).unwrap();
        assert_eq!(outline(&ws), "C");
        assert!(matches!(
            ws.move_page(child, Some(a), 0),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn edits_and_moves_on_another_device_survive_the_trash() {
        let mut a = Workspace::create().unwrap();
        let p = a.create_page(None, "Draft").unwrap().id();
        let q = a.create_page(None, "Folder").unwrap().id();
        let mut b = fork(&a);
        a.trash_page(p, 5).unwrap();
        b.set_title(p, "Draft two").unwrap();
        b.move_page(p, Some(q), 0).unwrap();
        sync(&mut a, &mut b);
        assert_eq!(outline(&a), "Folder");
        assert_eq!(outline(&b), outline(&a));
        a.restore_page(p).unwrap();
        assert_eq!(outline(&a), "Folder(Draft two)");
    }
}

#[test]
fn another_workspaces_changes_are_refused() {
    let mut a = Workspace::create().unwrap();
    let b = Workspace::create().unwrap();
    assert!(matches!(
        a.import(&b.export_snapshot().unwrap()),
        Err(Error::WrongDocument)
    ));
    assert!(a.is_poisoned());
}

#[test]
fn titles_are_one_line() {
    let mut ws = Workspace::create().unwrap();
    let page = ws.create_page(None, "two\nlines").unwrap().id();
    assert_eq!(ws.title(page).unwrap(), "two lines");
}

mod journal {
    use drusite_crdt::{Page, SettingValue, Workspace};
    use drusite_model::{
        Block, BlockKind, HeadingLevel, Marks, PageKind, Property, PropertyValue, Span,
    };

    use super::{fork, sync};

    fn mood(n: f64) -> PropertyValue {
        PropertyValue::Number(n)
    }

    #[test]
    fn kinds_and_properties_survive_a_reload() {
        let mut ws = Workspace::create().unwrap();
        let day = PropertyValue::Date("2026-11-26".parse().unwrap());
        let page = ws
            .create_page_of(
                None,
                "Thursday",
                PageKind::Journal,
                &[Property {
                    name: "Date".into(),
                    value: day.clone(),
                }],
            )
            .unwrap();
        ws.set_property(page.id(), "Mood", Some(&mood(4.0)))
            .unwrap();
        ws.set_property(
            page.id(),
            "Note",
            Some(&PropertyValue::Text("2026-11-26".into())),
        )
        .unwrap();
        let ws = Workspace::load(&ws.export_snapshot().unwrap()).unwrap();
        assert_eq!(ws.kind(page.id()).unwrap(), PageKind::Journal);
        let props = ws.properties(page.id()).unwrap();
        let names: Vec<_> = props.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Date", "Mood", "Note"]);
        assert_eq!(props[0].value, day);
        // A text that looks like a date stays text.
        assert_eq!(props[2].value, PropertyValue::Text("2026-11-26".into()));
        let node = &ws.pages().unwrap()[0];
        assert_eq!(node.kind, PageKind::Journal);
        assert_eq!(node.properties, props);
    }

    #[test]
    fn concurrent_property_changes_converge() {
        let mut mac = Workspace::create().unwrap();
        let page = mac.create_page(None, "Day").unwrap().id();
        let mut phone = fork(&mac);
        mac.set_property(page, "Mood", Some(&mood(2.0))).unwrap();
        phone.set_property(page, "Mood", Some(&mood(5.0))).unwrap();
        phone
            .set_property(page, "Weather", Some(&PropertyValue::Text("rain".into())))
            .unwrap();
        mac.set_property(page, "Slept", Some(&PropertyValue::Checkbox(true)))
            .unwrap();
        sync(&mut mac, &mut phone);
        let a = mac.properties(page).unwrap();
        assert_eq!(a, phone.properties(page).unwrap());
        // Different properties both stay; the same one has one winner on both devices.
        assert_eq!(a.len(), 3);
        // Removing one on one device removes it on both.
        phone.set_property(page, "Weather", None).unwrap();
        sync(&mut mac, &mut phone);
        assert!(
            !mac.properties(page)
                .unwrap()
                .iter()
                .any(|p| p.name == "Weather")
        );
    }

    #[test]
    fn property_names_are_checked() {
        let mut ws = Workspace::create().unwrap();
        let page = ws.create_page(None, "").unwrap().id();
        assert!(ws.set_property(page, "  ", Some(&mood(1.0))).is_err());
        assert!(ws.set_property(page, "a\nb", Some(&mood(1.0))).is_err());
    }

    #[test]
    fn settings_sync_with_the_workspace() {
        let mut mac = Workspace::create().unwrap();
        let mut phone = fork(&mac);
        mac.set_setting("journal.weekStart", Some(SettingValue::Int(1)))
            .unwrap();
        phone
            .set_setting(
                "journal.titleFormat",
                Some(SettingValue::Text("long".into())),
            )
            .unwrap();
        sync(&mut mac, &mut phone);
        let s = phone.settings().unwrap();
        assert_eq!(s, mac.settings().unwrap());
        assert_eq!(
            s,
            [
                (
                    "journal.titleFormat".to_owned(),
                    SettingValue::Text("long".into())
                ),
                ("journal.weekStart".to_owned(), SettingValue::Int(1)),
            ]
        );
        mac.set_setting("journal.weekStart", None).unwrap();
        sync(&mut mac, &mut phone);
        assert_eq!(phone.settings().unwrap().len(), 1);
    }

    #[test]
    fn a_page_fills_from_a_template() {
        let template = [
            Block {
                id: drusite_model::BlockId::new(),
                kind: BlockKind::Heading {
                    level: HeadingLevel::H2,
                },
                indent: 0,
                spans: vec![
                    Span::plain("Notes for "),
                    Span {
                        text: "{{date}}".into(),
                        marks: Marks {
                            bold: true,
                            ..Marks::default()
                        },
                    },
                ],
            },
            Block {
                id: drusite_model::BlockId::new(),
                kind: BlockKind::Todo { checked: false },
                indent: 1,
                spans: vec![Span::plain("Walk 🐕")],
            },
        ];
        let mut page = Page::create().unwrap();
        page.fill_from(&template, |t| t.replace("{{date}}", "2026-11-26"))
            .unwrap();
        let blocks = Page::load(&page.export_snapshot().unwrap())
            .unwrap()
            .blocks()
            .unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, template[0].kind);
        assert_eq!(blocks[0].text(), "Notes for 2026-11-26");
        assert!(blocks[0].spans.last().unwrap().marks.bold);
        assert_eq!(blocks[0].spans.last().unwrap().text, "2026-11-26");
        assert_eq!(blocks[1].text(), "Walk 🐕");
        assert_eq!(blocks[1].indent, 1);
        assert!(template.iter().all(|t| blocks.iter().all(|b| b.id != t.id)));
    }

    #[test]
    fn a_query_block_keeps_its_query() {
        let mut page = Page::create().unwrap();
        let first = page.order()[0];
        let kind = BlockKind::Query {
            query: r#"{"kind":"journal"}"#.into(),
        };
        page.set_kind(first, &kind).unwrap();
        let page = Page::load(&page.export_snapshot().unwrap()).unwrap();
        assert_eq!(page.blocks().unwrap()[0].kind, kind);
    }
}
