use vispace::{kicad_sync::requested_items, model::*, store::Store};
use std::collections::BTreeMap;
fn item(value: &str, package: &str, refs: &str, quantity: i64) -> BomItem {
    BomItem {
        value: value.into(),
        package: package.into(),
        designators: refs.into(),
        quantity,
        ..Default::default()
    }
}
fn family(mut item: BomItem, kind: &str) -> BomItem {
    item.extra
        .insert("kicadKind".into(), serde_json::json!(kind));
    item
}
fn data() -> Inventory {
    let mut d = Inventory::default();
    d.ensure_project("Demo").unwrap();
    d.ensure_project("Other").unwrap();
    d
}
#[test]
fn kicad_replaces_manual_matches_cleans_only_overlapping_refs_and_detaches_permanently() {
    let mut d = data();
    d.save_request("Demo", None, item("CHIP", "QFN", "U1", 5))
        .unwrap();
    d.save_request("Demo", None, item("Unrelated", "SOT", "U2, U3", 7))
        .unwrap();
    d.save_request("Other", None, item("CHIP", "QFN", "U1", 1))
        .unwrap();
    let rows = vec![item("CHIP", "QFN", "U9", 2), item("NEW", "SOT", "U2", 1)];
    assert!(d.sync_kicad_requests("Demo", &rows).unwrap());
    let manual = d
        .bom
        .iter()
        .find(|e| e.project == "Demo" && e.kicad_source.is_none())
        .unwrap();
    assert_eq!(manual.item.designators, "U3");
    assert_eq!(manual.item.quantity, 7);
    assert_eq!(
        d.bom
            .iter()
            .filter(|e| e.project == "Demo" && e.kicad_source.is_some())
            .count(),
        2
    );
    assert!(
        !d.sync_kicad_requests("Demo", &rows).unwrap(),
        "unchanged reload must not write or change IDs"
    );
    d.detach_kicad_requests("Demo");
    assert_eq!(d.bom.iter().filter(|e| e.project == "Demo").count(), 1);
    assert!(
        !d.bom
            .iter()
            .any(|e| e.project == "Demo" && e.item.value == "CHIP")
    );
    assert_eq!(d.bom.iter().filter(|e| e.project == "Other").count(), 1);
    d.validate().unwrap();
}
#[test]
fn passive_override_uses_value_and_package_not_just_value() {
    for (kind, old, new, refs) in [
        ("Resistor", "4K7", "4700Ω", "R1"),
        ("Capacitor", "0.1uF", "100nF", "C1"),
        ("Inductor", "4.7uH", "4700nH", "L1"),
        ("Ferrite", "600Ω", "600ohm", "FB1"),
        ("Crystal", "12MHz", "12000000Hz", "Y1"),
    ] {
        let mut d = data();
        d.save_request("Demo", None, item(old, "0603", refs, 10))
            .unwrap();
        d.save_request("Demo", None, item(old, "0805", "", 8))
            .unwrap();
        let row = family(item(new, "Library:R_0603_1608Metric", refs, 1), kind);
        d.sync_kicad_requests("Demo", &[row]).unwrap();
        assert_eq!(d.bom.len(), 2, "{kind}");
        assert!(
            d.bom
                .iter()
                .any(|e| e.kicad_source.is_none() && e.item.package == "0805")
        );
        assert_eq!(d.bom.iter().filter(|e| e.kicad_source.is_some()).count(), 1);
    }
}
#[test]
fn refresh_updates_quantity_removes_deleted_components_and_preserves_allocations() {
    let mut d = data();
    let part = Part {
        number: "CHIP".into(),
        stock: 10,
        ..Default::default()
    };
    d.save_part(part.clone()).unwrap();
    d.save_request("Demo", None, item("CHIP", "QFN", "U1,U2", 2))
        .unwrap();
    d.allocations.push(Allocation {
        id: "allocation".into(),
        part_id: part.id.clone(),
        project: "Demo".into(),
        quantity: 2,
        designators: "U1,U2".into(),
    });
    d.bom[0].part_id = Some(part.id.clone());
    d.bom[0].allocation_id = Some("allocation".into());
    d.sync_kicad_requests(
        "Demo",
        &[
            item("CHIP", "QFN", "U1,U2", 2),
            item("GONE", "QFN", "U3", 1),
        ],
    )
    .unwrap();
    let id = d
        .bom
        .iter()
        .find(|e| e.item.value == "CHIP")
        .unwrap()
        .id
        .clone();
    d.sync_kicad_requests("Demo", &[item("CHIP", "QFN", "U1", 1)])
        .unwrap();
    assert_eq!(d.bom.len(), 1);
    assert_eq!(d.bom[0].id, id);
    assert_eq!(d.bom[0].allocation_id.as_deref(), Some("allocation"));
    assert_eq!(d.requested(&d.bom[0]), 0);
    d.detach_kicad_requests("Demo");
    assert!(d.bom.is_empty());
    assert_eq!(d.allocations[0].quantity, 2);
    assert_eq!(d.parts[0].stock, 10);
    d.validate().unwrap();
}
fn fixture() -> inventory_kicad::Bundle {
    let files = BTreeMap::from([
        (
            "demo.kicad_pro",
            include_str!("../crates/kicad-reader/tests/fixtures/demo.kicad_pro"),
        ),
        (
            "demo.kicad_sch",
            include_str!("../crates/kicad-reader/tests/fixtures/demo.kicad_sch"),
        ),
        (
            "power.kicad_sch",
            include_str!("../crates/kicad-reader/tests/fixtures/power.kicad_sch"),
        ),
    ]);
    inventory_kicad::load_project("demo.kicad_pro", |p| {
        files
            .get(p)
            .map(|s| s.to_string())
            .ok_or_else(|| inventory_kicad::Error::new(p))
    })
    .unwrap()
}
#[test]
fn hierarchy_counts_instances_and_rejects_partial_bom() {
    let mut bundle = fixture();
    assert!(requested_items(&bundle).is_err());
    bundle.root.children.pop();
    let rows = requested_items(&bundle).unwrap();
    assert_eq!(rows.iter().map(|r| r.quantity).sum::<i64>(), 3);
    let caps = rows.iter().find(|r| r.value == "100nF").unwrap();
    assert_eq!(caps.quantity, 2);
    assert_eq!(caps.designators, "C101, C201");
}
#[test]
fn ownership_persists_through_reopen_and_project_rename() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .change(|d| {
            d.ensure_project("Demo")?;
            d.sync_kicad_requests("Demo", &[item("CHIP", "QFN", "U1", 2)])?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert!(store.data.bom[0].kicad_source.is_some());
    store
        .change(|d| d.rename_project("Demo", "Renamed", ""))
        .unwrap();
    store
        .change(|d| {
            d.detach_kicad_requests("Renamed");
            Ok(())
        })
        .unwrap();
    drop(store);
    assert!(Store::open(&path).unwrap().data.bom.is_empty());
}

#[test]
fn components_exclude_power_dnp_and_out_of_bom_and_count_multi_unit_once() {
    let source = r##"(kicad_sch (version 20250114) (uuid root)
   (symbol (lib_id "Test:Chip") (uuid a) (unit 1) (property "Reference" "U1") (property "Value" "Generic") (property "MPN" "REAL-CHIP") (property "Footprint" "QFN"))
   (symbol (lib_id "Test:Chip") (uuid b) (unit 2) (property "Reference" "U1") (property "Value" "Generic") (property "MPN" "REAL-CHIP") (property "Footprint" "QFN"))
   (symbol (lib_id "power:GND") (uuid c) (property "Reference" "#PWR01") (property "Value" "GND"))
   (symbol (lib_id "Test:R") (uuid d) (dnp yes) (property "Reference" "R1") (property "Value" "1k"))
   (symbol (lib_id "Test:R") (uuid e) (in_bom no) (property "Reference" "R2") (property "Value" "1k")))"##;
    let bundle = inventory_kicad::load_project("demo.kicad_pro", |p| {
        Ok(if p.ends_with(".kicad_pro") {
            "{}".into()
        } else {
            source.into()
        })
    })
    .unwrap();
    let rows = requested_items(&bundle).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].quantity, 1);
    assert_eq!(rows[0].value, "REAL-CHIP");
    assert_eq!(rows[0].designators, "U1");
}

