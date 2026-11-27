// SPDX-License-Identifier: AGPL-3.0-or-later

//! Databases through the Core API: the tasks template, cells, formulas, rollups, two-way
//! relations, views; and importing a Notion export (a synthetic one, in Notion's own format).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use drusite_core::api::{CellValue, Command, Response};
use drusite_core::database::{Clock, Template, ViewResult};
use drusite_core::{Core, StoreKey};
use drusite_model::PageId;
use drusite_model::database::{
    DateTime, Filter, FilterCondition, FilterOp, FilterValue, Layout, PropertyDef, PropertyKind,
    RollupFunction, Schema, Value, ViewSort, short_id,
};

fn core(dir: &Path) -> Core {
    Core::open(dir, &StoreKey::from_bytes([7; 32]), Box::new(|_| {})).unwrap()
}

fn clock() -> Clock {
    Clock {
        now: DateTime::parse("2026-11-27T12:00").unwrap(),
        utc_offset: 0,
    }
}

fn schema(core: &mut Core, db: PageId) -> Schema {
    match core.handle(Command::Database { database: db }).unwrap() {
        Response::Database { schema } => schema,
        other => panic!("{other:?}"),
    }
}

fn view(core: &mut Core, db: PageId, view: Option<String>) -> ViewResult {
    match core
        .handle(Command::QueryView {
            database: db,
            view,
            clock: clock(),
        })
        .unwrap()
    {
        Response::View { result } => result,
        other => panic!("{other:?}"),
    }
}

fn put(core: &mut Core, db: PageId, p: PropertyDef, twin: Option<&str>) -> Schema {
    match core
        .handle(Command::PutProperty {
            database: db,
            property: p,
            twin_name: twin.map(str::to_owned),
        })
        .unwrap()
    {
        Response::Database { schema } => schema,
        other => panic!("{other:?}"),
    }
}

fn row(core: &mut Core, db: PageId, title: &str, values: Vec<CellValue>) -> PageId {
    match core
        .handle(Command::CreateRow {
            database: db,
            title: title.into(),
            values,
            after: None,
        })
        .unwrap()
    {
        Response::PageCreated { page } => page.id,
        other => panic!("{other:?}"),
    }
}

fn database(core: &mut Core, title: &str, template: Template) -> PageId {
    match core
        .handle(Command::CreateDatabase {
            parent: None,
            title: title.into(),
            inline: false,
            template,
        })
        .unwrap()
    {
        Response::DatabaseCreated { page, .. } => page.id,
        other => panic!("{other:?}"),
    }
}

fn prop(name: &str, kind: PropertyKind) -> PropertyDef {
    PropertyDef {
        id: short_id(),
        name: name.into(),
        kind,
        options: Vec::new(),
        position: 10.0,
        description: None,
    }
}

fn cell_text(v: &ViewResult, row: PageId, prop: &str) -> String {
    let p = v.schema.property_named(prop).unwrap().id.clone();
    v.groups
        .iter()
        .flat_map(|g| &g.rows)
        .find(|r| r.id == row)
        .unwrap()
        .cells
        .iter()
        .find(|c| c.property == p)
        .map(|c| c.error.clone().unwrap_or_else(|| c.text.clone()))
        .unwrap()
}

