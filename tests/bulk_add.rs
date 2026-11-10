use vispace::{model::*, store::Store};
use std::collections::BTreeSet;

fn fixture() -> (tempfile::TempDir, Store, String, String) {
    let temp = tempfile::TempDir::new().unwrap();
    let mut store = Store::open(temp.path().join("inventory.sqlite")).unwrap();
    let a = Part {
        number: "R1K".into(),
        stock: 8,
        ..Default::default()
    };
    let b = Part {
        number: "C1u".into(),
        stock: 4,
        ..Default::default()
    };
    let ids = (a.id.clone(), b.id.clone());
    store
        .change(|d| {
            d.save_part(a)?;
            d.save_part(b)?;
            d.ensure_project("Board")?;
            d.storage.insert("Drawer".into(), "".into());
            Ok(())
        })
        .unwrap();
    (temp, store, ids.0, ids.1)
}
#[test]
fn bulk_allocations_are_atomic_reject_stale_rows_and_persist() {
    let (_temp, mut store, a, b) = fixture();
    assert!(
        store
            .change(|d| d.bulk_allocate(
                "Board",
                &[(a.clone(), 2, "R1,R2".into()), (b.clone(), 5, "".into())],
                false
            ))
            .is_err()
    );
    assert!(store.data.allocations.is_empty());
    store
        .change(|d| {
            d.bulk_allocate(
                "board",
                &[(a.clone(), 2, " R1,R2 ".into()), (b.clone(), 0, "".into())],
                false,
            )
        })
        .unwrap();
    assert_eq!(store.data.allocations.len(), 1);
    assert_eq!(store.data.allocations[0].designators, "R1,R2");
    assert!(
        store
            .change(|d| d.bulk_allocate(
                "Board",
                &[(b.clone(), 1, "C1".into()), (a.clone(), 1, "R3".into())],
                false
            ))
            .is_err()
    );
    let path = store.path.clone();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.data.allocations.len(), 1);
    assert!(store.data.bulk_allocate("Missing", &[], false).is_err());
}
#[test]
fn bulk_purchase_uses_remaining_quantity_and_rejects_duplicate_assignments() {
    let (_temp, mut store, _, _) = fixture();
    let cart = Part {
        number: "Cart".into(),
        stock: 5,
        ..Default::default()
    };
    let id = cart.id.clone();
    store
        .change(|d| {
            d.purchases.push(Purchase {
                part: cart,
                assignments: vec![],
            });
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .change(|d| d.bulk_allocate("Board", &[(id.clone(), 6, "".into())], true))
            .is_err()
    );
    assert!(store.data.purchases[0].assignments.is_empty());
    store
        .change(|d| d.bulk_allocate("Board", &[(id.clone(), 3, "R1-R3".into())], true))
        .unwrap();
    assert!(
        store
            .change(|d| d.bulk_allocate("Board", &[(id.clone(), 1, "".into())], true))
            .is_err()
    );
    let path = store.path.clone();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.data.purchases[0].assignments[0].quantity, 3);
}
#[test]
fn bulk_storage_moves_only_selected_parts_and_rejects_stale_selections() {
    let (_temp, mut store, a, b) = fixture();
    let missing = BTreeSet::from([a.clone(), "missing".into()]);
    assert!(
        store
            .change(|d| d.bulk_storage("Drawer", &missing))
            .is_err()
    );
    assert!(store.data.parts.iter().all(|p| p.storage.is_empty()));
    store
        .change(|d| d.bulk_storage("Drawer", &BTreeSet::from([a.clone()])))
        .unwrap();
    let path = store.path.clone();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.data.parts.iter().find(|p| p.id == a).unwrap().storage,
        "Drawer"
    );
    assert!(
        store
            .data
            .parts
            .iter()
            .find(|p| p.id == b)
            .unwrap()
            .storage
            .is_empty()
    );
    store
        .change(|d| d.bulk_storage("", &BTreeSet::from([a.clone()])))
        .unwrap();
    assert!(store.data.parts.iter().all(|p| p.storage.is_empty()));
}
