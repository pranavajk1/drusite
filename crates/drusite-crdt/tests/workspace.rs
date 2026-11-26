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
fn deleting_a_page_takes_its_subpages_out_of_the_tree() {
    let mut ws = Workspace::create().unwrap();
    let a = ws.create_page(None, "A").unwrap().id();
    ws.create_page(Some(a), "A child").unwrap();
    ws.create_page(None, "B").unwrap();
    ws.delete_page(a).unwrap();
    assert_eq!(outline(&ws), "B");
    assert!(matches!(ws.title(a), Err(Error::UnknownPage(_))));
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
