//! Authoritative schematic requests, without changing stock, allocations, or purchases.
use crate::{model::*, requests};
use anyhow::{Result, ensure};
use inventory_kicad::Bundle;
use std::collections::{BTreeMap, BTreeSet};

fn field(properties: &BTreeMap<String, String>, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| {
            properties
                .iter()
                .find(|(k, v)| key(k) == key(name) && !v.trim().is_empty())
                .map(|(_, v)| v.trim().to_owned())
        })
        .unwrap_or_default()
}
fn kind(item: &BomItem) -> String {
    if let Some(kind) = item.extra.get("kicadKind").and_then(|v| v.as_str()) {
        return kind.into();
    }
    let prefix: String = item
        .designators
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    match prefix.to_uppercase().as_str() {
        "R" => "Resistor",
        "C" => "Capacitor",
        "L" => "Inductor",
        "FB" | "FERRITE" => "Ferrite",
        "Y" | "X" | "XTAL" => "Crystal",
        _ => "",
    }
    .into()
}
fn value_key(value: &str, kind: &str) -> String {
    let family = if kind == "Ferrite" { "Resistor" } else { kind };
    requests::magnitude(value, family)
        .filter(|v| v.is_finite())
        .map(|v| format!("{v:.9e}"))
        .unwrap_or_else(|| key(value).replace(' ', "").replace(['µ', 'μ'], "u"))
}
fn passive(kind: &str) -> bool {
    matches!(
        kind,
        "Resistor" | "Capacitor" | "Inductor" | "Ferrite" | "Crystal"
    )
}
fn identity(item: &BomItem) -> String {
    let kind = kind(item);
    if passive(&kind) {
        format!(
            "{kind}|{}|{}",
            value_key(&item.value, &kind),
            requests::package(&item.package)
        )
    } else {
        format!("part|{}", key(&item.value))
    }
}
fn matches(existing: &BomItem, incoming: &BomItem) -> bool {
    let family = kind(incoming);
    if passive(&family) {
        let old_family = kind(existing);
        (old_family.is_empty() || old_family == family)
            && !existing.package.trim().is_empty()
            && requests::package(&existing.package) == requests::package(&incoming.package)
            && value_key(&existing.value, &family) == value_key(&incoming.value, &family)
    } else {
        key(&existing.value) == key(&incoming.value)
    }
}

