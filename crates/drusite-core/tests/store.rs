// SPDX-License-Identifier: AGPL-3.0-or-later

//! The encrypted local store: what goes in comes back out after a restart, the file on disk is
//! ciphertext, and a process killed in the middle of writing loses nothing it reported saved.

// Test helpers panic on failure by design.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use drusite_core::store::{COMPACT_AFTER_UPDATES, DocKind, Store};
use drusite_core::{Error, StoreKey};
use drusite_crdt::Workspace;
use drusite_model::{BlockKind, PageId};

fn texts(page: &drusite_crdt::Page) -> Vec<String> {
    page.blocks().unwrap().iter().map(|b| b.text()).collect()
}

#[test]
fn notes_survive_closing_and_reopening_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.drusite");
    let key = StoreKey::generate().unwrap();

    let (ws_id, page_id) = {
        let mut store = Store::open(&path, &key).unwrap();
        let mut ws = Workspace::create().unwrap();
        let mut ws_at = store.create_workspace(&ws).unwrap();
        let mut page = ws.create_page(None, "Today").unwrap();
        let mut at = store.create_page(&page).unwrap();
        store.save_workspace(&ws, &mut ws_at).unwrap();
        let first = page.order()[0];
        page.insert_text(first, 0, "Notes written on the Mac")
            .unwrap();
        store.save_page(&page, &mut at).unwrap();
        let (second, _) = page
            .split(
                first,
                5,
                BlockKind::Heading {
                    level: drusite_model::HeadingLevel::H2,
                },
            )
            .unwrap();
        store.save_page(&page, &mut at).unwrap();
        page.set_kind(second, &BlockKind::Quote).unwrap();
        store.save_page(&page, &mut at).unwrap();
        ws.set_title(page.id(), "Today, renamed").unwrap();
        store.save_workspace(&ws, &mut ws_at).unwrap();
        (ws.id(), page.id())
    };

    let store = Store::open(&path, &key).unwrap();
    assert_eq!(store.workspace_id().unwrap(), Some(ws_id));
    let (ws, _) = store.load_workspace(ws_id).unwrap();
    assert_eq!(ws.title(page_id).unwrap(), "Today, renamed");
    let (page, _) = store.load_page(page_id).unwrap();
    assert_eq!(texts(&page), ["Notes", " written on the Mac"]);
    assert_eq!(page.blocks().unwrap()[1].kind, BlockKind::Quote);
    assert!(store.integrity_problems().unwrap().is_empty());
}

#[test]
fn the_file_on_disk_is_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.drusite");
    let key = StoreKey::generate().unwrap();
    let secret = "a very recognisable sentence about nothing in particular";
    {
        let mut store = Store::open(&path, &key).unwrap();
        let mut ws = Workspace::create().unwrap();
        store.create_workspace(&ws).unwrap();
        let mut page = ws.create_page(None, "Secret").unwrap();
        let mut at = store.create_page(&page).unwrap();
        let first = page.order()[0];
        page.insert_text(first, 0, secret).unwrap();
        store.save_page(&page, &mut at).unwrap();
    }
    let mut on_disk = std::fs::read(&path).unwrap();
    // The WAL holds recent writes until a checkpoint; it must be ciphertext too.
    if let Ok(wal) = std::fs::read(dir.path().join("workspace.drusite-wal")) {
        on_disk.extend(wal);
    }
    assert!(
        !on_disk.starts_with(b"SQLite format 3\0"),
        "plain SQLite header"
    );
    let needle = &secret.as_bytes()[..24];
    assert!(
        !on_disk.windows(needle.len()).any(|w| w == needle),
        "plaintext on disk"
    );
}

#[test]
fn the_wrong_key_does_not_open_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.drusite");
    let key = StoreKey::generate().unwrap();
    {
        let mut store = Store::open(&path, &key).unwrap();
        store
            .create_workspace(&Workspace::create().unwrap())
            .unwrap();
    }
    let other = StoreKey::generate().unwrap();
    assert!(matches!(Store::open(&path, &other), Err(Error::WrongKey)));
    // And the right key still does.
    assert!(
        Store::open(&path, &key)
            .unwrap()
            .workspace_id()
            .unwrap()
            .is_some()
    );
}

