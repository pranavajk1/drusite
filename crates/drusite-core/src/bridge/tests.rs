// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used)]

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use drusite_model::{BlockKind, PageId, PropertyValue};

use crate::api::{Command, Edit, Response};
use crate::{Core, StoreKey};

struct Fixture {
    core: Core,
    folder: tempfile::TempDir,
    _data: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let core = Core::open(
        data.path(),
        &StoreKey::generate().unwrap(),
        Box::new(|_| {}),
    )
    .unwrap();
    Fixture {
        core,
        folder: tempfile::tempdir().unwrap(),
        _data: data,
    }
}

impl Fixture {
    fn root(&self) -> &Path {
        self.folder.path()
    }

    fn page(&mut self, parent: Option<PageId>, title: &str, text: &str) -> PageId {
        let Response::PageCreated { page } = self
            .core
            .handle(Command::CreatePage {
                parent,
                title: title.into(),
            })
            .unwrap()
        else {
            panic!()
        };
        self.type_into(page.id, text);
        page.id
    }

    fn type_into(&mut self, page: PageId, text: &str) {
        let Response::Opened { view } = self.core.handle(Command::OpenPage { page }).unwrap()
        else {
            panic!()
        };
        let last = view.blocks.last().unwrap();
        self.core
            .handle(Command::Edit {
                page,
                edits: vec![Edit::InsertText {
                    block: last.id,
                    pos: u32::try_from(last.text().encode_utf16().count()).unwrap(),
                    text: text.into(),
                }],
            })
            .unwrap();
        self.core.handle(Command::ClosePage { page }).unwrap();
    }

    fn texts(&mut self, page: PageId) -> Vec<String> {
        let Response::Opened { view } = self.core.handle(Command::OpenPage { page }).unwrap()
        else {
            panic!()
        };
        self.core.handle(Command::ClosePage { page }).unwrap();
        view.blocks.iter().map(|b| b.text()).collect()
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.root().join(path)).unwrap()
    }

    fn write(&self, path: &str, text: &str) {
        let full = self.root().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    fn sync(&mut self) -> super::Report {
        let report = self.core.bridge_sync().unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        report
    }

    fn titles(&mut self) -> Vec<(String, Option<String>)> {
        let Response::Pages { pages } = self.core.handle(Command::ListPages).unwrap() else {
            panic!()
        };
        let names: std::collections::HashMap<_, _> =
            pages.iter().map(|p| (p.id, p.title.clone())).collect();
        pages
            .iter()
            .map(|p| (p.title.clone(), p.parent.map(|q| names[&q].clone())))
            .collect()
    }
}

#[test]
fn pages_become_files_in_their_tree() {
    let mut f = fixture();
    let projects = f.page(None, "Projects", "All my projects");
    f.page(Some(projects), "Drusite: notes", "A notes app");
    let root = f.root().to_path_buf();
    let report = f.core.set_bridge_folder(Some(&root)).unwrap();
    assert_eq!(report.written, 2);
    let top = f.read("Projects.md");
    assert!(top.contains(&format!("drusite-id: {projects}")), "{top}");
    assert!(top.contains("All my projects ^"));
    let sub = f.read("Projects/Drusite- notes.md");
    // The title can't be the file's name, so the frontmatter keeps it.
    assert!(sub.contains("drusite-title: \"Drusite: notes\""), "{sub}");
    // Nothing changed, nothing written.
    assert_eq!(f.sync().written, 0);
}

#[test]
fn edits_in_the_file_and_the_app_both_land() {
    let mut f = fixture();
    let page = f.page(None, "Journal", "Walked to the lake");
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();

    // An editor changes the file: a word in the paragraph, a new to-do, a property.
    let file = f.read("Journal.md");
    let edited = file
        .replace("Walked to the lake", "Walked slowly to the lake")
        .replace("drusite-id:", "mood: 4\ndrusite-id:")
        + "\n- [ ] Buy bread\n";
    // Meanwhile, in the app, more is typed at the end of the same paragraph.
    f.type_into(page, " at dawn");
    f.write("Journal.md", &edited);
    let report = f.sync();
    assert_eq!(report.merged, 1);
    assert_eq!(
        f.texts(page),
        ["Walked slowly to the lake at dawn", "Buy bread"]
    );
    let Response::Pages { pages } = f.core.handle(Command::ListPages).unwrap() else {
        panic!()
    };
    assert_eq!(pages[0].properties[0].value, PropertyValue::Number(4.0));
    // The file now has both, and an ID for the new block.
    let back = f.read("Journal.md");
    assert!(
        back.contains("Walked slowly to the lake at dawn ^"),
        "{back}"
    );
    assert!(back.contains("- [ ] Buy bread ^"), "{back}");
    assert!(back.contains("mood: 4"));
    assert_eq!(f.sync(), super::Report::default());
}

