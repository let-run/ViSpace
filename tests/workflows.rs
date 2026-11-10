use vispace::{api, csv_io, model::*, store::Store};
use serde_json::json;
use tempfile::TempDir;

fn part(number: &str, stock: i64) -> Part {
    Part {
        number: number.into(),
        stock,
        ..Default::default()
    }
}
fn store() -> (TempDir, Store) {
    let temp = TempDir::new().unwrap();
    let store = Store::open(temp.path().join("inventory.sqlite")).unwrap();
    (temp, store)
}
#[test]
fn stock_and_allocation_limits_are_atomic() {
    let (_temp, mut s) = store();
    let p = part("R-4K7", 10);
    let id = p.id.clone();
    s.change(|d| d.save_part(p)).unwrap();
    s.change(|d| {
        d.allocate(&id, "Amplifier", 6, "R1-R6")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(s.data.available(&s.data.parts[0]), 4);
    assert!(
        s.change(|d| {
            d.allocate(&id, "Other", 5, "")?;
            Ok(())
        })
        .is_err()
    );
    assert_eq!(s.data.projects.len(), 1);
    let mut edited = s.data.parts[0].clone();
    edited.stock = 5;
    assert!(s.change(|d| d.save_part(edited)).is_err());
    assert_eq!(s.data.parts[0].stock, 10);
    let a = s.data.allocations[0].id.clone();
    s.change(|d| {
        d.release(&a);
        Ok(())
    })
    .unwrap();
    assert_eq!(s.data.available(&s.data.parts[0]), 10);
}
#[test]
fn failed_batch_does_not_commit_valid_prefix() {
    let (_temp, mut s) = store();
    let p = part("existing", 3);
    let mut duplicate = part("duplicate", 4);
    duplicate.barcode = p.barcode.clone();
    s.change(|d| d.save_part(p)).unwrap();
    assert!(
        s.change(|d| d.import_parts(vec![part("valid", 10), duplicate]))
            .is_err()
    );
    assert_eq!(s.data.parts.len(), 1);
    let path = s.path.clone();
    drop(s);
    let reloaded = Store::open(&path).unwrap();
    assert_eq!(reloaded.data.parts.len(), 1);
}
#[test]
fn csv_merges_preserving_identity_and_metadata() {
    let (_temp, mut s) = store();
    let mut p = part("ABC", 7);
    p.description = "Keep original".into();
    let id = p.id.clone();
    let barcode = p.barcode.clone();
    s.change(|d| d.save_part(p)).unwrap();
    let parts=csv_io::read_parts("Part Number,Quantity,Description,Labels,Unit Price\n abc ,5,Replacement,Resistor,0.02\nABC,2,Other,Passive,0.03\n".as_bytes()).unwrap();
    s.change(|d| d.import_parts(parts)).unwrap();
    let p = &s.data.parts[0];
    assert_eq!(p.stock, 14);
    assert_eq!(p.id, id);
    assert_eq!(p.barcode, barcode);
    assert_eq!(p.description, "Keep original");
    assert_eq!(p.labels.len(), 2);
}
#[test]
fn supplier_conflicts_reject_import() {
    let (_temp, mut s) = store();
    let a = part("A", 3);
    let mut b = part("B", 4);
    b.spn = "C123".into();
    b.supplier = "LCSC".into();
    s.change(|d| {
        d.save_part(a)?;
        d.save_part(b)
    })
    .unwrap();
    let mut incoming = part("A", 5);
    incoming.spn = "C123".into();
    incoming.supplier = "lcsc".into();
    assert!(s.change(|d| d.import_parts(vec![incoming])).is_err());
    assert_eq!(s.data.parts[0].stock, 3);
}
#[test]
fn supplier_csv_roundtrip_and_lcsc_header() {
    let input = "Manufacture Part Number,Quantity,LCSC Part #,Description,Unit Price\nRC0402,20,C100,resistor 4.7kOhm 1%,0.01\n";
    let parts = csv_io::read_parts(input.as_bytes()).unwrap();
    assert_eq!(parts[0].spn, "C100");
    assert_eq!(parts[0].supplier, "LCSC");
    // Swift normalizes unit spellings and only infers explicitly signed tolerance.
    assert_eq!(parts[0].value, "4.7kΩ");
    assert!(parts[0].tolerance.is_empty());
    let mut bytes = vec![];
    csv_io::write_parts(&mut bytes, &parts).unwrap();
    let again = csv_io::read_parts(&bytes[..]).unwrap();
    assert_eq!(again[0].custom_fields["Unit Price"], "0.01");
    assert_eq!(again[0].spn, "C100");
    assert_eq!(again[0].value, "4.7kΩ");
}
#[test]
fn purchases_receive_once_with_reservations() {
    let (_temp, mut s) = store();
    let mut p = part("CAP", 10);
    p.value = "100nF".into();
    let pid = p.id.clone();
    s.change(|d| {
        d.ensure_project("Board")?;
        d.purchases.push(Purchase {
            part: p,
            assignments: vec![Assignment {
                id: id(),
                project: "Board".into(),
                quantity: 3,
                designators: "C1-C3".into(),
            }],
        });
        Ok(())
    })
    .unwrap();
    s.change(|d| {
        d.receive(std::slice::from_ref(&pid))?;
        Ok(())
    })
    .unwrap();
    assert!(s.data.purchases.is_empty());
    assert_eq!(s.data.parts[0].stock, 10);
    assert_eq!(s.data.available(&s.data.parts[0]), 7);
    assert!(
        s.change(|d| {
            d.receive(&[pid])?;
            Ok(())
        })
        .is_err()
    );
    assert_eq!(s.data.parts[0].stock, 10);
}
#[test]
fn invalid_purchase_assignment_rolls_back() {
    let (_temp, mut s) = store();
    assert!(
        s.change(|d| {
            d.ensure_project("Board")?;
            d.purchases.push(Purchase {
                part: part("X", 2),
                assignments: vec![Assignment {
                    id: id(),
                    project: "Board".into(),
                    quantity: 3,
                    designators: String::new(),
                }],
            });
            Ok(())
        })
        .is_err()
    );
    assert!(s.data.projects.is_empty());
    assert!(s.data.purchases.is_empty());
}
#[test]
fn project_rename_and_delete_update_all_relations() {
    let (_temp, mut s) = store();
    let p = part("A", 20);
    let pid = p.id.clone();
    s.change(|d| {
        d.save_part(p)?;
        d.allocate(&pid, "Old", 5, "")?;
        d.purchases.push(Purchase {
            part: part("B", 4),
            assignments: vec![Assignment {
                id: id(),
                project: "Old".into(),
                quantity: 2,
                designators: String::new(),
            }],
        });
        Ok(())
    })
    .unwrap();
    s.change(|d| d.rename_project("Old", "New", "Board"))
        .unwrap();
    assert_eq!(s.data.allocations[0].project, "New");
    assert_eq!(s.data.purchases[0].assignments[0].project, "New");
    s.change(|d| {
        d.delete_project("New");
        Ok(())
    })
    .unwrap();
    assert!(s.data.allocations.is_empty());
    assert!(s.data.purchases[0].assignments.is_empty());
    assert_eq!(s.data.available(&s.data.parts[0]), 20);
}
#[test]
fn bom_shortage_release_and_reimport_subset() {
    let (_temp, mut s) = store();
    let mut p = part("RC", 3);
    p.value = "4.7kOhm".into();
    p.package = "0402".into();
    p.labels = vec!["Resistor".into()];
    let pid = p.id.clone();
    s.change(|d| {
        d.save_part(p)?;
        d.ensure_project("Board")?;
        Ok(())
    })
    .unwrap();
    let mut rows = csv_io::read_bom(
        "Designator,Footprint,Quantity,Value\nR1-R5,R_0402,5,4K7\n".as_bytes(),
        &s.data,
        "Board",
    )
    .unwrap();
    assert_eq!(rows[0].candidates, vec![pid.clone()]);
    rows[0].selected = Some(pid.clone());
    s.change(|d| csv_io::apply_bom(d, "Board", &rows)).unwrap();
    assert_eq!(s.data.allocated(&pid), 3);
    assert_eq!(s.data.requested(&s.data.bom[0]), 2);
    let mut export = vec![];
    csv_io::write_requested(&mut export, &s.data, "Board").unwrap();
    assert!(String::from_utf8(export).unwrap().contains("R4, R5"));
    let mut subset = csv_io::read_bom(
        "Designator,Footprint,Quantity,Value\nR2-R3,R_0402,2,4K7\n".as_bytes(),
        &s.data,
        "Board",
    )
    .unwrap();
    subset[0].selected = Some(pid.clone());
    s.change(|d| csv_io::apply_bom(d, "Board", &subset))
        .unwrap();
    assert_eq!(s.data.allocated(&pid), 3);
    assert_eq!(
        s.data.bom.iter().map(|e| s.data.requested(e)).sum::<i64>(),
        2
    );
    let ids = s
        .data
        .allocations
        .iter()
        .map(|a| a.id.clone())
        .collect::<Vec<_>>();
    s.change(|d| {
        for id in ids {
            d.release(&id);
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        s.data.bom.iter().map(|e| s.data.requested(e)).sum::<i64>(),
        5
    );
}
#[test]
fn designator_ranges_and_numeric_notation() {
    assert_eq!(references("r1-r3, R2; R7").unwrap().len(), 4);
    assert!(references("R3-R1").is_err());
    assert!(references("R1-C5").is_err());
    assert!(references("R1-R999999999").is_err());
    assert!(references("invalid").is_err());
    assert_eq!(csv_io::magnitude("4K7"), Some(4700.));
    assert_eq!(csv_io::magnitude("4.7kOhm"), Some(4700.));
    assert!((csv_io::magnitude("100nF").unwrap() - 1e-7).abs() < 1e-18);
}
#[test]
fn api_uses_same_transaction_guards_and_preserves_omitted_fields() {
    let (_temp, mut s) = store();
    let (_, value) = s
        .change(|d| {
            api::route(
                d,
                "POST",
                "/v1/parts",
                json!({"number":"ABC","stock":10,"manufacturer":"Maker"}),
            )
        })
        .unwrap();
    let id = value["id"].as_str().unwrap();
    s.change(|d| {
        api::route(
            d,
            "POST",
            "/v1/projects/USB%20Hub/allocations",
            json!({"partID":id,"quantity":4}),
        )
    })
    .unwrap();
    assert!(
        s.change(|d| api::route(d, "PATCH", &format!("/v1/parts/{id}"), json!({"stock":2})))
            .is_err()
    );
    assert_eq!(s.data.parts[0].stock, 10);
    s.change(|d| {
        api::route(
            d,
            "PATCH",
            &format!("/v1/parts/{id}"),
            json!({"storage":"Drawer"}),
        )
    })
    .unwrap();
    assert_eq!(s.data.parts[0].manufacturer, "Maker");
    assert_eq!(s.data.parts[0].storage, "Drawer");
    assert_eq!(s.data.projects[0].name, "USB Hub");
}
#[test]
fn swift_import_preserves_unknown_data_and_relocates_attachments() {
    let source = TempDir::new().unwrap();
    let path = source.path().join("inventory.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE parts(id TEXT PRIMARY KEY,barcode TEXT,payload TEXT);CREATE TABLE projects(name TEXT PRIMARY KEY,description TEXT);CREATE TABLE project_notes(project TEXT,attachment_id TEXT,payload TEXT);").unwrap();
    let attachments = source.path().join("Attachments/P1");
    std::fs::create_dir_all(&attachments).unwrap();
    let pdf = attachments.join("sheet.pdf");
    std::fs::write(&pdf, b"%PDF-1.4 fixture").unwrap();
    let raw = json!({"id":"P1","number":"Legacy","manufacturer":"M","stock":12,"barcode":"B1","lcsc":"C1","category":"Resistor","datasheet":pdf,"futureMetadata":{"keep":true},"notes":{"text":"Note","rtf":"aGVsbG8=","images":{}}});
    conn.execute("INSERT INTO parts VALUES ('P1','B1',?1)", [raw.to_string()])
        .unwrap();
    conn.execute("INSERT INTO projects VALUES ('Board','Description')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO project_notes VALUES ('Board','project-1',?1)",
        [json!({"text":"Board note","rtf":"","images":{}}).to_string()],
    )
    .unwrap();
    drop(conn);
    let (_temp, mut s) = store();
    s.import_swift(&path).unwrap();
    let p = &s.data.parts[0];
    assert_eq!(p.spn, "C1");
    assert_eq!(p.labels, vec!["Resistor"]);
    assert_eq!(p.extra["futureMetadata"]["keep"], true);
    assert_ne!(p.datasheet, pdf.to_string_lossy());
    assert!(std::path::Path::new(&p.datasheet).exists());
    assert!(pdf.exists());
    assert_eq!(s.data.projects[0].notes.text, "Board note");
    assert!(s.import_swift(&path).is_err());
    let source = rusqlite::Connection::open(&path).unwrap();
    let unchanged: String = source
        .query_row("SELECT payload FROM parts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&unchanged).unwrap(),
        raw
    );
}
#[test]
fn backup_is_a_consistent_snapshot_with_attachments() {
    let (temp, mut s) = store();
    let p = part("A", 5);
    s.change(|d| d.save_part(p)).unwrap();
    let attachments = temp.path().join("Attachments/test");
    std::fs::create_dir_all(&attachments).unwrap();
    std::fs::write(attachments.join("note.txt"), "fixture").unwrap();
    let folder = temp.path().join("backup");
    s.backup(&folder).unwrap();
    let restored = Store::open(folder.join("inventory.sqlite")).unwrap();
    assert_eq!(restored.data.parts[0].stock, 5);
    assert!(folder.join("Attachments/test/note.txt").exists());
    assert!(s.backup(&folder).is_err());
}
#[test]
fn search_is_case_insensitive_and_matches_tokens_across_fields() {
    let mut p = part("ABC", 5);
    p.manufacturer = "Yageo".into();
    p.labels = vec!["Resistor".into()];
    p.spn = "C100".into();
    assert!(p.matches("yageo RESISTOR abc"));
    assert!(p.matches("C100"));
    assert!(!p.matches("Yageo capacitor"));
}

#[test]
fn enormous_quantities_are_rejected_without_overflow_or_poisoning() {
    let (_temp, mut s) = store();
    let p = part("MAX", i64::MAX);
    let pid = p.id.clone();
    s.change(|d| d.save_part(p)).unwrap();
    s.change(|d| {
        d.allocate(&pid, "A", i64::MAX, "")?;
        Ok(())
    })
    .unwrap();
    assert!(
        s.change(|d| {
            d.allocations.push(Allocation {
                id: id(),
                part_id: pid.clone(),
                project: "A".into(),
                quantity: 1,
                designators: String::new(),
            });
            Ok(())
        })
        .is_err()
    );
    assert_eq!(s.data.allocated(&pid), i64::MAX);
    assert!(
        s.change(|d| {
            d.purchases.push(Purchase {
                part: part("Cart", i64::MAX),
                assignments: vec![
                    Assignment {
                        id: id(),
                        project: "A".into(),
                        quantity: i64::MAX,
                        designators: String::new(),
                    },
                    Assignment {
                        id: id(),
                        project: "A".into(),
                        quantity: 1,
                        designators: String::new(),
                    },
                ],
            });
            Ok(())
        })
        .is_err()
    );
    s.change(|d| {
        d.ensure_project("Still working")?;
        Ok(())
    })
    .unwrap();
}
#[test]
fn clipboard_pixels_are_encoded_and_thumbnails_keep_their_aspect_ratio() {
    use vispace::attachments::AttachmentDraft;
    let draft = AttachmentDraft::new().unwrap();
    let mut bytes = vec![0_u8; 320 * 80 * 4];
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[42, 108, 184, 128]);
    }
    let metadata = draft.rgba(320, 80, bytes).unwrap();
    let original = image::open(metadata["originalPath"].as_str().unwrap()).unwrap();
    let thumbnail = image::open(metadata["thumbnailPath"].as_str().unwrap()).unwrap();
    assert_eq!((original.width(), original.height()), (320, 80));
    assert_eq!((thumbnail.width(), thumbnail.height()), (160, 40));
    assert_eq!(original.to_rgba8().get_pixel(0, 0).0, [42, 108, 184, 128]);
    assert!(draft.rgba(20, 20, vec![0; 4]).is_err());
    assert!(draft.rgba(usize::MAX, 2, vec![]).is_err());
    assert!(draft.rgba(0, 0, vec![]).is_err());
}
#[test]
fn cancelled_attachment_drafts_are_removed_and_source_files_are_preserved() {
    use vispace::attachments::AttachmentDraft;
    let source = TempDir::new().unwrap();
    let path = source.path().join("source.png");
    image::RgbaImage::from_pixel(30, 60, image::Rgba([90, 80, 70, 255]))
        .save(&path)
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    let staged = {
        let draft = AttachmentDraft::new().unwrap();
        let metadata = draft.import_image(&path).unwrap();
        let staged = std::path::PathBuf::from(metadata["originalPath"].as_str().unwrap());
        assert!(staged.exists());
        staged
    };
    assert!(!staged.exists());
    assert_eq!(std::fs::read(path).unwrap(), original);
}
#[test]
fn attachment_import_rejects_invalid_images_and_pdfs() {
    use vispace::attachments::AttachmentDraft;
    let draft = AttachmentDraft::new().unwrap();
    assert!(draft.image_bytes(b"not an image").is_err());
    let source = TempDir::new().unwrap();
    let path = source.path().join("fake.pdf");
    std::fs::write(&path, b"<html>error</html>").unwrap();
    assert!(draft.import_pdf(&path).is_err());
    std::fs::write(&path, b"%PDF-1.4 test fixture").unwrap();
    let staged = draft.import_pdf(&path).unwrap();
    assert!(staged.starts_with(draft.root()));
    assert!(path.exists());
}
