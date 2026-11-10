//! Pure Rust action engine shared with the SQLite browser worker.
use crate::{
    action_log::{self, Record},
    mesh::Mesh,
    model::Inventory,
};
use std::path::Path;
use wasm_bindgen::prelude::*;
const ROOT: &str = "/browser/Attachments";
fn error(e: anyhow::Error) -> JsValue {
    JsValue::from_str(&e.to_string())
}
fn replay(history: &str) -> anyhow::Result<Mesh> {
    let records: Vec<Record> = serde_json::from_str(history)?;
    let mut mesh = Mesh::default();
    let mut legacy = Inventory::default();
    let mut prior = None;
    let mut lamport = 0;
    for r in records {
        if r.mesh.is_some() {
            mesh.insert(r)?;
        } else {
            let mut next = action_log::apply(&legacy, &r, Path::new(ROOT))?;
            crate::mesh::prepare_ids(&mut legacy);
            crate::mesh::prepare_ids(&mut next);
            if !r.actions.is_empty() {
                let mut converted = mesh.make(&legacy, &next, Path::new(ROOT), &r.id, 1)?;
                converted.id = r.id.clone();
                converted.timestamp_ms = r.timestamp_ms;
                converted.initial = r.initial;
                let m = converted.mesh.as_mut().unwrap();
                lamport += 1;
                m.lamport = lamport;
                m.context.clear();
                if let Some(id) = prior {
                    m.context.insert(id, 1);
                }
                prior = Some(r.id);
                mesh.insert(converted)?;
            }
            legacy = next;
        }
    }
    Ok(mesh)
}
#[wasm_bindgen]
pub fn browser_storage_replay(history: &str) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let mut mesh=replay(history)?;
        let data=mesh.project(Path::new(ROOT))?;
        let mut value=serde_json::to_value(data)?;
        action_log::paths(&mut value,Path::new(ROOT),true)?;
        Ok(serde_json::json!({"inventory":value,"events":mesh.events.into_values().collect::<Vec<_>>(),"clock":mesh.clock}).to_string())
    })().map_err(error)
}
#[wasm_bindgen]
pub fn browser_storage_make(
    before: &str,
    after: &str,
    history: &str,
    origin: &str,
) -> Result<String, JsValue> {
    (move || -> anyhow::Result<String> {
        let mut mesh = replay(history)?;
        mesh.project(Path::new(ROOT))?;
        let mut a = serde_json::from_str(before)?;
        let mut b = serde_json::from_str(after)?;
        action_log::paths(&mut a, Path::new(ROOT), false)?;
        action_log::paths(&mut b, Path::new(ROOT), false)?;
        let before: Inventory = serde_json::from_value(a)?;
        let after: Inventory = serde_json::from_value(b)?;
        after.validate()?;
        let seq = mesh
            .events
            .values()
            .filter_map(|r| r.mesh.as_ref())
            .filter(|m| m.origin == origin)
            .map(|m| m.sequence)
            .max()
            .unwrap_or(0)
            + 1;
        Ok(serde_json::to_string(&mesh.make(
            &before,
            &after,
            Path::new(ROOT),
            origin,
            seq,
        )?)?)
    })()
    .map_err(error)
}
#[wasm_bindgen]
pub fn browser_storage_frame(record: &str) -> Result<Vec<u8>, JsValue> {
    (move || -> anyhow::Result<Vec<u8>> {
        action_log::encode_frame(&serde_json::from_str::<Record>(record)?)
    })()
    .map_err(error)
}
#[wasm_bindgen]
pub fn browser_storage_decode(bytes: &[u8]) -> Result<String, JsValue> {
    (move || -> anyhow::Result<String> {
        anyhow::ensure!(bytes.starts_with(b"BOMLOG01"), "Invalid action chunk");
        let mut offset = 8;
        let mut records = vec![];
        while bytes.len() - offset >= 4 {
            let size = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            anyhow::ensure!(size > 0 && size <= 64 * 1024 * 1024, "Invalid record size");
            if bytes.len() - offset - 4 < size {
                break;
            }
            let record: Record = action_log::decode_frame(&bytes[offset..offset + 4 + size])?;
            records.push(record);
            offset += 4 + size;
        }
        Ok(serde_json::to_string(&records)?)
    })()
    .map_err(error)
}