#[test]
fn the_log_is_compacted_into_a_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.drusite");
    let key = StoreKey::generate().unwrap();
    let mut store = Store::open(&path, &key).unwrap();
    let mut ws = Workspace::create().unwrap();
    store.create_workspace(&ws).unwrap();
    let mut page = ws.create_page(None, "Long").unwrap();
    let mut at = store.create_page(&page).unwrap();
    let first = page.order()[0];
    let keystrokes = COMPACT_AFTER_UPDATES as usize * 2 + 7;
    for i in 0..keystrokes {
        page.insert_text(first, i, "x").unwrap();
        store.save_page(&page, &mut at).unwrap();
    }
    let stored = store.load(&page.id().to_string()).unwrap().unwrap();
    assert_eq!(
        stored.updates.len(),
        7,
        "only the updates since the last compaction are logged"
    );
    assert_eq!(stored.last_seq, keystrokes as u64);
    drop(store);
    let store = Store::open(&path, &key).unwrap();
    let (loaded, _) = store.load_page(page.id()).unwrap();
    assert_eq!(texts(&loaded), [("x").repeat(keystrokes)]);
    assert_eq!(store.docs(DocKind::Page).unwrap(), [page.id().to_string()]);
}

// ---- Killing the app mid-write ----

const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

/// The text after `n` keystrokes of the child's script: letter `i` is `ALPHABET[i % 26]`.
fn expected(n: usize) -> String {
    (0..n)
        .map(|i| char::from(ALPHABET[i % ALPHABET.len()]))
        .collect()
}

/// Run by the parent test as a separate process. Types forever, saving after each key, with
/// Enter every 40 keys, and prints `saved N` once keystroke N is committed.
#[test]
fn crash_writer_child() {
    let (Some(db), Some(key), Some(page)) = (
        std::env::var_os("DRUSITE_CRASH_DB"),
        std::env::var("DRUSITE_CRASH_KEY").ok(),
        std::env::var("DRUSITE_CRASH_PAGE").ok(),
    ) else {
        return; // Not running as the child.
    };
    let key = StoreKey::from_hex(&key).unwrap();
    let mut store = Store::open(Path::new(&db), &key).unwrap();
    let page_id: PageId = page.parse().unwrap();
    let (mut page, mut at) = store.load_page(page_id).unwrap();
    let mut n = texts(&page).concat().chars().count();
    loop {
        let last = *page.order().last().unwrap();
        let len = page.text_len(last).unwrap();
        if n % 40 == 39 {
            page.split(last, len, BlockKind::Paragraph).unwrap();
            store.save_page(&page, &mut at).unwrap();
        }
        let last = *page.order().last().unwrap();
        let len = page.text_len(last).unwrap();
        let letter = char::from(ALPHABET[n % ALPHABET.len()]).to_string();
        page.insert_text(last, len, &letter).unwrap();
        store.save_page(&page, &mut at).unwrap();
        n += 1;
        println!("saved {n}");
    }
}

#[test]
fn killing_the_app_mid_write_loses_nothing_it_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.drusite");
    let key = StoreKey::generate().unwrap();
    let page_id = {
        let mut store = Store::open(&path, &key).unwrap();
        let mut ws = Workspace::create().unwrap();
        store.create_workspace(&ws).unwrap();
        let page = ws.create_page(None, "Crash test").unwrap();
        store.create_page(&page).unwrap();
        page.id()
    };

    let mut rng = fastrand::Rng::with_seed(7);
    let rounds = std::env::var("CRASH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    // Keep going (within reason) until the log has been compacted at least once, however slowly
    // this machine saves: some kills must land during or after a compaction.
    let mut total_saved = 0;
    let mut round = 0;
    while round < rounds || (total_saved <= COMPACT_AFTER_UPDATES as usize + 50 && round < 400) {
        round += 1;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crash_writer_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("DRUSITE_CRASH_DB", &path)
            .env("DRUSITE_CRASH_KEY", key.to_hex().as_str())
            .env("DRUSITE_CRASH_PAGE", page_id.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut last = None;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(n) = line.strip_prefix("saved ") {
                    last = n.parse::<usize>().ok();
                }
            }
            last
        });
        std::thread::sleep(Duration::from_millis(rng.u64(150..900)));
        child.kill().unwrap();
        child.wait().unwrap();
        let reported = reader.join().unwrap();

        let store = Store::open(&path, &key).unwrap();
        assert!(
            store.integrity_problems().unwrap().is_empty(),
            "round {round}: damaged store"
        );
        let (page, _) = store.load_page(page_id).unwrap();
        let text = texts(&page).concat();
        let n = text.chars().count();
        if let Some(reported) = reported {
            // Everything reported saved is there; at most the keystroke in flight is extra.
            assert!(
                n >= reported && n <= reported + 1,
                "round {round}: {n} keystrokes stored, {reported} reported saved"
            );
        }
        assert_eq!(
            text,
            expected(n),
            "round {round}: stored text isn't a prefix of what was typed"
        );
        total_saved = n;
    }
    assert!(
        total_saved > COMPACT_AFTER_UPDATES as usize,
        "the run never got as far as a compaction ({total_saved} keystrokes)"
    );
}
