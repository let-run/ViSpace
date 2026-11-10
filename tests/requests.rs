use vispace::{model::*, requests::candidates, store::Store};
use std::collections::BTreeMap;
fn item(value: &str, refs: &str, quantity: i64) -> BomItem {
    BomItem {
        value: value.into(),
        designators: refs.into(),
        quantity,
        package: "0402".into(),
        ..Default::default()
    }
}
fn fixture() -> Inventory {
    let mut d = Inventory::default();
    d.ensure_project("Board").unwrap();
    d.save_part(Part {
        number: "R-PART".into(),
        value: "4.7kΩ".into(),
        package: "0402".into(),
        stock: 5,
        ..Default::default()
    })
    .unwrap();
    d
}
#[test]
fn editing_and_deleting_preserve_allocated_stock_and_persist() {
    let temp = tempfile::TempDir::new().unwrap();
    let mut store = Store::open(temp.path().join("inventory.sqlite")).unwrap();
    store
        .change(|d| {
            *d = fixture();
            d.save_request("board", None, item("4K7", "R1,R2,R3,R4,R5,R6,R7", 7))
        })
        .unwrap();
    let id = store.data.bom[0].id.clone();
    let part = store.data.parts[0].id.clone();
    store
        .change(|d| d.sync_requests("Board", &BTreeMap::from([(id.clone(), part.clone())])))
        .unwrap();
    assert_eq!(store.data.requested(&store.data.bom[0]), 2);
    store
        .change(|d| d.save_request("Board", Some(&id), item("4K7", "R1,R2,R3,R4,R5,R6", 1)))
        .unwrap();
    assert_eq!(store.data.bom[0].item.quantity, 6);
    assert_eq!(store.data.requested(&store.data.bom[0]), 1);
    store.change(|d| d.delete_request(&id)).unwrap();
    let path = store.path.clone();
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.data.allocations[0].quantity, 5);
    assert_eq!(reopened.data.bom[0].item.quantity, 5);
    assert_eq!(reopened.data.requested(&reopened.data.bom[0]), 0);
}
#[test]
fn sync_is_explicit_budgets_in_order_and_rejects_stale_matches_atomically() {
    let mut d = fixture();
    d.save_request("Board", None, item("4K7", "R1,R2,R3,R4", 4))
        .unwrap();
    d.save_request("Board", None, item("4700", "R5,R6,R7,R8", 4))
        .unwrap();
    assert_eq!(d.sync_requests("Board", &BTreeMap::new()).unwrap(), 0);
    let ids: Vec<_> = d.bom.iter().map(|e| e.id.clone()).collect();
    let part = d.parts[0].id.clone();
    let matches = BTreeMap::from([
        (ids[0].clone(), part.clone()),
        (ids[1].clone(), part.clone()),
    ]);
    let plan = d.request_sync_plan("Board", &matches).unwrap();
    assert_eq!((plan[0].2, plan[1].2), (4, 1));
    let mut bad = matches.clone();
    bad.insert(ids[1].clone(), "missing".into());
    assert!(d.sync_requests("Board", &bad).is_err());
    assert!(d.allocations.is_empty());
    assert_eq!(d.sync_requests("Board", &matches).unwrap(), 5);
    assert_eq!(d.requested(&d.bom[1]), 3);
    d.parts[0].stock += 2;
    assert_eq!(d.sync_requests("Board", &matches).unwrap(), 2);
    assert_eq!(d.allocations.len(), 2);
    assert_eq!(d.requested(&d.bom[1]), 1);
}
#[test]
fn shared_allocation_budget_and_request_validation() {
    let mut d = fixture();
    d.save_request("Board", None, item("4K7", "R1,R2,R3", 3))
        .unwrap();
    d.save_request("Board", None, item("4K7", "R4,R5,R6", 3))
        .unwrap();
    let a = Allocation {
        id: id(),
        part_id: d.parts[0].id.clone(),
        project: "Board".into(),
        quantity: 4,
        designators: "R1,R2,R3,R4".into(),
    };
    for e in &mut d.bom {
        e.allocation_id = Some(a.id.clone());
    }
    d.allocations.push(a);
    assert_eq!(d.requested(&d.bom[0]), 0);
    assert_eq!(d.requested(&d.bom[1]), 2);
    let id = d.bom[1].id.clone();
    assert_eq!(
        d.sync_requests("Board", &BTreeMap::from([(id, d.parts[0].id.clone())]))
            .unwrap(),
        1
    );
    assert_eq!(d.requested(&d.bom[1]), 1);
    assert_eq!(d.allocations.len(), 1);
    assert!(d.save_request("Missing", None, item("x", "", 1)).is_err());
    assert!(d.save_request("Board", None, item("", "", 1)).is_err());
    assert!(d.save_request("Board", None, item("x", "", 0)).is_err());
    let mut bad = item("x", "", 1);
    bad.spn = "C1234".into();
    assert!(d.save_request("Board", None, bad).is_err());
    d.save_request("Board", None, item("x", "", 1)).unwrap();
    let id = d.bom.last().unwrap().id.clone();
    d.delete_request(&id).unwrap();
    assert!(!d.bom.iter().any(|e| e.id == id));
}
#[test]
fn matching_uses_exact_then_measurements_then_suffix_and_confirmed_links() {
    let mut d = fixture();
    d.parts[0].labels.clear(); // Swift matching does not require a category label.
    let mut request = item("4K7", "R1", 1);
    assert_eq!(candidates(&request, &d.parts).len(), 1);
    request.package = "Resistor_SMD:R_0402_1005Metric".into();
    assert_eq!(candidates(&request, &d.parts).len(), 1);
    request.value = "900K".into();
    assert!(candidates(&request, &d.parts).is_empty());
    d.parts[0].spn = "123-ND".into();
    d.parts[0].supplier = "DigiKey".into();
    request.spn = "123-nd".into();
    request.supplier = "Digi-Key".into();
    assert_eq!(candidates(&request, &d.parts).len(), 1);
    request.spn.clear();
    request.value = "R-PAR".into();
    assert_eq!(candidates(&request, &d.parts).len(), 1);
    request.value = "Something else".into();
    d.save_request("Board", None, request).unwrap();
    d.bom[0].part_id = Some(d.parts[0].id.clone());
    assert_eq!(d.request_candidates(&d.bom[0]).len(), 1);
}