/// A staged history may contain conflicts while the visible SQLite snapshot stays unchanged.
#[wasm_bindgen]
pub fn browser_sync_inspect(history: &str, origin: &str, pulling: bool) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let mut mesh = replay(history)?;
        let data = mesh.project(Path::new(ROOT))?;
        let reviews = crate::merge_review::conflicts(&mesh.events, origin);
        let blocked: Vec<_> = reviews.iter().flat_map(|r| &r.events)
            .filter_map(|id| mesh.events.get(id).and_then(|r|r.mesh.as_ref())).collect();
        let publish: Vec<_> = mesh.missing(&Default::default()).into_iter().filter(|r| {
            let m = r.mesh.as_ref().unwrap();
            (!pulling || m.origin != origin) && !blocked.iter().any(|b|
                (m.origin == b.origin && m.sequence >= b.sequence) ||
                m.context.get(&b.origin).copied().unwrap_or(0) >= b.sequence)
        }).collect();
        let shared = publish.iter().cloned().map(|r|(r.id.clone(),r)).collect();
        let mut value = serde_json::to_value(data)?;
        action_log::paths(&mut value,Path::new(ROOT),true)?;
        let drafts: Vec<_> = reviews.iter().map(|r| if r.markdown {
            let mut value=r.ours.clone();
            let text=crate::merge_review::merge_text(&crate::merge_review::markdown(&r.base), &crate::merge_review::markdown(&r.ours), &crate::merge_review::markdown(&r.theirs));
            value["markdownSource"]=text.clone().into();value["text"]=text.into();value
        } else {r.ours.clone()}).collect();
        Ok(serde_json::json!({"inventory":value,"events":mesh.events.values().collect::<Vec<_>>(),"clock":mesh.clock,
            "appliedTip":mesh.applied_tip_hash()?,"head":crate::merge_review::head(&mesh.events,mesh.clock.clone()),"sharedHead":crate::merge_review::head(&shared,mesh.clock.clone()),
            "reviews":reviews,"drafts":drafts,"warnings":mesh.conflicts,"publish":publish}).to_string())
    })().map_err(error)
}
#[wasm_bindgen]
pub fn browser_sync_resolve(
    history: &str,
    origin: &str,
    id: &str,
    value: &str,
) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let mesh = replay(history)?;
        Ok(serde_json::to_string(&crate::merge_review::resolve(
            &mesh,
            Path::new(ROOT),
            origin,
            id,
            serde_json::from_str(value)?,
        )?)?)
    })()
    .map_err(error)
}
#[wasm_bindgen]
pub fn browser_sync_encode(record: &str, proposal: bool) -> Result<Vec<u8>, JsValue> {
    (|| -> anyhow::Result<Vec<u8>> {
        let record: Record = serde_json::from_str(record)?;
        action_log::encode_frame(
            &serde_json::json!({"kind":if proposal {"proposal"} else {"event"},"record":record}),
        )
    })()
    .map_err(error)
}
#[wasm_bindgen]
pub fn browser_proposal_reviews(history: &str, draft: &str) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        Ok(serde_json::to_string(&crate::merge_review::draft_reviews(
            &mut replay(history)?,
            &serde_json::from_str(draft)?,
            Path::new(ROOT),
        )?)?)
    })()
    .map_err(error)
}
#[wasm_bindgen]
pub fn browser_rebase_proposal(
    history: &str,
    draft: &str,
    choices: &str,
) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        Ok(serde_json::to_string(&crate::merge_review::rebase_draft(
            &mut replay(history)?,
            &serde_json::from_str(draft)?,
            &serde_json::from_str(choices)?,
            Path::new(ROOT),
        )?)?)
    })()
    .map_err(error)
}
#[wasm_bindgen]
pub fn browser_sync_decode(bytes: &[u8]) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let value: serde_json::Value = action_log::decode_frame(bytes)?;
        anyhow::ensure!(value["kind"] == "event", "Expected a committed event frame");
        let record: Record = serde_json::from_value(value["record"].clone())?;
        Ok(serde_json::to_string(&record)?)
    })()
    .map_err(error)
}
#[wasm_bindgen]
pub fn browser_sync_assets(record: &str) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        Ok(serde_json::to_string(&action_log::referenced_assets(
            &serde_json::from_str(record)?,
        )?)?)
    })()
    .map_err(error)
}

#[wasm_bindgen]
pub fn browser_storage_local(shared: &str, local: &str) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let mut shared: Inventory = serde_json::from_str(shared)?;
        let local: Inventory = serde_json::from_str(local)?;
        shared.restore_local_project_data(&local);
        shared.validate()?;
        Ok(serde_json::to_string(&shared)?)
    })()
    .map_err(error)
}

