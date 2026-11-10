//! Deterministic, causal event-set projection for multi-writer inventory sync.
use crate::{
    action_log::{self, Record},
    model::*,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub type Clock = BTreeMap<String, u64>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub sequence: u64,
    pub lamport: u64,
    pub context: Clock,
    pub mutations: Vec<Mutation>,
    #[serde(default)]
    pub receipts: Vec<Receipt>,
    #[serde(default)]
    pub resolutions: Vec<crate::merge_review::Resolution>,
    #[serde(default)]
    pub parents: Vec<String>,
}
impl Meta {
    pub fn author(&self) -> &str {
        self.author.as_deref().unwrap_or(&self.origin)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub part: String,
    pub claims: BTreeMap<String, i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mutation {
    pub entity: String,
    pub key: String,
    pub targets: Vec<String>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub position: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conflict {
    pub entity: String,
    pub id: String,
    pub message: String,
}
#[derive(Clone, Default)]
pub struct Mesh {
    pub events: BTreeMap<String, Record>,
    pub clock: Clock,
    pub aliases: BTreeMap<(String, String), Vec<String>>,
    pub conflicts: Vec<Conflict>,
    stocks: BTreeMap<(String, String), i128>,
}
#[derive(Clone, Default)]
struct Row {
    value: Value,
    versions: BTreeMap<String, Stamp>,
    live: bool,
    stock: i128,
    initialized: bool,
    created: Option<(Stamp, usize)>,
}
type Stamp = (u64, String, u64, String);
pub fn stable_id(name: &str) -> String {
    use sha1::{Digest, Sha1};
    let digest = Sha1::digest(format!("bom-inventory-project:{name}"));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 15) | 80;
    bytes[8] = (bytes[8] & 63) | 128;
    uuid::Uuid::from_bytes(bytes).to_string().to_uppercase()
}
pub fn prepare_ids(data: &mut Inventory) {
    for p in &mut data.projects {
        if p.id.is_empty() {
            p.id = p
                .notes_attachment_id
                .as_deref()
                .map(stable_id)
                .unwrap_or_else(|| stable_id(&key(&p.name)));
        }
    }
}
pub(crate) fn normalized(
    data: &Inventory,
    root: &Path,
) -> Result<BTreeMap<(String, String), Value>> {
    let mut data = data.clone();
    prepare_ids(&mut data);
    let mut result = BTreeMap::new();
    for ((entity, oldkey), mut value) in action_log::entities(&data, root)? {
        let mut identity = oldkey;
        match entity.as_str() {
            "projects" => identity = value["id"].as_str().context("Missing project ID")?.into(),
            "storage" => {
                let description = value;
                value = serde_json::json!({"name":identity,"description":description});
                identity = key(&identity);
            }
            _ => {}
        }
        result.insert((entity, identity), value);
    }
    Ok(result)
}
impl Mesh {
    pub(crate) fn counter(&self, entity: &str, targets: &[String]) -> Result<i64> {
        let total: i128 = targets
            .iter()
            .map(|id| {
                self.stocks
                    .get(&(entity.into(), id.clone()))
                    .copied()
                    .unwrap_or(0)
            })
            .sum();
        Ok(i64::try_from(total).context("Counter exceeds resolution range")?)
    }
    pub fn make(
        &self,
        before: &Inventory,
        after: &Inventory,
        root: &Path,
        origin: &str,
        sequence: u64,
    ) -> Result<Record> {
        let mut record = action_log::diff(before, after, root, false)?;
        let a = normalized(before, root)?;
        let b = normalized(after, root)?;
        let mut mutations = vec![];
        for identity in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
            if a.get(identity) == b.get(identity) {
                continue;
            }
            mutations.push(Mutation {
                entity: identity.0.clone(),
                key: identity.1.clone(),
                targets: self
                    .aliases
                    .get(identity)
                    .cloned()
                    .unwrap_or_else(|| vec![identity.1.clone()]),
                before: a.get(identity).cloned(),
                after: b.get(identity).cloned(),
                position: position(after, &identity.0, &identity.1),
            });
        }
        let mut receipts = vec![];
        let mut capacity: BTreeMap<String, i64> = mutations
            .iter()
            .filter(|m| m.entity == "parts")
            .filter_map(|m| {
                let after = m.after.as_ref()?;
                let delta = after["stock"].as_i64()?
                    - m.before
                        .as_ref()
                        .and_then(|v| v["stock"].as_i64())
                        .unwrap_or(0);
                Some((m.key.clone(), delta))
            })
            .collect();
        for removed in mutations
            .iter()
            .filter(|m| m.entity == "purchases" && m.after.is_none())
        {
            let Some(before) = removed.before.as_ref() else {
                continue;
            };
            let quantity = before["part"]["stock"].as_i64().unwrap_or(0);
            if let Some(destination) = mutations.iter().find(|m| {
                m.entity == "parts"
                    && m.after.as_ref().is_some_and(|p| {
                        key(p["number"].as_str().unwrap_or(""))
                            == key(before["part"]["number"].as_str().unwrap_or(""))
                    })
                    && capacity.get(&m.key).copied().unwrap_or(0) >= quantity
            }) {
                let mut claims = BTreeMap::new();
                let mut remaining = quantity;
                for member in &removed.targets {
                    let amount = self
                        .stocks
                        .get(&("purchases".into(), member.clone()))
                        .copied()
                        .unwrap_or(quantity as i128)
                        .max(0)
                        .min(remaining as i128) as i64;
                    if amount > 0 {
                        claims.insert(member.clone(), amount);
                        remaining -= amount;
                    }
                }
                if remaining == 0 {
                    *capacity.get_mut(&destination.key).unwrap() -= quantity;
                    receipts.push(Receipt {
                        part: destination.key.clone(),
                        claims,
                    });
                }
            }
        }
        let lamport = self
            .events
            .values()
            .filter_map(|r| r.mesh.as_ref().map(|m| m.lamport))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("Logical clock exhausted")?;
        record.version = 2;
        record.mesh = Some(Meta {
            origin: origin.into(),
            author: None,
            sequence,
            lamport,
            context: self.clock.clone(),
            mutations,
            receipts,
            resolutions: vec![],
            parents: vec![crate::merge_review::head(&self.events, self.clock.clone()).hash],
        });
        Ok(record)
    }
    pub fn insert(&mut self, record: Record) -> Result<bool> {
        let meta = record.mesh.as_ref().context("Mesh metadata required")?;
        ensure!(
            record.version == 2
                && meta.sequence > 0
                && meta.lamport > 0
                && meta.sequence <= i64::MAX as u64
                && meta.lamport <= i64::MAX as u64
                && record.timestamp_ms <= i64::MAX as u64,
            "Invalid mesh event version or counters"
        );
        ensure!(
            uuid::Uuid::parse_str(&record.id).is_ok()
                && (crate::network_identity::valid_node_id(&meta.origin)
                    || uuid::Uuid::parse_str(&meta.origin).is_ok())
                && meta
                    .author
                    .as_ref()
                    .is_none_or(|author| crate::network_identity::valid_node_id(author)),
            "Invalid event ID, origin, or signing node"
        );
        ensure!(
            meta.context.get(&meta.origin).copied().unwrap_or(0) == meta.sequence - 1,
            "Invalid origin predecessor"
        );
        if let Some(old) = self.events.get(&record.id) {
            ensure!(
                serde_json::to_value(old)? == serde_json::to_value(&record)?,
                "Event ID reused with different contents"
            );
            return Ok(false);
        }
        ensure!(
            !self.events.values().any(|r| r
                .mesh
                .as_ref()
                .is_some_and(|m| m.origin == meta.origin && m.sequence == meta.sequence)),
            "Origin sequence collision: a database identity was cloned"
        );
        for mutation in &meta.mutations {
            ensure!(
                [
                    "parts",
                    "purchases",
                    "projects",
                    "storage",
                    "allocations",
                    "bom"
                ]
                .contains(&mutation.entity.as_str())
                    && !mutation.targets.is_empty(),
                "Invalid mutation entity or targets"
            );
            for value in [&mutation.before, &mutation.after].into_iter().flatten() {
                ensure!(value.is_object(), "Mutation payload must be an object");
                if mutation.entity == "purchases" {
                    ensure!(
                        value["part"].is_object() && value["assignments"].is_array(),
                        "Invalid purchase payload"
                    );
                }
            }
            if let Some(value) = &mutation.after {
                let identity = match mutation.entity.as_str() {
                    "parts" | "allocations" | "bom" | "projects" => value["id"].as_str(),
                    "purchases" => value["part"]["id"].as_str(),
                    "storage" => Some(mutation.key.as_str()),
                    _ => None,
                };
                ensure!(
                    identity == Some(mutation.key.as_str()),
                    "Mutation identity does not match payload"
                );
                match mutation.entity.as_str() {
                    "parts" | "purchases" => {
                        let part = if mutation.entity == "parts" {
                            value
                        } else {
                            &value["part"]
                        };
                        ensure!(
                            part["barcode"].as_str().is_some_and(|s| !s.is_empty())
                                && part["stock"].as_i64().is_some(),
                            "Complete part payload is required"
                        );
                        serde_json::from_value::<Part>(part.clone())?.validate()?;
                    }
                    "projects" => {
                        let p: Project = serde_json::from_value(value.clone())?;
                        ensure!(
                            !p.name.trim().is_empty() && p.notes_attachment_id.is_some(),
                            "Project name and notes owner are required"
                        );
                    }
                    "allocations" => {
                        let a: Allocation = serde_json::from_value(value.clone())?;
                        ensure!(a.quantity > 0, "Invalid allocation quantity");
                    }
                    "bom" => {
                        let b: BomEntry = serde_json::from_value(value.clone())?;
                        ensure!(b.item.quantity > 0, "Invalid BOM quantity");
                    }
                    _ => {}
                }
            }
        }
        self.events.insert(record.id.clone(), record);
        Ok(true)
    }
    /// The last state-changing record in the deterministic applied projection.
    pub fn applied_tip_hash(&self) -> Result<String> {
        self.events
            .values()
            .filter(|record| {
                let meta = record.mesh.as_ref().unwrap();
                (!meta.mutations.is_empty() || !meta.resolutions.is_empty())
                    && self
                        .clock
                        .get(&meta.origin)
                        .is_some_and(|sequence| *sequence >= meta.sequence)
            })
            .max_by_key(|record| stamp(record))
            .map(action_log::record_hash)
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub fn missing(&self, clock: &Clock) -> Vec<Record> {
        let mut records: Vec<_> = self
            .events
            .values()
            .filter(|r| {
                let m = r.mesh.as_ref().unwrap();
                m.sequence > clock.get(&m.origin).copied().unwrap_or(0)
            })
            .cloned()
            .collect();
        records.sort_by_key(stamp);
        records
    }
    pub fn project(&mut self, root: &Path) -> Result<Inventory> {
        self.conflicts.clear();
        self.aliases.clear();
        self.clock.clear();
        let mut rows: BTreeMap<(String, String), Row> = BTreeMap::new();
        let dependency_times: BTreeMap<_, _> = self
            .events
            .values()
            .map(|r| {
                let m = r.mesh.as_ref().unwrap();
                ((m.origin.clone(), m.sequence), m.lamport)
            })
            .collect();
        let mut consumed: BTreeMap<String, i64> = BTreeMap::new();
        let mut pending: Vec<_> = self.events.values().collect();
        pending.sort_by_key(|r| stamp(r));
        // A causal predecessor always has a lower Lamport time. Deferred/gapped events stay durable.
        for record in pending {
            let meta = record.mesh.as_ref().unwrap();
            if !meta
                .context
                .iter()
                .all(|(o, s)| self.clock.get(o).copied().unwrap_or(0) >= *s)
            {
                continue;
            }
            ensure!(
                meta.context.iter().all(|(o, s)| *s == 0
                    || dependency_times
                        .get(&(o.clone(), *s))
                        .is_some_and(|time| *time < meta.lamport)),
                "Invalid causal logical clock"
            );
            let mut deductions: BTreeMap<String, i128> = BTreeMap::new();
            for receipt in &meta.receipts {
                for (purchase, quantity) in &receipt.claims {
                    ensure!(*quantity > 0, "Invalid purchase receipt claim");
                    let old = consumed.entry(purchase.clone()).or_default();
                    let duplicate = (*quantity).min(*old);
                    *old = (*old).max(*quantity);
                    *deductions.entry(receipt.part.clone()).or_default() += duplicate as i128;
                }
            }
            for mutation in &meta.mutations {
                apply(
                    &mut rows,
                    mutation,
                    &stamp(record),
                    if mutation.entity == "parts" {
                        deductions.get(&mutation.key).copied().unwrap_or(0)
                    } else {
                        0
                    },
                )?;
            }
            self.clock.insert(meta.origin.clone(), meta.sequence);
        }
        self.stocks = rows
            .iter()
            .map(|(key, row)| (key.clone(), row.stock))
            .collect();
        let mut ordered: Vec<_> = rows.iter().collect();
        ordered
            .sort_by_key(|((entity, id), row)| (entity.clone(), row.created.clone(), id.clone()));
        let mut result = Inventory::default();
        let mut project_names = BTreeMap::new();
        let mut names = BTreeSet::new();
        for ((entity, id), row) in ordered.iter().copied() {
            if entity != "projects" || !row.live {
                continue;
            }
            let mut p: Project = serde_json::from_value(row.value.clone())?;
            p.id = id.clone();
            if !names.insert(key(&p.name)) {
                p.name = format!("{} [{}]", p.name, id);
                while !names.insert(key(&p.name)) {
                    p.name.push('_');
                }
                self.conflicts.push(Conflict {
                    entity: entity.clone(),
                    id: id.clone(),
                    message: "Projects with the same name were kept separately with an ID suffix"
                        .into(),
                });
            }
            project_names.insert(id.clone(), p.name.clone());
            result.projects.push(p);
        }
        let mut part_alias = BTreeMap::new();
        for entity in ["parts", "purchases"] {
            let mut groups: BTreeMap<String, Vec<(&String, &Row)>> = BTreeMap::new();
            for ((kind, id), row) in &rows {
                if kind == entity && row.live {
                    let p = if entity == "purchases" {
                        &row.value["part"]
                    } else {
                        &row.value
                    };
                    groups
                        .entry(key(p["number"].as_str().unwrap_or("")))
                        .or_default()
                        .push((id, row));
                }
            }
            for members in groups.values() {
                let canonical = members[0].0.clone();
                let mut merged = serde_json::Map::new();
                let mut versions: BTreeMap<String, Stamp> = BTreeMap::new();
                let mut stock = 0i128;
                let mut assignments = BTreeMap::new();
                for project in &mut result.projects {
                    project.kicad = None;
                }
                let mut barcodes = BTreeSet::new();
                for (id, row) in members {
                    if entity == "parts" {
                        part_alias.insert((*id).clone(), canonical.clone());
                    }
                    stock = stock
                        .checked_add(row.stock)
                        .context("Stock counter overflow")?;
                    let part = if entity == "purchases" {
                        &row.value["part"]
                    } else {
                        &row.value
                    };
                    if let Some(code) = part["barcode"].as_str() {
                        barcodes.insert(code.to_owned());
                    }
                    for (field, value) in part.as_object().context("Invalid part payload")? {
                        let path = if entity == "purchases" {
                            format!("part.{field}")
                        } else {
                            field.clone()
                        };
                        let version = row.versions.get(&path).cloned().unwrap_or_default();
                        if !versions.contains_key(field) || version > versions[field] {
                            versions.insert(field.clone(), version);
                            merged.insert(field.clone(), value.clone());
                        }
                    }
                    if entity == "purchases" {
                        for assignment in row.value["assignments"]
                            .as_array()
                            .context("Invalid assignments")?
                        {
                            assignments.insert(
                                assignment["id"].as_str().unwrap_or("").to_owned(),
                                assignment.clone(),
                            );
                        }
                    }
                }
                if stock > i64::MAX as i128 {
                    self.conflicts.push(Conflict {
                        entity: entity.into(),
                        id: canonical.clone(),
                        message: format!(
                            "Combined stock exceeds the supported quantity range by {}",
                            stock - i64::MAX as i128
                        ),
                    });
                }
                if stock < 0 {
                    self.conflicts.push(Conflict {
                        entity: entity.into(),
                        id: canonical.clone(),
                        message: format!(
                            "Concurrent stock removals exceeded available stock by {}",
                            -stock
                        ),
                    });
                }
                merged.insert("id".into(), canonical.clone().into());
                merged.insert(
                    "stock".into(),
                    (stock.clamp(0, i64::MAX as i128) as i64).into(),
                );
                if barcodes.len() > 1 {
                    merged.insert("syncBarcodeAliases".into(), serde_json::to_value(barcodes)?);
                }
                let part: Part = serde_json::from_value(Value::Object(merged))?;
                self.aliases.insert(
                    (entity.into(), canonical.clone()),
                    members.iter().map(|(id, _)| (*id).clone()).collect(),
                );
                if entity == "parts" {
                    result.parts.push(part);
                } else {
                    if part.stock == 0 {
                        self.conflicts.push(Conflict{entity:"purchases".into(),id:canonical.clone(),message:"Purchase is hidden because concurrent quantity edits exhausted its stock; its contributions remain in history".into()});
                        continue;
                    }
                    let mut remaining = part.stock;
                    let mut accepted = vec![];
                    for value in assignments.into_values() {
                        let mut a: Assignment = serde_json::from_value(value)?;
                        if let Some(name) = project_names.get(&a.project) {
                            a.project = name.clone();
                            let requested = a.quantity;
                            a.quantity = a.quantity.min(remaining).max(0);
                            remaining -= a.quantity;
                            if a.quantity != requested {
                                self.conflicts.push(Conflict{entity:"purchase_assignment".into(),id:a.id.clone(),message:format!("Accepted {} of {requested} requested units after concurrent edits",a.quantity)});
                            }
                            if a.quantity > 0 {
                                accepted.push(a);
                            }
                        }
                    }
                    result.purchases.push(Purchase {
                        part,
                        assignments: accepted,
                    });
                }
            }
        }
        result.parts.sort_by_key(|p| {
            self.aliases
                .get(&("parts".into(), p.id.clone()))
                .and_then(|ids| {
                    ids.iter()
                        .filter_map(|id| {
                            rows.get(&("parts".into(), id.clone()))
                                .and_then(|r| r.created.clone())
                        })
                        .min()
                })
        });
        result.purchases.sort_by_key(|p| {
            self.aliases
                .get(&("purchases".into(), p.part.id.clone()))
                .and_then(|ids| {
                    ids.iter()
                        .filter_map(|id| {
                            rows.get(&("purchases".into(), id.clone()))
                                .and_then(|r| r.created.clone())
                        })
                        .min()
                })
        });
        let mut remaining: BTreeMap<_, _> = result
            .parts
            .iter()
            .map(|p| (p.id.clone(), p.stock))
            .collect();
        for ((entity, id), row) in ordered.iter().copied() {
            if !row.live {
                continue;
            }
            match entity.as_str() {
                "storage" => {
                    result.storage.insert(
                        row.value["name"].as_str().unwrap_or(id).into(),
                        row.value["description"].as_str().unwrap_or("").into(),
                    );
                }
                "allocations" => {
                    let mut a: Allocation = serde_json::from_value(row.value.clone())?;
                    if let (Some(part), Some(project)) =
                        (part_alias.get(&a.part_id), project_names.get(&a.project))
                    {
                        a.part_id = part.clone();
                        a.project = project.clone();
                        let requested = a.quantity;
                        let budget = remaining.get_mut(part).unwrap();
                        a.quantity = a.quantity.min(*budget).max(0);
                        *budget -= a.quantity;
                        if a.quantity != requested {
                            self.conflicts.push(Conflict{entity:entity.clone(),id:id.clone(),message:format!("Accepted {} of {requested} allocated units after concurrent edits",a.quantity)});
                        }
                        if a.quantity > 0 {
                            result.allocations.push(a);
                        }
                    } else {
                        self.conflicts.push(Conflict{entity:entity.clone(),id:id.clone(),message:"Allocation is retained in history until its part and project are available".into()});
                    }
                }
                "bom" => {
                    let mut b: BomEntry = serde_json::from_value(row.value.clone())?;
                    if b.kicad_source.is_some() {
                        continue;
                    }
                    if let Some(name) = project_names.get(&b.project) {
                        b.project = name.clone();
                        b.part_id = b
                            .part_id
                            .as_ref()
                            .and_then(|id| part_alias.get(id))
                            .cloned();
                        if b.allocation_id
                            .as_ref()
                            .is_some_and(|id| !result.allocations.iter().any(|a| &a.id == id))
                        {
                            b.allocation_id = None;
                        }
                        result.bom.push(b);
                    }
                }
                _ => {}
            }
        }
        for project in &mut result.projects {
            project.kicad = None;
        }
        let mut barcodes = BTreeSet::new();
        for p in &mut result.parts {
            if !barcodes.insert(p.barcode.clone()) {
                p.barcode = format!("SYNC-{}", p.id);
            }
        }
        result.reconcile_designator_priority(&Inventory::default())?;
        let mut value = serde_json::to_value(&result)?;
        action_log::paths(&mut value, root, false)?;
        result = serde_json::from_value(value)?;
        result.validate()?;
        Ok(result)
    }
}
fn position(data: &Inventory, entity: &str, id: &str) -> usize {
    match entity {
        "parts" => data.parts.iter().position(|p| p.id == id),
        "purchases" => data.purchases.iter().position(|p| p.part.id == id),
        "projects" => data.projects.iter().position(|p| p.id == id),
        "allocations" => data.allocations.iter().position(|p| p.id == id),
        "bom" => data
            .bom
            .iter()
            .filter(|p| p.kicad_source.is_none())
            .position(|p| p.id == id),
        _ => Some(0),
    }
    .unwrap_or(0)
}
fn stamp(record: &Record) -> Stamp {
    let m = record.mesh.as_ref().unwrap();
    (m.lamport, m.origin.clone(), m.sequence, record.id.clone())
}
fn flatten(value: &Value, entity: &str) -> BTreeMap<String, Value> {
    let mut fields = BTreeMap::new();
    if let Some(object) = value.as_object() {
        for (k, v) in object {
            if entity == "purchases" && k == "part" {
                for (field, v) in v.as_object().unwrap() {
                    fields.insert(format!("part.{field}"), v.clone());
                }
            } else if entity == "projects" && k == "git_tips" {
                if let Some(tips) = v.as_object() {
                    for (publisher, tip) in tips {
                        fields.insert(format!("git_tips.{publisher}"), tip.clone());
                    }
                }
            } else {
                fields.insert(k.clone(), v.clone());
            }
        }
    }
    fields
}
fn apply(
    rows: &mut BTreeMap<(String, String), Row>,
    mutation: &Mutation,
    version: &Stamp,
    stock_deduction: i128,
) -> Result<()> {
    let before = mutation
        .before
        .as_ref()
        .map(|v| flatten(v, &mutation.entity))
        .unwrap_or_default();
    let after = mutation
        .after
        .as_ref()
        .map(|v| flatten(v, &mutation.entity))
        .unwrap_or_default();
    let stockfield = if mutation.entity == "purchases" {
        "part.stock"
    } else {
        "stock"
    };
    for (index, target) in mutation.targets.iter().enumerate() {
        let row = rows
            .entry((mutation.entity.clone(), target.clone()))
            .or_default();
        if mutation.after.is_none() {
            row.live = false;
            continue;
        }
        if !row.initialized {
            row.value = mutation.after.clone().unwrap();
            row.initialized = true;
            row.created = Some((version.clone(), mutation.position));
            for field in after.keys() {
                row.versions.insert(field.clone(), version.clone());
            }
        }
        row.live = true;
        for field in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            if field == stockfield {
                continue;
            }
            if before.get(field) == after.get(field) {
                continue;
            }
            let value = after.get(field).cloned().unwrap_or(Value::Null);
            if let Some(name) = field.strip_prefix("part.") {
                row.value["part"][name] = value;
            } else if let Some(publisher) = field.strip_prefix("git_tips.") {
                if value.is_null() {
                    if let Some(tips) = row.value["git_tips"].as_object_mut() {
                        tips.remove(publisher);
                    }
                } else {
                    row.value["git_tips"][publisher] = value;
                }
            } else {
                row.value[field] = value;
            }
            row.versions.insert(field.clone(), version.clone());
        }
        if matches!(mutation.entity.as_str(), "parts" | "purchases") && index == 0 {
            let new = after
                .get(stockfield)
                .and_then(Value::as_i64)
                .context("Missing stock quantity")? as i128;
            let old = before.get(stockfield).and_then(Value::as_i64).unwrap_or(0) as i128;
            if mutation.before.is_some()
                || row.created.as_ref() == Some(&(version.clone(), mutation.position))
            {
                row.stock = row
                    .stock
                    .checked_add(new - old - stock_deduction)
                    .context("Stock delta overflow")?;
            }
        }
    }
    Ok(())
}
