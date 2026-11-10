//! Explicit, durable three-way merge decisions over the causal event graph.
use crate::{
    action_log::Record,
    mesh::{Clock, Mutation},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resolution {
    pub conflict: String,
    pub entity: String,
    pub key: String,
    pub field: String,
    pub value: Value,
    pub parents: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub entity: String,
    pub key: String,
    pub field: String,
    pub base: Value,
    pub ours: Value,
    pub theirs: Value,
    pub events: Vec<String>,
    pub markdown: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Head {
    pub hash: String,
    pub timestamp_ms: u64,
    pub clock: Clock,
}
pub fn head(events: &BTreeMap<String, Record>, clock: Clock) -> Head {
    let mut hash = Sha1::new();
    let mut time = 0;
    for event in events.values() {
        let bytes = serde_json::to_vec(event).unwrap();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        time = time.max(event.timestamp_ms);
    }
    Head {
        hash: hex(&hash.finalize()),
        timestamp_ms: time,
        clock,
    }
}
fn seen(event: &Record, other: &Record) -> bool {
    let a = event.mesh.as_ref().unwrap();
    let b = other.mesh.as_ref().unwrap();
    a.context.get(&b.origin).copied().unwrap_or(0) >= b.sequence
}
fn fields(m: &Mutation) -> BTreeMap<String, Value> {
    let mut result = BTreeMap::new();
    if m.after.is_none() {
        result.insert("$entity".into(), Value::Null);
        return result;
    }
    let after = m.after.as_ref().unwrap();
    if let Some(after) = after.as_object() {
        for (k, v) in after {
            if m.entity == "projects" && k == "kicad" {
                continue;
            }
            if m.entity == "purchases" && k == "part" {
                for (k, v) in v.as_object().into_iter().flatten() {
                    if m.before.as_ref().map(|b| &b["part"][k]) != Some(v) {
                        result.insert(format!("part.{k}"), v.clone());
                    }
                }
            } else if m.before.as_ref().map(|b| &b[k]) != Some(v) {
                result.insert(k.clone(), v.clone());
            }
        }
    }
    result
}
pub fn get(value: &Value, field: &str) -> Value {
    if field == "$entity" {
        return value.clone();
    }
    if let Some(field) = field.strip_prefix("part.") {
        value["part"][field].clone()
    } else {
        value[field].clone()
    }
}
pub fn set(value: &mut Value, field: &str, new: Value) {
    if field == "$entity" {
        *value = new;
    } else if let Some(field) = field.strip_prefix("part.") {
        value["part"][field] = new;
    } else {
        value[field] = new;
    }
}

/// Fields touched by a proposal that have changed since its base clock.
/// The caller must supply only committed events and check the proposal in commit order.
pub fn stale_fields<'a>(
    base: &Clock,
    proposed: &[Mutation],
    committed: impl IntoIterator<Item = &'a Record>,
) -> BTreeSet<(String, String, String)> {
    let mut stale = BTreeSet::new();
    for record in committed {
        let Some(meta) = &record.mesh else { continue };
        if base.get(&meta.origin).copied().unwrap_or(0) >= meta.sequence {
            continue;
        }
        for change in &meta.mutations {
            let changed_fields = fields(change);
            for draft in proposed.iter().filter(|draft| {
                draft.entity == change.entity
                    && (draft.key == change.key
                        || draft.targets.iter().any(|id| change.targets.contains(id)))
            }) {
                let draft_fields = fields(draft);
                if !draft_fields.is_empty()
                    && !changed_fields.is_empty()
                    && (draft.after.is_none()
                        || change.after.is_none()
                        || draft.before.is_none()
                        || change.before.is_none())
                {
                    stale.insert((draft.entity.clone(), draft.key.clone(), "$entity".into()));
                } else {
                    for field in draft_fields
                        .into_keys()
                        .filter(|field| changed_fields.contains_key(field))
                    {
                        stale.insert((draft.entity.clone(), draft.key.clone(), field));
                    }
                }
            }
        }
    }
    stale
}

pub fn draft_reviews(
    mesh: &mut crate::mesh::Mesh,
    draft: &Record,
    root: &std::path::Path,
) -> anyhow::Result<Vec<Review>> {
    use anyhow::Context;
    let draft_meta = draft.mesh.as_ref().context("Draft is missing metadata")?;
    let data = mesh.project(root)?;
    let rows = crate::mesh::normalized(&data, root)?;
    let stale = stale_fields(
        &draft_meta.context,
        &draft_meta.mutations,
        mesh.events.values(),
    );
    let mut reviews = Vec::new();
    for (entity, key, field) in stale {
        let mutation = draft_meta
            .mutations
            .iter()
            .find(|m| m.entity == entity && m.key == key)
            .context("Draft field has no mutation")?;
        let value = |row: Option<&Value>| row.map_or(Value::Null, |row| get(row, &field));
        let base = value(mutation.before.as_ref());
        let ours = value(mutation.after.as_ref());
        let theirs = value(current_row(mesh, &rows, mutation).map(|(_, row)| row));
        let id = hex(&Sha1::digest(serde_json::to_vec(&(
            &draft.id, &entity, &key, &field, &theirs,
        ))?));
        let markdown = field == "notes" && [&base, &ours, &theirs].iter().all(|v| v.is_object());
        reviews.push(Review {
            id,
            entity,
            key,
            field,
            base,
            ours,
            theirs,
            events: Vec::new(),
            markdown,
        });
    }
    Ok(reviews)
}

fn current_row<'a>(
    mesh: &crate::mesh::Mesh,
    rows: &'a BTreeMap<(String, String), Value>,
    mutation: &Mutation,
) -> Option<(&'a (String, String), &'a Value)> {
    rows.get_key_value(&(mutation.entity.clone(), mutation.key.clone()))
        .or_else(|| {
            rows.iter().find(|((entity, key), _)| {
                entity == &mutation.entity
                    && mesh
                        .aliases
                        .get(&(entity.clone(), key.clone()))
                        .is_some_and(|targets| {
                            targets
                                .iter()
                                .any(|target| mutation.targets.contains(target))
                        })
            })
        })
}