#[test]
fn kicad_requests_cannot_be_edited_deleted_or_replaced_by_csv_but_manual_rows_can() {
    let mut d = data();
    d.sync_kicad_requests("Demo", &[item("CHIP", "QFN", "TP_3V1", 1)])
        .unwrap();
    let id = d.bom[0].id.clone();
    let before = serde_json::to_value(&d).unwrap();
    assert!(
        d.save_request("Demo", Some(&id), item("CHANGED", "QFN", "U1", 2))
            .unwrap_err()
            .to_string()
            .contains("KiCad")
    );
    assert!(
        d.delete_request(&id)
            .unwrap_err()
            .to_string()
            .contains("KiCad")
    );
    let rows = vec![vispace::csv_io::ReviewRow {
        item: item("CHANGED", "QFN", "TP_3V1", 1),
        candidates: vec![],
        selected: None,
    }];
    assert!(
        vispace::csv_io::apply_bom(&mut d, "Demo", &rows)
            .unwrap_err()
            .to_string()
            .contains("KiCad")
    );
    assert_eq!(serde_json::to_value(&d).unwrap(), before);
    d.save_request("Demo", None, item("MANUAL", "SOT", "U1", 1))
        .unwrap();
    let manual = d.bom.last().unwrap().id.clone();
    d.save_request("Demo", Some(&manual), item("EDITED", "SOT", "U1", 2))
        .unwrap();
    d.delete_request(&manual).unwrap();
    assert_eq!(d.bom.len(), 1);
    d.validate().unwrap();
}