#[test]
fn new_files_become_pages_where_their_folder_says() {
    let mut f = fixture();
    let projects = f.page(None, "Projects", "");
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();
    f.write("Projects/Garden.md", "# Beds\n\nTomatoes by the wall.\n");
    f.write("Recipes/Bread.md", "Flour, water, salt.\n");
    let report = f.sync();
    assert_eq!(
        report.created, 2,
        "Garden and Bread (and a page for Recipes, their folder)"
    );
    let titles = f.titles();
    assert!(titles.contains(&("Garden".into(), Some("Projects".into()))));
    assert!(titles.contains(&("Recipes".into(), None)));
    assert!(titles.contains(&("Bread".into(), Some("Recipes".into()))));
    // Written back with IDs, and nothing more to do.
    assert!(f.read("Projects/Garden.md").contains("drusite-id:"));
    assert!(f.read("Recipes.md").contains("drusite-id:"));
    assert_eq!(f.sync(), super::Report::default());
    let _ = projects;
}

#[test]
fn deleting_a_file_moves_its_page_to_the_trash_and_restoring_brings_it_back() {
    let mut f = fixture();
    let page = f.page(None, "Scratch", "Throwaway");
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();
    std::fs::remove_file(root.join("Scratch.md")).unwrap();
    assert_eq!(f.sync().trashed, 1);
    let Response::Trash { pages } = f.core.handle(Command::Trash).unwrap() else {
        panic!()
    };
    assert_eq!(pages[0].id, page);
    f.core.handle(Command::RestorePage { page }).unwrap();
    f.sync();
    assert!(f.read("Scratch.md").contains("Throwaway"));

    // Trashed in the app: the file goes.
    f.core.handle(Command::DeletePage { page }).unwrap();
    f.sync();
    assert!(!root.join("Scratch.md").exists());
}

#[test]
fn renames_and_moves_go_both_ways() {
    let mut f = fixture();
    let a = f.page(None, "Alpha", "a");
    f.page(None, "Beta", "b");
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();

    // Renamed and moved under Beta in the folder.
    std::fs::create_dir_all(root.join("Beta")).unwrap();
    std::fs::rename(root.join("Alpha.md"), root.join("Beta/Gamma.md")).unwrap();
    f.sync();
    assert!(f.titles().contains(&("Gamma".into(), Some("Beta".into()))));
    assert_eq!(f.texts(a), ["a"]);

    // Renamed in the app: the file follows.
    f.core
        .handle(Command::RenamePage {
            page: a,
            title: "Delta".into(),
        })
        .unwrap();
    f.sync();
    assert!(root.join("Beta/Delta.md").exists());
    assert!(!root.join("Beta/Gamma.md").exists());
    assert_eq!(f.sync(), super::Report::default());
}

#[test]
fn links_between_pages_are_paths_in_files() {
    let mut f = fixture();
    let target = f.page(None, "Target page", "here");
    let from = f.page(None, "From", "");
    let Response::Opened { view } = f.core.handle(Command::OpenPage { page: from }).unwrap() else {
        panic!()
    };
    let block = view.blocks[0].id;
    f.core
        .handle(Command::Edit {
            page: from,
            edits: vec![
                Edit::InsertText {
                    block,
                    pos: 0,
                    text: "see it".into(),
                },
                Edit::Format {
                    block,
                    start: 4,
                    end: 6,
                    mark: drusite_model::Mark::Link {
                        href: format!("{}{target}", super::PAGE_LINK),
                    },
                    on: true,
                },
            ],
        })
        .unwrap();
    f.core.handle(Command::ClosePage { page: from }).unwrap();
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();
    assert!(
        f.read("From.md").contains("see [it](<Target page.md>)"),
        "{}",
        f.read("From.md")
    );

    // A wikilink typed into a new file finds the page by title.
    f.write("Notes.md", "Read [[target page]] first.\n");
    f.sync();
    let Response::Pages { pages } = f.core.handle(Command::ListPages).unwrap() else {
        panic!()
    };
    let notes = pages.iter().find(|p| p.title == "Notes").unwrap().id;
    let Response::Opened { view } = f.core.handle(Command::OpenPage { page: notes }).unwrap()
    else {
        panic!()
    };
    let link = view.blocks[0]
        .spans
        .iter()
        .find_map(|s| s.marks.link.clone())
        .unwrap();
    assert_eq!(link, format!("{}{target}", super::PAGE_LINK));
}

