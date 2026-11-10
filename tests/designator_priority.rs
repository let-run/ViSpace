use vispace::{api::route, model::*, requests::RequestReview, store::Store};
use serde_json::json;
use std::collections::BTreeMap;
fn fixture() -> Inventory {
    let mut d = Inventory::default();
    d.ensure_project("Board").unwrap();
    d.ensure_project("Other").unwrap();
    d.save_part(Part {
        number: "RES".into(),
        stock: 10,
        ..Default::default()
    })
    .unwrap();
    d.save_request(
        "Board",
        None,
        BomItem {
            value: "RES".into(),
            designators: "R1-R4".into(),
            quantity: 4,
            ..Default::default()
        },
    )
    .unwrap();
    d.purchases.push(Purchase {
        part: Part {
            number: "BUY".into(),
            stock: 10,
            ..Default::default()
        },
        assignments: vec![],
    });
    d
}
fn assign(d: &mut Inventory, refs: &str, project: &str) {
    d.purchases[0].assignments.push(Assignment {
        id: id(),
        project: project.into(),
        quantity: references(refs).unwrap().len() as i64,
        designators: refs.into(),
    });
}
#[test]
fn purchase_hides_only_covered_references_and_release_restores_source() {
    let mut d = fixture();
    d.bom[0].kicad_source = Some("board.kicad_sch".into());
    assign(&mut d, "r1, R3", "Board");
    let pending = d.requested_items();
    assert_eq!(pending[&d.bom[0].id].designators, "R2, R4");
    assert_eq!(d.requested(&d.bom[0]), 2);
    assert_eq!(d.bom[0].item.designators, "R1-R4");
    let (_, body) = route(&mut d, "GET", "/v1/projects/Board/requested", json!({})).unwrap();
    assert_eq!(body[0]["designators"], "R2, R4");
    assert_eq!(body[0]["quantity"], 2);
    let mut csv = Vec::new();
    vispace::csv_io::write_requested(&mut csv, &d, "Board").unwrap();
    assert!(String::from_utf8(csv).unwrap().contains("\"R2, R4\""));
    d.purchases[0].assignments.clear();
    assert_eq!(d.requested(&d.bom[0]), 4);
    assign(&mut d, "R1-R4", "Other");
    assert_eq!(d.requested(&d.bom[0]), 4);
}
#[test]
fn allocation_wins_and_persists_without_decreasing_purchase_quantity() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("inventory.sqlite")).unwrap();
    store
        .change(|d| {
            *d = fixture();
            assign(d, "R1-R3", "Board");
            Ok(())
        })
        .unwrap();
    let part = store.data.parts[0].id.clone();
    let allocation = store
        .change(|d| d.allocate(&part, "Board", 1, "r2"))
        .unwrap();
    assert_eq!(
        references(&store.data.purchases[0].assignments[0].designators).unwrap(),
        references("R1,R3").unwrap()
    );
    assert_eq!(store.data.purchases[0].assignments[0].quantity, 3);
    store
        .change(|d| {
            let a = d
                .allocations
                .iter_mut()
                .find(|a| a.id == allocation)
                .unwrap();
            a.designators = "R1-R2".into();
            a.quantity = 2;
            Ok(())
        })
        .unwrap();
    assert_eq!(store.data.purchases[0].assignments[0].designators, "R3");
    assert_eq!(store.data.requested(&store.data.bom[0]), 1);
    let path = store.path.clone();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.data.purchases[0].assignments[0].designators, "R3");
    store
        .change(|d| {
            d.release(&allocation);
            Ok(())
        })
        .unwrap();
    assert_eq!(store.data.requested(&store.data.bom[0]), 3);
}
#[test]
fn purchase_cannot_claim_allocated_references_via_ui_store_or_api() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("inventory.sqlite")).unwrap();
    store
        .change(|d| {
            *d = fixture();
            let p = d.parts[0].id.clone();
            d.allocate(&p, "Board", 1, "R2")?;
            assign(d, "R1", "Board");
            Ok(())
        })
        .unwrap();
    let revision = store.revision();
    let error = store
        .change(|d| {
            d.purchases[0].assignments[0].designators = "r1-r2".into();
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("R2"));
    assert_eq!(store.revision(), revision);
    assert_eq!(store.data.purchases[0].assignments[0].designators, "R1");
    let purchase = store.data.purchases[0].part.id.clone();
    let before = serde_json::to_value(&store.data).unwrap();
    assert!(
        route(
            &mut store.data,
            "PATCH",
            &format!("/v1/projects/Board/purchase/{purchase}"),
            json!({"designators":"R2"})
        )
        .unwrap_err()
        .to_string()
        .contains("Allocated")
    );
    assert_eq!(serde_json::to_value(&store.data).unwrap(), before);
}
#[test]
fn request_sync_allocates_only_uncovered_designators_then_release_restores_them() {
    let mut d = fixture();
    assign(&mut d, "R1,R3", "Board");
    let entry = d.bom[0].id.clone();
    let part = d.parts[0].id.clone();
    let review = RequestReview::build(&d, "Board", || false).unwrap();
    assert_eq!(review.rows[0].requested, 2);
    assert_eq!(review.rows[0].entry.item.designators, "R2, R4");
    assert_eq!(
        d.sync_requests("Board", &BTreeMap::from([(entry, part)]))
            .unwrap(),
        2
    );
    assert_eq!(d.allocations[0].designators, "R2, R4");
    assert_eq!(d.requested(&d.bom[0]), 0);
    d.purchases[0].assignments.clear();
    assert_eq!(d.requested(&d.bom[0]), 2);
    assert_eq!(d.requested_items()[&d.bom[0].id].designators, "R1, R3");
    d.allocations.clear();
    assert_eq!(d.requested(&d.bom[0]), 4);
}
#[test]
fn bulk_purchase_rejects_allocated_refs_and_api_allocation_edits_take_priority() {
    let mut d = fixture();
    let part = d.parts[0].id.clone();
    let allocation = d.allocate(&part, "Board", 1, "R1").unwrap();
    let purchase = d.purchases[0].part.id.clone();
    assert!(
        d.bulk_allocate("Board", &[(purchase, 1, "r1".into())], true)
            .unwrap_err()
            .to_string()
            .contains("Allocated")
    );
    assert!(d.purchases[0].assignments.is_empty());
    assign(&mut d, "R2-R3", "Board");
    route(
        &mut d,
        "PATCH",
        &format!("/v1/projects/Board/allocations/{allocation}"),
        json!({"designators":"R1-R2"}),
    )
    .unwrap();
    assert_eq!(d.purchases[0].assignments[0].designators, "R3");
}
#[test]
fn legacy_partial_reservations_do_not_take_purchased_refs_on_sync() {
    let mut d = fixture();
    let part = d.parts[0].id.clone();
    let allocation = id();
    d.allocations.push(Allocation {
        id: allocation.clone(),
        part_id: part.clone(),
        project: "Board".into(),
        quantity: 1,
        designators: "R1-R4".into(),
    });
    d.bom[0].allocation_id = Some(allocation);
    assign(&mut d, "R2", "Board");
    assert_eq!(d.requested_items()[&d.bom[0].id].designators, "R3, R4");
    d.sync_requests("Board", &BTreeMap::from([(d.bom[0].id.clone(), part)]))
        .unwrap();
    assert_eq!(d.allocations[0].designators, "R1, R3, R4");
    assert_eq!(d.purchases[0].assignments[0].designators, "R2");
    assert_eq!(d.requested(&d.bom[0]), 0);
}
