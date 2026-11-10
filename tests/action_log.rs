use vispace::{
    action_log::{self, Journal},
    model::*,
    store::{Store, copy_tree},
};
use serde_json::json;
use tempfile::TempDir;

fn part() -> Part {
    Part {
        number: "R1".into(),
        stock: 20,
        ..Default::default()
    }
}
fn canonical(data: &Inventory) -> serde_json::Value {
    // Replay order is identity order, not UI insertion order.
    let record = action_log::diff(
        &Inventory::default(),
        data,
        std::path::Path::new("/unused"),
        true,
    )
    .unwrap();
    json!(record.actions)
}
#[test]
fn operations_reconstruct_fresh_database_and_reopen_idempotently() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    let p = part();
    let id = p.id.clone();
    store
        .change(|d| {
            d.save_part(p)?;
            d.allocate(&id, "Amplifier", 6, "R1-R6")?;
            d.storage.insert("Bin A".into(), "Top shelf".into());
            d.parts[0].storage = "Bin A".into();
            d.parts[0].notes.text = "Keep dry".into();
            d.purchases.push(Purchase {
                part: part(),
                assignments: vec![Assignment {
                    id: vispace::model::id(),
                    project: "Amplifier".into(),
                    quantity: 2,
                    designators: "R7-R8".into(),
                }],
            });
            Ok(())
        })
        .unwrap();
    store
        .change(|d| {
            d.parts[0].stock = 30;
            d.parts[0].notes.text = "Updated".into();
            Ok(())
        })
        .unwrap();
    let expected = canonical(&store.data);
    assert_eq!(store.action_log().unwrap().len(), 3);
    let target = TempDir::new().unwrap();
    copy_tree(
        &path.with_extension("actions"),
        &target.path().join("inventory.actions"),
    )
    .unwrap();
    drop(store);
    let restored = Store::open(target.path().join("inventory.sqlite")).unwrap();
    assert_eq!(canonical(&restored.data), expected);
    drop(restored);
    let restored = Store::open(target.path().join("inventory.sqlite")).unwrap();
    assert_eq!(canonical(&restored.data), expected);
    assert_eq!(restored.action_log().unwrap().len(), 3);
}
#[test]
fn pending_wal_replays_after_crash_and_partial_tail_is_ignored() {
    use std::io::Write;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("inventory.sqlite");
    let store = Store::open(&path).unwrap();
    drop(store);
    let mut next = Inventory::default();
    next.save_part(part()).unwrap();
    let record = action_log::diff(
        &Inventory::default(),
        &next,
        &dir.path().join("Attachments"),
        false,
    )
    .unwrap();
    let mut journal = Journal::open(path.with_extension("actions")).unwrap();
    let (chunk, _) = journal.append(&record).unwrap();
    drop(journal);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path.with_extension("actions").join(chunk))
        .unwrap();
    file.write_all(&[12, 0, 0, 0, 1, 2]).unwrap();
    drop(file);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.data.parts.len(), 1);
    store
        .change(|d| {
            d.parts[0].stock = 24;
            Ok(())
        })
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.data.parts[0].stock, 24);
}
#[test]
fn invalid_changes_do_not_log_and_writer_is_exclusive() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    assert!(Store::open(&path).is_err());
    assert!(
        store
            .change(|d| {
                d.parts.push(Part::default());
                Ok(())
            })
            .is_err()
    );
    assert_eq!(store.action_log().unwrap().len(), 1);
}
#[test]
fn assets_are_references_and_relocate_without_file_bytes() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("Attachments");
    let mut data = Inventory::default();
    let mut p = part();
    p.image = Some(json!({"originalPath":root.join("unique-id/original.png")}));
    p.datasheet = root
        .join("unique-id/datasheet.pdf")
        .to_string_lossy()
        .into_owned();
    data.parts.push(p);
    let record = action_log::diff(&Inventory::default(), &data, &root, true).unwrap();
    let json = serde_json::to_string(&record).unwrap();
    assert!(!json.contains(dir.path().to_str().unwrap()));
    assert!(json.contains("asset:"));
    let other = TempDir::new().unwrap();
    let restored = action_log::apply(&Inventory::default(), &record, other.path()).unwrap();
    assert_eq!(
        restored.parts[0].datasheet,
        other
            .path()
            .join("unique-id/datasheet.pdf")
            .to_str()
            .unwrap()
    );
}
#[test]
fn damaged_complete_frame_fails_instead_of_silently_skipping() {
    let dir = TempDir::new().unwrap();
    let mut journal = Journal::open(dir.path().join("actions")).unwrap();
    let record = action_log::diff(
        &Inventory::default(),
        &Inventory::default(),
        dir.path(),
        true,
    )
    .unwrap();
    let (name, _) = journal.append(&record).unwrap();
    drop(journal);
    let path = dir.path().join("actions").join(name);
    let mut bytes = std::fs::read(&path).unwrap();
    let i = bytes.len() - 5;
    bytes[i] ^= 0xff;
    std::fs::write(path, bytes).unwrap();
    assert!(action_log::read_directory(&dir.path().join("actions")).is_err());
}

