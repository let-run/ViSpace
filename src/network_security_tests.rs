//! Real TLS/WebSocket integration, temporary inventories only. No OS trust changes.
use crate::{
    model::Part,
    network_tls,
    store::{SharedStore, Store},
    sync::{ClientStatus, SyncClient, SyncServer},
};
use anyhow::{Context, Result, ensure};
use std::sync::{Arc, Mutex};
fn store(root: &std::path::Path, name: &str, tls: &network_tls::Settings) -> Result<SharedStore> {
    let s = Store::open(root.join(name).join("inventory.sqlite"))?;
    network_tls::save(&s, tls)?;
    Ok(Arc::new(Mutex::new(s)))
}
async fn until(mut predicate: impl FnMut() -> bool) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !predicate() {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await?;
    Ok(())
}
fn approve(a: &SharedStore, b: &SharedStore) -> Result<()> {
    let key = b.lock().unwrap().network_identity().public_key()?;
    a.lock().unwrap().network_authorize(key, false)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn wss_pairing_signed_gossip_and_revocation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let tls = network_tls::test_settings();
    let a = store(dir.path(), "a", &tls)?;
    let b = store(dir.path(), "b", &tls)?;
    let c = store(dir.path(), "c", &tls)?;
    let aid = a.lock().unwrap().application_id().to_owned();
    let bid = b.lock().unwrap().application_id().to_owned();
    let cid = c.lock().unwrap().application_id().to_owned();
    a.lock().unwrap().change(|d| {
        d.parts.push(Part {
            number: "SIGNED-1".into(),
            stock: 3,
            ..Default::default()
        });
        Ok(())
    })?;
    let server = SyncServer::start(a.clone(), "127.0.0.1:0".parse()?)?;
    let client = SyncClient::start_pairing(
        b.clone(),
        &format!("wss://localhost:{}", server.address.port()),
        None,
    )?;
    until(|| a.lock().unwrap().network_pairing_ids().contains(&bid)
        && b.lock().unwrap().network_trusted(&aid))
        .await
        .with_context(|| format!("No pairing request: {:?}", client.status()))?;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(
        b.lock().unwrap().data.parts.is_empty(),
        "Unapproved peer received inventory"
    );
    assert!(!a.lock().unwrap().network_trusted(&bid));
    approve(&a, &b)?;
    until(|| matches!(client.status(), ClientStatus::Connected { .. }))
        .await
        .with_context(|| format!("Pairing did not complete: {:?}", client.status()))?;
    until(|| b.lock().unwrap().data.parts.len() == 1)
        .await
        .with_context(|| format!("Signed history did not arrive: {:?}", client.status()))?;
    assert!(a.lock().unwrap().network_pairing_ids().is_empty());
    let record = a
        .lock()
        .unwrap()
        .events_after(&Default::default())?
        .last()
        .unwrap()
        .clone();
    assert_eq!(crate::network_identity::verify_record(&record)?, aid);
    let mut forged = record.clone();
    forged.timestamp_ms += 1;
    assert!(b.lock().unwrap().network_verify_record(&forged).is_err());
    // C trusts its explicitly paired B. A already trusts B, so B may sponsor C.
    approve(&b, &c)?;
    approve(&c, &b)?;
    let server_b = SyncServer::start(b.clone(), "127.0.0.1:0".parse()?)?;
    let client_c = SyncClient::start(
        c.clone(),
        &format!("wss://localhost:{}", server_b.address.port()),
    )?;
    until(|| matches!(client_c.status(), ClientStatus::Connected { .. }))
        .await
        .with_context(|| format!("Delegated pairing failed: {:?}", client_c.status()))?;
    until(|| c.lock().unwrap().data.parts.len() == 1 && a.lock().unwrap().network_trusted(&cid))
        .await?;
    b.lock().unwrap().change(|d| {
        d.parts[0].description = "Signed by B".into();
        Ok(())
    })?;
    until(|| {
        a.lock().unwrap().data.parts[0].description == "Signed by B"
            && c.lock().unwrap().data.parts[0].description == "Signed by B"
    })
    .await?;
    let old = b
        .lock()
        .unwrap()
        .events_after(&Default::default())?
        .into_iter()
        .find(|r| r.mesh.as_ref().unwrap().author() == bid)
        .unwrap();
    let bkey = b.lock().unwrap().network_identity().public_key()?;
    a.lock().unwrap().network_authorize(bkey, true)?;
    until(|| matches!(client.status(), ClientStatus::Failed(_))).await?;
    assert!(!a.lock().unwrap().network_trusted(&bid));
    assert!(
        a.lock().unwrap().network_verify_record(&old).is_ok(),
        "Accepted history lost on revocation"
    );
    let mut changed = old.clone();
    changed.id = uuid::Uuid::new_v4().to_string();
    b.lock()
        .unwrap()
        .network_identity()
        .sign_record(&mut changed)?;
    assert!(a.lock().unwrap().network_verify_record(&changed).is_err());
    client.shutdown().await;
    client_c.shutdown().await;
    server.shutdown().await;
    server_b.shutdown().await;
    // Private identity and signed approvals survive restart.
    drop(a);
    let reopened = Store::open(dir.path().join("a/inventory.sqlite"))?;
    assert_eq!(reopened.application_id(), aid);
    assert!(!reopened.network_trusted(&bid));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "requires local multicast"]
