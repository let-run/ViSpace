use vispace::{model::*, store::Store};
#[test]
fn project_rich_notes_are_saved_independently_and_reopened() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("inventory.sqlite");
    let mut store = Store::open(&path).unwrap();
    let inventory = Part {
        number: "INVENTORY".into(),
        notes: Notes {
            text: "Inventory notes".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let purchase = Part {
        number: "CART".into(),
        stock: 1,
        notes: Notes {
            text: "Purchase notes".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    store
        .change(|d| {
            d.ensure_project("Clock / sensor")?;
            d.ensure_project("Other")?;
            d.save_part(inventory.clone())?;
            d.purchases.push(Purchase {
                part: purchase.clone(),
                assignments: vec![],
            });
            Ok(())
        })
        .unwrap();
    let image = folder.path().join("note.png");
    image::RgbaImage::from_pixel(2, 1, image::Rgba([255, 0, 0, 255]))
        .save(&image)
        .unwrap();
    let managed = store.attach(&image, "project-notes-test").unwrap();
    let mut notes = Notes {
        text: "Project equation \\(V=IR\\)".into(),
        rtf: "e1xccnRmMSBub3Rlc30=".into(),
        ..Default::default()
    };
    notes
        .images
        .insert("image.png".into(), managed.to_string_lossy().into());
    notes.extra.insert(
        "visualHTML".into(),
        serde_json::json!("<b>Project equation</b>"),
    );
    notes.extra.insert(
        "visualTokens".into(),
        serde_json::json!([{"kind":"math","value":"V=IR"},{"kind":"image","width":480}]),
    );
    store
        .change(|d| {
            d.save_notes(
                &NotesTarget::Project("CLOCK / SENSOR".into()),
                notes.clone(),
            )
        })
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .data
            .projects
            .iter()
            .find(|p| p.name == "Clock / sensor")
            .unwrap()
            .notes,
        notes
    );
    assert!(
        store
            .data
            .projects
            .iter()
            .find(|p| p.name == "Other")
            .unwrap()
            .notes
            .text
            .is_empty()
    );
    assert_eq!(store.data.parts[0].notes, inventory.notes);
    assert_eq!(store.data.purchases[0].part.notes, purchase.notes);
    assert!(managed.is_file());
    store
        .change(|d| d.rename_project("Clock / sensor", "Renamed", "Description"))
        .unwrap();
    assert_eq!(
        store
            .data
            .projects
            .iter()
            .find(|p| p.name == "Renamed")
            .unwrap()
            .notes,
        notes
    );
    let previous = store
        .data
        .projects
        .iter()
        .find(|p| p.name == "Renamed")
        .unwrap()
        .notes
        .clone();
    assert!(
        store
            .change(|d| d.save_notes(&NotesTarget::Project("Missing".into()), Notes::default()))
            .is_err()
    );
    assert_eq!(
        store
            .data
            .projects
            .iter()
            .find(|p| p.name == "Renamed")
            .unwrap()
            .notes,
        previous
    );
}
#[test]
fn note_targets_do_not_change_part_or_purchase_identity() {
    let mut data = Inventory::default();
    let part = Part {
        number: "A".into(),
        stock: 12,
        ..Default::default()
    };
    data.parts.push(part.clone());
    let cart = Part {
        number: "B".into(),
        stock: 3,
        ..Default::default()
    };
    data.purchases.push(Purchase {
        part: cart.clone(),
        assignments: vec![],
    });
    data.save_notes(
        &NotesTarget::Part(part.id.clone()),
        Notes {
            text: "Part".into(),
            ..Default::default()
        },
    )
    .unwrap();
    data.save_notes(
        &NotesTarget::Purchase(cart.id.clone()),
        Notes {
            text: "Cart".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(data.parts[0].stock, 12);
    assert_eq!(data.parts[0].barcode, part.barcode);
    assert_eq!(data.parts[0].notes.text, "Part");
    assert_eq!(data.purchases[0].part.stock, 3);
    assert_eq!(data.purchases[0].part.barcode, cart.barcode);
    assert_eq!(data.purchases[0].part.notes.text, "Cart");
}