pub fn rebase_draft(
    mesh: &mut crate::mesh::Mesh,
    draft: &Record,
    choices: &BTreeMap<String, Value>,
    root: &std::path::Path,
) -> anyhow::Result<Record> {
    use anyhow::{Context, ensure};
    let reviews = draft_reviews(mesh, draft, root)?;
    ensure!(
        reviews
            .iter()
            .all(|review| choices.contains_key(&review.id)),
        "The shared values changed. Review the latest comparison."
    );
    for review in reviews.iter().filter(|review| review.markdown) {
        let text = markdown(&choices[&review.id]);
        ensure!(
            !["<<<<<<< OURS", ">>>>>>> THEIRS", "||||||| BASE"]
                .iter()
                .any(|marker| text.contains(marker)),
            "Remove the conflict markers before resolving"
        );
    }
    let original = draft.mesh.as_ref().context("Draft is missing metadata")?;
    let before = mesh.project(root)?;
    let rows = crate::mesh::normalized(&before, root)?;
    let sequence = mesh.clock.get(&original.origin).copied().unwrap_or(0) + 1;
    let mut candidate = mesh.make(&before, &before, root, &original.origin, sequence)?;
    let mut mutations = Vec::new();
    for mutation in &original.mutations {
        let current = current_row(mesh, &rows, mutation);
        let prior = current.map(|(_, row)| row.clone());
        let comparisons = || {
            reviews
                .iter()
                .filter(|review| review.entity == mutation.entity && review.key == mutation.key)
        };
        let mut after = if let Some(review) = comparisons().find(|review| review.field == "$entity")
        {
            let value = &choices[&review.id];
            (!value.is_null()).then(|| value.clone())
        } else if mutation.before.is_none() || mutation.after.is_none() {
            mutation.after.clone()
        } else {
            let mut value = prior.clone().context("Draft entity no longer exists")?;
            for (field, original) in fields(mutation) {
                let selected = comparisons()
                    .find(|review| review.field == field)
                    .map_or(original, |review| choices[&review.id].clone());
                set(&mut value, &field, selected);
            }
            Some(value)
        };
        let mut updated = mutation.clone();
        if let Some(((_, canonical), _)) = current {
            updated.key.clone_from(canonical);
            if let Some(targets) = mesh
                .aliases
                .get(&(mutation.entity.clone(), canonical.clone()))
            {
                updated.targets.clone_from(targets);
            }
            if let Some(row) = &mut after {
                match updated.entity.as_str() {
                    "purchases" => row["part"]["id"] = canonical.clone().into(),
                    "storage" => {}
                    _ => row["id"] = canonical.clone().into(),
                }
            }
        }
        if after != prior {
            updated.before = prior;
            updated.after = after;
            mutations.push(updated);
        }
    }
    candidate.mesh.as_mut().unwrap().mutations = mutations;
    let mut projection = mesh.clone();
    projection.insert(candidate)?;
    let after = projection.project(root)?;
    let mut record = mesh.make(&before, &after, root, &original.origin, sequence)?;
    record
        .mesh
        .as_mut()
        .unwrap()
        .author
        .clone_from(&original.author);
    Ok(record)
}