#[test]
fn collection_order_and_literal_asset_text_survive_replay() {
    let root = std::path::Path::new("/Attachments");
    let mut data = Inventory::default();
    let mut a = part();
    a.id = "Z".into();
    a.notes.text = "asset:literal note".into();
    let mut b = part();
    b.id = "A".into();
    b.notes.text = "literal:second note".into();
    data.parts = vec![a, b];
    let initial = action_log::diff(&Inventory::default(), &data, root, true).unwrap();
    let restored = action_log::apply(&Inventory::default(), &initial, root).unwrap();
    assert_eq!(json!(restored), json!(data));
    let mut next = data.clone();
    next.parts.reverse();
    let update = action_log::diff(&data, &next, root, false).unwrap();
    assert_eq!(update.actions.len(), 2);
    let restored = action_log::apply(&data, &update, root).unwrap();
    assert_eq!(json!(restored), json!(next));
}
#[test]
fn existing_database_exports_full_initial_state_and_history_api_is_read_only() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .change(|d| {
            d.save_part(part())?;
            d.ensure_project("Board")?;
            d.storage.insert("Drawer".into(), "Shelf".into());
            Ok(())
        })
        .unwrap();
    let expected = canonical(&store.data);
    drop(store);
    // Model an existing installation upgrading from the version without an action log.
    std::fs::remove_dir_all(path.with_extension("actions")).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE sync_events")
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    let entries = store.action_log().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].record.initial);
    let restored = action_log::apply(
        &Inventory::default(),
        &entries[0].record,
        &dir.path().join("Attachments"),
    )
    .unwrap();
    assert_eq!(canonical(&restored), expected);
    let (status, history) =
        vispace::api::handle_request(&mut store, "GET", "/v1/action-log", json!(null))
            .unwrap();
    assert_eq!(status, 200);
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(store.action_log().unwrap().len(), 1);
}
#[test]
fn removal_actions_reconstruct_empty_inventory() {
    let root = std::path::Path::new("/Attachments");
    let mut before = Inventory::default();
    before.save_part(part()).unwrap();
    before.ensure_project("Board").unwrap();
    let record = action_log::diff(&before, &Inventory::default(), root, false).unwrap();
    assert!(
        record
            .actions
            .iter()
            .all(|a| a.kind.ends_with("_removed") && a.value.is_none())
    );
    let restored = action_log::apply(&before, &record, root).unwrap();
    assert_eq!(json!(restored), json!(Inventory::default()));
}

#[test]
fn sqlite_reopen_preserves_project_order_for_future_deltas() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .change(|d| {
            d.ensure_project("Zebra")?;
            d.ensure_project("Alpha")?;
            Ok(())
        })
        .unwrap();
    let expected = json!(store.data);
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(json!(store.data), expected);
    store
        .change(|d| {
            d.ensure_project("Beta")?;
            Ok(())
        })
        .unwrap();
    let expected = json!(store.data);
    let mut restored = Inventory::default();
    for entry in store.action_log().unwrap() {
        restored =
            action_log::apply(&restored, &entry.record, &dir.path().join("Attachments")).unwrap();
    }
    assert_eq!(json!(restored), expected);
}

