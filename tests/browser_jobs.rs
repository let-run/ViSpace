use vispace::{
    browser_jobs::{Input, run_worker_job},
    csv_import::CsvImport,
    model::Inventory,
};
#[test]
fn browser_csv_worker_round_trip_preserves_custom_fields_and_labels() {
    let request = Input::Csv {
        bytes: b"Part Number,Qty,Labels,Custom voltage\nWEB-R1,12,Demo,16V\n".to_vec(),
        filename: "preview.csv".into(),
    };
    let result: serde_json::Value =
        serde_json::from_str(&run_worker_job(&serde_json::to_string(&request).unwrap())).unwrap();
    assert!(result.get("error").is_none());
    let document: CsvImport = serde_json::from_value(result["value"].clone()).unwrap();
    assert_eq!(document.rows.len(), 1);
    let request = Input::CsvApply {
        document,
        suggestions: vec!["Demo".into()],
        data: Inventory::default(),
    };
    let result: serde_json::Value =
        serde_json::from_str(&run_worker_job(&serde_json::to_string(&request).unwrap())).unwrap();
    let data: Inventory = serde_json::from_value(result["value"].clone()).unwrap();
    assert_eq!(data.parts[0].number, "WEB-R1");
    assert_eq!(data.parts[0].stock, 12);
    assert_eq!(data.parts[0].labels, vec!["Demo"]);
    assert_eq!(data.parts[0].custom_fields["Custom voltage"], "16V");
}
#[test]
fn browser_worker_reports_bad_input_as_a_result() {
    let result: serde_json::Value = serde_json::from_str(&run_worker_job("invalid")).unwrap();
    assert!(result["error"].as_str().is_some());
}

#[test]
fn browser_kicad_worker_returns_renderable_hierarchy() {
    let request = Input::Kicad {
        path: "demo.kicad_pro".into(),
        folder: None,
        files: std::collections::BTreeMap::from([
            (
                "demo.kicad_pro".into(),
                include_str!("../crates/kicad-reader/tests/fixtures/demo.kicad_pro").into(),
            ),
            (
                "demo.kicad_sch".into(),
                include_str!("../crates/kicad-reader/tests/fixtures/demo.kicad_sch").into(),
            ),
            (
                "power.kicad_sch".into(),
                include_str!("../crates/kicad-reader/tests/fixtures/power.kicad_sch").into(),
            ),
        ]),
    };
    let result: serde_json::Value =
        serde_json::from_str(&run_worker_job(&serde_json::to_string(&request).unwrap())).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    let bundle: inventory_kicad::Bundle = serde_json::from_value(result["value"].clone()).unwrap();
    assert_eq!(bundle.nodes().len(), 4);
    assert_eq!(bundle.schematics.len(), 2);
    assert!(
        bundle.root.children[2]
            .error
            .as_ref()
            .unwrap()
            .contains("Sheet not supplied")
    );
    assert!(!bundle.schematics["power.kicad_sch"].primitives.is_empty());
}

#[test]
fn browser_notes_pdf_worker_preserves_unicode_outline_and_links() {
    use base64::Engine;
    let request = Input::NotesPdf {
        source: "# Résumé\n\nOhm: $R=\\frac{V}{I}$.\n\n## Measurements\n\nCurrent.".into(),
        images: Default::default(),
    };
    let result: serde_json::Value =
        serde_json::from_str(&run_worker_job(&serde_json::to_string(&request).unwrap())).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(result["value"].as_str().unwrap())
        .unwrap();
    assert!(bytes.starts_with(b"%PDF-1.4"));
    let pdf = String::from_utf8_lossy(&bytes);
    assert!(pdf.contains("/Type /Outlines"));
    assert!(pdf.contains("/Subtype /Link"));
}

#[test]
fn browser_board_loading_is_independent_of_schematic_loading() {
    use std::collections::BTreeMap;
    let files = BTreeMap::from([
        ("demo.kicad_pro".into(), "{}".into()),
        (
            "demo.kicad_sch".into(),
            "(kicad_sch (version 20250114) (uuid root))".into(),
        ),
        ("demo.kicad_pcb".into(), String::new()),
    ]);
    let request = Input::Kicad {
        path: "demo.kicad_pro".into(),
        folder: None,
        files,
    };
    let result: serde_json::Value =
        serde_json::from_str(&run_worker_job(&serde_json::to_string(&request).unwrap())).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    let bundle: inventory_kicad::Bundle = serde_json::from_value(result["value"].clone()).unwrap();
    assert!(bundle.pcb_available && bundle.pcb.is_none() && bundle.pcb_error.is_none());
    let request = Input::KicadBoard {
        path: "demo.kicad_pro".into(),
        folder: None,
        files: BTreeMap::from([
            ("demo.kicad_pro".into(), "{}".into()),
            (
                "demo.kicad_pcb".into(),
                "(kicad_pcb (version 20250114) (layers (0 \"F.Cu\" signal)))".into(),
            ),
        ]),
        stamps: vec![],
        cache: None,
    };
    let result: serde_json::Value =
        serde_json::from_str(&run_worker_job(&serde_json::to_string(&request).unwrap())).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    let board: inventory_kicad::Pcb = serde_json::from_value(result["value"].clone()).unwrap();
    assert_eq!(board.layers[0].name, "F.Cu");
}