#[wasm_bindgen]
pub struct BrowserTopology {
    inner: crate::topology::Topology,
}
#[wasm_bindgen]
impl BrowserTopology {
    #[wasm_bindgen(constructor)]
    pub fn new(id: String, epoch: u32, now: f64) -> Self {
        let title = format!("Browser {}", id.chars().take(8).collect::<String>());
        Self {
            inner: crate::topology::Topology::new(id, title, epoch.into(), now as u64),
        }
    }
    pub fn connect(&mut self, id: &str, now: f64) -> Result<(), JsValue> {
        self.inner.connect(id, now as u64).map_err(error)
    }
    pub fn disconnect(&mut self, id: &str, now: f64) {
        self.inner.disconnect(id, now as u64);
    }
    pub fn receive(&mut self, ads: &str, now: f64) -> Result<bool, JsValue> {
        (|| -> anyhow::Result<bool> { self.inner.receive(serde_json::from_str(ads)?, now as u64) })(
        )
        .map_err(error)
    }
    pub fn heartbeat(&mut self, now: f64) -> bool {
        self.inner.heartbeat(now as u64)
    }
    pub fn ads(&mut self, now: f64) -> String {
        serde_json::to_string(&self.inner.advertisements(now as u64)).unwrap()
    }
    pub fn graph(&mut self, now: f64) -> String {
        serde_json::to_string(&self.inner.graph(now as u64)).unwrap()
    }
}

#[wasm_bindgen]
pub fn browser_history_chunk(
    bytes: &[u8],
    chunk: &str,
    before: Option<u32>,
    limit: usize,
) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let page = action_log::history_chunk_page(bytes, chunk, before.map(u64::from), limit)?;
        Ok(serde_json::to_string(&page)?)
    })()
    .map_err(error)
}

#[wasm_bindgen]
pub fn browser_branch_merge(
    local: &str,
    remote: &str,
    author: &str,
    choices: &str,
    apply: bool,
) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let local: Vec<Record> = serde_json::from_str(local)?;
        let remote: Vec<Record> = serde_json::from_str(remote)?;
        let local_ids = local.iter().map(|record| record.id.as_str()).collect();
        let mut mesh = Mesh::default();
        for record in local.iter().chain(&remote) {
            mesh.insert(record.clone())?;
        }
        let root = Path::new(ROOT);
        mesh.project(root)?;
        let choices: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_str(choices)?;
        let reviews = crate::merge_review::conflicts_for_branch(&mesh.events, author, &local_ids);
        if !apply {
            return Ok(serde_json::to_string(
                &reviews
                    .into_iter()
                    .filter(|review| !choices.contains_key(&review.id))
                    .collect::<Vec<_>>(),
            )?);
        }
        let origin = crate::model::id();
        for review in reviews {
            use anyhow::Context;
            let value = choices
                .get(&review.id)
                .cloned()
                .context("Review new overlaps before connecting")?;
            let mut record = crate::merge_review::resolve(&mesh, root, &origin, &review.id, value)?;
            record.mesh.as_mut().unwrap().author = Some(author.into());
            mesh.insert(record)?;
            mesh.project(root)?;
        }
        anyhow::ensure!(
            crate::merge_review::conflicts(&mesh.events, author).is_empty(),
            "Unresolved merge"
        );
        Ok(serde_json::to_string(&mesh.missing(&Default::default()))?)
    })()
    .map_err(error)
}

#[wasm_bindgen]
pub fn browser_storage_project_references(
    snapshot: &str,
    store_ids: bool,
) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let mut data: Inventory = serde_json::from_str(snapshot)?;
        data.convert_project_references(store_ids);
        Ok(serde_json::to_string(&data)?)
    })()
    .map_err(error)
}

/// Decide and apply a received snapshot in Rust, inside the SQLite worker.
#[wasm_bindgen]
pub fn browser_apply_git_snapshot(source: &str, project_id: &str, tip: &str, selection: &str) -> Result<String, JsValue> {
    (|| -> anyhow::Result<String> {
        let mut data: Inventory = serde_json::from_str(source)?;
        let selection: crate::web::FolderSelection = serde_json::from_str(selection)?;
        if let Some(project) = data.projects.iter_mut().find(|p| p.id == project_id) {
            let tips = project.unresolved_git_tips();
            let marker = format!("git-snapshot:{project_id}");
            if tips.len() == 1 && tips.iter().next().copied() == Some(tip)
                && project.kicad.as_ref().is_none_or(|link| link.revision.starts_with(&marker)
                    || (link.browser_folder.is_some() && link.browser_folder == selection.git_managed_folder))
                && let (Some(folder), Some(first)) = (selection.folder, selection.choices.first())
            {
                let link = project.kicad.get_or_insert_with(Default::default);
                if !selection.choices.contains(&link.path) { link.path = first.clone(); }
                link.revision = selection.managed_revision.unwrap_or_else(||format!("{marker}:{tip}"));
                link.browser_folder = Some(folder);
                link.files.clear();
            }
        }
        data.validate()?;
        Ok(serde_json::to_string(&data)?)
    })().map_err(error)
}