#[test]
fn renaming_a_project_keeps_its_identity_and_notes_in_history() {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(dir.path().join("inventory.sqlite")).unwrap();
    store
        .change(|data| {
            data.ensure_project("Kek")?;
            data.projects[0].notes.text = "Keep these notes".into();
            Ok(())
        })
        .unwrap();
    let before = store.data.clone();
    store
        .change(|data| data.rename_project("Kek", "CNC", ""))
        .unwrap();
    let record = store.action_log().unwrap().pop().unwrap().record;
    assert_eq!(record.actions.len(), 1);
    let action = &record.actions[0];
    assert_eq!(action.kind, "project_updated");
    assert_eq!(action.label, "Kek → CNC");
    assert!(action.changes.is_empty());
    let replayed = action_log::apply(&before, &record, dir.path()).unwrap();
    assert_eq!(replayed.projects[0].id, before.projects[0].id);
    assert_eq!(replayed.projects[0].name, "CNC");
    assert_eq!(replayed.projects[0].notes.text, "Keep these notes");

    let mut legacy = record.clone();
    legacy.actions[0].key = "CNC".into();
    legacy.actions[0].kind = "project_added".into();
    let mut removed = action.clone();
    removed.kind = "project_removed".into();
    removed.key = "Kek".into();
    removed.value = None;
    removed.position = None;
    legacy.actions.push(removed);
    let replayed = action_log::apply(&before, &legacy, dir.path()).unwrap();
    assert_eq!(replayed.projects.len(), 1);
    assert_eq!(replayed.projects[0].name, "CNC");
    assert_eq!(replayed.projects[0].notes.text, "Keep these notes");
}

#[test]
fn project_rename_leaves_stored_relationship_ids_unchanged() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    let payloads = || -> Vec<String> {
        let db = rusqlite::Connection::open(&path).unwrap();
        ["allocations", "purchases", "project_bom_entries"]
            .into_iter()
            .map(|table| {
                db.query_row(&format!("SELECT payload FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
            })
            .collect()
    };
    store
        .change(|data| {
            let item = part();
            let part_id = item.id.clone();
            data.save_part(item)?;
            data.allocate(&part_id, "StarLight", 1, "")?;
            data.purchases.push(Purchase {
                part: Part {
                    number: "Purchased".into(),
                    ..part()
                },
                assignments: vec![Assignment {
                    id: id(),
                    project: "StarLight".into(),
                    quantity: 1,
                    designators: String::new(),
                }],
            });
            data.bom.push(BomEntry {
                id: id(),
                project: "StarLight".into(),
                item: BomItem {
                    quantity: 1,
                    ..Default::default()
                },
                part_id: None,
                allocation_id: None,
                kicad_source: None,
            });
            Ok(())
        })
        .unwrap();
    let before = store.data.clone();
    let project_id = &before.projects[0].id;
    let stored = payloads();
    let values: Vec<serde_json::Value> = stored
        .iter()
        .map(|payload| serde_json::from_str(payload).unwrap())
        .collect();
    assert_eq!(values[0]["project"], *project_id);
    assert_eq!(values[1]["assignments"][0]["project"], *project_id);
    assert_eq!(values[2]["project"], *project_id);
    store
        .change(|data| data.rename_project("StarLight", "StarLight2", ""))
        .unwrap();
    let record = store.action_log().unwrap().pop().unwrap().record;
    assert_eq!(record.actions.len(), 1);
    assert_eq!(record.actions[0].kind, "project_updated");
    assert_eq!(stored, payloads());

    let restored = action_log::apply(&before, &record, dir.path()).unwrap();
    assert_eq!(restored.allocations[0].project, "StarLight2");
    assert_eq!(restored.purchases[0].assignments[0].project, "StarLight2");
    assert_eq!(restored.bom[0].project, "StarLight2");
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.data.allocations[0].project, "StarLight2");
    assert_eq!(
        reopened.data.purchases[0].assignments[0].project,
        "StarLight2"
    );
    assert_eq!(reopened.data.bom[0].project, "StarLight2");
}