async fn nearby_wss_pins_the_discovered_identity() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let tls = network_tls::test_settings();
    let a = store(dir.path(), "a", &tls)?;
    let b = store(dir.path(), "b", &tls)?;
    let aid = a.lock().unwrap().application_id().to_owned();
    let leaf = dir.path().join("nearby-tls");
    ensure!(
        std::process::Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/script/generate_test_certificates.py"
            ))
            .arg("--ca-directory")
            .arg(tls.certificate.parent().unwrap())
            .arg("--directory")
            .arg(&leaf)
            .arg("--node-id")
            .arg(&aid)
            .status()?
            .success()
    );
    network_tls::save(
        &a.lock().unwrap(),
        &network_tls::Settings {
            certificate: leaf.join("server.pem"),
            private_key: leaf.join("server-key.pem"),
        },
    )?;
    approve(&a, &b)?;
    approve(&b, &a)?;
    let server = SyncServer::start(a.clone(), "0.0.0.0:0".parse()?)?;
    ensure!(server.discovery_hostname == crate::discovery::hostname(&aid));
    let mut advertiser = crate::discovery::Discovery::new()?;
    advertiser.advertise(&aid, Some(server.address))?;
    let mut browser = crate::discovery::Discovery::new()?;
    let mut node = None;
    until(|| {
        node = browser.nodes().into_iter().find(|node| node.node_id == aid);
        node.is_some()
    })
    .await
    .context("DNS-SD announcement did not arrive")?;
    let node = node.unwrap();
    let wrong = SyncClient::start_expected(
        b.clone(),
        &node.address,
        Some(crate::network_identity::Identity::generate().id()?),
    )?;
    until(|| format!("{:?}", wrong.status()).contains("identity"))
        .await
        .with_context(|| format!("Wrong peer identity was not rejected: {:?}", wrong.status()))?;
    let client = SyncClient::start_expected(b, &node.address, Some(node.node_id))?;
    until(|| matches!(client.status(), ClientStatus::Connected { .. }))
        .await
        .with_context(|| format!("Discovered peer did not connect: {:?}", client.status()))?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn desktop_pairing_code_grants_access_and_pins_tls_across_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let host = store(dir.path(), "host", &network_tls::Settings::default())?;
    let guest = store(dir.path(), "guest", &network_tls::Settings::default())?;
    let host_id = host.lock().unwrap().application_id().to_owned();
    let guest_id = guest.lock().unwrap().application_id().to_owned();
    let server = SyncServer::start(host.clone(), "127.0.0.1:0".parse()?)?;
    let address = server.address;
    let url = format!("wss://{address}");
    let client = SyncClient::start_code_pairing(guest.clone(), &url, None, false)?;
    until(|| matches!(client.status(), ClientStatus::WaitingForApproval)).await?;
    until(|| host.lock().unwrap().pairing_code(&guest_id).is_some()).await?;
    assert!(host.lock().unwrap().show_pairing_code(&guest_id));
    let code = host.lock().unwrap().pairing_code(&guest_id).flatten().unwrap();
    assert_eq!(code.len(), 4);
    until(|| matches!(client.status(), ClientStatus::PairingCode { remaining: 3 })).await?;
    client.enter_pairing_code(if code == "0000" { "0001" } else { "0000" }.into());
    until(|| matches!(client.status(), ClientStatus::PairingCode { remaining: 2 })).await?;
    assert!(!host.lock().unwrap().network_trusted(&guest_id));
    assert!(!guest.lock().unwrap().network_trusted(&host_id));
    client.enter_pairing_code(code);
    until(|| matches!(client.status(), ClientStatus::Connected { .. })).await?;
    assert!(host.lock().unwrap().network_trusted(&guest_id));
    assert!(guest.lock().unwrap().network_trusted(&host_id));

    let guest_pin = network_tls::local_server_pin(&guest.lock().unwrap())?;
    assert_eq!(host.lock().unwrap().network_tls_pin("", Some(&guest_id))?.unwrap().1, guest_pin);
    client.shutdown().await;
    server.shutdown().await;
    let _server = SyncServer::start(host, address)?;
    let reconnected = SyncClient::start(guest, &url)?;
    until(|| matches!(reconnected.status(), ClientStatus::Connected { .. })).await?;
    Ok(())
}
