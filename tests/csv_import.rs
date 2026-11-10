use vispace::{csv_import::CsvImport, csv_io, model::*};
fn column(doc: &CsvImport, name: &str) -> usize {
    doc.headers.iter().position(|h| h == name).unwrap()
}
#[test]
fn custom_columns_are_editable_excludable_and_restorable() {
    let mut doc = CsvImport::read(
        "Part Number,Quantity,Voltage,Vendor note\nR1,5,16V,Keep\nR2,2,25V,Skip\n".as_bytes(),
        "parts.csv".into(),
    )
    .unwrap();
    let voltage = column(&doc, "Voltage");
    let note = column(&doc, "Vendor note");
    assert!(csv_io::part_field(&doc.headers[voltage]).is_none());
    doc.set_value(0, voltage, "50V".into());
    doc.ignored_custom.insert(note);
    doc.rows[1].selected = false;
    let selected = doc.selected_parts(&[]).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].custom_fields["Voltage"], "50V");
    assert!(!selected[0].custom_fields.contains_key("Vendor note"));
    doc.ignored_custom.remove(&note);
    assert_eq!(
        doc.selected_parts(&[]).unwrap()[0].custom_fields["Vendor note"],
        "Keep"
    );
}
#[test]
fn labels_use_imported_and_existing_suggestions_and_flush_pending_selected_rows() {
    let mut doc = CsvImport::read(
        "MPN,Qty,Labels\nA,1,\"Capacitor, capacitor, RF\"\nB,1,\"RF, Prototype\"\n".as_bytes(),
        "labels.csv".into(),
    )
    .unwrap();
    let suggestions = doc.suggestions(["CAPACITOR".into(), "Tested".into()]);
    assert_eq!(suggestions, vec!["CAPACITOR", "Tested", "RF", "Prototype"]);
    doc.add_labels(0, "tested, Tested, New label", &suggestions);
    doc.rows[0].pending_label = "prototype, Pending".into();
    doc.rows[1].pending_label = "Do not import".into();
    doc.rows[1].selected = false;
    let parts = doc.selected_parts(&suggestions).unwrap();
    assert_eq!(
        parts[0].labels,
        vec![
            "Capacitor",
            "RF",
            "Tested",
            "New label",
            "Prototype",
            "Pending"
        ]
    );
    assert!(doc.rows[0].pending_label.is_empty());
    assert_eq!(doc.rows[1].pending_label, "Do not import");
    let c = doc.label_column();
    doc.set_value(0, c, "RF".into());
    assert_eq!(doc.selected_parts(&[]).unwrap()[0].labels, vec!["RF"]);
}
#[test]
fn invalid_rows_can_be_fixed_or_skipped_in_preview() {
    let mut doc = CsvImport::read(
        "Part Number,Qty,Supplier part number,Supplier\n,broken,C123,\nGood,2,,\n".as_bytes(),
        "fix.csv".into(),
    )
    .unwrap();
    assert!(doc.selected_parts(&[]).is_err());
    doc.rows[0].selected = false;
    assert_eq!(doc.selected_parts(&[]).unwrap().len(), 1);
    doc.rows[0].selected = true;
    doc.set_value(0, 0, "Fixed".into());
    doc.set_value(0, 1, "3".into());
    doc.set_value(0, 3, "LCSC".into());
    let parts = doc.selected_parts(&[]).unwrap();
    assert_eq!(parts[0].stock, 3);
    assert_eq!(parts[0].supplier, "LCSC");
    for row in &mut doc.rows {
        row.selected = false;
    }
    assert!(doc.selected_parts(&[]).is_err());
}
#[test]
fn description_edits_refresh_detected_values_without_overwriting_manual_changes() {
    let mut doc = CsvImport::read(
        "MPN,Qty,Description\nR1,1,10kΩ ±1% Resistor 0402\n".as_bytes(),
        "analysis.csv".into(),
    )
    .unwrap();
    let value = column(&doc, "Value");
    let tolerance = column(&doc, "Tolerance");
    assert_eq!(doc.rows[0].values[tolerance], "±1%");
    doc.set_value(0, value, "Manual".into());
    doc.set_value(0, 2, "20kΩ ±5% Resistor 0402".into());
    assert_eq!(doc.rows[0].values[value], "Manual");
    assert_eq!(doc.rows[0].values[tolerance], "±5%");
    let parts = doc.selected_parts(&[]).unwrap();
    assert_eq!(parts[0].value, "Manual");
}
#[test]
fn malformed_headers_and_long_rows_are_rejected_but_short_rows_are_padded() {
    for csv in ["Name,name\nx,y\n", "MPN,\na,b\n", "MPN\na,b\n", ""] {
        assert!(CsvImport::read(csv.as_bytes(), "bad.csv".into()).is_err());
    }
    let doc = CsvImport::read("MPN,Qty,Description\nA,1\n".as_bytes(), "short.csv".into()).unwrap();
    assert!(doc.rows.iter().all(|r| r.values.len() == doc.headers.len()));
}
#[test]
fn preview_import_preserves_existing_barcode_and_merges_stock() {
    let mut inventory = Inventory::default();
    let existing = Part {
        number: "R1".into(),
        stock: 4,
        ..Default::default()
    };
    let barcode = existing.barcode.clone();
    inventory.save_part(existing).unwrap();
    let mut doc = CsvImport::read(
        "Part Number,Qty,Price\nR1,2,1.25\nNew,1,3.5\n".as_bytes(),
        "stock.csv".into(),
    )
    .unwrap();
    inventory
        .import_parts(doc.selected_parts(&[]).unwrap())
        .unwrap();
    assert_eq!(inventory.parts[0].stock, 6);
    assert_eq!(inventory.parts[0].barcode, barcode);
    assert_eq!(inventory.parts.len(), 2);
}