pub fn conflicts(events: &BTreeMap<String, Record>, local: &str) -> Vec<Review> {
    conflicts_for_branch(events, local, &Default::default())
}

pub(crate) fn conflicts_for_branch(
    events: &BTreeMap<String, Record>,
    local: &str,
    local_branch: &BTreeSet<&str>,
) -> Vec<Review> {
    let resolved: std::collections::BTreeSet<_> = events
        .values()
        .flat_map(|r| {
            r.mesh
                .as_ref()
                .unwrap()
                .resolutions
                .iter()
                .map(|r| r.conflict.clone())
        })
        .collect();
    let ordered: Vec<_> = events.values().collect();
    let mut output = BTreeMap::new();
    for (i, left) in ordered.iter().enumerate() {
        for right in &ordered[i + 1..] {
            if seen(left, right) || seen(right, left) {
                continue;
            }
            for a in &left.mesh.as_ref().unwrap().mutations {
                for b in &right.mesh.as_ref().unwrap().mutations {
                    if a.entity != b.entity || !a.targets.iter().any(|id| b.targets.contains(id)) {
                        continue;
                    }
                    // Independent creations retain the existing additive merge rule.
                    if a.before.is_none() && b.before.is_none() {
                        continue;
                    }
                    let derived = |m: &Mutation| {
                        m.entity == "bom"
                            && [&m.before, &m.after]
                                .into_iter()
                                .flatten()
                                .any(|v| !v["kicad_source"].is_null())
                    };
                    if derived(a) || derived(b) {
                        continue;
                    }
                    let af = fields(a);
                    let bf = fields(b);
                    if af.is_empty() || bf.is_empty() {
                        continue;
                    }
                    let deletion = a.after.is_none() != b.after.is_none();
                    let names: Vec<String> = if deletion {
                        vec!["$entity".into()]
                    } else {
                        af.keys()
                            .filter(|field| bf.contains_key(*field))
                            .cloned()
                            .collect()
                    };
                    for field in names {
                        let av = if field == "$entity" {
                            a.after.clone().unwrap_or(Value::Null)
                        } else {
                            af[&field].clone()
                        };
                        let bv = if field == "$entity" {
                            b.after.clone().unwrap_or(Value::Null)
                        } else {
                            bf[&field].clone()
                        };
                        if av == bv {
                            continue;
                        }
                        let id = hex(&Sha1::digest(format!(
                            "{}:{}:{}:{}:{}",
                            left.id, right.id, a.entity, a.key, field
                        )));
                        if resolved.contains(&id) {
                            continue;
                        }
                        let left_local = local_branch.contains(left.id.as_str());
                        let right_local = local_branch.contains(right.id.as_str());
                        let local_left = if left_local != right_local {
                            left_local
                        } else {
                            left.mesh.as_ref().unwrap().author() == local
                                || right.mesh.as_ref().unwrap().author() != local
                        };
                        let (ours, theirs) = if local_left { (av, bv) } else { (bv, av) };
                        let mut base = a
                            .before
                            .as_ref()
                            .map(|v| get(v, &field))
                            .unwrap_or(Value::Null);
                        let mut common: Vec<_> = events
                            .values()
                            .filter(|event| seen(left, event) && seen(right, event))
                            .collect();
                        common.sort_by_key(|r| {
                            let m = r.mesh.as_ref().unwrap();
                            (m.lamport, m.origin.clone(), m.sequence, r.id.clone())
                        });
                        let left_base = a
                            .before
                            .as_ref()
                            .map(|v| get(v, &field))
                            .unwrap_or(Value::Null);
                        let right_base = b
                            .before
                            .as_ref()
                            .map(|v| get(v, &field))
                            .unwrap_or(Value::Null);
                        if left_base != right_base {
                            for event in common {
                                for mutation in &event.mesh.as_ref().unwrap().mutations {
                                    if mutation.entity == a.entity
                                        && mutation.targets.iter().any(|id| a.targets.contains(id))
                                    {
                                        if field == "$entity" {
                                            base = mutation.after.clone().unwrap_or(Value::Null);
                                        } else if let Some(value) = fields(mutation).get(&field) {
                                            base = value.clone();
                                        }
                                    }
                                }
                            }
                        }
                        output.insert(
                            id.clone(),
                            Review {
                                id,
                                entity: a.entity.clone(),
                                key: a.key.clone(),
                                markdown: field == "notes" || field == "part.notes",
                                field,
                                base,
                                ours,
                                theirs,
                                events: vec![left.id.clone(), right.id.clone()],
                            },
                        );
                    }
                }
            }
        }
    }
    output.into_values().collect()
}
/// Combine non-overlapping single edit ranges; otherwise show editable conflict markers.
pub fn merge_text(base: &str, ours: &str, theirs: &str) -> String {
    if ours == theirs {
        return ours.into();
    }
    if ours == base {
        return theirs.into();
    }
    if theirs == base {
        return ours.into();
    }
    let base: Vec<_> = base.split_inclusive('\n').collect();
    let a: Vec<_> = ours.split_inclusive('\n').collect();
    let b: Vec<_> = theirs.split_inclusive('\n').collect();
    fn edit<'a>(base: &[&str], next: &[&'a str]) -> (usize, usize, Vec<&'a str>) {
        let mut start = 0;
        while start < base.len().min(next.len()) && base[start] == next[start] {
            start += 1;
        }
        let mut end = base.len();
        let mut tail = next.len();
        while end > start && tail > start && base[end - 1] == next[tail - 1] {
            end -= 1;
            tail -= 1;
        }
        (start, end, next[start..tail].to_vec())
    }
    let x = edit(&base, &a);
    let y = edit(&base, &b);
    if x.1 <= y.0 && !(x.0 == x.1 && x.0 == y.0) {
        return [
            base[..x.0].to_vec(),
            x.2,
            base[x.1..y.0].to_vec(),
            y.2,
            base[y.1..].to_vec(),
        ]
        .concat()
        .join("");
    }
    if y.1 <= x.0 && !(y.0 == y.1 && y.0 == x.0) {
        return [
            base[..y.0].to_vec(),
            y.2,
            base[y.1..x.0].to_vec(),
            x.2,
            base[x.1..].to_vec(),
        ]
        .concat()
        .join("");
    }
    format!(
        "<<<<<<< OURS\n{ours}\n||||||| BASE\n{}\n=======\n{theirs}\n>>>>>>> THEIRS",
        base.join("")
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn markdown(value: &Value) -> &str {
    value["markdownSource"]
        .as_str()
        .or_else(|| value["text"].as_str())
        .unwrap_or("")
}

pub fn auto_notes(review: &Review) -> Option<Value> {
    if !review.markdown {
        return None;
    }
    let merged = merge_text(
        markdown(&review.base),
        markdown(&review.ours),
        markdown(&review.theirs),
    );
    if merged.contains("<<<<<<< OURS") {
        return None;
    }
    let mut value = review.ours.clone();
    let mut images = serde_json::Map::new();
    let keys: std::collections::BTreeSet<_> = [&review.base, &review.ours, &review.theirs]
        .into_iter()
        .filter_map(|v| v["images"].as_object())
        .flat_map(|m| m.keys())
        .collect();
    for key in keys {
        let b = &review.base["images"][key];
        let a = &review.ours["images"][key];
        let c = &review.theirs["images"][key];
        let selected = if a == c || c == b {
            a
        } else if a == b {
            c
        } else {
            return None;
        };
        if !selected.is_null() {
            images.insert(key.clone(), selected.clone());
        }
    }
    if let Some(object) = value.as_object_mut() {
        for key in ["visualTokens", "visualHTML", "latexSource"] {
            object.remove(key);
        }
    }
    value["images"] = Value::Object(images);
    value["text"] = merged.clone().into();
    value["markdownSource"] = merged.into();
    value["rtf"] = "".into();
    Some(value)
}

pub fn branch_head(events: &BTreeMap<String, Record>, id: &str) -> String {
    let tip = &events[id];
    let subset = events
        .values()
        .filter(|r| r.id == id || seen(tip, r))
        .map(|r| (r.id.clone(), r.clone()))
        .collect();
    head(&subset, tip.mesh.as_ref().unwrap().context.clone()).hash
}

pub fn display(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string_pretty(value).unwrap_or_default())
}