#[test]
fn a_tasks_board_with_formulas_rollups_and_two_way_relations() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path());
    let tasks = database(&mut core, "Tasks", Template::Tasks);
    let s = schema(&mut core, tasks);
    assert_eq!(
        s.views.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
        ["Board", "All tasks", "Calendar"]
    );
    let status = s.property_named("Status").unwrap().clone();
    let priority = s.property_named("Priority").unwrap().clone();
    let done = status
        .options
        .iter()
        .find(|o| o.name == "Done")
        .unwrap()
        .id
        .clone();
    let high = priority.options[0].id.clone();

    let points = prop(
        "Points",
        PropertyKind::Number {
            format: Default::default(),
        },
    );
    put(&mut core, tasks, points.clone(), None);
    put(
        &mut core,
        tasks,
        prop(
            "Estimate",
            PropertyKind::Formula {
                expression: r#"prop("Points") * 2 + " hours""#.into(),
            },
        ),
        None,
    );
    // Notion refuses a formula that doesn't type-check, and so does Drusite.
    let bad = core.handle(Command::PutProperty {
        database: tasks,
        property: prop(
            "Bad",
            PropertyKind::Formula {
                expression: r#"lower(prop("Points"))"#.into(),
            },
        ),
        twin_name: None,
    });
    assert!(bad.is_err());

    let a = row(
        &mut core,
        tasks,
        "Sow seeds",
        vec![
            CellValue {
                property: points.id.clone(),
                value: Value::Number(3.0),
            },
            CellValue {
                property: priority.id.clone(),
                value: Value::Select(high.clone()),
            },
        ],
    );
    let b = row(&mut core, tasks, "Water", vec![]);
    core.handle(Command::SetCell {
        row: b,
        property: points.id.clone(),
        value: Some(Value::Number(5.0)),
    })
    .unwrap();

    // The board: Not started holds both new rows (a new row starts in the first to-do option).
    let board = view(&mut core, tasks, None);
    assert_eq!(board.view.layout, Layout::Board);
    let not_started = board
        .groups
        .iter()
        .find(|g| g.label == "Not started")
        .unwrap();
    assert_eq!(not_started.rows.len(), 2);
    assert_eq!(cell_text(&board, a, "Estimate"), "6 hours");
    // Drag "Water" to Done.
    let done_group = board.groups.iter().find(|g| g.label == "Done").unwrap();
    core.handle(Command::SetCell {
        row: b,
        property: status.id.clone(),
        value: done_group.value.clone(),
    })
    .unwrap();
    let board = view(&mut core, tasks, None);
    let done_rows: Vec<PageId> = board
        .groups
        .iter()
        .find(|g| g.key == done)
        .unwrap()
        .rows
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(done_rows, [b]);

    // A second database, related both ways, with a rollup over the relation.
    let projects = database(&mut core, "Projects", Template::Empty);
    let rel = prop(
        "Project",
        PropertyKind::Relation {
            database: projects,
            twin: None,
            mirror: false,
            single: false,
        },
    );
    let s = put(&mut core, tasks, rel.clone(), Some("Tasks"));
    assert!(matches!(
        s.property(&rel.id).unwrap().kind,
        PropertyKind::Relation { twin: Some(_), .. }
    ));
    let ps = schema(&mut core, projects);
    let far = ps.property_named("Tasks").unwrap().clone();
    let garden = row(&mut core, projects, "Garden", vec![]);
    // Link from the far side: the tasks get the project.
    core.handle(Command::SetCell {
        row: garden,
        property: far.id.clone(),
        value: Some(Value::Relation(vec![a, b])),
    })
    .unwrap();
    let t = view(&mut core, tasks, None);
    assert_eq!(cell_text(&t, a, "Project"), "Garden");
    put(
        &mut core,
        projects,
        prop(
            "Total points",
            PropertyKind::Rollup {
                relation: far.id.clone(),
                target: points.id.clone(),
                function: RollupFunction::Sum,
            },
        ),
        None,
    );
    put(
        &mut core,
        projects,
        prop(
            "Open",
            PropertyKind::Formula {
                expression: r#"prop("Tasks").filter(current.prop("Status") != "Done").length()"#
                    .into(),
            },
        ),
        None,
    );
    let p = view(&mut core, projects, None);
    assert_eq!(cell_text(&p, garden, "Total points"), "8");
    assert_eq!(cell_text(&p, garden, "Open"), "1");
    assert_eq!(cell_text(&p, garden, "Tasks"), "Sow seeds, Water");

    // Unlink from the near side; the far side follows.
    core.handle(Command::SetCell {
        row: b,
        property: rel.id.clone(),
        value: None,
    })
    .unwrap();
    let p = view(&mut core, projects, None);
    assert_eq!(cell_text(&p, garden, "Tasks"), "Sow seeds");
    assert_eq!(cell_text(&p, garden, "Total points"), "3");

    // A filtered, sorted view.
    let mut s = schema(&mut core, tasks);
    let mut v = s.views.remove(1);
    v.filter = Some(Filter::Condition(FilterCondition {
        property: points.id.clone(),
        op: FilterOp::Greater,
        value: Some(FilterValue::Number(1.0)),
    }));
    v.sorts = vec![ViewSort {
        property: points.id.clone(),
        descending: true,
    }];
    core.handle(Command::PutView {
        database: tasks,
        view: v.clone(),
    })
    .unwrap();
    let r = view(&mut core, tasks, Some(v.id));
    let order: Vec<PageId> = r.groups[0].rows.iter().map(|r| r.id).collect();
    assert_eq!(order, [b, a]);

    // Everything survives a restart.
    drop(core);
    let mut core = self::core(dir.path());
    let p = view(&mut core, projects, None);
    assert_eq!(cell_text(&p, garden, "Total points"), "3");
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notion-export")
}