#[test]
fn api_cannot_modify_kicad_requests() {
    let folder = tempfile::tempdir().unwrap();
    let mut store = Store::open(folder.path().join("inventory.sqlite")).unwrap();
    store
        .change(|d| {
            d.ensure_project("Demo")?;
            d.sync_kicad_requests("Demo", &[item("CHIP", "QFN", "TP_3V1", 1)])?;
            Ok(())
        })
        .unwrap();
    let id = store.data.bom[0].id.clone();
    let before = serde_json::to_value(&store.data).unwrap();
    for method in ["PATCH", "DELETE"] {
        let error = vispace::api::handle_request(
            &mut store,
            method,
            &format!("/v1/projects/Demo/requested/{id}"),
            serde_json::json!({"value":"changed"}),
        )
        .unwrap_err();
        assert!(error.to_string().contains("KiCad"));
    }
    assert_eq!(serde_json::to_value(&store.data).unwrap(), before);
}

#[test]
fn custom_kicad_references_flow_through_parser_bom_and_validation() {
    assert_eq!(
        references("TP_3V1, TP_5V0, LED_STATUS1, U1A, R1-R3")
            .unwrap()
            .len(),
        7
    );
    for invalid in [
        "invalid",
        "R3-R1",
        "R1-C3",
        "R1-R999999999",
        "TP_3V1-TP_5V0",
        "<bad1>",
    ] {
        assert!(references(invalid).is_err(), "{invalid}");
    }
    let source = r#"(kicad_sch (version 20250114) (uuid root) (symbol (lib_id "Test:Point") (uuid tp) (property "Reference" "TP_3V1") (property "Value" "TestPoint")))"#;
    let bundle = inventory_kicad::load_project("demo.kicad_pro", |p| {
        Ok(if p.ends_with(".kicad_pro") {
            "{}".into()
        } else {
            source.into()
        })
    })
    .unwrap();
    let rows = requested_items(&bundle).unwrap();
    let mut d = data();
    d.sync_kicad_requests("Demo", &rows).unwrap();
    d.validate().unwrap();
    assert_eq!(d.bom[0].item.designators, "TP_3V1");
}

#[test]
fn unnamed_symbols_are_skipped_and_mpn_only_parts_are_retained() {
    let source = r#"(kicad_sch (version 20250114) (uuid root)
    (symbol (lib_id "Test:Chip") (uuid blank) (property "Reference" "U5") (property "Value" "  "))
    (symbol (lib_id "Test:Chip") (uuid unannotated) (property "Reference" "U?") (property "Value" ""))
    (symbol (lib_id "Test:Chip") (uuid named) (property "Reference" "U1") (property "Value" "CHIP"))
    (symbol (lib_id "Test:R") (uuid mpn) (property "Reference" "R1") (property "Value" "") (property "MPN" "RESISTOR-MPN")))"#;
    let bundle = inventory_kicad::load_project("demo.kicad_pro", |p| {
        Ok(if p.ends_with(".kicad_pro") {
            "{}".into()
        } else {
            source.into()
        })
    })
    .unwrap();
    let rows = requested_items(&bundle).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r.value == "CHIP"));
    assert!(rows.iter().any(|r| r.value == "RESISTOR-MPN"));
    let mut d = data();
    d.sync_kicad_requests("Demo", &rows).unwrap();
    d.validate().unwrap();
}

#[test]
fn excluded_symbols_and_sheets_are_removed_from_generated_requested_rows() {
    let build = |excluded: bool| {
        inventory_kicad::load_project("demo.kicad_pro",|path|Ok(match path {
        "demo.kicad_pro"=>"{}".into(),
        "demo.kicad_sch"=>format!(r#"(kicad_sch (version 20250114) (uuid root)
          (symbol (lib_id "Test:R") (uuid r1) (in_bom {}) (property "Reference" "R1") (property "Value" "1k") (property "Footprint" "0603"))
          (sheet (uuid child) (in_bom {}) (property "Sheetname" "Child") (property "Sheetfile" "child.kicad_sch")))"#,if excluded{"no"}else{"yes"},if excluded{"no"}else{"yes"}),
        "child.kicad_sch"=>r#"(kicad_sch (version 20250114) (uuid sheet) (symbol (lib_id "Test:C") (uuid c1) (property "Reference" "C1") (property "Value" "1uF") (property "Footprint" "0402")))"#.into(),
        "demo.kicad_pcb"=>return Err(inventory_kicad::Error::new("No board in this fixture")),
        _=>panic!("{path}"),
    })).unwrap()
    };
    let mut data = Inventory::default();
    data.ensure_project("Demo").unwrap();
    let rows = requested_items(&build(false)).unwrap();
    assert_eq!(rows.len(), 2);
    data.sync_kicad_requests("Demo", &rows).unwrap();
    assert_eq!(data.bom.len(), 2);
    let rows = requested_items(&build(true)).unwrap();
    assert!(rows.is_empty());
    data.sync_kicad_requests("Demo", &rows).unwrap();
    assert!(data.bom.is_empty());
}
