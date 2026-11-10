//! Requested BOM editing and explicit inventory matching, mirroring ProjectBOMOperations.swift.
use crate::{description_analysis::DescriptionAnalysis, model::*};
use anyhow::{Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::OnceLock};

impl Inventory {
    pub fn save_request(
        &mut self,
        project: &str,
        existing: Option<&str>,
        item: BomItem,
    ) -> Result<()> {
        let project = self
            .projects
            .iter()
            .find(|p| key(&p.name) == key(project))
            .ok_or_else(|| anyhow::anyhow!("Project no longer exists"))?
            .name
            .clone();
        ensure!(
            item.quantity > 0 && !item.value.trim().is_empty(),
            "Enter a value or part number and a positive whole-number quantity."
        );
        ensure!(
            item.spn.trim().is_empty() || !item.supplier.trim().is_empty(),
            "Select or enter a supplier for the supplier part number (SPN)."
        );
        references(&item.designators)?;
        if let Some(existing) = existing {
            let entry = self
                .bom
                .iter()
                .find(|e| e.id == existing && key(&e.project) == key(&project))
                .ok_or_else(|| anyhow::anyhow!("Request no longer exists"))?;
            ensure!(
                entry.kicad_source.is_none(),
                "KiCad requested parts are read-only. Edit this part in KiCad."
            );
            let allocated = entry.item.quantity - self.requested(entry);
            let quantity = item
                .quantity
                .checked_add(allocated)
                .ok_or_else(|| anyhow::anyhow!("Quantity is too large"))?;
            let entry = self.bom.iter_mut().find(|e| e.id == existing).unwrap();
            entry.item = item;
            entry.item.quantity = quantity;
        } else {
            self.bom.push(BomEntry {
                kicad_source: None,
                id: id(),
                project,
                item,
                part_id: None,
                allocation_id: None,
            });
        }
        Ok(())
    }
    pub fn delete_request(&mut self, id: &str) -> Result<()> {
        let entry = self
            .bom
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| anyhow::anyhow!("Request no longer exists"))?;
        ensure!(
            entry.kicad_source.is_none(),
            "KiCad requested parts are read-only. Edit this part in KiCad."
        );
        let allocated = entry.item.quantity - self.requested(entry);
        if allocated > 0 {
            self.bom
                .iter_mut()
                .find(|e| e.id == id)
                .unwrap()
                .item
                .quantity = allocated;
        } else {
            self.bom.retain(|e| e.id != id);
        }
        Ok(())
    }
    pub fn request_candidates(&self, entry: &BomEntry) -> Vec<&Part> {
        if let Some(allocation) = self
            .allocations
            .iter()
            .find(|a| Some(&a.id) == entry.allocation_id.as_ref())
        {
            return self
                .parts
                .iter()
                .filter(|p| p.id == allocation.part_id)
                .collect();
        }
        let mut matches = candidates(&entry.item, &self.parts);
        if let Some(confirmed) = self
            .parts
            .iter()
            .find(|p| Some(&p.id) == entry.part_id.as_ref())
            && !matches.iter().any(|p| p.id == confirmed.id)
        {
            matches.insert(0, confirmed);
        }
        matches
    }
    /// Review and Apply share this plan, including selections hidden by search.
    pub fn request_sync_plan(
        &self,
        project: &str,
        matches: &BTreeMap<String, String>,
    ) -> Result<Vec<(String, String, i64)>> {
        RequestReview::build(self, project, || false)?.plan(matches)
    }
    pub fn sync_requests(
        &mut self,
        project: &str,
        matches: &BTreeMap<String, String>,
    ) -> Result<i64> {
        self.sync_requests_cancellable(project, matches, || false)
    }
    pub fn sync_requests_cancellable(
        &mut self,
        project: &str,
        matches: &BTreeMap<String, String>,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<i64> {
        let plan = RequestReview::build(self, project, &mut cancelled)?.plan(matches)?;
        let pending = self.requested_items();
        let mut next = self.clone();
        let mut total = 0_i64;
        for (entry_id, part_id, count) in plan {
            ensure!(!cancelled(), "Sync cancelled");
            if count <= 0 {
                continue;
            }
            let refs = pending
                .get(&entry_id)
                .map(|i| references(&i.designators))
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .take(count as usize)
                .collect::<Vec<_>>()
                .join(", ");
            let entry = next.bom.iter_mut().find(|e| e.id == entry_id).unwrap();
            let allocation = next
                .allocations
                .iter_mut()
                .find(|a| Some(&a.id) == entry.allocation_id.as_ref());
            let allocation_id = if let Some(a) = allocation {
                let mut combined: std::collections::BTreeSet<_> = references(&a.designators)?
                    .into_iter()
                    .take(a.quantity.max(0) as usize)
                    .collect();
                a.quantity = a
                    .quantity
                    .checked_add(count)
                    .ok_or_else(|| anyhow::anyhow!("Quantity is too large"))?;
                combined.extend(references(&refs)?);
                a.designators = combined.into_iter().collect::<Vec<_>>().join(", ");
                a.id.clone()
            } else {
                let allocation_id = id();
                next.allocations.push(Allocation {
                    id: allocation_id.clone(),
                    part_id: part_id.clone(),
                    project: entry.project.clone(),
                    quantity: count,
                    designators: refs,
                });
                allocation_id
            };
            entry.part_id = Some(part_id);
            entry.allocation_id = Some(allocation_id);
            total = total
                .checked_add(count)
                .ok_or_else(|| anyhow::anyhow!("Quantity is too large"))?;
        }
        next.reconcile_designator_priority(self)?;
        next.validate()?;
        *self = next;
        Ok(total)
    }
}
fn supplier_key(raw: &str) -> String {
    let normalized: String = key(raw).chars().filter(|c| c.is_alphanumeric()).collect();
    if ["lcsc", "digikey", "mouser", "newark"].contains(&normalized.as_str()) {
        normalized
    } else {
        key(raw)
    }
}
/// Portable worker result. No UI, SQLite, threads, or platform APIs in this payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestReview {
    pub project: String,
    pub rows: Vec<ReviewRow>,
    pub parts: BTreeMap<String, Part>,
    pub available: BTreeMap<String, i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewRow {
    pub entry: BomEntry,
    pub requested: i64,
    pub candidates: Vec<String>,
}
impl RequestReview {
    pub fn build(
        data: &Inventory,
        project: &str,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self> {
        let project = data
            .projects
            .iter()
            .find(|p| key(&p.name) == key(project))
            .ok_or_else(|| anyhow::anyhow!("Project no longer exists"))?
            .name
            .clone();
        let mut available: BTreeMap<_, _> =
            data.parts.iter().map(|p| (p.id.clone(), p.stock)).collect();
        let mut allocations = BTreeMap::new();
        let pending = data.requested_items();
        for a in &data.allocations {
            if let Some(stock) = available.get_mut(&a.part_id) {
                *stock = stock.saturating_sub(a.quantity);
            }
            allocations.insert(a.id.as_str(), a);
        }
        for stock in available.values_mut() {
            *stock = (*stock).max(0);
        }
        let mut index = MatchIndex::new(&data.parts);
        let mut rows = Vec::new();
        let mut parts = BTreeMap::new();
        // Consume shared allocation budgets in the original BOM order, including other projects.
        for entry in &data.bom {
            ensure!(!cancelled(), "Sync cancelled");
            let item = pending.get(&entry.id).unwrap();
            let requested = item.quantity;
            if key(&entry.project) != key(&project) || requested <= 0 {
                continue;
            }
            let ids = if let Some(a) = entry
                .allocation_id
                .as_deref()
                .and_then(|id| allocations.get(id))
            {
                data.parts
                    .iter()
                    .position(|p| p.id == a.part_id)
                    .into_iter()
                    .collect()
            } else {
                let mut ids = index.candidates(&entry.item, &mut cancelled)?;
                if let Some(i) = entry
                    .part_id
                    .as_ref()
                    .and_then(|id| data.parts.iter().position(|p| &p.id == id))
                    && !ids.contains(&i)
                {
                    ids.insert(0, i);
                }
                ids
            };
            let candidates = ids
                .into_iter()
                .map(|i| {
                    let p = &data.parts[i];
                    parts.entry(p.id.clone()).or_insert_with(|| p.clone());
                    p.id.clone()
                })
                .collect();
            rows.push(ReviewRow {
                entry: BomEntry {
                    item: BomItem {
                        designators: item.designators.clone(),
                        ..entry.item.clone()
                    },
                    ..entry.clone()
                },
                requested,
                candidates,
            });
        }
        Ok(Self {
            project,
            rows,
            parts,
            available,
        })
    }
    pub fn plan(&self, matches: &BTreeMap<String, String>) -> Result<Vec<(String, String, i64)>> {
        let mut budget = self.available.clone();
        let mut plan = Vec::new();
        for row in &self.rows {
            let Some(part) = matches.get(&row.entry.id) else {
                continue;
            };
            ensure!(
                row.candidates.contains(part),
                "A selected match changed. Cancel sync and review the requests again."
            );
            let available = budget
                .get_mut(part)
                .ok_or_else(|| anyhow::anyhow!("Selected part no longer exists"))?;
            let count = row.requested.min(*available);
            *available -= count;
            plan.push((row.entry.id.clone(), part.clone(), count));
        }
        Ok(plan)
    }
    /// Cheap integer-only recalculation, once per selection change, never matching again.
    pub fn previews(&self, matches: &BTreeMap<String, String>) -> Vec<Vec<i64>> {
        let mut budget = self.available.clone();
        self.rows
            .iter()
            .map(|row| {
                let counts = row
                    .candidates
                    .iter()
                    .map(|id| row.requested.min(*budget.get(id).unwrap_or(&0)))
                    .collect();
                if let Some(id) = matches.get(&row.entry.id)
                    && row.candidates.contains(id)
                    && let Some(stock) = budget.get_mut(id)
                {
                    *stock -= row.requested.min(*stock);
                }
                counts
            })
            .collect()
    }
}
struct MatchIndex<'a> {
    parts: &'a [Part],
    numbers: BTreeMap<String, Vec<usize>>,
    suppliers: BTreeMap<(String, String), Vec<usize>>,
    packages: Vec<String>,
    values: Vec<Option<String>>,
    numeric: BTreeMap<String, BTreeMap<String, Vec<(f64, usize)>>>,
    memo: BTreeMap<(String, String, String, String), Vec<usize>>,
}
impl<'a> MatchIndex<'a> {
    fn new(parts: &'a [Part]) -> Self {
        let mut this = Self {
            parts,
            numbers: BTreeMap::new(),
            suppliers: BTreeMap::new(),
            packages: Vec::new(),
            values: vec![None; parts.len()],
            numeric: BTreeMap::new(),
            memo: BTreeMap::new(),
        };
        for (i, p) in parts.iter().enumerate() {
            this.numbers.entry(key(&p.number)).or_default().push(i);
            if !p.spn.trim().is_empty() && !p.supplier.trim().is_empty() {
                this.suppliers
                    .entry((supplier_key(&p.supplier), key(&p.spn)))
                    .or_default()
                    .push(i);
            }
            this.packages.push(package(&p.package));
        }
        this
    }
    fn candidates(
        &mut self,
        item: &BomItem,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<usize>> {
        let prefix: String = item
            .designators
            .chars()
            .take_while(|c| c.is_alphabetic())
            .collect();
        let signature = (
            item.value.clone(),
            item.package.clone(),
            item.spn.clone(),
            format!("{}\0{}", item.supplier, prefix),
        );
        if let Some(ids) = self.memo.get(&signature) {
            return Ok(ids.clone());
        }
        let value = key(&item.value);
        let mut ids = self.numbers.get(&value).cloned().unwrap_or_default();
        if let Some(exact) = self
            .suppliers
            .get(&(supplier_key(&item.supplier), key(&item.spn)))
        {
            ids.extend(exact);
        }
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            let kind = component_kind(item);
            if let Some(value) = magnitude(&item.value, &kind).filter(|_| !item.package.is_empty())
            {
                if !self.numeric.contains_key(&kind) {
                    let mut measurements: BTreeMap<String, Vec<(f64, usize)>> = BTreeMap::new();
                    for (i, p) in self.parts.iter().enumerate() {
                        ensure!(!cancelled(), "Sync cancelled");
                        let text = self.values[i].get_or_insert_with(|| {
                            if p.value.is_empty() {
                                DescriptionAnalysis::analyze(&p.description, "").value
                            } else {
                                p.value.clone()
                            }
                        });
                        if let Some(v) = magnitude(text, &kind) {
                            measurements
                                .entry(self.packages[i].clone())
                                .or_default()
                                .push((v, i));
                        }
                    }
                    self.numeric.insert(kind.clone(), measurements);
                }
                if let Some(bucket) = self.numeric[&kind].get(&package(&item.package)) {
                    ids.extend(
                        bucket
                            .iter()
                            .filter(|(v, _)| (v - value).abs() <= (value.abs() * 1e-9).max(1e-18))
                            .map(|(_, i)| *i),
                    );
                }
            } else if value.chars().count() >= 5 {
                for (number, indices) in self.numbers.range(value.clone()..) {
                    if !number.starts_with(&value) {
                        break;
                    }
                    ids.extend(indices);
                }
            }
        }
        ids.sort_by(|a, b| self.parts[*a].number.cmp(&self.parts[*b].number));
        self.memo.insert(signature, ids.clone());
        Ok(ids)
    }
}
pub fn candidates<'a>(item: &BomItem, parts: &'a [Part]) -> Vec<&'a Part> {
    MatchIndex::new(parts)
        .candidates(item, &mut || false)
        .expect("uncancelled matcher")
        .into_iter()
        .map(|i| &parts[i])
        .collect()
}
fn component_kind(item: &BomItem) -> String {
    let label = DescriptionAnalysis::analyze(&item.value, "").label;
    if !label.is_empty() {
        return label;
    }
    match item
        .designators
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect::<String>()
        .to_uppercase()
        .as_str()
    {
        "R" => "Resistor",
        "C" => "Capacitor",
        "L" => "Inductor",
        "Y" | "X" => "Crystal",
        _ => "",
    }
    .into()
}
pub(crate) fn package(raw: &str) -> String {
    let text = raw.rsplit(':').next().unwrap_or(raw);
    static PACKAGE: OnceLock<Regex> = OnceLock::new();
    let re = PACKAGE.get_or_init(|| {
        Regex::new(r"(?:01005|0201|0402|0603|0805|1206|1210|1812|2010|2512|3225)").unwrap()
    });
    for m in re.find_iter(text) {
        if !text[..m.start()]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_digit())
            && !text[m.end()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        {
            return m.as_str().into();
        }
    }
    key(text).chars().filter(|c| c.is_alphanumeric()).collect()
}
fn scale(s: &str) -> f64 {
    match s {
        "p" | "P" => 1e-12,
        "n" | "N" => 1e-9,
        "u" | "U" => 1e-6,
        "m" => 1e-3,
        "k" | "K" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        _ => 1.,
    }
}
pub(crate) fn magnitude(raw: &str, kind: &str) -> Option<f64> {
    let text = raw.trim().replace(['µ', 'μ'], "u");
    if kind == "Resistor" {
        static EMBEDDED: OnceLock<Regex> = OnceLock::new();
        let re = EMBEDDED
            .get_or_init(|| Regex::new(r"^(\d+)([RrKkMm])(\d+)(?:(?i:ohms?)|[ΩΩ])?").unwrap());
        if let Some(c) = re.captures(&text)
            && !text[c.get(0)?.end()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        {
            return Some(format!("{}.{}", &c[1], &c[3]).parse::<f64>().ok()? * scale(&c[2]));
        }
    }
    static RESISTOR: OnceLock<Regex> = OnceLock::new();
    static CAPACITOR: OnceLock<Regex> = OnceLock::new();
    static INDUCTOR: OnceLock<Regex> = OnceLock::new();
    static FREQUENCY: OnceLock<Regex> = OnceLock::new();
    let (cache, suffix) = match kind {
        "Resistor" => (&RESISTOR, r"(?:[Rr]|(?i:ohms?)|[ΩΩ])?"),
        "Capacitor" => (&CAPACITOR, "F"),
        "Inductor" => (&INDUCTOR, "H"),
        "Crystal" | "Oscillator" => (&FREQUENCY, "(?i:Hz)"),
        _ => return None,
    };
    let re = cache.get_or_init(|| {
        Regex::new(&format!(
            r"^(\d+(?:\.\d+)?|\.\d+)\s*([pPnNuUmMkKG]?)\s*(?:{suffix})"
        ))
        .unwrap()
    });
    let c = re.captures(&text)?;
    if text[c.get(0)?.end()..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return None;
    }
    Some(c[1].parse::<f64>().ok()? * scale(&c[2]))
}