/// The synthetic export, zipped as Notion zips one (files under the zip's root).
fn fixture_zip() -> Vec<u8> {
    let root = fixture();
    let mut files = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                dirs.push(e.path());
            } else {
                let rel = e
                    .path()
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.push((rel, std::fs::read(e.path()).unwrap()));
            }
        }
    }
    files.sort();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    // Notion wraps big exports as zips of part zips; check that path too.
    let inner = drusite_core::database::zip::write_zip(&refs, true);
    drusite_core::database::zip::write_zip(&[("Export-Part-1.zip", &inner)], false)
}

#[test]
fn a_notion_export_imports_with_its_board_relations_and_values() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path());
    let zip = dir.path().join("Export.zip");
    std::fs::write(&zip, fixture_zip()).unwrap();
    let report = match core
        .handle(Command::ImportNotion {
            path: zip.to_string_lossy().into_owned(),
        })
        .unwrap()
    {
        Response::NotionImported { report } => report,
        other => panic!("{other:?}"),
    };
    assert_eq!(report.databases, 2, "{report:?}");
    assert_eq!(report.rows, 7, "{report:?}");
    assert_eq!(report.attachments, 1, "{report:?}");
    assert!(report.unmapped.iter().any(|u| u.contains("no views")));

    let pages = match core.handle(Command::ListPages).unwrap() {
        Response::Pages { pages } => pages,
        other => panic!("{other:?}"),
    };
    let find = |t: &str| pages.iter().find(|p| p.title == t).unwrap().id;
    let tasks = find("Tasks");
    let projects = find("Projects");
    let s = schema(&mut core, tasks);
    let kind = |n: &str| s.property_named(n).unwrap().kind.clone();
    assert_eq!(kind("Status"), PropertyKind::Status);
    assert_eq!(kind("Tags"), PropertyKind::MultiSelect);
    assert_eq!(kind("Due"), PropertyKind::Date);
    assert_eq!(kind("Done?"), PropertyKind::Checkbox);
    assert_eq!(kind("Created"), PropertyKind::CreatedTime);
    assert!(matches!(kind("ID"), PropertyKind::UniqueId { .. }));
    assert!(
        matches!(kind("Project"), PropertyKind::Relation { database, .. } if database == projects)
    );

    // The board groups by Status.
    let board_id = s
        .views
        .iter()
        .find(|v| v.layout == Layout::Board)
        .unwrap()
        .id
        .clone();
    let board = view(&mut core, tasks, Some(board_id));
    let count = |label: &str| {
        board
            .groups
            .iter()
            .find(|g| g.label == label)
            .map_or(0, |g| g.rows.len())
    };
    assert_eq!(count("Done"), 1);
    assert_eq!(count("In progress"), 1);
    assert_eq!(count("Not started"), 3);
    let water = find("Water the seedlings");
    assert_eq!(cell_text(&board, water, "Due"), "November 21, 2026");
    assert_eq!(cell_text(&board, water, "Project"), "Garden app");

    // The two relations are one, two-way: Projects › Tasks mirrors Tasks › Project.
    let ps = schema(&mut core, projects);
    assert!(
        matches!(
            ps.property_named("Tasks").unwrap().kind,
            PropertyKind::Relation { mirror: true, .. }
        ) || matches!(
            s.property_named("Project").unwrap().kind,
            PropertyKind::Relation { mirror: true, .. }
        )
    );
    let p = view(&mut core, projects, None);
    let garden = find("Garden app");
    assert_eq!(
        cell_text(&p, garden, "Tasks"),
        "Water the seedlings, Write recipes, part one"
    );
    assert_eq!(cell_text(&p, garden, "Budget"), "$1,250.00");

    // Row pages keep their content, and links become links to pages.
    let view_ = match core.handle(Command::OpenPage { page: water }).unwrap() {
        Response::Opened { view } => view,
        other => panic!("{other:?}"),
    };
    let text: Vec<String> = view_
        .blocks
        .iter()
        .map(drusite_model::Block::text)
        .collect();
    assert_eq!(
        text,
        ["Twice a day while it is warm.", "Morning", "Evening"]
    );
    // The top page shows both databases inline, and the image.
    let top = find("Team space");
    let view_ = match core.handle(Command::OpenPage { page: top }).unwrap() {
        Response::Opened { view } => view,
        other => panic!("{other:?}"),
    };
    let dbs = view_
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, drusite_model::BlockKind::Database { .. }))
        .count();
    assert_eq!(dbs, 2);
    assert!(
        view_
            .blocks
            .iter()
            .any(|b| matches!(b.kind, drusite_model::BlockKind::Attachment { .. }))
    );
    assert!(
        view_
            .blocks
            .iter()
            .any(|b| matches!(b.kind, drusite_model::BlockKind::Callout { .. }))
    );
    assert!(view_.blocks.iter().any(|b| b.spans.iter().any(|s| {
        s.marks
            .link
            .as_deref()
            .is_some_and(|l| l.starts_with("drusite:page/"))
    })));
}

