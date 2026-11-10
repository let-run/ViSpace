use vispace::{
    action_log,
    model::*,
    store::{SharedStore, Store},
    sync::{ClientStatus, Control, SYNC_VERSION, SyncClient, SyncServer, Transfer},
    sync_blob::{Blob, MAGIC},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;
use tokio_tungstenite::{connect_async, tungstenite::Message};
fn asset(relative: &str) -> String {
    format!(
        "asset:{}",
        relative
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn fixture() -> (TempDir, SharedStore) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("inventory.sqlite")).unwrap();
    (dir, Arc::new(Mutex::new(store)))
}
fn part() -> Part {
    Part {
        number: "R1".into(),
        stock: 10,
        ..Default::default()
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

async fn hello(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    assert!(matches!(
        serde_json::from_str::<Control>(&ws.next().await.unwrap().unwrap().into_text().unwrap())
            .unwrap(),
        Control::Hello {
            version: SYNC_VERSION,
            ..
        }
    ));
    ws.send(Message::Text(json!({"type":"hello","version":SYNC_VERSION,"application_id":vispace::model::id(),"clock":{}}).to_string().into())).await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blobs_precede_mesh_event_and_forward_across_a_chain() {
    let (a, source) = fixture();
    let (b, middle) = fixture();
    let (c, target) = fixture();
    let root = a.path().join("Attachments");
    let files = [
        ("part/original.png", b"original".as_slice()),
        ("part/preview.png", b"preview".as_slice()),
        ("part/thumbnail.png", b"thumb".as_slice()),
        ("part/sheet.pdf", b"%PDF-sheet".as_slice()),
        ("notes/diagram.png", b"notes image".as_slice()),
    ];
    for (path, bytes) in files {
        write(&root, path, bytes);
    }
    source.lock().unwrap().change(|d|{let mut p=part();p.image=Some(json!({"originalPath":root.join("part/original.png"),"previewPath":root.join("part/preview.png"),"thumbnailPath":root.join("part/thumbnail.png")}));p.datasheet=root.join("part/sheet.pdf").to_string_lossy().into_owned();p.notes.images.insert("diagram".into(),root.join("notes/diagram.png").to_string_lossy().into_owned());d.save_part(p)}).unwrap();
    let s1 = SyncServer::start(source.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("ws://{}/sync", s1.address);
    let (mut ws, _) = connect_async(&url).await.unwrap();
    hello(&mut ws).await;
    let mut seen = std::collections::BTreeSet::new();
    let mut count = 0;
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Binary(bytes) if bytes.starts_with(MAGIC) => {
                assert!(seen.insert(Blob::decode(&bytes).unwrap().asset));
            }
            Message::Binary(bytes) => {
                let transfer: Transfer = action_log::decode_frame(&bytes).unwrap();
                assert_eq!(
                    seen,
                    action_log::referenced_assets(&transfer.record).unwrap()
                );
                count += 1;
            }
            Message::Text(text) => match serde_json::from_str::<Control>(&text).unwrap() {
                Control::Topology { .. } => continue,
                Control::Ready { .. } => break,
                other => panic!("Unexpected control: {other:?}"),
            },
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seen.len(), 5);
    assert_eq!(count, 1);
    drop(ws);
    let s2 = SyncServer::start(middle.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let first = SyncClient::start(middle.clone(), &url).unwrap();
    let second = SyncClient::start(target.clone(), &format!("ws://{}/sync", s2.address)).unwrap();
    until(|| target.lock().unwrap().data.parts.len() == 1).await;
    for (dir, _) in [(&b, &middle), (&c, &target)] {
        for (path, bytes) in files {
            assert_eq!(
                std::fs::read(dir.path().join("Attachments").join(path)).unwrap(),
                bytes
            );
        }
    }
    write(
        &c.path().join("Attachments"),
        "part/new.pdf",
        b"%PDF-from-end-of-chain",
    );
    target
        .lock()
        .unwrap()
        .change(|d| {
            d.parts[0].datasheet = c
                .path()
                .join("Attachments/part/new.pdf")
                .to_string_lossy()
                .into_owned();
            Ok(())
        })
        .unwrap();
    until(|| {
        source.lock().unwrap().data.parts[0]
            .datasheet
            .ends_with("new.pdf")
    })
    .await;
    assert_eq!(
        std::fs::read(a.path().join("Attachments/part/new.pdf")).unwrap(),
        b"%PDF-from-end-of-chain"
    );
    second.shutdown().await;
    let clock = target.lock().unwrap().sync_clock();
    std::fs::remove_file(c.path().join("Attachments/part/original.png")).unwrap();
    let second = SyncClient::start(target.clone(), &format!("ws://{}/sync", s2.address)).unwrap();
    until(|| c.path().join("Attachments/part/original.png").is_file()).await;
    assert_eq!(clock, target.lock().unwrap().sync_clock());
    first.shutdown().await;
    second.shutdown().await;
    s1.shutdown().await;
    s2.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mesh_event_is_accepted_only_after_its_blob_is_saved() {
    let (a, origin) = fixture();
    let (b, store) = fixture();
    let root = a.path().join("Attachments");
    write(&root, "id/sheet.pdf", b"%PDF-test");
    origin
        .lock()
        .unwrap()
        .change(|d| {
            let mut p = part();
            p.datasheet = root.join("id/sheet.pdf").to_string_lossy().into_owned();
            d.save_part(p)
        })
        .unwrap();
    let event = origin
        .lock()
        .unwrap()
        .events_after(&Default::default())
        .unwrap()
        .remove(0);
    let transfer = Transfer {
        kind: vispace::sync::TransferKind::Event,
        record: event,
    };
    let server = SyncServer::start(store.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("ws://{}/sync", server.address);
    for upload in [false, true] {
        let (mut ws, _) = connect_async(&url).await.unwrap();
        hello(&mut ws).await;
        loop {
            match serde_json::from_str::<Control>(
                &ws.next().await.unwrap().unwrap().into_text().unwrap(),
            )
            .unwrap()
            {
                Control::Topology { .. } => continue,
                Control::Ready { .. } => break,
                other => panic!("Unexpected control: {other:?}"),
            }
        }
        if upload {
            let blob = Blob {
                asset: asset("id/sheet.pdf"),
                bytes: b"%PDF-test".to_vec(),
            };
            for _ in 0..2 {
                ws.send(Message::Binary(blob.encode().unwrap().into()))
                    .await
                    .unwrap();
            }
        }
        ws.send(Message::Binary(
            action_log::encode_frame(&transfer).unwrap().into(),
        ))
        .await
        .unwrap();
        if upload {
            loop {
                if let Message::Text(text) = ws.next().await.unwrap().unwrap() {
                    if matches!(
                        serde_json::from_str::<Control>(&text).unwrap(),
                        Control::Ack { applied: true, .. }
                    ) {
                        break;
                    }
                }
            }
            assert_eq!(
                std::fs::read(b.path().join("Attachments/id/sheet.pdf")).unwrap(),
                b"%PDF-test"
            );
        } else {
            let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
                .await
                .unwrap();
            assert!(store.lock().unwrap().data.parts.is_empty());
            assert_eq!(store.lock().unwrap().action_log().unwrap().len(), 1);
        }
    }
    server.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_source_attachment_stops_replay_before_record_is_logged() {
    let (a, source) = fixture();
    let (_b, target) = fixture();
    source
        .lock()
        .unwrap()
        .change(|d| {
            let mut p = part();
            p.datasheet = a
                .path()
                .join("Attachments/missing.pdf")
                .to_string_lossy()
                .into_owned();
            d.save_part(p)
        })
        .unwrap();
    let server = SyncServer::start(source, "127.0.0.1:0".parse().unwrap()).unwrap();
    let client =
        SyncClient::start(target.clone(), &format!("ws://{}/sync", server.address)).unwrap();
    until(|| matches!(client.status(), ClientStatus::Failed(_))).await;
    assert!(target.lock().unwrap().data.parts.is_empty());
    assert_eq!(target.lock().unwrap().action_log().unwrap().len(), 1);
    client.shutdown().await;
    server.shutdown().await;
}
#[test]
fn corrupt_truncated_and_unsafe_blob_messages_are_rejected() {
    let blob = Blob {
        asset: asset("notes/diagram.png"),
        bytes: vec![42; 1024],
    };
    let message = blob.encode().unwrap();
    assert_eq!(Blob::decode(&message).unwrap().bytes, blob.bytes);
    for end in [0, 7, 11, message.len() - 1] {
        assert!(Blob::decode(&message[..end]).is_err());
    }
    let mut corrupt = message.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(Blob::decode(&corrupt).is_err());
    for name in [
        "../outside.pdf",
        "/absolute.png",
        "folder/../outside.png",
        "folder\\outside.png",
        "C:/escape.pdf",
        "folder/./image.png",
        "",
    ] {
        let id = asset(name);
        assert!(action_log::asset_relative_path(&id).is_err());
        let header = json!({"asset":id,"size":0,"crc32":0})
            .to_string()
            .into_bytes();
        let mut frame = MAGIC.to_vec();
        frame.extend_from_slice(&(header.len() as u32).to_le_bytes());
        frame.extend(header);
        assert!(Blob::decode(&frame).is_err());
    }
}
#[test]
fn duplicate_blobs_reuse_files_and_conflicting_bytes_never_overwrite() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("Attachments");
    let mut blob = Blob {
        asset: asset("id/sheet.pdf"),
        bytes: b"%PDF-original".to_vec(),
    };
    blob.save(&root).unwrap();
    blob.save(&root).unwrap();
    blob.bytes = b"replacement".to_vec();
    assert!(blob.save(&root).is_err());
    assert_eq!(
        std::fs::read(root.join("id/sheet.pdf")).unwrap(),
        b"%PDF-original"
    );
    assert_eq!(std::fs::read_dir(root.join("id")).unwrap().count(), 1);
}
#[cfg(unix)]
#[test]
fn symlinks_cannot_redirect_blob_reads_or_writes_outside_attachment_root() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let root = dir.path().join("Attachments");
    std::fs::create_dir(&root).unwrap();
    symlink(outside.path(), root.join("escape")).unwrap();
    let blob = Blob {
        asset: asset("escape/file.pdf"),
        bytes: b"pdf".to_vec(),
    };
    assert!(blob.save(&root).is_err());
    assert!(Blob::read(&root, &blob.asset).is_err());
    assert!(!outside.path().join("file.pdf").exists());
    std::fs::write(outside.path().join("secret.png"), b"private").unwrap();
    symlink(outside.path().join("secret.png"), root.join("file.png")).unwrap();
    let blob = Blob {
        asset: asset("file.png"),
        bytes: b"new".to_vec(),
    };
    assert!(blob.save(&root).is_err());
    assert!(Blob::read(&root, &blob.asset).is_err());
    assert_eq!(
        std::fs::read(outside.path().join("secret.png")).unwrap(),
        b"private"
    );
}
