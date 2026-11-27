// SPDX-License-Identifier: AGPL-3.0-or-later

//! Databases in the workspace document: schemas, views and rows, edited on several devices at
//! once and merged.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use drusite_crdt::Workspace;
use drusite_model::PageId;
use drusite_model::database::{
    Color, Filter, FilterCondition, FilterOp, FilterValue, Layout, PropertyDef, PropertyKind,
    Schema, SelectOption, Value, View, ViewSort,
};
use proptest::prelude::*;

fn fork(ws: &Workspace) -> Workspace {
    Workspace::load(&ws.export_snapshot().unwrap()).unwrap()
}

fn sync(a: &mut Workspace, b: &mut Workspace) {
    b.import(&a.export_updates(&b.version()).unwrap()).unwrap();
    a.import(&b.export_updates(&a.version()).unwrap()).unwrap();
}

fn option(id: &str, name: &str) -> SelectOption {
    SelectOption {
        id: id.into(),
        name: name.into(),
        color: Color::Blue,
        group: None,
        position: 0.0,
    }
}

fn prop(id: &str, name: &str, kind: PropertyKind) -> PropertyDef {
    PropertyDef {
        id: id.into(),
        name: name.into(),
        kind,
        options: Vec::new(),
        position: 1.0,
        description: None,
    }
}

/// A database with a Tags multi-select (x, y, z) and a Points number, and one row.
fn setup() -> (Workspace, PageId, PageId) {
    let mut ws = Workspace::create().unwrap();
    let (db, _) = ws.create_database(None, "Tasks", false).unwrap();
    let db = db.id();
    let mut tags = prop("tags", "Tags", PropertyKind::MultiSelect);
    tags.options = vec![option("x", "x"), option("y", "y"), option("z", "z")];
    ws.put_property(db, &tags).unwrap();
    let mut points = prop(
        "pts",
        "Points",
        PropertyKind::Number {
            format: Default::default(),
        },
    );
    points.position = 2.0;
    ws.put_property(db, &points).unwrap();
    let row = ws.create_page(Some(db), "Write the spec").unwrap().id();
    (ws, db, row)
}

fn schema(ws: &Workspace, db: PageId) -> Schema {
    ws.schema(db).unwrap()
}

#[test]
fn a_database_and_its_rows_survive_a_reload() {
    let (mut ws, db, row) = setup();
    ws.set_value(row, "pts", Some(&Value::Number(3.0))).unwrap();
    ws.set_value(
        row,
        "tags",
        Some(&Value::MultiSelect(vec!["x".into(), "z".into()])),
    )
    .unwrap();
    let ws = fork(&ws);
    let s = schema(&ws, db);
    assert_eq!(
        s.properties
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["Name", "Tags", "Points"]
    );
    assert_eq!(s.views.len(), 1);
    assert_eq!(ws.rows(db).unwrap(), [row]);
    assert_eq!(ws.database_of(row), Some(db));
    let data = ws.row(row, &s).unwrap();
    assert_eq!(data.values["pts"], Value::Number(3.0));
    assert_eq!(
        data.values["tags"],
        Value::MultiSelect(vec!["x".into(), "z".into()])
    );
}

#[test]
fn a_rename_and_a_new_value_on_two_devices_both_stay() {
    let (mut a, db, row) = setup();
    let mut b = fork(&a);
    let mut renamed = schema(&a, db).property("pts").unwrap().clone();
    renamed.name = "Estimate".into();
    a.put_property(db, &renamed).unwrap();
    b.set_value(row, "pts", Some(&Value::Number(5.0))).unwrap();
    sync(&mut a, &mut b);
    for ws in [&a, &b] {
        let s = schema(ws, db);
        assert_eq!(s.property("pts").unwrap().name, "Estimate");
        assert_eq!(ws.row(row, &s).unwrap().values["pts"], Value::Number(5.0));
    }
}

#[test]
fn tags_added_on_two_devices_are_both_kept_and_a_removal_wins_only_its_own() {
    let (mut a, db, row) = setup();
    a.set_value(row, "tags", Some(&Value::MultiSelect(vec!["z".into()])))
        .unwrap();
    let mut b = fork(&a);
    a.set_value(
        row,
        "tags",
        Some(&Value::MultiSelect(vec!["z".into(), "x".into()])),
    )
    .unwrap();
    // b removes z and adds y.
    b.set_value(row, "tags", Some(&Value::MultiSelect(vec!["y".into()])))
        .unwrap();
    sync(&mut a, &mut b);
    for ws in [&a, &b] {
        let s = schema(ws, db);
        let Value::MultiSelect(mut tags) = ws.row(row, &s).unwrap().values["tags"].clone() else {
            panic!("not tags")
        };
        tags.sort();
        assert_eq!(tags, ["x", "y"]);
    }
}

