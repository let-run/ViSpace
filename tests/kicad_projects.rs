use vispace::{model::*, store::Store};
use std::collections::BTreeMap;

#[test]
fn linked_project_survives_reopen_rename_and_detach_without_changing_notes() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    let link = KicadProject {
        path: "design/demo.kicad_pro".into(),
        revision: "test-revision".into(),
        files: BTreeMap::from([("design/demo.kicad_pro".into(), "{}".into())]),
        browser_folder: None,
        models_directory: Some("/models/kicad".into()),
        browser_models_folder: Some("browser-model-folder".into()),
    };
    store
        .change(|d| {
            d.ensure_project("Demo")?;
            d.projects[0].kicad = Some(link.clone());
            d.projects[0].notes.text = "Keep project notes".into();
            Ok(())
        })
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.data.projects[0].kicad.as_ref().unwrap().files,
        link.files
    );
    store
        .change(|d| d.rename_project("Demo", "Renamed", "Description"))
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    let project = &store.data.projects[0];
    assert_eq!(project.name, "Renamed");
    assert_eq!(project.kicad.as_ref().unwrap().path, link.path);
    assert_eq!(project.notes.text, "Keep project notes");
    assert_eq!(
        project.kicad.as_ref().unwrap().models_directory,
        link.models_directory
    );
    assert_eq!(
        project.kicad.as_ref().unwrap().browser_models_folder,
        link.browser_models_folder
    );
    store
        .change(|d| {
            d.projects[0].kicad = None;
            Ok(())
        })
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store.data.projects[0].kicad.is_none());
    assert_eq!(store.data.projects[0].notes.text, "Keep project notes");
}

#[test]
fn existing_inventory_without_kicad_table_migrates() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    store.change(|d| d.ensure_project("Existing")).unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE project_kicad; PRAGMA user_version=10;")
        .unwrap();
    drop(db);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.data.projects[0].name, "Existing");
    assert!(store.data.projects[0].kicad.is_none());
}
