//! Optional loopback-only Chrome extension WebSocket service sharing the same transactional store as egui.
use crate::purchase_import::{patch_part, purchase, quantity, string};
use crate::{
    attachments::AttachmentDraft,
    model::*,
    store::{SharedStore, Store},
};
use anyhow::{Result, bail, ensure};
use base64::Engine;
use serde_json::{Value, json};
pub(crate) mod transport {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

    pub async fn serve<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send>(
        mut socket: WebSocketStream<S>,
        store: SharedStore,
    ) -> Result<()> {
        while let Some(message) = socket.next().await {
            let message = message?;
            if message.is_close() {
                break;
            }
            let Message::Text(message) = message else {
                continue;
            };
            if message.len() > 1_048_576 {
                socket
                    .send(Message::Text(
                        json!({"error": "Request too large"}).to_string().into(),
                    ))
                    .await?;
                break;
            }
            let request: Value = match serde_json::from_str(&message) {
                Ok(request) => request,
                Err(_) => {
                    socket
                        .send(Message::Text(
                            json!({"error": "Invalid JSON request"}).to_string().into(),
                        ))
                        .await?;
                    continue;
                }
            };
            let result = match request["operation"].as_str() {
                Some("health") => Ok(
                    json!({"application": "bom-inventory", "protocol": 1, "purchaseRequestIDs": true}),
                ),
                Some("projects") => Ok(json!(store.lock().unwrap().data.projects)),
                Some("purchase") => {
                    let store = store.clone();
                    tokio::task::spawn_blocking(move || {
                        handle_request(
                            &mut store.lock().unwrap(),
                            "POST",
                            "/v1/purchases",
                            request["payload"].clone(),
                        )
                        .map(|(_, value)| value)
                    })
                    .await?
                }
                _ => Err(anyhow::anyhow!("Unknown extension operation")),
            };
            let response = match result {
                Ok(value) => json!({"value": value}),
                Err(error) => json!({"error": error.to_string()}),
            };
            socket
                .send(Message::Text(response.to_string().into()))
                .await?;
            socket.close(None).await?;
            break;
        }
        Ok(())
    }
}
/// The extension and tests share the attachment-aware transactional entry point.
/// Temporary pixels and any promoted files are discarded if validation or persistence fails.
pub fn handle_request(
    store: &mut Store,
    method: &str,
    path: &str,
    body: Value,
) -> Result<(u16, Value)> {
    if method == "GET" && path == "/v1/action-log" {
        return Ok((200, json!(store.action_log()?)));
    }
    if method == "POST"
        && path == "/v1/purchases"
        && let Some(existing) = crate::purchase_import::existing(&store.data, &body)?
    {
        return Ok((200, json!(existing)));
    }
    let Some(encoded) = body.get("imageBase64") else {
        return if method == "GET" {
            route(&mut store.data.clone(), method, path, body)
        } else {
            store.change(|data| route(data, method, path, body))
        };
    };
    let url = url::Url::parse(&format!("http://localhost{path}"))?;
    let pieces: Vec<_> = url
        .path_segments()
        .unwrap()
        .filter(|s| !s.is_empty())
        .collect();
    ensure!(
        (method == "POST" && pieces == ["v1", "purchases"])
            || (method == "PATCH" && pieces.len() == 3 && pieces[..2] == ["v1", "purchases"]),
        "imageBase64 is supported on purchase POST and PATCH requests"
    );
    const MESSAGE: &str = "imageBase64 must contain a supported image up to 600 KB.";
    let encoded = encoded.as_str().ok_or_else(|| anyhow::anyhow!(MESSAGE))?;
    ensure!(!encoded.is_empty() && encoded.len() <= 800_000, MESSAGE);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!(MESSAGE))?;
    ensure!(!bytes.is_empty() && bytes.len() <= 600_000, MESSAGE);
    let draft = AttachmentDraft::new()?;
    let staged = draft
        .image_bytes(&bytes)
        .map_err(|_| anyhow::anyhow!(MESSAGE))?;
    let mut next = store.data.clone();
    let (status, body) = route_inner(&mut next, method, path, body, Some(&staged))?;
    next.validate()?;
    let id = body["part"]["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Purchase response is missing its ID"))?
        .to_owned();
    let index = next
        .purchases
        .iter()
        .position(|p| p.part.id == id)
        .ok_or_else(|| anyhow::anyhow!("Purchase not found"))?;
    let mut created = Vec::new();
    let result = (|| -> Result<(u16, Value)> {
        let item = &mut next.purchases[index];
        let image = item.part.image.as_mut().unwrap();
        for key in ["originalPath", "thumbnailPath", "previewPath"] {
            let path = image[key]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing image derivative"))?;
            let saved = store.attach(std::path::Path::new(path), &id)?;
            image[key] = json!(saved);
            created.push(saved);
        }
        let response = json!(item);
        store.change(|data| {
            *data = next;
            Ok((status, response))
        })
    })();
    if result.is_err() && !store.recovery_required() {
        for path in &created {
            let _ = std::fs::remove_file(path);
        }
        for path in created {
            if let Some(parent) = path.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
    result
}
pub fn route(data: &mut Inventory, method: &str, path: &str, body: Value) -> Result<(u16, Value)> {
    let mut next = data.clone();
    let result = route_inner(&mut next, method, path, body, None)?;
    next.reconcile_designator_priority(data)?;
    next.validate()?;
    *data = next;
    Ok(result)
}
fn requested_rows(data: &Inventory, project: &str) -> Value {
    let pending = data.requested_items();
    json!(data.bom.iter().filter(|e| key(&e.project) == key(project) && pending.get(&e.id).is_some_and(|i| i.quantity > 0)).map(|e| {
        let i = &pending[&e.id];
        json!({"id": e.id, "designators": i.designators, "package": i.package, "quantity": i.quantity, "value": i.value, "spn": i.spn, "supplier": i.supplier})
    }).collect::<Vec<_>>())
}
fn route_inner(
    data: &mut Inventory,
    method: &str,
    path: &str,
    body: Value,
    image: Option<&Value>,
) -> Result<(u16, Value)> {
    let url = url::Url::parse(&format!("http://localhost{path}"))?;
    let pieces: Vec<String> = url
        .path_segments()
        .unwrap()
        .filter(|s| !s.is_empty())
        .map(|s| {
            url::form_urlencoded::parse(format!("v={}", s.replace('+', "%2B")).as_bytes())
                .next()
                .unwrap()
                .1
                .into_owned()
        })
        .collect();
    let p: Vec<&str> = pieces.iter().map(String::as_str).collect();
    match (method, p.as_slice()) {
        ("GET", ["health"]) => Ok((
            200,
            json!({"status":"ok","implementation":"rust","purchaseRequestIDs":true}),
        )),
        ("GET", ["v1", "parts"]) => {
            let q = url
                .query_pairs()
                .find(|(k, _)| k == "q")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            Ok((
                200,
                json!(
                    data.parts
                        .iter()
                        .filter(|p| p.matches(&q))
                        .collect::<Vec<_>>()
                ),
            ))
        }
        ("POST", ["v1", "parts"]) => {
            let mut fields = body.clone();
            let barcode = fields.as_object_mut().and_then(|o| o.remove("barcode"));
            let mut p = patch_part(&Part::default(), &fields)?;
            if let Some(v) = barcode {
                p.barcode = v
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Barcode must be a string"))?
                    .into();
            }
            data.save_part(p.clone())?;
            Ok((201, json!(p)))
        }
        (m, ["v1", "parts", id]) => {
            let old = data
                .parts
                .iter()
                .find(|p| p.id == *id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Not found: part"))?;
            match m {
                "GET" => Ok((200, json!(old))),
                "PATCH" => {
                    let p = patch_part(&old, &body)?;
                    data.save_part(p.clone())?;
                    Ok((200, json!(p)))
                }
                "DELETE" => {
                    data.delete_part(id);
                    Ok((200, json!({"deleted":id})))
                }
                _ => bail!("Unsupported route or method"),
            }
        }
        ("GET", ["v1", "projects"]) => Ok((200, json!(data.projects))),
        ("POST", ["v1", "projects"]) => {
            let name = string(&body, "name")?;
            ensure!(
                !data.projects.iter().any(|p| key(&p.name) == key(name)),
                "Project already exists"
            );
            let name = data.ensure_project(name)?;
            let project = data.projects.iter_mut().find(|p| p.name == name).unwrap();
            project.description = body
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
            Ok((201, json!(project)))
        }
        (m, ["v1", "projects", name]) => {
            let old = data
                .projects
                .iter()
                .find(|p| key(&p.name) == key(name))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Not found: project"))?;
            match m {
                "GET" => Ok((200, json!(old))),
                "PATCH" => {
                    let new = body
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&old.name);
                    let description = body
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or(&old.description);
                    data.rename_project(&old.name, new, description)?;
                    Ok((
                        200,
                        json!(
                            data.projects
                                .iter()
                                .find(|p| key(&p.name) == key(new))
                                .unwrap()
                        ),
                    ))
                }
                "DELETE" => {
                    data.delete_project(name);
                    Ok((200, json!({"deleted":name})))
                }
                _ => bail!("Unsupported route or method"),
            }
        }
        ("GET", ["v1", "projects", name, "allocations"]) => Ok((
            200,
            json!(
                data.allocations
                    .iter()
                    .filter(|a| key(&a.project) == key(name))
                    .collect::<Vec<_>>()
            ),
        )),
        ("POST", ["v1", "projects", name, "allocations"]) => {
            let id = data.allocate(
                string(&body, "partID")?,
                name,
                quantity(&body, 1)?,
                body.get("designators")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )?;
            Ok((
                201,
                json!(data.allocations.iter().find(|a| a.id == id).unwrap()),
            ))
        }
        (m, ["v1", "projects", name, "allocations", id]) => {
            let i = data
                .allocations
                .iter()
                .position(|a| a.id == *id && key(&a.project) == key(name))
                .ok_or_else(|| anyhow::anyhow!("Not found: allocation"))?;
            match m {
                "DELETE" => {
                    data.release(id);
                    Ok((200, json!({"released":id})))
                }
                "PATCH" => {
                    let a = &mut data.allocations[i];
                    if let Some(v) = body.get("designators") {
                        a.designators = v
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid designators"))?
                            .into();
                    }
                    a.quantity =
                        if body.get("designators").is_some() && !a.designators.trim().is_empty() {
                            references(&a.designators)?.len() as i64
                        } else {
                            quantity(&body, a.quantity)?
                        };
                    Ok((200, json!(a)))
                }
                _ => bail!("Unsupported route or method"),
            }
        }
        ("GET", ["v1", "projects", name, "requested"]) => Ok((200, requested_rows(data, name))),
        ("POST", ["v1", "projects", name, "requested"]) => {
            let project = data.ensure_project(name)?;
            let item: BomItem = serde_json::from_value(body)?;
            ensure!(
                item.quantity > 0 && !item.value.trim().is_empty(),
                "Value and positive quantity are required"
            );
            references(&item.designators)?;
            let e = BomEntry {
                kicad_source: None,
                id: id(),
                project,
                item,
                part_id: None,
                allocation_id: None,
            };
            data.bom.push(e.clone());
            Ok((201, json!(e)))
        }
        (m, ["v1", "projects", name, "requested", id]) => {
            let i = data
                .bom
                .iter()
                .position(|e| e.id == *id && key(&e.project) == key(name))
                .ok_or_else(|| anyhow::anyhow!("Not found: request"))?;
            ensure!(
                !matches!(m, "PATCH" | "DELETE") || data.bom[i].kicad_source.is_none(),
                "KiCad requested parts are read-only. Edit this part in KiCad."
            );
            match m {
                "DELETE" => {
                    data.bom.remove(i);
                    Ok((200, json!({"deleted":id})))
                }
                "PATCH" => {
                    let mut v = serde_json::to_value(&data.bom[i].item)?;
                    for (k, value) in body
                        .as_object()
                        .ok_or_else(|| anyhow::anyhow!("Expected object"))?
                    {
                        v[k] = value.clone();
                    }
                    let item: BomItem = serde_json::from_value(v)?;
                    ensure!(
                        item.quantity > 0 && !item.value.trim().is_empty(),
                        "Value and positive quantity are required"
                    );
                    references(&item.designators)?;
                    data.bom[i].item = item;
                    Ok((200, json!(data.bom[i])))
                }
                _ => bail!("Unsupported route or method"),
            }
        }
        ("GET", ["v1", "purchases"]) => Ok((200, json!(data.purchases))),
        ("POST", ["v1", "purchases"]) => {
            let (created, p) = crate::purchase_import::import(data, &body, image)?;
            Ok((if created { 201 } else { 200 }, json!(p)))
        }
        ("POST", ["v1", "purchases", "receive"]) => {
            let ids: Vec<String> = serde_json::from_value(
                body.get("ids")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("IDs required"))?,
            )?;
            let received = data.receive(&ids)?;
            Ok((
                200,
                json!({"receivedPartIDs":received,"receivedPurchaseIDs":ids}),
            ))
        }
        (m, ["v1", "purchases", id]) => {
            let i = data
                .purchases
                .iter()
                .position(|p| p.part.id == *id)
                .ok_or_else(|| anyhow::anyhow!("Not found: purchase"))?;
            match m {
                "GET" => Ok((200, json!(data.purchases[i]))),
                "DELETE" => {
                    data.purchases.remove(i);
                    Ok((200, json!({"deleted":id})))
                }
                "PATCH" => {
                    let p = purchase(&body, Some(&data.purchases[i]), image)?;
                    for a in &p.assignments {
                        data.ensure_project(&a.project)?;
                    }
                    data.purchases[i] = p.clone();
                    Ok((200, json!(p)))
                }
                _ => bail!("Unsupported route or method"),
            }
        }
        ("GET", ["v1", "projects", name, "purchase"]) => Ok((
            200,
            json!(
                data.purchases
                    .iter()
                    .filter(|p| p.assignments.iter().any(|a| key(&a.project) == key(name)))
                    .collect::<Vec<_>>()
            ),
        )),
        (m, segments)
            if (m == "POST" && matches!(segments, ["v1", "projects", _, "purchase"]))
                || ((m == "PATCH" || m == "DELETE")
                    && matches!(segments, ["v1", "projects", _, "purchase", _])) =>
        {
            let project = data.ensure_project(segments[2])?;
            let purchase_id = if m == "POST" {
                string(&body, "purchaseID")?
            } else {
                segments[4]
            };
            let p = data
                .purchases
                .iter_mut()
                .find(|p| p.part.id == purchase_id)
                .ok_or_else(|| anyhow::anyhow!("Not found: purchase"))?;
            if m == "DELETE" {
                p.assignments.retain(|a| key(&a.project) != key(&project));
                return Ok((200, json!(p)));
            }
            let refs = body
                .get("designators")
                .and_then(Value::as_str)
                .unwrap_or("");
            let q = if m == "PATCH" && !refs.is_empty() {
                references(refs)?.len() as i64
            } else {
                quantity(&body, 1)?
            };
            if let Some(a) = p
                .assignments
                .iter_mut()
                .find(|a| key(&a.project) == key(&project))
            {
                if m == "POST" {
                    a.quantity = a
                        .quantity
                        .checked_add(q)
                        .ok_or_else(|| anyhow::anyhow!("Quantity overflow"))?;
                    if !refs.is_empty() {
                        if !a.designators.is_empty() {
                            a.designators.push_str(", ");
                        }
                        a.designators.push_str(refs);
                    }
                } else {
                    if body.get("quantity").is_some() || body.get("designators").is_some() {
                        a.quantity = q;
                    }
                    if body.get("designators").is_some() {
                        a.designators = refs.into();
                    }
                }
            } else {
                ensure!(m == "POST", "Not found: assignment");
                p.assignments.push(Assignment {
                    id: id(),
                    project,
                    quantity: q,
                    designators: refs.into(),
                });
            }
            let assigned = p
                .assignments
                .iter()
                .try_fold(0_i64, |sum, a| sum.checked_add(a.quantity))
                .ok_or_else(|| anyhow::anyhow!("Quantity overflow"))?;
            p.part.stock = p.part.stock.max(assigned);
            Ok((200, json!(p)))
        }
        _ => bail!("Unsupported route or method"),
    }
}