#[test]
fn options_and_view_fields_merge_one_by_one() {
    let (mut a, db, _) = setup();
    let mut b = fork(&a);
    a.put_option(db, "tags", &option("w", "w")).unwrap();
    b.put_option(db, "tags", &option("v", "v")).unwrap();
    let view = schema(&a, db).views[0].clone();
    let mut va = view.clone();
    va.filter = Some(Filter::Condition(FilterCondition {
        property: "pts".into(),
        op: FilterOp::Greater,
        value: Some(FilterValue::Number(1.0)),
    }));
    a.put_view(db, &va).unwrap();
    let mut vb = view;
    vb.sorts = vec![ViewSort {
        property: "pts".into(),
        descending: true,
    }];
    vb.layout = Layout::Board;
    b.put_view(db, &vb).unwrap();
    sync(&mut a, &mut b);
    for ws in [&a, &b] {
        let s = schema(ws, db);
        let tags = s.property("tags").unwrap();
        assert_eq!(tags.options.len(), 5);
        let v = &s.views[0];
        assert!(v.filter.is_some());
        assert_eq!(v.sorts.len(), 1);
        assert_eq!(v.layout, Layout::Board);
    }
}

#[test]
fn a_property_removed_while_another_device_fills_it_stays_removed() {
    let (mut a, db, row) = setup();
    let mut b = fork(&a);
    a.delete_property(db, "pts").unwrap();
    b.set_value(row, "pts", Some(&Value::Number(8.0))).unwrap();
    sync(&mut a, &mut b);
    for ws in [&a, &b] {
        let s = schema(ws, db);
        assert!(s.property("pts").is_none());
        assert!(ws.row(row, &s).unwrap().values.get("pts").is_none());
    }
}

#[test]
fn rows_made_on_two_devices_are_both_there() {
    let (mut a, db, _) = setup();
    let mut b = fork(&a);
    let ra = a.create_page(Some(db), "From a").unwrap().id();
    let rb = b.create_page(Some(db), "From b").unwrap().id();
    a.set_value(ra, "pts", Some(&Value::Number(1.0))).unwrap();
    b.set_value(rb, "pts", Some(&Value::Number(2.0))).unwrap();
    sync(&mut a, &mut b);
    assert_eq!(a.rows(db).unwrap(), b.rows(db).unwrap());
    assert_eq!(a.rows(db).unwrap().len(), 3);
}