#[test]
fn worker_payload_roundtrip_previews_and_cancellation() {
    use vispace::requests::RequestReview;
    let mut d = fixture();
    d.save_request("Board", None, item("4K7", "R1,R2,R3,R4", 4))
        .unwrap();
    d.save_request("Board", None, item("4K7", "R5,R6,R7,R8", 4))
        .unwrap();
    let review = RequestReview::build(&d, "Board", || false).unwrap();
    let bytes = serde_json::to_vec(&review).unwrap();
    let review: RequestReview = serde_json::from_slice(&bytes).unwrap();
    let part = d.parts[0].id.clone();
    // A selected row hidden by search still consumes the same stock budget.
    let matches = BTreeMap::from([
        (d.bom[0].id.clone(), part.clone()),
        (d.bom[1].id.clone(), part),
    ]);
    let previews = review.previews(&matches);
    let plan = review.plan(&matches).unwrap();
    assert_eq!((previews[0][0], previews[1][0]), (4, 1));
    assert_eq!(previews[0][0], plan[0].2);
    assert_eq!(previews[1][0], plan[1].2);
    assert!(RequestReview::build(&d, "Board", || true).is_err());
    assert_eq!(
        d.requested_quantities()[&d.bom[0].id],
        d.requested(&d.bom[0])
    );
}
#[test]
fn background_commit_rejects_concurrent_changes_and_keeps_failed_saves_atomic() {
    let temp = tempfile::TempDir::new().unwrap();
    let mut store = Store::open(temp.path().join("inventory.sqlite")).unwrap();
    store
        .change(|d| {
            *d = fixture();
            d.save_request("Board", None, item("4K7", "R1", 1))
        })
        .unwrap();
    let revision = store.revision();
    let mut computed = store.data.clone();
    computed
        .sync_requests(
            "Board",
            &BTreeMap::from([(computed.bom[0].id.clone(), computed.parts[0].id.clone())]),
        )
        .unwrap();
    store
        .change(|d| {
            d.parts[0].description = "Concurrent API edit".into();
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .commit_if_revision(revision, computed.clone())
            .is_err()
    );
    assert!(store.data.allocations.is_empty());
    assert_eq!(store.data.parts[0].description, "Concurrent API edit");
    let revision = store.revision();
    rusqlite::Connection::open(&store.path)
        .unwrap()
        .execute_batch("DROP TABLE project_bom_entries")
        .unwrap();
    assert!(store.commit_if_revision(revision, computed).is_err());
    assert_eq!(store.revision(), revision);
    assert!(store.data.allocations.is_empty());
}
#[test]
fn indexed_review_handles_large_bom_and_shared_stock_without_rematching_previews() {
    use vispace::requests::RequestReview;
    let mut d = Inventory::default();
    d.ensure_project("Large").unwrap();
    for i in 0..1000 {
        d.parts.push(Part {
            number: format!("RES-{i:04}"),
            value: if i % 10 == 0 { "4.7kΩ" } else { "10kΩ" }.into(),
            package: "0402".into(),
            stock: 5,
            ..Default::default()
        });
    }
    for i in 0..200 {
        d.save_request("Large", None, item("4K7", &format!("R{i}"), 2))
            .unwrap();
    }
    let started = std::time::Instant::now();
    let review = RequestReview::build(&d, "Large", || false).unwrap();
    eprintln!(
        "1,000 parts / 200 requests: review in {:?}",
        started.elapsed()
    );
    assert_eq!(review.rows.len(), 200);
    assert!(review.rows.iter().all(|r| r.candidates.len() == 100));
    let selected = review.rows[0].candidates[0].clone();
    let matches = review
        .rows
        .iter()
        .map(|r| (r.entry.id.clone(), selected.clone()))
        .collect();
    let previews = review.previews(&matches);
    assert_eq!(
        (
            previews[0][0],
            previews[1][0],
            previews[2][0],
            previews[3][0]
        ),
        (2, 2, 1, 0)
    );
    assert_eq!(
        review
            .plan(&matches)
            .unwrap()
            .iter()
            .map(|(_, _, n)| n)
            .sum::<i64>(),
        5
    );
}
