use anyhow::{Result, bail, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}
pub fn key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Case-insensitive ordering with digit runs compared numerically (R2 before R10).
pub fn part_number_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let (mut a, mut b) = (a.as_str(), b.as_str());
    while !a.is_empty() && !b.is_empty() {
        let ac = a.chars().next().unwrap();
        let bc = b.chars().next().unwrap();
        if ac.is_ascii_digit() && bc.is_ascii_digit() {
            let an = a.bytes().take_while(u8::is_ascii_digit).count();
            let bn = b.bytes().take_while(u8::is_ascii_digit).count();
            let av = a[..an].trim_start_matches('0');
            let bv = b[..bn].trim_start_matches('0');
            let order = av.len().cmp(&bv.len()).then_with(|| av.cmp(bv));
            if !order.is_eq() {
                return order;
            }
            a = &a[an..];
            b = &b[bn..];
        } else {
            let order = ac.cmp(&bc);
            if !order.is_eq() {
                return order;
            }
            a = &a[ac.len_utf8()..];
            b = &b[bc.len_utf8()..];
        }
    }
    a.len().cmp(&b.len())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Notes {
    pub text: String,
    pub rtf: String,
    pub images: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl Notes {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }
}
/// Identifies the document owner without confusing project, cart and inventory notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotesTarget {
    Part(String),
    Purchase(String),
    Project(String),
}
impl Inventory {
    pub fn save_notes(&mut self, target: &NotesTarget, notes: Notes) -> Result<()> {
        match target {
            NotesTarget::Part(id) => {
                self.parts
                    .iter_mut()
                    .find(|p| &p.id == id)
                    .ok_or_else(|| anyhow::anyhow!("Part not found"))?
                    .notes = notes
            }
            NotesTarget::Purchase(id) => {
                self.purchases
                    .iter_mut()
                    .find(|p| &p.part.id == id)
                    .ok_or_else(|| anyhow::anyhow!("Purchase not found"))?
                    .part
                    .notes = notes
            }
            NotesTarget::Project(name) => {
                self.projects
                    .iter_mut()
                    .find(|p| key(&p.name) == key(name))
                    .ok_or_else(|| anyhow::anyhow!("Project not found"))?
                    .notes = notes
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Part {
    pub id: String,
    pub number: String,
    pub manufacturer: String,
    pub spn: String,
    pub supplier: String,
    pub custom_fields: BTreeMap<String, String>,
    pub labels: Vec<String>,
    pub tolerance: String,
    pub description: String,
    pub package: String,
    pub value: String,
    pub storage: String,
    pub datasheet: String,
    pub barcode: String,
    pub stock: i64,
    pub notes: Notes,
    pub image: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl Default for Part {
    fn default() -> Self {
        Self {
            id: id(),
            barcode: id(),
            number: String::new(),
            manufacturer: String::new(),
            spn: String::new(),
            supplier: String::new(),
            custom_fields: BTreeMap::new(),
            labels: vec![],
            tolerance: String::new(),
            description: String::new(),
            package: String::new(),
            value: String::new(),
            storage: String::new(),
            datasheet: String::new(),
            stock: 0,
            notes: Notes::default(),
            image: None,
            extra: BTreeMap::new(),
        }
    }
}
impl Part {
    pub fn migrate(&mut self) {
        if self.spn.is_empty()
            && let Some(v) = self
                .extra
                .remove("lcsc")
                .and_then(|v| v.as_str().map(str::to_owned))
            && !v.is_empty()
        {
            self.spn = v;
            self.supplier = "LCSC".into();
        }
        if self.labels.is_empty()
            && let Some(v) = self
                .extra
                .remove("category")
                .and_then(|v| v.as_str().map(str::to_owned))
            && !v.trim().is_empty()
        {
            self.labels.push(v);
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.number.trim().is_empty(), "Part number is required");
        ensure!(self.stock >= 0, "Stock must be nonnegative");
        ensure!(
            !self.id.is_empty() && !self.barcode.trim().is_empty(),
            "ID and barcode are required"
        );
        ensure!(
            self.spn.trim().is_empty() || !self.supplier.trim().is_empty(),
            "Supplier is required with an SPN"
        );
        Ok(())
    }
    pub fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {} {} {} {} {} {} {} {} {}",
            self.number,
            self.manufacturer,
            self.spn,
            self.supplier,
            self.description,
            self.package,
            self.value,
            self.tolerance,
            self.barcode,
            self.labels.join(" "),
            self.custom_fields
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
        .to_lowercase();
        let haystack = format!(
            "{haystack} {}",
            self.extra
                .get("syncBarcodeAliases")
                .map(|v| v.to_string().to_lowercase())
                .unwrap_or_default()
        );
        query
            .to_lowercase()
            .split_whitespace()
            .all(|q| haystack.contains(q))
    }
    pub fn same_identity(&self, other: &Self) -> bool {
        key(&self.number) == key(&other.number)
            || (!self.spn.trim().is_empty()
                && !other.spn.trim().is_empty()
                && key(&self.spn) == key(&other.spn)
                && key(&self.supplier) == key(&other.supplier))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Allocation {
    pub id: String,
    #[serde(rename = "partID")]
    pub part_id: String,
    pub project: String,
    pub quantity: i64,
    #[serde(default)]
    pub designators: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Assignment {
    pub id: String,
    pub project: String,
    pub quantity: i64,
    #[serde(default)]
    pub designators: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Purchase {
    pub part: Part,
    #[serde(default)]
    pub assignments: Vec<Assignment>,
}
impl Purchase {
    pub fn matches(&self, query: &str) -> bool {
        let p = &self.part;
        let fields = format!(
            "{} {} {} {} {} {} {} {} {} {} {}",
            p.number,
            p.manufacturer,
            p.spn,
            p.supplier,
            p.description,
            p.package,
            p.value,
            p.tolerance,
            p.barcode,
            p.labels.join(" "),
            self.assignments
                .iter()
                .map(|a| format!("{} {}", a.project, a.designators))
                .collect::<Vec<_>>()
                .join(" ")
        )
        .to_lowercase();
        query
            .to_lowercase()
            .split_whitespace()
            .all(|word| fields.contains(word))
    }
}
/// Desktop links use paths. Browsers retain a folder handle ID or imported-file snapshots.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KicadProject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_models_folder: Option<String>,
    #[serde(default)]
    pub revision: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
    /// Opaque IndexedDB handle ID; the browser retains read permission separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_folder: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

impl Default for GitRemote {
    fn default() -> Self {
        Self {
            name: "origin".into(),
            url: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitTip {
    pub reference: String,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Project {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicad: Option<KicadProject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<GitRemote>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub git_tips: BTreeMap<String, GitTip>,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub notes: Notes,
    #[serde(default, rename = "notesAttachmentID")]
    pub notes_attachment_id: Option<String>,
}
impl Project {
    pub fn unresolved_git_tips(&self) -> BTreeSet<&str> {
        self.git_tips
            .values()
            .filter(|candidate| {
                !self.git_tips.values().any(|other| {
                    other.commit != candidate.commit && other.includes.contains(&candidate.commit)
                })
            })
            .map(|tip| tip.commit.as_str())
            .collect()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BomItem {
    pub designators: String,
    pub package: String,
    pub quantity: i64,
    pub value: String,
    #[serde(default)]
    pub spn: String,
    #[serde(default)]
    pub supplier: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BomEntry {
    /// KiCad-owned rows are replaced on refresh and removed on detach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicad_source: Option<String>,
    pub id: String,
    pub project: String,
    pub item: BomItem,
    #[serde(rename = "partID")]
    pub part_id: Option<String>,
    #[serde(rename = "allocationID")]
    pub allocation_id: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Inventory {
    pub parts: Vec<Part>,
    pub allocations: Vec<Allocation>,
    pub purchases: Vec<Purchase>,
    pub projects: Vec<Project>,
    pub storage: BTreeMap<String, String>,
    pub bom: Vec<BomEntry>,
}
impl Inventory {
    pub fn allocated(&self, part: &str) -> i64 {
        self.allocations
            .iter()
            .filter(|a| a.part_id == part)
            .fold(0_i64, |sum, a| sum.saturating_add(a.quantity))
    }
    pub fn available(&self, part: &Part) -> i64 {
        part.stock.saturating_sub(self.allocated(&part.id))
    }
    /// All remaining BOM quantities in one pass, including shared reservations.
    pub fn requested_quantities(&self) -> BTreeMap<String, i64> {
        self.requested_items()
            .into_iter()
            .map(|(id, item)| (id, item.quantity))
            .collect()
    }
    pub fn requested(&self, e: &BomEntry) -> i64 {
        self.requested_quantities()
            .get(&e.id)
            .copied()
            .unwrap_or(e.item.quantity)
    }
    pub fn validate(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        let mut barcodes = BTreeSet::new();
        for p in &self.parts {
            p.validate()?;
            ensure!(ids.insert(&p.id), "Duplicate part ID");
            ensure!(barcodes.insert(&p.barcode), "Duplicate barcode");
            ensure!(
                i128::from(p.stock)
                    >= self
                        .allocations
                        .iter()
                        .filter(|a| a.part_id == p.id)
                        .map(|a| i128::from(a.quantity))
                        .sum::<i128>(),
                "Stock below allocated quantity for {}",
                p.number
            );
        }
        let mut projects = BTreeSet::new();
        let mut project_ids = BTreeSet::new();
        for p in &self.projects {
            ensure!(
                p.id.is_empty() || project_ids.insert(&p.id),
                "Duplicate project ID"
            );
            ensure!(
                !p.name.trim().is_empty() && projects.insert(key(&p.name)),
                "Empty or duplicate project name"
            );
        }
        let mut allocations = BTreeSet::new();
        for a in &self.allocations {
            ensure!(allocations.insert(&a.id), "Duplicate allocation ID");
            ensure!(
                ids.contains(&a.part_id),
                "Allocation references missing part"
            );
            ensure!(a.quantity > 0, "Allocation quantity must be positive");
            ensure!(
                projects.contains(&key(&a.project)),
                "Allocation references missing project"
            );
        }
        let mut purchase_ids = BTreeSet::new();
        for p in &self.purchases {
            p.part.validate()?;
            ensure!(purchase_ids.insert(&p.part.id), "Duplicate purchase ID");
            ensure!(
                p.part.stock > 0
                    && p.assignments
                        .iter()
                        .map(|a| i128::from(a.quantity))
                        .sum::<i128>()
                        <= i128::from(p.part.stock),
                "Purchase assignments exceed quantity"
            );
            for a in &p.assignments {
                ensure!(
                    a.quantity > 0 && projects.contains(&key(&a.project)),
                    "Invalid purchase assignment"
                );
            }
        }
        let mut bom_ids = BTreeSet::new();
        for e in &self.bom {
            ensure!(bom_ids.insert(&e.id), "Duplicate BOM entry ID");
            ensure!(
                e.item.spn.is_empty() || !e.item.supplier.trim().is_empty(),
                "BOM supplier is required with an SPN"
            );
            references(&e.item.designators)?;
            ensure!(
                e.item.quantity > 0 && projects.contains(&key(&e.project)),
                "Invalid BOM entry"
            );
        }
        Ok(())
    }
    pub fn save_part(&mut self, mut part: Part) -> Result<()> {
        part.number = part.number.trim().into();
        part.validate()?;
        if let Some(old) = self.parts.iter().find(|p| p.id == part.id) {
            ensure!(old.barcode == part.barcode, "Barcode is immutable");
        }
        ensure!(
            part.stock >= self.allocated(&part.id),
            "Stock cannot be below allocated quantity"
        );
        ensure!(
            !self
                .parts
                .iter()
                .any(|p| p.id != part.id && p.barcode == part.barcode),
            "Barcode already exists"
        );
        if !part.storage.is_empty() {
            self.storage.entry(part.storage.clone()).or_default();
        }
        if let Some(p) = self.parts.iter_mut().find(|p| p.id == part.id) {
            *p = part;
        } else {
            self.parts.push(part);
        }
        Ok(())
    }
    pub fn ensure_project(&mut self, name: &str) -> Result<String> {
        ensure!(!name.trim().is_empty(), "Project name is required");
        if let Some(p) = self.projects.iter().find(|p| key(&p.name) == key(name)) {
            return Ok(p.name.clone());
        }
        let name = name.trim().to_owned();
        self.projects.push(Project {
            id: id(),
            name: name.clone(),
            ..Default::default()
        });
        Ok(name)
    }
    /// Apply a reviewed batch atomically; stale candidates must be reviewed again.
    pub fn bulk_allocate(
        &mut self,
        project: &str,
        drafts: &[(String, i64, String)],
        purchase: bool,
    ) -> Result<()> {
        let project = self
            .projects
            .iter()
            .find(|p| key(&p.name) == key(project))
            .ok_or_else(|| anyhow::anyhow!("Project no longer exists"))?
            .name
            .clone();
        let mut next = self.clone();
        let mut seen = BTreeSet::new();
        for (part_id, quantity, refs) in drafts {
            let refs = refs.trim();
            if *quantity == 0 && refs.is_empty() {
                continue;
            }
            ensure!(seen.insert(part_id), "Duplicate part in bulk adding");
            ensure!(*quantity > 0, "Enter a positive whole-number quantity");
            references(refs)?;
            if purchase {
                let item = next
                    .purchases
                    .iter_mut()
                    .find(|p| &p.part.id == part_id)
                    .ok_or_else(|| anyhow::anyhow!("Purchase item no longer exists"))?;
                ensure!(
                    !item
                        .assignments
                        .iter()
                        .any(|a| key(&a.project) == key(&project)),
                    "Item already assigned; cancel bulk adding and refresh"
                );
                let assigned: i128 = item
                    .assignments
                    .iter()
                    .map(|a| i128::from(a.quantity))
                    .sum();
                ensure!(
                    assigned + i128::from(*quantity) <= i128::from(item.part.stock),
                    "{}: quantity exceeds unassigned purchase quantity",
                    item.part.number
                );
                item.assignments.push(Assignment {
                    id: id(),
                    project: project.clone(),
                    quantity: *quantity,
                    designators: refs.into(),
                });
            } else {
                let part = next
                    .parts
                    .iter()
                    .find(|p| &p.id == part_id)
                    .ok_or_else(|| anyhow::anyhow!("Part no longer exists"))?;
                ensure!(
                    !next
                        .allocations
                        .iter()
                        .any(|a| &a.part_id == part_id && key(&a.project) == key(&project)),
                    "Part already allocated; cancel bulk adding and refresh"
                );
                ensure!(
                    *quantity <= next.available(part),
                    "{}: quantity exceeds available stock ({})",
                    part.number,
                    next.available(part)
                );
                next.allocations.push(Allocation {
                    id: id(),
                    part_id: part_id.clone(),
                    project: project.clone(),
                    quantity: *quantity,
                    designators: refs.into(),
                });
            }
        }
        next.reconcile_designator_priority(self)?;
        next.validate()?;
        *self = next;
        Ok(())
    }
    pub fn bulk_storage(&mut self, location: &str, part_ids: &BTreeSet<String>) -> Result<()> {
        ensure!(
            location.is_empty() || self.storage.keys().any(|n| key(n) == key(location)),
            "Storage location no longer exists"
        );
        ensure!(
            part_ids
                .iter()
                .all(|id| self.parts.iter().any(|p| &p.id == id)),
            "A selected part no longer exists; reopen bulk adding"
        );
        for part in &mut self.parts {
            if part_ids.contains(&part.id) {
                part.storage = location.into();
            }
        }
        Ok(())
    }
    pub fn allocate(
        &mut self,
        part_id: &str,
        project: &str,
        quantity: i64,
        refs: &str,
    ) -> Result<String> {
        let quantity = if refs.trim().is_empty() {
            quantity
        } else {
            references(refs)?.len() as i64
        };
        ensure!(quantity > 0, "Quantity must be positive");
        let p = self
            .parts
            .iter()
            .find(|p| p.id == part_id)
            .ok_or_else(|| anyhow::anyhow!("Part not found"))?;
        ensure!(
            self.available(p) >= quantity,
            "Insufficient available stock"
        );
        let project = self.ensure_project(project)?;
        let allocation = Allocation {
            id: id(),
            part_id: part_id.into(),
            project,
            quantity,
            designators: refs.into(),
        };
        let id = allocation.id.clone();
        let previous = self.clone();
        self.allocations.push(allocation);
        if let Err(error) = self.reconcile_designator_priority(&previous) {
            *self = previous;
            return Err(error);
        }
        Ok(id)
    }
    pub fn release(&mut self, id: &str) {
        self.allocations.retain(|a| a.id != id);
    }
    pub fn delete_part(&mut self, id: &str) {
        self.parts.retain(|p| p.id != id);
        self.allocations.retain(|a| a.part_id != id);
        for e in &mut self.bom {
            if e.part_id.as_deref() == Some(id) {
                e.part_id = None;
                e.allocation_id = None;
            }
        }
    }
    /// Convert relationship references at the storage boundary. The working
    /// inventory exposes project names to its UI and import/export callers.
    pub(crate) fn convert_project_references(&mut self, store_ids: bool) {
        let mapping: BTreeMap<_, _> = self
            .projects
            .iter()
            .filter(|project| !project.id.is_empty())
            .map(|project| {
                if store_ids {
                    (key(&project.name), &project.id)
                } else {
                    (project.id.clone(), &project.name)
                }
            })
            .collect();
        let convert = |reference: &mut String| {
            let lookup = if store_ids {
                key(reference)
            } else {
                reference.clone()
            };
            if let Some(value) = mapping.get(&lookup) {
                reference.clone_from(value);
            }
        };
        for allocation in &mut self.allocations {
            convert(&mut allocation.project);
        }
        for purchase in &mut self.purchases {
            for assignment in &mut purchase.assignments {
                convert(&mut assignment.project);
            }
        }
        for entry in &mut self.bom {
            convert(&mut entry.project);
        }
    }

    pub fn rename_project(&mut self, old: &str, name: &str, description: &str) -> Result<()> {
        ensure!(!name.trim().is_empty(), "Name is required");
        ensure!(
            !self
                .projects
                .iter()
                .any(|p| key(&p.name) != key(old) && key(&p.name) == key(name)),
            "Project already exists"
        );
        let p = self
            .projects
            .iter_mut()
            .find(|p| key(&p.name) == key(old))
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
        p.name = name.trim().into();
        p.description = description.into();
        for a in &mut self.allocations {
            if key(&a.project) == key(old) {
                a.project = name.trim().into();
            }
        }
        for p in &mut self.purchases {
            for a in &mut p.assignments {
                if key(&a.project) == key(old) {
                    a.project = name.trim().into();
                }
            }
        }
        for e in &mut self.bom {
            if key(&e.project) == key(old) {
                e.project = name.trim().into();
            }
        }
        Ok(())
    }
    pub fn delete_project(&mut self, name: &str) {
        self.projects.retain(|p| key(&p.name) != key(name));
        self.allocations.retain(|a| key(&a.project) != key(name));
        self.bom.retain(|e| key(&e.project) != key(name));
        for p in &mut self.purchases {
            p.assignments.retain(|a| key(&a.project) != key(name));
        }
    }
    pub fn import_parts(&mut self, parts: Vec<Part>) -> Result<()> {
        let mut next = self.clone();
        for mut p in parts {
            p.validate()?;
            let matches: Vec<_> = next
                .parts
                .iter()
                .enumerate()
                .filter(|(_, old)| old.same_identity(&p))
                .map(|(i, _)| i)
                .collect();
            ensure!(
                matches.len() <= 1,
                "Conflicting identifiers for {}",
                p.number
            );
            if let Some(i) = matches.first() {
                let old = &mut next.parts[*i];
                old.stock = old
                    .stock
                    .checked_add(p.stock)
                    .ok_or_else(|| anyhow::anyhow!("Stock overflow"))?;
                for label in p.labels {
                    if !old.labels.iter().any(|l| key(l) == key(&label)) {
                        old.labels.push(label);
                    }
                }
                if old.spn.is_empty() {
                    old.spn = p.spn;
                    old.supplier = p.supplier;
                }
            } else {
                if next.parts.iter().any(|old| old.id == p.id) {
                    p.id = id();
                }
                next.save_part(p)?;
            }
        }
        next.reconcile_designator_priority(self)?;
        next.validate()?;
        *self = next;
        Ok(())
    }
    pub fn receive(&mut self, ids: &[String]) -> Result<Vec<String>> {
        ensure!(!ids.is_empty(), "Select purchases to receive");
        ensure!(
            ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
            "Duplicate purchase IDs"
        );
        let mut next = self.clone();
        let mut received = vec![];
        for purchase_id in ids {
            let purchase = next
                .purchases
                .iter()
                .find(|p| &p.part.id == purchase_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Purchase not found or already received"))?;
            let matches: Vec<_> = next
                .parts
                .iter()
                .filter(|p| p.same_identity(&purchase.part))
                .map(|p| p.id.clone())
                .collect();
            ensure!(
                matches.len() <= 1,
                "Ambiguous inventory match for {}",
                purchase.part.number
            );
            let part_id = if let Some(id) = matches.first() {
                let p = next.parts.iter_mut().find(|p| &p.id == id).unwrap();
                p.stock = p
                    .stock
                    .checked_add(purchase.part.stock)
                    .ok_or_else(|| anyhow::anyhow!("Stock overflow"))?;
                if !purchase.part.notes.text.is_empty() {
                    if !p.notes.text.is_empty() {
                        p.notes.text.push_str("\n\n");
                    }
                    p.notes.text.push_str(&purchase.part.notes.text);
                }
                id.clone()
            } else {
                let mut p = purchase.part.clone();
                if next.parts.iter().any(|old| old.id == p.id) {
                    p.id = id();
                }
                let id = p.id.clone();
                next.save_part(p)?;
                id
            };
            for a in purchase.assignments {
                let project = next.ensure_project(&a.project)?;
                let allocation_id = id();
                next.allocations.push(Allocation {
                    id: allocation_id.clone(),
                    part_id: part_id.clone(),
                    project: project.clone(),
                    quantity: a.quantity,
                    designators: a.designators.clone(),
                });
                for e in &mut next.bom {
                    if key(&e.project) == key(&project)
                        && e.allocation_id.is_none()
                        && references(&e.item.designators)? == references(&a.designators)?
                        && !a.designators.trim().is_empty()
                    {
                        e.part_id = Some(part_id.clone());
                        e.allocation_id = Some(allocation_id.clone());
                        break;
                    }
                }
            }
            next.purchases.retain(|p| &p.part.id != purchase_id);
            received.push(part_id);
        }
        next.reconcile_designator_priority(self)?;
        next.validate()?;
        *self = next;
        Ok(received)
    }
}

/// Expand unique designators and inclusive ranges, rejecting malformed or huge ranges.
pub fn references(raw: &str) -> Result<BTreeSet<String>> {
    static RANGE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static CUSTOM: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RANGE.get_or_init(|| Regex::new(r"^([A-Z]+)(\d+)$").unwrap());
    let custom = CUSTOM.get_or_init(|| Regex::new(r"^[A-Z_][A-Z0-9_.]*$").unwrap());
    let mut out = BTreeSet::new();
    for token in raw
        .to_uppercase()
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|s| !s.is_empty())
    {
        if let Some((a, b)) = token.split_once('-') {
            let a = re
                .captures(a)
                .ok_or_else(|| anyhow::anyhow!("Invalid designator range"))?;
            let b = re
                .captures(b)
                .ok_or_else(|| anyhow::anyhow!("Invalid designator range"))?;
            let start: u32 = a[2].parse()?;
            let end: u32 = b[2].parse()?;
            ensure!(
                a[1] == b[1] && end >= start && end - start <= 10_000,
                "Invalid designator range"
            );
            for n in start..=end {
                out.insert(format!("{}{n}", &a[1]));
            }
        } else {
            ensure!(
                custom.is_match(token) && token.chars().any(|c| c.is_ascii_digit()),
                "Invalid designator: {token}"
            );
            out.insert(token.into());
        }
        if out.len() > 100_000 {
            bail!("Too many designators");
        }
    }
    Ok(out)
}

impl Inventory {
    /// KiCad bindings and schematic-derived requests are a per-device cache, never mesh state.
    pub fn restore_local_project_data(&mut self, local: &Inventory) {
        for project in &mut self.projects {
            let old = local.projects.iter().find(|p| {
                if !project.id.is_empty() && !p.id.is_empty() {
                    p.id == project.id
                } else {
                    key(&p.name) == key(&project.name)
                }
            });
            project.kicad = old.and_then(|p| p.kicad.clone());
        }
        self.bom.retain(|b| b.kicad_source.is_none());
        for entry in local.bom.iter().filter(|b| b.kicad_source.is_some()) {
            let Some(old_project) = local
                .projects
                .iter()
                .find(|p| key(&p.name) == key(&entry.project))
            else {
                continue;
            };
            let Some(project) = self.projects.iter().find(|p| {
                if !p.id.is_empty() && !old_project.id.is_empty() {
                    p.id == old_project.id
                } else {
                    key(&p.name) == key(&old_project.name)
                }
            }) else {
                continue;
            };
            let mut entry = entry.clone();
            entry.project = project.name.clone();
            if entry
                .part_id
                .as_ref()
                .is_some_and(|id| !self.parts.iter().any(|p| &p.id == id))
            {
                entry.part_id = None;
            }
            if entry
                .allocation_id
                .as_ref()
                .is_some_and(|id| !self.allocations.iter().any(|a| &a.id == id))
            {
                entry.allocation_id = None;
            }
            self.bom.retain(|b| b.id != entry.id);
            self.bom.push(entry);
        }
    }
}
