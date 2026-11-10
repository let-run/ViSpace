use vispace::{api, model::*, store::Store};
use serde_json::json;
#[test]
fn supplier_retry_returns_same_purchase_and_retains_allocations_and_price() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("inventory.sqlite")).unwrap();
    let payload = json!({"requestID":"supplier-123","part":{"number":"TEST","customFields":{"LCSC Price":"$0.15"}},"quantity":5,"assignments":[{"project":"Demo","quantity":2}]});
    let first = api::handle_request(&mut store, "POST", "/v1/purchases", payload.clone())
        .unwrap()
        .1;
    let second = api::handle_request(&mut store, "POST", "/v1/purchases", payload)
        .unwrap()
        .1;
    assert_eq!(first["part"]["id"], second["part"]["id"]);
    assert_eq!(store.data.purchases.len(), 1);
    assert_eq!(store.data.purchases[0].assignments[0].quantity, 2);
    assert_eq!(
        store.data.purchases[0].part.custom_fields["LCSC Price"],
        "$0.15"
    );
    assert!(
        api::handle_request(
            &mut store,
            "POST",
            "/v1/purchases",
            json!({"requestID":"","part":{"number":"BAD"},"quantity":1})
        )
        .is_err()
    );
}
#[test]
fn shared_purchase_validation_rolls_back_excess_project_quantities() {
    let mut data = Inventory::default();
    let purchase=vispace::purchase_import::purchase(&json!({"part":{"number":"TEST"},"quantity":1,"assignments":[{"project":"Demo","quantity":2}]}),None,None).unwrap();
    data.ensure_project("Demo").unwrap();
    data.purchases.push(purchase);
    assert!(data.validate().is_err());
}