#[test]
fn relations_added_on_two_devices_are_both_kept() {
    let (mut a, db, row) = setup();
    let other = a.create_page(Some(db), "Other").unwrap().id();
    let third = a.create_page(Some(db), "Third").unwrap().id();
    a.put_property(
        db,
        &prop(
            "rel",
            "Blocked by",
            PropertyKind::Relation {
                database: db,
                twin: None,
                mirror: false,
                single: false,
            },
        ),
    )
    .unwrap();
    let mut b = fork(&a);
    a.set_value(row, "rel", Some(&Value::Relation(vec![other])))
        .unwrap();
    b.set_value(row, "rel", Some(&Value::Relation(vec![third])))
        .unwrap();
    sync(&mut a, &mut b);
    let s = schema(&a, db);
    let Value::Relation(mut got) = a.row(row, &s).unwrap().values["rel"].clone() else {
        panic!()
    };
    got.sort();
    let mut want = vec![other, third];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn bad_ids_and_titles_are_refused() {
    let (mut ws, db, row) = setup();
    assert!(
        ws.put_property(db, &prop("a:b", "A", PropertyKind::Text))
            .is_err()
    );
    assert!(
        ws.put_property(db, &prop("ok", "", PropertyKind::Text))
            .is_err()
    );
    assert!(
        ws.put_property(db, &prop("title", "T", PropertyKind::Text))
            .is_err()
    );
    assert!(ws.delete_property(db, "title").is_err());
    assert!(
        ws.set_value(row, "title", Some(&Value::Text("x".into())))
            .is_err()
    );
    assert!(ws.schema(row).is_err(), "a row isn't a database");
}

#[derive(Debug, Clone)]
enum Op {
    Tag { device: usize, tags: Vec<u8> },
    Points { device: usize, n: i8 },
    Rename { device: usize, name: u8 },
    AddOption { device: usize, id: u8 },
    ViewLayout { device: usize, board: bool },
    ViewSort { device: usize, desc: bool },
    NewRow { device: usize },
    Sync { from: usize, to: usize },
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    let d = 0usize..3;
    prop::collection::vec(
        prop_oneof![
            (d.clone(), prop::collection::vec(0u8..3, 0..3))
                .prop_map(|(device, tags)| Op::Tag { device, tags }),
            (d.clone(), any::<i8>()).prop_map(|(device, n)| Op::Points { device, n }),
            (d.clone(), 0u8..5).prop_map(|(device, name)| Op::Rename { device, name }),
            (d.clone(), 0u8..10).prop_map(|(device, id)| Op::AddOption { device, id }),
            (d.clone(), any::<bool>()).prop_map(|(device, board)| Op::ViewLayout { device, board }),
            (d.clone(), any::<bool>()).prop_map(|(device, desc)| Op::ViewSort { device, desc }),
            d.clone().prop_map(|device| Op::NewRow { device }),
            (d.clone(), d).prop_map(|(from, to)| Op::Sync { from, to }),
        ],
        1..40,
    )
}

fn apply(ws: &mut [Workspace], db: PageId, row: PageId, op: &Op) {
    match op {
        Op::Tag { device, tags } => {
            let names = ["x", "y", "z"];
            let v = Value::MultiSelect(tags.iter().map(|t| names[*t as usize].into()).collect());
            ws[*device].set_value(row, "tags", Some(&v)).unwrap();
        }
        Op::Points { device, n } => {
            ws[*device]
                .set_value(row, "pts", Some(&Value::Number(f64::from(*n))))
                .unwrap();
        }
        Op::Rename { device, name } => {
            let mut p = ws[*device]
                .schema(db)
                .unwrap()
                .property("pts")
                .unwrap()
                .clone();
            p.name = format!("Points {name}");
            ws[*device].put_property(db, &p).unwrap();
        }
        Op::AddOption { device, id } => {
            ws[*device]
                .put_option(db, "tags", &option(&format!("o{id}"), &format!("O{id}")))
                .unwrap();
        }
        Op::ViewLayout { device, board } => {
            let mut v: View = ws[*device].schema(db).unwrap().views[0].clone();
            v.layout = if *board { Layout::Board } else { Layout::Table };
            ws[*device].put_view(db, &v).unwrap();
        }
        Op::ViewSort { device, desc } => {
            let mut v: View = ws[*device].schema(db).unwrap().views[0].clone();
            v.sorts = vec![ViewSort {
                property: "pts".into(),
                descending: *desc,
            }];
            ws[*device].put_view(db, &v).unwrap();
        }
        Op::NewRow { device } => {
            ws[*device].create_page(Some(db), "row").unwrap();
        }
        Op::Sync { from, to } => {
            if from != to {
                let bytes = ws[*from].export_updates(&ws[*to].version()).unwrap();
                ws[*to].import(&bytes).unwrap();
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever three devices do to one database, once they have each other's changes they
    /// agree on its schema, its views and every row's values.
    #[test]
    fn concurrent_database_edits_converge(ops in ops()) {
        let (base, db, row) = setup();
        let mut ws: Vec<Workspace> = (0..3).map(|_| fork(&base)).collect();
        for op in &ops {
            apply(&mut ws, db, row, op);
        }
        for _ in 0..2 {
            for i in 0..3 {
                for j in 0..3 {
                    if i != j {
                        apply(&mut ws, db, row, &Op::Sync { from: i, to: j });
                    }
                }
            }
        }
        let first = ws[0].schema(db).unwrap();
        let rows = ws[0].rows(db).unwrap();
        for w in &ws[1..] {
            let s = w.schema(db).unwrap();
            prop_assert_eq!(&s, &first);
            prop_assert_eq!(&w.rows(db).unwrap(), &rows);
            for r in &rows {
                prop_assert_eq!(w.row(*r, &s).unwrap(), ws[0].row(*r, &first).unwrap());
            }
        }
    }
}
