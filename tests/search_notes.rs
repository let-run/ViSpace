use vispace::model::*;

#[test]
fn part_search_uses_swift_fields_and_requires_every_word() {
    let mut part = Part {
        number: "R10".into(),
        manufacturer: "Yageo".into(),
        description: "Thick film".into(),
        storage: "DrawerSecret".into(),
        ..Default::default()
    };
    part.labels.push("Resistor".into());
    part.custom_fields.insert("Voltage".into(), "50V".into());
    part.notes.text = "NoteSecret".into();
    assert!(part.matches("  YAGEO\tfilm\n50v resistor "));
    assert!(!part.matches("yageo capacitor"));
    assert!(!part.matches("DrawerSecret"));
    assert!(!part.matches("NoteSecret"));
    assert!(part.matches(" \t "));
}
#[test]
fn purchase_search_includes_assignments_but_not_storage_custom_fields_or_notes() {
    let mut part = Part {
        number: "CartR".into(),
        storage: "DrawerSecret".into(),
        ..Default::default()
    };
    part.custom_fields
        .insert("CustomSecret".into(), "value".into());
    part.notes.text = "NoteSecret".into();
    let purchase = Purchase {
        part,
        assignments: vec![Assignment {
            id: id(),
            project: "Power Board".into(),
            quantity: 2,
            designators: "R1, R2".into(),
        }],
    };
    assert!(purchase.matches("cartr POWER r2"));
    assert!(!purchase.matches("power r3"));
    for excluded in ["DrawerSecret", "CustomSecret", "NoteSecret"] {
        assert!(!purchase.matches(excluded));
    }
}
#[test]
fn notes_presence_matches_swift_for_whitespace_and_image_only_notes() {
    let mut notes = Notes {
        text: " \n\t".into(),
        rtf: "legacy metadata".into(),
        ..Default::default()
    };
    assert!(notes.is_empty());
    notes.images.insert("picture".into(), "image.png".into());
    assert!(!notes.is_empty());
    notes.images.clear();
    notes.text = "A note".into();
    assert!(!notes.is_empty());
}
#[test]
fn part_order_compares_numeric_runs_without_overflow() {
    let mut numbers = ["R10", "r2", "R1", "R99999999999999999999999999"];
    numbers.sort_by(|a, b| part_number_cmp(a, b));
    assert_eq!(numbers, ["R1", "r2", "R10", "R99999999999999999999999999"]);
}
