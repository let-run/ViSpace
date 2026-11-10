use vispace::{
    action_log,
    mesh::Clock,
    model::*,
    store::{SharedStore, Store, copy_tree},
    sync::{ClientStatus, SyncClient, SyncServer},
};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;
fn fixture() -> (TempDir, SharedStore) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("inventory.sqlite")).unwrap();
    (dir, Arc::new(Mutex::new(store)))
}
fn part(number: &str, stock: i64) -> Part {
    Part {
        number: number.into(),
        stock,
        ..Default::default()
    }
}
async fn until(mut test: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("mesh failed to converge");
}
fn snapshot(store: &SharedStore) -> serde_json::Value {
    json!(store.lock().unwrap().data)
}
async fn connect(from: &SharedStore, to: &SharedStore) -> (SyncServer, SyncClient) {
    let server = SyncServer::start(to.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let client = SyncClient::start(from.clone(), &format!("ws://{}/sync", server.address)).unwrap();
    (server, client)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_writers_merge_parts_and_purchases_without_pausing_edits() {
    let (_a, a) = fixture();
    let (_b, b) = fixture();
    for (store, stock) in [(&a, 5), (&b, 7)] {
        store
            .lock()
            .unwrap()
            .change(|d| {
                d.save_part(part("R1", stock))?;
                d.purchases.push(Purchase {
                    part: part("C1", stock),
                    assignments: vec![],
                });
                Ok(())
            })
            .unwrap();
    }
    let (server, client) = connect(&a, &b).await;
    until(|| {
        a.lock().unwrap().data.parts[0].stock == 12 && b.lock().unwrap().data.parts[0].stock == 12
    })
    .await;
    assert_eq!(snapshot(&a), snapshot(&b));
    assert_eq!(a.lock().unwrap().data.purchases[0].part.stock, 12);
    a.lock()
        .unwrap()
        .change(|d| {
            d.parts[0].stock += 2;
            d.parts[0].description = "from A".into();
            Ok(())
        })
        .unwrap();
    b.lock()
        .unwrap()
        .change(|d| {
            d.parts[0].stock += 3;
            d.parts[0].notes.text = "from B".into();
            Ok(())
        })
        .unwrap();
    until(|| {
        !a.lock().unwrap().merge_reviews().is_empty()
            || !b.lock().unwrap().merge_reviews().is_empty()
            || a.lock().unwrap().data.parts[0].stock == 17
    })
    .await;
    for node in [&a, &b] {
        let reviews = node.lock().unwrap().merge_reviews();
        for review in reviews {
            let total = review.ours.as_i64().unwrap() + review.theirs.as_i64().unwrap()
                - review.base.as_i64().unwrap();
            node.lock()
                .unwrap()
                .resolve_merge(&review.id, json!(total))
                .unwrap();
        }
    }
    until(|| a.lock().unwrap().data.parts[0].stock == 17 && snapshot(&a) == snapshot(&b)).await;
    assert_eq!(a.lock().unwrap().data.parts[0].description, "from A");
    assert_eq!(a.lock().unwrap().data.parts[0].notes.text, "from B");
    client.shutdown().await;
    server.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_stars_chain_and_cycle_forward_each_event_exactly_once() {
    let fixtures: Vec<_> = (0..6).map(|_| fixture()).collect();
    let stores: Vec<_> = fixtures.iter().map(|(_, s)| s.clone()).collect();
    for (i, s) in stores.iter().enumerate() {
        s.lock()
            .unwrap()
            .change(|d| d.save_part(part("Shared", (i + 1) as i64)))
            .unwrap();
    }
    let servers: Vec<_> = stores
        .iter()
        .map(|s| SyncServer::start(s.clone(), "127.0.0.1:0".parse().unwrap()).unwrap())
        .collect();
    let mut clients = vec![];
    for (from, to) in [(0, 1), (0, 2), (2, 3), (3, 4), (3, 5), (5, 0)] {
        clients.push(
            SyncClient::start(
                stores[from].clone(),
                &format!("ws://{}/sync", servers[to].address),
            )
            .unwrap(),
        );
    }
    until(|| {
        stores
            .iter()
            .all(|s| s.lock().unwrap().data.parts[0].stock == 21)
    })
    .await;
    for s in &stores {
        s.lock()
            .unwrap()
            .change(|d| {
                d.save_part(part("Shared", 1))?;
                Ok(())
            })
            .unwrap();
    }
    until(|| {
        stores
            .iter()
            .all(|s| s.lock().unwrap().data.parts[0].stock == 27)
    })
    .await;
    let expected = snapshot(&stores[0]);
    for s in &stores {
        assert_eq!(snapshot(s), expected);
        assert_eq!(
            s.lock().unwrap().events_after(&Clock::new()).unwrap().len(),
            12
        );
    }
    for c in clients {
        c.shutdown().await;
    }
    for s in servers {
        s.shutdown().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_concurrent_allocations_converge_and_conflicts_restore_when_stock_arrives() {
    let (_a, a) = fixture();
    let (_b, b) = fixture();
    a.lock()
        .unwrap()
        .change(|d| {
            d.save_part(part("A", 10))?;
            d.ensure_project("Board")?;
            Ok(())
        })
        .unwrap();
    let (server, client) = connect(&b, &a).await;
    until(|| b.lock().unwrap().data.parts.len() == 1).await;
    client.shutdown().await;
    server.shutdown().await;
    let project_id = a.lock().unwrap().data.projects[0].id.clone();
    for s in [&a, &b] {
        s.lock()
            .unwrap()
            .change(|d| {
                let id = d.parts[0].id.clone();
                d.allocate(&id, "Board", 8, "")?;
                Ok(())
            })
            .unwrap();
    }
    let (server, client) = connect(&a, &b).await;
    until(|| snapshot(&a) == snapshot(&b) && !a.lock().unwrap().sync_conflicts().is_empty()).await;
    assert_eq!(
        a.lock()
            .unwrap()
            .data
            .allocations
            .iter()
            .map(|a| a.quantity)
            .sum::<i64>(),
        10
    );
    a.lock()
        .unwrap()
        .change(|d| {
            d.parts[0].stock += 10;
            d.rename_project("Board", "Renamed", "")
        })
        .unwrap();
    until(|| snapshot(&a) == snapshot(&b) && b.lock().unwrap().data.projects[0].name == "Renamed")
        .await;
    assert_eq!(a.lock().unwrap().data.projects[0].id, project_id);
    assert_eq!(
        a.lock()
            .unwrap()
            .data
            .allocations
            .iter()
            .map(|a| a.quantity)
            .sum::<i64>(),
        16
    );
    client.shutdown().await;
    server.shutdown().await;
}
#[test]
fn origin_ids_clocks_and_out_of_order_pending_events_survive_restart() {
    let (dir, a) = fixture();
    let (_b, b) = fixture();
    let app = a.lock().unwrap().application_id().to_owned();
    a.lock()
        .unwrap()
        .change(|d| d.save_part(part("A", 5)))
        .unwrap();
    a.lock()
        .unwrap()
        .change(|d| {
            d.parts[0].stock = 7;
            Ok(())
        })
        .unwrap();
    let events = a.lock().unwrap().events_after(&Clock::new()).unwrap();
    assert_eq!(events.len(), 2);
    {
        let mut target = b.lock().unwrap();
        assert!(target.replay_record(&events[1], None).unwrap());
        assert!(target.data.parts.is_empty());
        assert_eq!(target.sync_clock().get(&app), None);
        target.replay_record(&events[0], None).unwrap();
        assert_eq!(target.data.parts[0].stock, 7);
        assert!(!target.replay_record(&events[1], None).unwrap());
    }
    drop(a);
    let store = Store::open(dir.path().join("inventory.sqlite")).unwrap();
    assert_eq!(store.application_id(), app);
    assert_eq!(store.sync_clock().get(&app), Some(&2));
    assert_eq!(store.data.parts[0].stock, 7);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_shared_history_is_migrated_without_double_counting() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    let mut data = Inventory::default();
    data.save_part(part("Old", 10)).unwrap();
    let record = action_log::diff(
        &Inventory::default(),
        &data,
        &a.path().join("Attachments"),
        true,
    )
    .unwrap();
    let mut journal = action_log::Journal::open(a.path().join("inventory.actions")).unwrap();
    journal.append(&record).unwrap();
    drop(journal);
    copy_tree(
        &a.path().join("inventory.actions"),
        &b.path().join("inventory.actions"),
    )
    .unwrap();
    let a = Arc::new(Mutex::new(
        Store::open(a.path().join("inventory.sqlite")).unwrap(),
    ));
    let b = Arc::new(Mutex::new(
        Store::open(b.path().join("inventory.sqlite")).unwrap(),
    ));
    let (server, client) = connect(&a, &b).await;
    until(|| matches!(client.status(), ClientStatus::Connected { .. })).await;
    a.lock()
        .unwrap()
        .change(|d| {
            d.parts[0].stock += 2;
            Ok(())
        })
        .unwrap();
    until(|| b.lock().unwrap().data.parts[0].stock == 12).await;
    b.lock()
        .unwrap()
        .change(|d| {
            d.parts[0].stock += 3;
            Ok(())
        })
        .unwrap();
    until(|| {
        a.lock().unwrap().data.parts[0].stock == 15 && b.lock().unwrap().data.parts[0].stock == 15
    })
    .await;
    assert_eq!(
        a.lock().unwrap().events_after(&Clock::new()).unwrap().len(),
        3
    );
    client.shutdown().await;
    server.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_projects_with_equal_names_keep_unique_ids_and_valid_links() {
    let (_a, a) = fixture();
    let (_b, b) = fixture();
    for s in [&a, &b] {
        s.lock()
            .unwrap()
            .change(|d| {
                d.save_part(part("A", 10))?;
                let p = d.parts[0].id.clone();
                d.allocate(&p, "Board", 2, "")?;
                Ok(())
            })
            .unwrap();
    }
    let (server, client) = connect(&a, &b).await;
    until(|| a.lock().unwrap().data.projects.len() == 2 && snapshot(&a) == snapshot(&b)).await;
    let d = &a.lock().unwrap().data;
    assert_ne!(d.projects[0].id, d.projects[1].id);
    assert_ne!(d.projects[0].name, d.projects[1].name);
    d.validate().unwrap();
    client.shutdown().await;
    server.shutdown().await;
}
#[test]
fn flushed_mesh_event_recovers_after_sqlite_failure() {
    let (dir, a) = fixture();
    let (_b, b) = fixture();
    b.lock()
        .unwrap()
        .change(|d| d.save_part(part("Recovered", 3)))
        .unwrap();
    let event = b
        .lock()
        .unwrap()
        .events_after(&Clock::new())
        .unwrap()
        .remove(0);
    {
        let mut a = a.lock().unwrap();
        rusqlite::Connection::open(&a.path)
            .unwrap()
            .execute_batch("DROP TABLE purchases")
            .unwrap();
        assert!(a.replay_record(&event, None).is_err());
        assert!(a.recovery_required());
    }
    drop(a);
    let mut restored = Store::open(dir.path().join("inventory.sqlite")).unwrap();
    assert_eq!(restored.data.parts[0].stock, 3);
    assert!(!restored.replay_record(&event, None).unwrap());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_identity_and_last_receive_state_are_persisted() {
    let (a, source) = fixture();
    let (_b, target) = fixture();
    let peer = target.lock().unwrap().application_id().to_owned();
    let (server, client) = connect(&source, &target).await;
    target
        .lock()
        .unwrap()
        .change(|d| d.save_part(part("From peer", 4)))
        .unwrap();
    until(|| source.lock().unwrap().data.parts.len() == 1).await;
    let db = rusqlite::Connection::open(a.path().join("inventory.sqlite")).unwrap();
    let (count, time): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*),MAX(last_received_ms) FROM sync_peers WHERE id=?1",
            [peer],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert!(time > 0);
    client.shutdown().await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_receiving_the_same_purchase_does_not_create_stock_twice() {
    let (_a, a) = fixture();
    let (_b, b) = fixture();
    a.lock()
        .unwrap()
        .change(|d| {
            d.purchases.push(Purchase {
                part: part("Delivered", 8),
                assignments: vec![],
            });
            Ok(())
        })
        .unwrap();
    let (server, client) = connect(&b, &a).await;
    until(|| b.lock().unwrap().data.purchases.len() == 1).await;
    client.shutdown().await;
    server.shutdown().await;
    for s in [&a, &b] {
        s.lock()
            .unwrap()
            .change(|d| {
                let id = d.purchases[0].part.id.clone();
                d.receive(&[id])?;
                Ok(())
            })
            .unwrap();
    }
    let (server, client) = connect(&a, &b).await;
    until(|| snapshot(&a) == snapshot(&b)).await;
    assert_eq!(a.lock().unwrap().data.parts[0].stock, 8);
    assert!(a.lock().unwrap().data.purchases.is_empty());
    client.shutdown().await;
    server.shutdown().await;
}
#[test]
fn missing_chunk_directory_is_repaired_without_reusing_origin_sequence() {
    let (dir, a) = fixture();
    let app = a.lock().unwrap().application_id().to_owned();
    a.lock()
        .unwrap()
        .change(|d| d.save_part(part("A", 2)))
        .unwrap();
    drop(a);
    std::fs::remove_dir_all(dir.path().join("inventory.actions")).unwrap();
    let mut a = Store::open(dir.path().join("inventory.sqlite")).unwrap();
    assert_eq!(a.application_id(), app);
    assert_eq!(a.sync_clock().get(&app), Some(&1));
    a.change(|d| {
        d.parts[0].stock += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(a.sync_clock().get(&app), Some(&2));
    assert_eq!(a.data.parts[0].stock, 3);
}

#[test]
fn combined_quantity_overflow_is_reported_without_breaking_convergence() {
    let (_a, a) = fixture();
    let (_b, b) = fixture();
    for s in [&a, &b] {
        s.lock()
            .unwrap()
            .change(|d| d.save_part(part("Large", i64::MAX)))
            .unwrap();
    }
    let left = a
        .lock()
        .unwrap()
        .events_after(&Clock::new())
        .unwrap()
        .remove(0);
    let right = b
        .lock()
        .unwrap()
        .events_after(&Clock::new())
        .unwrap()
        .remove(0);
    a.lock().unwrap().replay_record(&right, None).unwrap();
    b.lock().unwrap().replay_record(&left, None).unwrap();
    assert_eq!(snapshot(&a), snapshot(&b));
    assert_eq!(a.lock().unwrap().data.parts[0].stock, i64::MAX);
    assert!(!a.lock().unwrap().sync_conflicts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_purchase_quantity_reductions_can_exhaust_cart_without_blocking_sync() {
    let (_a, a) = fixture();
    let (_b, b) = fixture();
    a.lock()
        .unwrap()
        .change(|d| {
            d.purchases.push(Purchase {
                part: part("Cart", 5),
                assignments: vec![],
            });
            Ok(())
        })
        .unwrap();
    let (server, client) = connect(&b, &a).await;
    until(|| b.lock().unwrap().data.purchases.len() == 1).await;
    client.shutdown().await;
    server.shutdown().await;
    a.lock()
        .unwrap()
        .change(|d| {
            d.purchases[0].part.stock = 2;
            Ok(())
        })
        .unwrap();
    b.lock()
        .unwrap()
        .change(|d| {
            d.purchases[0].part.stock = 3;
            Ok(())
        })
        .unwrap();
    let (server, client) = connect(&a, &b).await;
    until(|| {
        !a.lock().unwrap().merge_reviews().is_empty()
            || !b.lock().unwrap().merge_reviews().is_empty()
    })
    .await;
    for node in [&a, &b] {
        let reviews = node.lock().unwrap().merge_reviews();
        for review in reviews {
            node.lock()
                .unwrap()
                .resolve_merge(&review.id, json!(0))
                .unwrap();
        }
    }
    until(|| {
        a.lock().unwrap().data.purchases.is_empty() && b.lock().unwrap().data.purchases.is_empty()
    })
    .await;
    assert_eq!(snapshot(&a), snapshot(&b));
    assert!(!a.lock().unwrap().sync_conflicts().is_empty());
    client.shutdown().await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn topology_gossip_discovers_chains_cycles_and_disconnects_without_inventory_events() {
    let (_a, a) = fixture();
    let (_b, b) = fixture();
    let (_c, c) = fixture();
    let (sa, ba) = connect(&b, &a).await;
    let (sb, cb) = connect(&c, &b).await;
    until(|| {
        [&a, &b, &c].iter().all(|s| {
            let g = s.lock().unwrap().network_graph();
            g.nodes.len() == 3 && g.edges.len() == 2
        })
    })
    .await;
    let graph = a.lock().unwrap().network_graph();
    let cid = c.lock().unwrap().application_id().to_owned();
    assert!(!graph.nodes.iter().find(|n| n.id == cid).unwrap().direct);
    assert!(graph.nodes.iter().all(|n| n.reachable));
    let (sc, ac) = connect(&a, &c).await;
    until(|| {
        [&a, &b, &c]
            .iter()
            .all(|s| s.lock().unwrap().network_graph().edges.len() == 3)
    })
    .await;
    ac.shutdown().await;
    until(|| {
        [&a, &b, &c]
            .iter()
            .all(|s| s.lock().unwrap().network_graph().edges.len() == 2)
    })
    .await;
    cb.shutdown().await;
    until(|| {
        let g = a.lock().unwrap().network_graph();
        g.edges.len() == 1 && !g.nodes.iter().find(|n| n.id == cid).unwrap().reachable
    })
    .await;
    for s in [&a, &b, &c] {
        assert_eq!(s.lock().unwrap().action_log().unwrap().len(), 1);
    }
    ba.shutdown().await;
    sa.shutdown().await;
    sb.shutdown().await;
    sc.shutdown().await;
}

#[tokio::test]
async fn retired_raft_endpoint_reports_where_to_connect() {
    use vispace::{
        store::Store,
        sync::{SyncClient, SyncServer},
    };
    use std::sync::{Arc, Mutex};
    use tokio_tungstenite::{connect_async, tungstenite::Error};
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(
        Store::open(temp.path().join("inventory.sqlite")).unwrap(),
    ));
    let server = SyncServer::start(store.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = format!("ws://{}/raft", server.address);
    assert!(
        SyncClient::start(store, &address)
            .err()
            .unwrap()
            .to_string()
            .contains("Raft sync has been removed")
    );
    let Err(Error::Http(response)) = connect_async(address).await else {
        panic!("Expected the server to reject the retired endpoint");
    };
    assert_eq!(response.status(), 410);
    assert!(
        String::from_utf8_lossy(response.body().as_deref().unwrap_or_default())
            .contains("Use the WebSocket base address in Sync")
    );
}

#[test]
fn retired_raft_workspace_recovers_committed_history_into_partition_sync() {
    use vispace::{
        mesh::Mesh,
        model::{Inventory, Part, id},
        store::Store,
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("inventory.sqlite");
    let author = id();
    let mut inventory = Inventory::default();
    inventory
        .save_part(Part {
            number: "RECOVERED".into(),
            ..Default::default()
        })
        .unwrap();
    let record = Mesh::default()
        .make(
            &Inventory::default(),
            &inventory,
            &temp.path().join("Attachments"),
            &author,
            1,
        )
        .unwrap();
    let legacy = rusqlite::Connection::open(path.with_extension("consensus.sqlite")).unwrap();
    legacy
        .execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY, value BLOB NOT NULL)")
        .unwrap();
    for (key, value) in [
        ("application_id", serde_json::to_value(&author).unwrap()),
        ("state", serde_json::json!({"records": [record]})),
    ] {
        legacy
            .execute(
                "INSERT INTO metadata VALUES(?1,?2)",
                rusqlite::params![key, serde_json::to_vec(&value).unwrap()],
            )
            .unwrap();
    }
    drop(legacy);

    let store = Store::open(&path).unwrap();
    assert!(store.uses_component());
    assert_eq!(store.application_id(), author);
    assert_eq!(store.data.parts[0].number, "RECOVERED");
    drop(store);

    let reopened = Store::open(&path).unwrap();
    assert!(reopened.uses_component());
    assert_eq!(reopened.records_after(None).unwrap().len(), 1);
}