/// Build the same durable merge decision on native and browser peers.
pub fn resolve(
    mesh: &crate::mesh::Mesh,
    root: &std::path::Path,
    origin: &str,
    id: &str,
    value: Value,
) -> anyhow::Result<Record> {
    use anyhow::{Context, ensure};
    let review = conflicts(&mesh.events, origin)
        .into_iter()
        .find(|r| r.id == id)
        .context("Conflict changed; refresh the comparison")?;
    if review.markdown {
        let text = markdown(&value);
        ensure!(
            !text.contains("<<<<<<< OURS")
                && !text.contains(">>>>>>> THEIRS")
                && !text.contains("||||||| BASE"),
            "Remove the conflict markers before resolving"
        );
    }
    let mut projected = mesh.clone();
    let candidate = projected.project(root)?;
    let seq = mesh
        .events
        .values()
        .filter_map(|r| r.mesh.as_ref())
        .filter(|m| m.origin == origin)
        .map(|m| m.sequence)
        .max()
        .unwrap_or(0)
        + 1;
    let mut record = projected.make(&candidate, &candidate, root, origin, seq)?;
    let meta = record.mesh.as_mut().unwrap();
    meta.parents = review
        .events
        .iter()
        .map(|id| branch_head(&mesh.events, id))
        .collect();
    let source = mesh
        .events
        .values()
        .flat_map(|record| {
            let m = record.mesh.as_ref().unwrap();
            m.mutations.iter().map(move |mutation| {
                (
                    (m.lamport, m.origin.clone(), m.sequence, record.id.clone()),
                    mutation,
                )
            })
        })
        .filter(|(_, m)| m.entity == review.entity && m.key == review.key && m.after.is_some())
        .max_by_key(|(stamp, _)| stamp.clone())
        .map(|(_, mutation)| mutation)
        .context("Missing conflicted entity")?;
    let mut after = source.after.clone().unwrap();
    set(&mut after, &review.field, value.clone());
    let mut before = after.clone();
    set(&mut before, &review.field, serde_json::Value::Null);
    if value.is_null() && review.field != "$entity" {
        set(
            &mut before,
            &review.field,
            serde_json::json!("__previous_value__"),
        );
    }
    if review.field == "$entity" {
        before = source.after.clone().unwrap();
        if !after.is_null() {
            before = after.clone();
            if let Some(object) = before.as_object_mut() {
                for (field, value) in object.iter_mut() {
                    if field != "stock" && field != "part" {
                        *value = serde_json::Value::Null;
                    }
                }
            }
        }
        if !after.is_null() {
            if review.entity == "purchases" {
                after["part"]["id"] = review.key.clone().into();
                before["part"]["stock"] = projected.counter("purchases", &source.targets)?.into();
            } else if review.entity == "parts" {
                after["id"] = review.key.clone().into();
                before["stock"] = projected.counter("parts", &source.targets)?.into();
            }
        }
    }
    if review.field == "stock" {
        before["stock"] = projected.counter("parts", &source.targets)?.into();
    }
    if review.field == "part.stock" {
        before["part"]["stock"] = projected.counter("purchases", &source.targets)?.into();
    }
    meta.mutations.push(crate::mesh::Mutation {
        entity: review.entity.clone(),
        key: review.key.clone(),
        targets: source.targets.clone(),
        before: Some(before),
        after: if after.is_null() { None } else { Some(after) },
        position: source.position,
    });
    meta.resolutions.push(Resolution {
        conflict: review.id,
        entity: review.entity,
        key: review.key,
        field: review.field,
        value,
        parents: review.events,
    });
    Ok(record)
}
