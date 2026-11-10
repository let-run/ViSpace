//! Versioned, portable inventory operations. See docs/action-log.md.
use crate::model::Inventory;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};
#[cfg(not(target_arch = "wasm32"))]
use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
};

const MAGIC: &[u8; 8] = b"BOMLOG01";
const MAX_RECORD: usize = 64 * 1024 * 1024;
pub const CHUNK_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub kind: String,
    pub entity: String,
    pub key: String,
    pub label: String,
    /// Complete replacement; None means deletion. Asset bytes are never included.
    pub value: Option<Value>,
    pub changes: Vec<String>,
    /// Position in the resulting collection; omitted for storage and deletions.
    pub position: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub id: String,
    pub timestamp_ms: u64,
    pub initial: bool,
    pub actions: Vec<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<crate::mesh::Meta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<crate::network_identity::Proof>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub chunk: String,
    pub offset: u64,
    pub record: Record,
}
/// Content fingerprint of an individual action record, independent of gzip/chunk framing.
pub fn record_hash(record: &Record) -> Result<String> {
    use sha1::{Digest, Sha1};
    Ok(Sha1::digest(serde_json::to_vec(record)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryCursor {
    pub chunk: String,
    pub offset: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub entry: LogEntry,
    pub hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    pub next: Option<HistoryCursor>,
}
#[cfg(target_arch = "wasm32")]
fn now() -> u64 {
    js_sys::Date::now() as u64
}
#[cfg(not(target_arch = "wasm32"))]
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Replace managed paths with reversible, root-independent unique references.
pub(crate) fn paths(value: &mut Value, root: &Path, encode: bool) -> Result<()> {
    match value {
        Value::String(s) => {
            if encode {
                if s.starts_with("asset:") || s.starts_with("literal:") {
                    *s = format!("literal:{s}");
                    return Ok(());
                }
                ensure!(
                    !s.starts_with("data:image/") && !s.starts_with("data:application/pdf"),
                    "Embedded attachment bytes cannot be written to the action log"
                );
                if let Ok(relative) = Path::new(s).strip_prefix(root) {
                    let name = relative.to_str().context("Attachment path must be UTF-8")?;
                    *s = format!(
                        "asset:{}",
                        name.as_bytes()
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    );
                }
            } else if let Some(literal) = s.strip_prefix("literal:") {
                *s = literal.to_owned();
            } else if s.starts_with("asset:") {
                *s = root
                    .join(asset_relative_path(s)?)
                    .to_string_lossy()
                    .into_owned();
            }
        }
        Value::Array(a) => {
            for v in a {
                paths(v, root, encode)?;
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                paths(v, root, encode)?;
            }
        }
        _ => {}
    }
    Ok(())
}
/// Decode only portable relative paths; the same validation is used for blob I/O.
pub fn asset_relative_path(asset: &str) -> Result<std::path::PathBuf> {
    let encoded = asset
        .strip_prefix("asset:")
        .context("Invalid asset reference")?;
    ensure!(
        encoded.len() % 2 == 0 && encoded.is_ascii(),
        "Invalid asset reference"
    );
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let relative = String::from_utf8(bytes)?;
    ensure!(
        !relative.is_empty()
            && !relative.contains(['\\', ':', '\0'])
            && relative
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != ".."),
        "Unsafe asset reference"
    );
    let path = std::path::PathBuf::from(relative);
    ensure!(
        path.components()
            .all(|c| matches!(c, std::path::Component::Normal(_))),
        "Unsafe asset reference"
    );
    Ok(path)
}
/// References inside replacement values, including derivatives and note images.
/// Escaped literal text and external URLs are not attachment references.
pub fn referenced_assets(record: &Record) -> Result<std::collections::BTreeSet<String>> {
    fn visit(value: &Value, result: &mut std::collections::BTreeSet<String>) -> Result<()> {
        match value {
            Value::String(s) if s.starts_with("asset:") => {
                asset_relative_path(s)?;
                result.insert(s.clone());
            }
            Value::Array(values) => {
                for v in values {
                    visit(v, result)?;
                }
            }
            Value::Object(values) => {
                for v in values.values() {
                    visit(v, result)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut result = std::collections::BTreeSet::new();
    for action in &record.actions {
        if let Some(value) = &action.value {
            visit(value, &mut result)?;
        }
    }
    if let Some(meta) = record.mesh.as_ref() {
        for mutation in &meta.mutations {
            if let Some(value) = mutation.after.as_ref() {
                visit(value, &mut result)?;
            }
        }
    }
    Ok(result)
}

fn entity_identity<'a>(entity: &str, value: &'a Value) -> Option<&'a str> {
    match entity {
        "projects" => value["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .or_else(|| value["name"].as_str()),
        "purchases" => value["part"]["id"].as_str(),
        _ => value["id"].as_str(),
    }
}

pub(crate) fn entities(data: &Inventory, root: &Path) -> Result<BTreeMap<(String, String), Value>> {
    let mut shared = data.clone();
    shared.bom.retain(|b| b.kicad_source.is_none());
    for project in &mut shared.projects {
        project.kicad = None;
    }
    shared.convert_project_references(true);
    let mut snapshot = serde_json::to_value(shared)?;
    paths(&mut snapshot, root, true)?;
    let mut result = BTreeMap::new();
    for entity in [
        "projects",
        "storage",
        "parts",
        "purchases",
        "allocations",
        "bom",
    ] {
        if entity == "storage" {
            for (key, value) in snapshot[entity].as_object().context("Invalid storage")? {
                result.insert((entity.into(), key.clone()), value.clone());
            }
        } else {
            for value in snapshot[entity].as_array().context("Invalid collection")? {
                let key = entity_identity(entity, value).context("Missing entity identity")?;
                result.insert((entity.into(), key.into()), value.clone());
            }
        }
    }
    Ok(result)
}
fn positions(data: &Inventory) -> Result<BTreeMap<(String, String), usize>> {
    let mut shared = data.clone();
    shared.bom.retain(|b| b.kicad_source.is_none());
    let value = serde_json::to_value(shared)?;
    let mut result = BTreeMap::new();
    for entity in ["parts", "purchases", "projects", "allocations", "bom"] {
        for (position, item) in value[entity]
            .as_array()
            .context("Invalid collection")?
            .iter()
            .enumerate()
        {
            let identity = entity_identity(entity, item).context("Missing identity")?;
            result.insert((entity.to_owned(), identity.to_owned()), position);
        }
    }
    Ok(result)
}
fn details(entity: &str, old: Option<&Value>, new: Option<&Value>) -> Vec<String> {
    let mut changes = Vec::new();
    let empty = Value::Null;
    let (mut a, mut b) = (old.unwrap_or(&empty), new.unwrap_or(&empty));
    if entity == "purchases" {
        if a["assignments"] != b["assignments"] {
            changes.push("purchase_assignments_changed".into());
        }
        a = &a["part"];
        b = &b["part"];
    }
    if matches!(entity, "parts" | "purchases" | "projects") {
        for (field, label) in [
            ("notes", "notes"),
            ("image", "image"),
            ("datasheet", "datasheet"),
            ("storage", "storage_assignment"),
            ("stock", "stock"),
            ("kicad", "kicad_link"),
            ("git_remote", "git_remote"),
            ("git_tips", "git_tips"),
        ] {
            if a[field] != b[field] {
                let present = |v: &Value| {
                    !v.is_null()
                        && v != ""
                        && (field != "notes"
                            || v["text"].as_str().is_some_and(|s| !s.is_empty())
                            || v["images"].as_object().is_some_and(|o| !o.is_empty())
                            || v["rtf"].as_str().is_some_and(|s| !s.is_empty()))
                };
                if !present(&a[field]) && !present(&b[field]) {
                    continue;
                }
                let verb = if !present(&b[field]) {
                    "removed"
                } else if !present(&a[field]) {
                    "added"
                } else {
                    "updated"
                };
                changes.push(format!("{label}_{verb}"));
            }
        }
    }
    changes
}
pub fn diff(before: &Inventory, after: &Inventory, root: &Path, initial: bool) -> Result<Record> {
    let a = entities(before, root)?;
    let b = entities(after, root)?;
    let before_positions = positions(before)?;
    let after_positions = positions(after)?;
    let mut actions = Vec::new();
    for (entity, key) in a
        .keys()
        .chain(b.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let identity = (entity.clone(), key.clone());
        let old = a.get(&identity);
        let new = b.get(&identity);
        if old == new && before_positions.get(&identity) == after_positions.get(&identity) {
            continue;
        }
        let name = match entity.as_str() {
            "parts" => "part",
            "purchases" => "purchase",
            "projects" => "project",
            "storage" => "storage",
            "allocations" => "allocation",
            _ => "bom_entry",
        };
        let verb = if new.is_none() {
            "removed"
        } else if old.is_none() {
            "added"
        } else {
            "updated"
        };
        let payload = new.or(old).unwrap();
        let project_label = || {
            let id = payload["project"].as_str().unwrap_or_default();
            let source = if new.is_some() { after } else { before };
            source
                .projects
                .iter()
                .find(|project| project.id == id && !id.is_empty())
                .map_or(id, |project| project.name.as_str())
        };
        let label = match entity.as_str() {
            "parts" => payload["number"].as_str().unwrap_or(key).to_owned(),
            "purchases" => payload["part"]["number"].as_str().unwrap_or(key).to_owned(),
            "projects" => {
                let current = payload["name"].as_str().unwrap_or(key);
                match old.and_then(|value| value["name"].as_str()) {
                    Some(previous) if previous != current => format!("{previous} → {current}"),
                    _ => current.to_owned(),
                }
            }
            "allocations" => format!("{} · {} units", project_label(), payload["quantity"]),
            "bom" => format!(
                "{} · {} units",
                project_label(),
                payload["item"]["quantity"]
            ),
            _ => key.clone(),
        };
        actions.push(Action {
            label,
            kind: format!("{name}_{verb}"),
            entity: entity.clone(),
            key: key.clone(),
            value: new.cloned(),
            changes: details(entity, old, new),
            position: after_positions.get(&identity).copied(),
        });
    }
    Ok(Record {
        version: 1,
        id: crate::model::id(),
        timestamp_ms: now(),
        initial,
        actions,
        mesh: None,
        proof: None,
    })
}
pub fn apply(data: &Inventory, record: &Record, root: &Path) -> Result<Inventory> {
    ensure!(
        matches!(record.version, 1 | 2),
        "Unsupported action protocol version {}",
        record.version
    );
    let mut entities = entities(data, root)?;
    let mut order = positions(data)?;
    let previous_project_keys: BTreeMap<_, _> = entities
        .iter()
        .filter_map(|(key, value)| {
            (key.0 == "projects").then(|| {
                (
                    value["name"].as_str().unwrap_or(&key.1).to_owned(),
                    key.clone(),
                )
            })
        })
        .collect();
    for action in &record.actions {
        ensure!(
            [
                "parts",
                "purchases",
                "projects",
                "storage",
                "allocations",
                "bom"
            ]
            .contains(&action.entity.as_str()),
            "Unknown action entity"
        );
        let prefix = match action.entity.as_str() {
            "parts" => "part",
            "purchases" => "purchase",
            "projects" => "project",
            "storage" => "storage",
            "allocations" => "allocation",
            "bom" => "bom_entry",
            _ => unreachable!(),
        };
        ensure!(
            ["added", "updated", "removed"]
                .iter()
                .any(|verb| action.kind == format!("{prefix}_{verb}")),
            "Unknown action kind"
        );
        ensure!(
            action.kind.ends_with("_removed") == action.value.is_none(),
            "Action kind and value disagree"
        );
        let mut key = (action.entity.clone(), action.key.clone());
        if let Some(value) = &action.value {
            if action.entity != "storage" {
                let identity =
                    entity_identity(&action.entity, value).context("Missing action identity")?;
                // Older project actions used the project name as their key.
                ensure!(
                    identity == action.key
                        || (action.entity == "projects"
                            && value["name"].as_str() == Some(&action.key)),
                    "Action identity does not match its payload"
                );
                key.1 = identity.to_owned();
                order.insert(
                    key.clone(),
                    action.position.context("Missing action position")?,
                );
            }
            entities.insert(key, value.clone());
        } else {
            if action.entity == "projects"
                && !entities.contains_key(&key)
                && let Some(previous_key) = previous_project_keys.get(&action.key)
            {
                if entities
                    .get(previous_key)
                    .and_then(|value| value["name"].as_str())
                    != Some(&action.key)
                {
                    continue;
                }
                key.clone_from(previous_key);
            }
            order.remove(&key);
            entities.remove(&key);
        }
    }
    let mut value = serde_json::json!({"parts":[],"purchases":[],"projects":[],"storage":{},"allocations":[],"bom":[]});
    let mut entities: Vec<_> = entities.into_iter().collect();
    entities.sort_by_key(|(identity, _)| {
        (
            identity.0.clone(),
            order.get(identity).copied().unwrap_or(0),
        )
    });
    for ((entity, key), v) in entities {
        if entity == "storage" {
            value[entity][key] = v;
        } else {
            let list = value[&entity].as_array_mut().unwrap();
            ensure!(
                order.get(&(entity.clone(), key)).copied() == Some(list.len()),
                "Invalid collection positions"
            );
            list.push(v);
        }
    }
    paths(&mut value, root, false)?;
    let mut next: Inventory = serde_json::from_value(value)?;
    next.convert_project_references(false);
    next.validate()?;
    Ok(next)
}

/// The same bounded binary frame is used by chunk files and WebSocket messages.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(value)?;
    ensure!(json.len() <= MAX_RECORD, "Expanded action exceeds limit");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&json)?;
    let bytes = encoder.finish()?;
    ensure!(bytes.len() <= MAX_RECORD, "Action record is too large");
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&bytes);
    Ok(frame)
}
pub fn decode_frame<T: serde::de::DeserializeOwned>(frame: &[u8]) -> Result<T> {
    ensure!(frame.len() >= 4, "Incomplete action frame");
    let length = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
    ensure!(
        length > 0 && length <= MAX_RECORD && frame.len() == length + 4,
        "Invalid action frame length"
    );
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(&frame[4..])
        .take(MAX_RECORD as u64 + 1)
        .read_to_end(&mut json)?;
    ensure!(json.len() <= MAX_RECORD, "Expanded action exceeds limit");
    Ok(serde_json::from_slice(&json)?)
}

#[cfg(not(target_arch = "wasm32"))]
pub struct Journal {
    pub directory: PathBuf,
    writer: Option<File>,
    name: String,
    size: u64,
    // OS releases this lock even after a crash. Never remove the lock file.
    _lock: File,
    pub failed: bool,
    last_chunk_time: u64,
}
#[cfg(not(target_arch = "wasm32"))]
impl Journal {
    pub fn open(directory: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("writer.lock"))?;
        lock.try_lock()
            .context("Another application is using this action log")?;
        let last_chunk_time = std::fs::read_dir(&directory)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(".wal"))
                    .and_then(|n| n.split('-').next())
                    .and_then(|n| n.parse::<u64>().ok())
            })
            .max()
            .unwrap_or(0);
        Ok(Self {
            directory,
            writer: None,
            name: String::new(),
            size: 0,
            _lock: lock,
            failed: false,
            last_chunk_time,
        })
    }
    pub fn entries(&self) -> Result<Vec<LogEntry>> {
        read_directory(&self.directory)
    }
    pub fn page(&self, before: Option<&HistoryCursor>, limit: usize) -> Result<HistoryPage> {
        read_history_page(&self.directory, before, limit)
    }
    pub fn start_chunk(&mut self) -> Result<()> {
        ensure!(
            !self.failed,
            "Action log needs recovery; restart the application"
        );
        let result = (|| -> Result<()> {
            let time = now().max(
                self.last_chunk_time
                    .checked_add(1)
                    .context("Chunk sequence exhausted")?,
            );
            let name = format!("{time:020}-{}.wal", crate::model::id());
            // A crash while creating the header must not leave a corrupt .wal file.
            let temporary = self
                .directory
                .join(format!(".opening-{}", crate::model::id()));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(MAGIC)?;
            file.sync_all()?;
            std::fs::rename(&temporary, self.directory.join(&name))?;
            // Windows does not support opening a directory as a regular file.
            // The chunk header and subsequent records are flushed on all platforms.
            #[cfg(unix)]
            File::open(&self.directory)?.sync_all()?;
            self.last_chunk_time = time;
            self.name = name;
            self.writer = Some(file);
            self.size = 8;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn append(&mut self, record: &Record) -> Result<(String, u64)> {
        ensure!(
            !self.failed,
            "Action log needs recovery; restart the application before making changes"
        );
        let frame = encode_frame(record)?;
        if self.writer.is_none() || self.size + frame.len() as u64 > CHUNK_LIMIT {
            self.start_chunk()?;
        }
        let result = (|| -> Result<()> {
            let writer = self.writer.as_mut().unwrap();
            writer.write_all(&frame)?;
            writer.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.failed = true;
            return Err(error);
        }
        self.size += frame.len() as u64;
        Ok((self.name.clone(), self.size))
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub fn read_directory(directory: &Path) -> Result<Vec<LogEntry>> {
    let mut files = std::fs::read_dir(directory)?
        .map(|e| Ok(e?.path()))
        .collect::<Result<Vec<_>>>()?;
    files.retain(|p| p.extension().is_some_and(|e| e == "wal"));
    files.sort();
    let mut entries = Vec::new();
    for path in files {
        let bytes = std::fs::read(&path)?;
        ensure!(
            bytes.starts_with(MAGIC),
            "Invalid action chunk {}",
            path.display()
        );
        let mut offset = 8;
        while offset < bytes.len() {
            if bytes.len() - offset < 4 {
                break;
            } // crash during final append
            let length = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            ensure!(
                length <= MAX_RECORD && length > 0,
                "Invalid action frame length"
            );
            if bytes.len() - offset - 4 < length {
                break;
            }
            let record: Record = decode_frame(&bytes[offset..offset + 4 + length])
                .with_context(|| format!("Invalid action in {} at {offset}", path.display()))?;
            ensure!(
                matches!(record.version, 1 | 2),
                "Unsupported action protocol version"
            );
            offset += 4 + length;
            entries.push(LogEntry {
                chunk: path.file_name().unwrap().to_string_lossy().into_owned(),
                offset: offset as u64,
                record,
            });
        }
    }
    Ok(entries)
}

/// Read newest-first pages without decompressing the rest of the journal. An end-offset
/// cursor stays stable when new records arrive in this chunk or a newer chunk.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_history_page(
    directory: &Path,
    before: Option<&HistoryCursor>,
    limit: usize,
) -> Result<HistoryPage> {
    ensure!(
        (1..=200).contains(&limit),
        "History page size must be between 1 and 200"
    );
    let mut files = std::fs::read_dir(directory)?
        .map(|e| Ok(e?.path()))
        .collect::<Result<Vec<_>>>()?;
    files.retain(|p| p.extension().is_some_and(|e| e == "wal"));
    files.sort();
    let mut entries: Vec<HistoryEntry> = Vec::with_capacity(limit);
    for path in files.into_iter().rev() {
        let chunk = path
            .file_name()
            .context("Missing chunk name")?
            .to_str()
            .context("Invalid chunk name")?
            .to_owned();
        if before.is_some_and(|c| chunk > c.chunk) {
            continue;
        }
        let bytes = std::fs::read(&path)?;
        let before_offset = before
            .filter(|cursor| cursor.chunk == chunk)
            .map(|cursor| cursor.offset);
        let (page, more) =
            history_chunk_page(&bytes, &chunk, before_offset, limit - entries.len())?;
        entries.extend(page);
        if more {
            let last = &entries.last().unwrap().entry;
            return Ok(HistoryPage {
                next: Some(HistoryCursor {
                    chunk: last.chunk.clone(),
                    offset: last.offset,
                }),
                entries,
            });
        }
    }
    Ok(HistoryPage {
        entries,
        next: None,
    })
}

/// Scan frame boundaries and decode only the requested records.
pub(crate) fn history_chunk_page(
    bytes: &[u8],
    chunk: &str,
    before: Option<u64>,
    limit: usize,
) -> Result<(Vec<HistoryEntry>, bool)> {
    ensure!(bytes.starts_with(MAGIC), "Invalid action chunk {chunk}");
    let mut frames = Vec::new();
    let mut offset = MAGIC.len();
    while bytes.len() - offset >= 4 {
        let length = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        ensure!(
            length > 0 && length <= MAX_RECORD,
            "Invalid action frame length"
        );
        if bytes.len() - offset - 4 < length {
            break;
        }
        let end = offset + 4 + length;
        if before.is_some_and(|cursor| end as u64 >= cursor) {
            break;
        }
        frames.push((offset, end));
        offset = end;
    }
    let more = frames.len() > limit;
    let entries = frames
        .into_iter()
        .rev()
        .take(limit)
        .map(|(start, end)| {
            let record: Record = decode_frame(&bytes[start..end])
                .with_context(|| format!("Invalid action in {chunk} at {start}"))?;
            ensure!(
                matches!(record.version, 1 | 2),
                "Unsupported action protocol version"
            );
            Ok(HistoryEntry {
                hash: record_hash(&record)?,
                entry: LogEntry {
                    chunk: chunk.to_owned(),
                    offset: end as u64,
                    record,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((entries, more))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn size_rotation_and_clock_rollback_keep_chunks_ordered() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("actions")).unwrap();
        let record = diff(
            &Inventory::default(),
            &Inventory::default(),
            dir.path(),
            true,
        )
        .unwrap();
        let (first, _) = journal.append(&record).unwrap();
        journal.size = CHUNK_LIMIT;
        journal.last_chunk_time = now() + 100_000;
        let (second, _) = journal.append(&record).unwrap();
        assert!(second > first);
        assert_eq!(journal.entries().unwrap().len(), 2);
    }
}

pub(crate) fn timestamp_now() -> u64 { now() }
