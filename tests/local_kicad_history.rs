use vispace::{action_log, model::*, store::Store};
use std::path::Path;
fn linked(data: &mut Inventory) {
    data.ensure_project("Board").unwrap();
    data.projects[0].kicad = Some(KicadProject {
        path: "/local-only/board.kicad_pro".into(),
        files: std::collections::BTreeMap::from([(
            "board.kicad_sch".into(),
            "LOCAL_SCHEMATIC_CONTENT".into(),
        )]),
        ..Default::default()
    });
    data.sync_kicad_requests(
        "Board",
        &[BomItem {
            designators: "R1, R2".into(),
            value: "10k".into(),
            package: "0603".into(),
            quantity: 2,
            ..Default::default()
        }],
    )
    .unwrap();
}
#[test]
fn initial_history_excludes_kicad_files_bindings_and_generated_requests_but_keeps_manual_rows() {
    let mut data = Inventory::default();
    linked(&mut data);
    data.bom.push(BomEntry {
        id: id(),
        project: "Board".into(),
        item: BomItem {
            designators: "U1".into(),
            value: "Manual IC".into(),
            quantity: 1,
            ..Default::default()
        },
        kicad_source: None,
        part_id: None,
        allocation_id: None,
    });
    let record = action_log::diff(
        &Inventory::default(),
        &data,
        Path::new("/Attachments"),
        true,
    )
    .unwrap();
    let json = serde_json::to_string(&record).unwrap();
    assert!(!json.contains("LOCAL_SCHEMATIC_CONTENT"));
    assert!(!json.contains("/local-only"));
    assert!(!json.contains("10k"));
    let replay =
        action_log::apply(&Inventory::default(), &record, Path::new("/Attachments")).unwrap();
    assert_eq!(replay.bom.len(), 1);
    assert_eq!(replay.bom[0].item.value, "Manual IC");
    assert!(replay.projects[0].kicad.is_none());
    let mut next = data.clone();
    next.bom.swap(0, 1);
    next.projects[0].kicad.as_mut().unwrap().revision = "new file hash".into();
    next.bom
        .iter_mut()
        .find(|b| b.kicad_source.is_some())
        .unwrap()
        .item
        .quantity = 3;
    assert!(
        action_log::diff(&data, &next, Path::new("/Attachments"), false)
            .unwrap()
            .actions
            .is_empty()
    );
}
#[test]
fn local_requests_are_durable_without_events_and_survive_remote_edits_renames_and_restart() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let path = a.path().join("inventory.sqlite");
    let mut source = Store::open(&path).unwrap();
    let mut remote = Store::open(b.path().join("inventory.sqlite")).unwrap();
    source
        .change(|d| {
            linked(d);
            Ok(())
        })
        .unwrap();
    for record in source.events_after(&Default::default()).unwrap() {
        remote.replay_record(&record, None).unwrap();
    }
    assert_eq!(source.data.bom.len(), 1);
    assert!(source.data.projects[0].kicad.is_some());
    assert!(remote.data.bom.is_empty());
    assert!(remote.data.projects[0].kicad.is_none());
    let count = source.action_log().unwrap().len();
    source
        .change(|d| {
            d.sync_kicad_requests(
                "Board",
                &[BomItem {
                    designators: "R1, R2, R3".into(),
                    value: "10k".into(),
                    package: "0603".into(),
                    quantity: 3,
                    ..Default::default()
                }],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(source.action_log().unwrap().len(), count);
    assert_eq!(source.data.bom[0].item.quantity, 3);
    remote
        .change(|d| {
            d.projects[0].name = "Renamed board".into();
            d.projects[0].description = "Remote description".into();
            Ok(())
        })
        .unwrap();
    for record in remote.events_after(&source.sync_clock()).unwrap() {
        source.replay_record(&record, None).unwrap();
    }
    assert_eq!(source.data.bom[0].project, "Renamed board");
    assert_eq!(source.data.bom[0].item.quantity, 3);
    assert!(source.data.projects[0].kicad.is_some());
    drop(source);
    let source = Store::open(&path).unwrap();
    assert_eq!(source.data.bom[0].item.quantity, 3);
    assert_eq!(source.data.bom[0].project, "Renamed board");
    assert!(source.data.projects[0].kicad.is_some());
    assert!(remote.data.bom.is_empty());
}

#[test]
fn git_remote_replays_and_persists_without_the_local_kicad_link() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let mut source = Store::open(a.path().join("inventory.sqlite")).unwrap();
    let path = b.path().join("inventory.sqlite");
    let mut peer = Store::open(&path).unwrap();
    let settings = GitRemote {
        name: "upstream".into(),
        url: "https://git.example.com/board.git".into(),
    };
    source
        .change(|data| {
            linked(data);
            data.projects[0].git_remote = Some(settings.clone());
            Ok(())
        })
        .unwrap();
    for record in source.events_after(&Default::default()).unwrap() {
        peer.replay_record(&record, None).unwrap();
    }
    assert_eq!(peer.data.projects[0].git_remote.as_ref(), Some(&settings));
    assert!(peer.data.projects[0].kicad.is_none());
    drop(peer);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened.data.projects[0].git_remote.as_ref(),
        Some(&settings)
    );
}