#[test]
fn the_unzipped_folder_imports_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path());
    let report = match core
        .handle(Command::ImportNotion {
            path: fixture().to_string_lossy().into_owned(),
        })
        .unwrap()
    {
        Response::NotionImported { report } => report,
        other => panic!("{other:?}"),
    };
    assert_eq!((report.databases, report.rows), (2, 7));
}

/// A real export from Notion, when `DRUSITE_NOTION_EXPORT` names one (never committed: real
/// exports hold someone's notes). Prints what came in.
#[test]
fn a_real_notion_export_imports_when_given() {
    let Ok(path) = std::env::var("DRUSITE_NOTION_EXPORT") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path());
    let report = match core.handle(Command::ImportNotion { path }).unwrap() {
        Response::NotionImported { report } => report,
        other => panic!("{other:?}"),
    };
    println!("{report:#?}");
    assert!(report.databases + report.pages > 1);
    let pages = match core.handle(Command::ListPages).unwrap() {
        Response::Pages { pages } => pages,
        other => panic!("{other:?}"),
    };
    for p in pages
        .iter()
        .filter(|p| p.kind == drusite_model::PageKind::Database)
    {
        let s = schema(&mut core, p.id);
        println!(
            "{}: {:?}",
            p.title,
            s.properties
                .iter()
                .map(|p| (p.name.clone(), p.kind.name()))
                .collect::<Vec<_>>()
        );
        for g in view(&mut core, p.id, None).groups {
            for r in g.rows {
                println!(
                    "  {} | {}",
                    r.title,
                    r.cells
                        .iter()
                        .map(|c| c.text.clone())
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
            }
        }
    }
}