/// One row per physical component group; multi-unit symbols count once.
/// Incomplete hierarchies must not replace the last complete BOM.
pub fn requested_items(bundle: &Bundle) -> Result<Vec<BomItem>> {
    ensure!(
        bundle.nodes().iter().all(|n| n.error.is_none()),
        "KiCad hierarchy is incomplete; keeping the previous requests until every sheet can be read."
    );
    let mut physical: BTreeMap<String, (String, BomItem)> = BTreeMap::new();
    let nodes = bundle.nodes();
    for node in &nodes {
        if nodes.iter().any(|parent| {
            parent.excluded_from_bom
                && (parent.instance_path == node.instance_path
                    || node
                        .instance_path
                        .starts_with(&format!("{}/", parent.instance_path)))
        }) {
            continue;
        }
        let scene = bundle
            .schematics
            .get(&node.file)
            .ok_or_else(|| anyhow::anyhow!("Missing schematic {}", node.file))?;
        for component in &scene.components {
            if !component.in_bom || component.dnp {
                continue;
            }
            let reference = component
                .references
                .get(&node.instance_path)
                .unwrap_or(&component.reference)
                .trim()
                .to_uppercase();
            if reference.is_empty() || reference.starts_with('#') {
                continue;
            }
            let properties = &component.properties;
            let value = field(properties, &["Value"]);
            let lib = component.lib_id.to_lowercase();
            let prefix: String = reference
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            let family = if lib.contains("ferrite") || prefix == "FB" {
                "Ferrite"
            } else {
                match prefix.as_str() {
                    "R" => "Resistor",
                    "C" => "Capacitor",
                    "L" => "Inductor",
                    "Y" | "X" | "XTAL" => "Crystal",
                    _ => "",
                }
            };
            let mpn = field(
                properties,
                &[
                    "MPN",
                    "Manufacturer Part Number",
                    "Manufacturer_Part_Number",
                    "Part Number",
                    "PartNumber",
                ],
            );
            let mut item = BomItem {
                value: if value.trim().is_empty() {
                    mpn
                } else if passive(family) || mpn.is_empty() {
                    value
                } else {
                    mpn
                },
                package: field(properties, &["Footprint", "Package"]),
                designators: reference.clone(),
                quantity: 1,
                supplier: field(properties, &["Supplier"]),
                spn: field(properties, &["Supplier Part Number", "SPN"]),
                ..Default::default()
            };
            if item.spn.is_empty() {
                item.spn = field(
                    properties,
                    &["LCSC Part #", "LCSC Part", "LCSC", "LCSC Part Number"],
                );
                if !item.spn.is_empty() {
                    item.supplier = "LCSC".into();
                }
            }
            if item.supplier.is_empty() {
                item.spn.clear();
            }
            for (name, value) in &bundle.project.text_variables {
                item.value = item.value.replace(&format!("${{{name}}}"), value);
                item.package = item.package.replace(&format!("${{{name}}}"), value);
            }
            if item.value.trim().is_empty() {
                continue;
            }
            references(&reference)?;
            item.extra
                .insert("kicadKind".into(), serde_json::json!(family));
            let identity = identity(&item);
            if let Some((old, _)) = physical.get(&reference) {
                ensure!(
                    *old == identity,
                    "Conflicting KiCad units or duplicate reference: {reference}"
                );
            } else {
                physical.insert(reference, (identity, item));
            }
        }
    }
    let mut groups: BTreeMap<String, BomItem> = BTreeMap::new();
    for (_, (identity, item)) in physical {
        if let Some(group) = groups.get_mut(&identity) {
            group.quantity += 1;
            group.designators.push_str(", ");
            group.designators.push_str(&item.designators);
        } else {
            groups.insert(identity, item);
        }
    }
    Ok(groups.into_values().collect())
}
impl Inventory {
    pub fn detach_kicad_requests(&mut self, project: &str) {
        self.bom
            .retain(|e| key(&e.project) != key(project) || e.kicad_source.is_none());
    }
    /// Returns whether a write is necessary. Unchanged refreshes preserve row IDs and allocation links.
    pub fn sync_kicad_requests(&mut self, project: &str, items: &[BomItem]) -> Result<bool> {
        let project = self
            .projects
            .iter()
            .find(|p| key(&p.name) == key(project))
            .ok_or_else(|| anyhow::anyhow!("Project no longer exists"))?
            .name
            .clone();
        let before = serde_json::to_value(&self.bom)?;
        let old: Vec<_> = self
            .bom
            .iter()
            .filter(|e| key(&e.project) == key(&project))
            .cloned()
            .collect();
        let all_refs: BTreeSet<_> = items
            .iter()
            .map(|i| references(&i.designators))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        let mut retained = Vec::new();
        for mut entry in self.bom.drain(..) {
            if key(&entry.project) != key(&project) {
                retained.push(entry);
                continue;
            }
            if entry.kicad_source.is_some() || items.iter().any(|i| matches(&entry.item, i)) {
                continue;
            }
            let refs = references(&entry.item.designators)?;
            if !refs.is_disjoint(&all_refs) {
                entry.item.designators = refs
                    .difference(&all_refs)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
            }
            retained.push(entry);
        }
        for item in items {
            let identity = identity(item);
            let previous = old
                .iter()
                .find(|e| e.kicad_source.as_deref() == Some(&identity))
                .or_else(|| {
                    old.iter()
                        .find(|e| e.kicad_source.is_none() && matches(&e.item, item))
                });
            let entry = BomEntry {
                id: previous.map(|e| e.id.clone()).unwrap_or_else(id),
                project: project.clone(),
                item: item.clone(),
                kicad_source: Some(identity),
                part_id: previous.and_then(|e| e.part_id.clone()),
                allocation_id: previous.and_then(|e| e.allocation_id.clone()),
            };
            retained.push(entry);
        }
        self.bom = retained;
        Ok(serde_json::to_value(&self.bom)? != before)
    }
}