#[test]
fn attachments_are_files_beside_the_notes() {
    let mut f = fixture();
    let page = f.page(None, "Trip", "");
    let Response::Attachment { kind } = f
        .core
        .handle(Command::AddAttachment {
            name: "beach.png".into(),
            mime: "image/png".into(),
            data: STANDARD.encode(b"png bytes"),
        })
        .unwrap()
    else {
        panic!()
    };
    f.core.handle(Command::OpenPage { page }).unwrap();
    f.core
        .handle(Command::Edit {
            page,
            edits: vec![Edit::InsertBlock { after: None, kind }],
        })
        .unwrap();
    f.core.handle(Command::ClosePage { page }).unwrap();
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();
    let md = f.read("Trip.md");
    let blob = blake3::hash(b"png bytes").to_hex().to_string();
    let path = format!("_attachments/{}/beach.png", &blob[..16]);
    assert!(md.contains(&format!("![](<{path}>)")), "{md}");
    assert_eq!(std::fs::read(root.join(&path)).unwrap(), b"png bytes");

    // A picture dropped next to the notes and embedded becomes an attachment.
    f.write("cat.jpg", "jpeg bytes");
    f.write("Pets.md", "![[cat.jpg]]\n");
    f.sync();
    let cat = blake3::hash(b"jpeg bytes").to_hex().to_string();
    assert!(f.core.attachment(&cat).unwrap().is_some());
    assert!(
        f.read("Pets.md")
            .contains(&format!("_attachments/{}/cat.jpg", &cat[..16]))
    );
    let Response::Pages { pages } = f.core.handle(Command::ListPages).unwrap() else {
        panic!()
    };
    let pets = pages.iter().find(|p| p.title == "Pets").unwrap().id;
    let Response::Opened { view } = f.core.handle(Command::OpenPage { page: pets }).unwrap() else {
        panic!()
    };
    assert!(
        matches!(&view.blocks[0].kind, BlockKind::Attachment { blob, mime, .. } if *blob == cat && mime == "image/jpeg")
    );
}

#[test]
fn a_folder_that_isnt_the_bridges_deletes_nothing() {
    let mut f = fixture();
    f.page(None, "Keep", "me");
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();
    // Replaced by an empty folder (or a drive that didn't mount).
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert!(f.core.bridge_sync().is_err());
    assert_eq!(f.titles().len(), 1);
}

#[test]
fn a_copied_file_is_a_new_page() {
    let mut f = fixture();
    let page = f.page(None, "Original", "text");
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();
    std::fs::copy(root.join("Original.md"), root.join("Copy.md")).unwrap();
    f.sync();
    let titles = f.titles();
    assert!(titles.contains(&("Original".into(), None)));
    assert!(titles.contains(&("Copy".into(), None)));
    assert_eq!(f.texts(page), ["text"]);
    assert_eq!(f.sync(), super::Report::default());
}

/// The watcher runs a round when a file changes and when a page does.
#[cfg(feature = "watch")]
#[test]
fn the_watcher_keeps_both_sides_in_step() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let mut f = fixture();
    let page = f.page(None, "Live", "first");
    let root = f.root().to_path_buf();
    f.core.set_bridge_folder(Some(&root)).unwrap();
    let Fixture {
        core,
        folder,
        _data,
    } = f;
    let core = Arc::new(Mutex::new(core));
    let watch = super::watch::watch(Arc::clone(&core), |_: &_| {}).unwrap();
    let wait = |what: &str, check: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !check() {
            assert!(start.elapsed() < Duration::from_secs(20), "never: {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let file = folder.path().join("Live.md");
    let text = std::fs::read_to_string(&file)
        .unwrap()
        .replace("first", "first, edited on disk");
    std::fs::write(&file, text).unwrap();
    let page_text = || {
        let mut c = core.lock().unwrap();
        let Response::Opened { view } = c.handle(Command::OpenPage { page }).unwrap() else {
            panic!()
        };
        c.handle(Command::ClosePage { page }).unwrap();
        view.blocks[0].text()
    };
    wait("the file's edit reaches the page", &|| {
        page_text() == "first, edited on disk"
    });

    {
        let mut c = core.lock().unwrap();
        let Response::Opened { view } = c.handle(Command::OpenPage { page }).unwrap() else {
            panic!()
        };
        c.handle(Command::Edit {
            page,
            edits: vec![Edit::InsertText {
                block: view.blocks[0].id,
                pos: 0,
                text: "App: ".into(),
            }],
        })
        .unwrap();
    }
    wait("the app's edit reaches the file", &|| {
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("App: first, edited on disk")
    });
    drop(watch);
}
