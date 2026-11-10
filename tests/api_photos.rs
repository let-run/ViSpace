use base64::Engine;
use vispace::{api, store::Store, sync::SyncServer};
use image::{DynamicImage, ImageFormat};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

fn photo(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(width, height)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}
fn encoded(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
fn files(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(files(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}
fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open(dir.path().join("inventory.sqlite")).unwrap();
    (dir, store)
}

#[test]
fn purchase_post_patch_and_receive_preserve_managed_photos() {
    let (dir, mut store) = store();
    let (status,saved)=api::handle_request(&mut store,"POST","/v1/purchases",json!({"part":{"number":"PHOTO-R","manufacturer":"Maker"},"quantity":4,"imageBase64":encoded(&photo(320,160,ImageFormat::Png))})).unwrap();
    assert_eq!(status, 201);
    let id = saved["part"]["id"].as_str().unwrap();
    let image = &saved["part"]["image"];
    for (key, dimensions) in [
        ("originalPath", (320, 160)),
        ("thumbnailPath", (160, 80)),
        ("previewPath", (800, 400)),
    ] {
        let path = Path::new(image[key].as_str().unwrap());
        assert!(path.starts_with(dir.path().join("Attachments").join(id)));
        assert_eq!(image::image_dimensions(path).unwrap(), dimensions);
    }
    let old = image.clone();
    api::handle_request(
        &mut store,
        "PATCH",
        &format!("/v1/purchases/{id}"),
        json!({"quantity":5}),
    )
    .unwrap();
    assert_eq!(store.data.purchases[0].part.image.as_ref().unwrap(), &old);
    let (_, updated) = api::handle_request(
        &mut store,
        "PATCH",
        &format!("/v1/purchases/{id}"),
        json!({"imageBase64":encoded(&photo(400,800,ImageFormat::Jpeg))}),
    )
    .unwrap();
    assert_eq!(updated["part"]["manufacturer"], "Maker");
    assert_eq!(updated["part"]["stock"], 5);
    assert_ne!(updated["part"]["image"], old);
    assert_eq!(
        image::image_dimensions(updated["part"]["image"]["thumbnailPath"].as_str().unwrap())
            .unwrap(),
        (80, 160)
    );
    let path = store.path.clone();
    drop(store);
    let mut reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened.data.purchases[0].part.image.as_ref().unwrap(),
        &updated["part"]["image"]
    );
    api::handle_request(
        &mut reopened,
        "POST",
        "/v1/purchases/receive",
        json!({"ids":[id]}),
    )
    .unwrap();
    assert!(reopened.data.purchases.is_empty());
    assert_eq!(
        reopened.data.parts[0].image.as_ref().unwrap(),
        &updated["part"]["image"]
    );
    assert!(
        Path::new(
            reopened.data.parts[0].image.as_ref().unwrap()["originalPath"]
                .as_str()
                .unwrap()
        )
        .is_file()
    );
}
#[test]
fn bad_photo_or_purchase_rejects_the_entire_save_without_attachment_leaks() {
    let (dir, mut store) = store();
    let valid = encoded(&photo(2, 2, ImageFormat::Png));
    for bad in [
        json!(null),
        json!(42),
        json!(""),
        json!("%%%"),
        json!(encoded(b"not an image")),
        json!(format!("data:image/png;base64,{valid}")),
        json!(encoded(&vec![0; 600_001])),
    ] {
        assert!(
            api::handle_request(
                &mut store,
                "POST",
                "/v1/purchases",
                json!({"part":{"number":"Rejected"},"quantity":1,"imageBase64":bad})
            )
            .is_err()
        );
    }
    assert!(api::handle_request(&mut store,"POST","/v1/purchases",json!({"part":{"number":"Rejected"},"quantity":1,"assignments":[{"project":"Bad project","quantity":2}],"imageBase64":valid})).is_err());
    assert!(store.data.purchases.is_empty());
    assert!(store.data.projects.is_empty());
    assert!(files(&dir.path().join("Attachments")).is_empty());
    let path = store.path.clone();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert!(store.data.purchases.is_empty());
    assert!(api::handle_request(&mut store,"POST","/v1/purchases",json!({"part":{"number":"No paths","image":{"originalPath":"/tmp/arbitrary"}},"quantity":1})).is_err());
}
#[test]
fn failed_patch_keeps_existing_photo_and_pending_wal_preserves_promoted_files() {
    let (dir, mut store) = store();
    let (_,saved)=api::handle_request(&mut store,"POST","/v1/purchases",json!({"part":{"number":"Existing"},"quantity":1,"imageBase64":encoded(&photo(8,4,ImageFormat::Png))})).unwrap();
    let before = files(&dir.path().join("Attachments"));
    let id = saved["part"]["id"].as_str().unwrap();
    assert!(
        api::handle_request(
            &mut store,
            "PATCH",
            &format!("/v1/purchases/{id}"),
            json!({"imageBase64":encoded(b"broken")})
        )
        .is_err()
    );
    assert_eq!(
        store.data.purchases[0].part.image.as_ref().unwrap(),
        &saved["part"]["image"]
    );
    rusqlite::Connection::open(&store.path)
        .unwrap()
        .execute_batch("DROP TABLE purchases")
        .unwrap();
    assert!(
        api::handle_request(
            &mut store,
            "PATCH",
            &format!("/v1/purchases/{id}"),
            json!({"imageBase64":encoded(&photo(10,5,ImageFormat::Png))})
        )
        .is_err()
    );
    let mut after = files(&dir.path().join("Attachments"));
    let mut before = before;
    after.sort();
    before.sort();
    assert!(store.recovery_required());
    assert_eq!(after.len(), before.len() + 3);
    assert!(before.iter().all(|path| after.contains(path)));
    assert_eq!(
        store.data.purchases[0].part.image.as_ref().unwrap(),
        &saved["part"]["image"]
    );
    let path = store.path.clone();
    drop(store);
    let recovered = Store::open(path).unwrap();
    let image = recovered.data.purchases[0].part.image.as_ref().unwrap();
    assert_ne!(image, &saved["part"]["image"]);
    for key in ["originalPath", "thumbnailPath", "previewPath"] {
        assert!(Path::new(image[key].as_str().unwrap()).is_file());
    }
}
async fn extension_request(address: &str, command: Value) -> Value {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

    let mut request = format!("ws://{address}/extension")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "origin",
        "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .parse()
            .unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(Message::Text(command.to_string().into()))
        .await
        .unwrap();
    serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
}

#[test]
fn extension_listener_imports_photos_and_rejects_other_operations() {
    let (_dir, store) = store();
    let store = Arc::new(Mutex::new(store));
    let server = SyncServer::start(store.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let runtime = vispace::async_io::runtime().unwrap();
    let mut boundary = photo(4, 2, ImageFormat::Png);
    boundary.resize(600_000, 0);
    runtime.block_on(async {
        let saved = extension_request(&server.address.to_string(), json!({"operation":"purchase","payload":{"part":{"number":"Extension import"},"quantity":2,"imageBase64":encoded(&boundary)}})).await;
        assert!(Path::new(saved["value"]["part"]["image"]["originalPath"].as_str().unwrap()).is_file());
        let denied = extension_request(&server.address.to_string(), json!({"operation":"deletePart"})).await;
        assert_eq!(denied["error"], "Unknown extension operation");
        let invalid = extension_request(&server.address.to_string(), json!({"operation":"purchase","payload":{"part":{"number":"Invalid"},"imageBase64":"invalid"}})).await;
        assert!(invalid["error"].is_string());
    });
    assert_eq!(store.lock().unwrap().data.purchases.len(), 1);
}

#[test]
fn extension_listener_rejects_http_and_restarts_on_same_port() {
    let (_dir, store) = store();
    let store = Arc::new(Mutex::new(store));
    let server = SyncServer::start(store.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = server.address.to_string();
    let bind = server.address;
    let response = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
        .get(format!("http://{address}/v1/projects"))
        .send();
    assert!(!response.is_ok_and(|response| response.status().is_success()));
    let runtime = vispace::async_io::runtime().unwrap();
    assert!(
        runtime
            .block_on(tokio_tungstenite::connect_async(format!(
                "ws://{address}/extension"
            )))
            .is_err(),
        "connections without an extension origin must be rejected"
    );
    runtime.block_on(server.shutdown());
    let restarted = SyncServer::start(store, bind).unwrap();
    let response = runtime.block_on(extension_request(
        &restarted.address.to_string(),
        json!({"operation":"projects"}),
    ));
    assert_eq!(response["value"], json!([]));
}
