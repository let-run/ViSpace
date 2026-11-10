use crate::model::*;
use anyhow::{Result, ensure};
use regex::Regex;
use std::{
    collections::BTreeSet,
    io::{Read, Write},
};

fn header(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn supplier(s: &str) -> Option<&'static str> {
    let normalized = header(s);
    for (prefix, name) in [
        ("lcsc", "LCSC"),
        ("digikey", "DigiKey"),
        ("mouser", "Mouser"),
        ("newark", "Newark"),
    ] {
        if [
            prefix.to_owned(),
            format!("{prefix}part"),
            format!("{prefix}partnumber"),
            format!("{prefix}partno"),
        ]
        .contains(&normalized)
        {
            return Some(name);
        }
    }
    None
}
pub fn part_field(raw: &str) -> Option<&'static str> {
    if supplier(raw).is_some() {
        return Some("supplierNumber");
    }
    Some(match header(raw).as_str() {
        "number" | "partnumber" | "mpn" | "manufacturepartnumber" | "manufacturerpartnumber" => {
            "number"
        }
        "manufacturer" | "manufacture" => "manufacturer",
        "description" => "description",
        "package" | "footprint" => "package",
        "value" => "value",
        "tolerance" => "tolerance",
        "storage" | "location" => "storage",
        "quantity" | "qty" | "stock" | "onhandstock" => "stock",
        "datasheet" => "datasheet",
        "barcode" => "barcode",
        "labels" | "type" | "category" => "labels",
        "spn" | "supplierpartnumber" => "spn",
        "supplier" => "supplier",
        _ => return None,
    })
}
pub fn read_parts(reader: impl Read) -> Result<Vec<Part>> {
    read_parts_impl(reader, true)
}
pub(crate) fn read_preview_parts(reader: impl Read) -> Result<Vec<Part>> {
    read_parts_impl(reader, false)
}
fn read_parts_impl(reader: impl Read, detect: bool) -> Result<Vec<Part>> {
    let mut csv = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(reader);
    let headers = csv.headers()?.clone();
    let mut parts = vec![];
    for (n, row) in csv.records().enumerate() {
        let row = row?;
        let named_supplier = headers
            .iter()
            .zip(row.iter())
            .find_map(|(h, v)| (!v.is_empty()).then(|| supplier(h)).flatten());
        if let Some(named) = named_supplier
            && let Some(explicit) = headers
                .iter()
                .zip(row.iter())
                .find_map(|(h, v)| (header(h) == "supplier" && !v.is_empty()).then_some(v))
        {
            ensure!(
                key(supplier(explicit).unwrap_or(explicit)) == key(named),
                "Row {}: supplier conflicts with the supplier column name",
                n + 2
            );
        }
        let mut p = Part::default();
        let mut suppliers = 0;
        for (h, v) in headers.iter().zip(row.iter()) {
            if let Some(s) = supplier(h) {
                if !v.is_empty() {
                    suppliers += 1;
                    p.supplier = s.into();
                    p.spn = v.into();
                }
                continue;
            }
            match header(h).as_str() {
                "number"
                | "partnumber"
                | "mpn"
                | "manufacturepartnumber"
                | "manufacturerpartnumber" => p.number = v.into(),
                "manufacturer" | "manufacture" => p.manufacturer = v.into(),
                "description" => p.description = v.into(),
                "package" | "footprint" => p.package = v.into(),
                "value" => p.value = v.into(),
                "tolerance" => p.tolerance = v.into(),
                "storage" | "location" => p.storage = v.into(),
                "quantity" | "qty" | "stock" | "onhandstock" => {
                    p.stock = if v.is_empty() { "0" } else { v }.parse().map_err(|_| {
                        anyhow::anyhow!("Row {}: quantity must be a whole number", n + 2)
                    })?
                }
                "datasheet" => p.datasheet = v.into(),
                "barcode" => {
                    if !v.is_empty() {
                        p.barcode = v.into();
                    }
                }
                "labels" | "type" | "category" => {
                    p.labels = crate::csv_import::labels(v);
                }
                "spn" | "supplierpartnumber" => {
                    if !v.is_empty() {
                        suppliers += 1;
                        p.spn = v.into();
                    }
                }
                "supplier" => {
                    if !v.is_empty() {
                        p.supplier = v.into();
                    }
                }
                _ => {
                    p.custom_fields.insert(h.into(), v.into());
                }
            }
        }
        ensure!(
            suppliers <= 1,
            "Row {}: multiple supplier part numbers",
            n + 2
        );
        if detect {
            analyze(&mut p);
        }
        p.validate()?;
        parts.push(p);
    }
    ensure!(!parts.is_empty(), "CSV contains no parts");
    Ok(parts)
}
pub fn write_parts(writer: impl Write, parts: &[Part]) -> Result<()> {
    let suppliers: BTreeSet<_> = parts
        .iter()
        .filter(|p| !p.supplier.is_empty())
        .map(|p| p.supplier.clone())
        .collect();
    let custom: BTreeSet<_> = parts
        .iter()
        .flat_map(|p| p.custom_fields.keys().cloned())
        .collect();
    let mut w = csv::Writer::from_writer(writer);
    let mut headers: Vec<String> = [
        "Part Number",
        "Manufacturer",
        "Description",
        "Package",
        "Value",
        "Tolerance",
        "Stock",
        "Storage",
        "Datasheet",
        "Barcode",
        "Labels",
    ]
    .map(str::to_owned)
    .to_vec();
    headers.extend(suppliers.iter().cloned());
    headers.extend(custom.iter().cloned());
    w.write_record(headers)?;
    for p in parts {
        let mut row = vec![
            p.number.clone(),
            p.manufacturer.clone(),
            p.description.clone(),
            p.package.clone(),
            p.value.clone(),
            p.tolerance.clone(),
            p.stock.to_string(),
            p.storage.clone(),
            p.datasheet.clone(),
            p.barcode.clone(),
            p.labels.join(", "),
        ];
        row.extend(suppliers.iter().map(|s| {
            if s == &p.supplier {
                p.spn.clone()
            } else {
                String::new()
            }
        }));
        row.extend(
            custom
                .iter()
                .map(|s| p.custom_fields.get(s).cloned().unwrap_or_default()),
        );
        w.write_record(row)?;
    }
    w.flush()?;
    Ok(())
}
#[derive(Clone, Debug)]
pub struct ReviewRow {
    pub item: BomItem,
    pub candidates: Vec<String>,
    pub selected: Option<String>,
}
pub fn read_bom(reader: impl Read, inventory: &Inventory, project: &str) -> Result<Vec<ReviewRow>> {
    let mut csv = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(reader);
    let headers = csv.headers()?.clone();
    let column = |names: &[&str]| {
        headers
            .iter()
            .position(|h| names.contains(&header(h).as_str()))
    };
    let refs = column(&["designator", "designators", "reference", "references"])
        .ok_or_else(|| anyhow::anyhow!("Missing Designator column"))?;
    let value =
        column(&["value", "partnumber"]).ok_or_else(|| anyhow::anyhow!("Missing Value column"))?;
    let qty =
        column(&["quantity", "qty"]).ok_or_else(|| anyhow::anyhow!("Missing Quantity column"))?;
    let pkg = column(&["footprint", "package"])
        .ok_or_else(|| anyhow::anyhow!("Missing Footprint column"))?;
    let mut seen = BTreeSet::new();
    let mut rows = vec![];
    for row in csv.records() {
        let row = row?;
        let cell = |i| row.get(i).unwrap_or("");
        let mut item = BomItem {
            designators: cell(refs).into(),
            value: cell(value).into(),
            quantity: cell(qty).parse()?,
            package: cell(pkg).into(),
            ..Default::default()
        };
        ensure!(
            item.quantity > 0 && !item.value.is_empty(),
            "BOM needs positive quantity and value"
        );
        let refs = references(&item.designators)?;
        ensure!(
            !refs.is_empty() && seen.is_disjoint(&refs),
            "Missing or repeated BOM references"
        );
        seen.extend(refs.clone());
        let mut count = 0;
        for (h, v) in headers.iter().zip(row.iter()) {
            if let Some(s) = supplier(h)
                && !v.is_empty()
            {
                count += 1;
                item.spn = v.into();
                item.supplier = s.into();
            }
        }
        if let Some(i) = column(&["spn", "supplierpartnumber"])
            && !cell(i).is_empty()
        {
            count += 1;
            item.spn = cell(i).into();
            item.supplier = column(&["supplier"])
                .map(|i| cell(i).to_owned())
                .unwrap_or_default();
        }
        ensure!(
            count <= 1 && (item.spn.is_empty() || !item.supplier.is_empty()),
            "Use one SPN with a supplier per BOM row"
        );
        let candidates = inventory
            .parts
            .iter()
            .filter(|p| bom_matches(&item, p))
            .map(|p| p.id.clone())
            .collect::<Vec<_>>();
        let selected = inventory
            .bom
            .iter()
            .find(|e| {
                key(&e.project) == key(project)
                    && references(&e.item.designators).ok().as_ref() == Some(&refs)
            })
            .and_then(|e| e.part_id.clone())
            .filter(|id| candidates.contains(id));
        rows.push(ReviewRow {
            item,
            candidates,
            selected,
        });
    }
    ensure!(!rows.is_empty(), "BOM contains no rows");
    Ok(rows)
}
pub fn apply_bom(data: &mut Inventory, project: &str, rows: &[ReviewRow]) -> Result<()> {
    let mut next = data.clone();
    let project = next.ensure_project(project)?;
    let imported: BTreeSet<_> = rows
        .iter()
        .map(|r| references(&r.item.designators))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    for entry in next
        .bom
        .iter()
        .filter(|e| key(&e.project) == key(&project) && e.kicad_source.is_some())
    {
        ensure!(
            references(&entry.item.designators)?.is_disjoint(&imported),
            "KiCad requested parts are read-only. Edit these designators in KiCad instead of importing replacements."
        );
    }
    // Split prior groups so re-importing a subset keeps references outside the new BOM.
    for old in next
        .bom
        .clone()
        .into_iter()
        .filter(|e| key(&e.project) == key(&project))
    {
        let refs = references(&old.item.designators)?;
        if refs.is_disjoint(&imported) {
            continue;
        }
        let remaining = refs.difference(&imported).cloned().collect::<Vec<_>>();
        next.bom.retain(|e| e.id != old.id);
        let allocation = old
            .allocation_id
            .as_ref()
            .and_then(|id| next.allocations.iter().find(|a| &a.id == id))
            .cloned();
        if let Some(id) = &old.allocation_id {
            next.release(id);
        }
        if !remaining.is_empty() {
            let mut residual = old;
            residual.item.designators = remaining.join(", ");
            residual.item.quantity = remaining.len() as i64;
            residual.allocation_id = None;
            if let Some(mut a) = allocation {
                let allocated_refs = references(&a.designators)?
                    .difference(&imported)
                    .cloned()
                    .collect::<Vec<_>>();
                if !allocated_refs.is_empty() {
                    a.designators = allocated_refs.join(", ");
                    a.quantity = allocated_refs.len() as i64;
                    residual.allocation_id = Some(a.id.clone());
                    next.allocations.push(a);
                }
            }
            next.bom.push(residual);
        }
    }
    for row in rows {
        let mut entry = BomEntry {
            kicad_source: None,
            id: id(),
            project: project.clone(),
            item: row.item.clone(),
            part_id: row.selected.clone(),
            allocation_id: None,
        };
        if let Some(part_id) = &row.selected {
            let p = next
                .parts
                .iter()
                .find(|p| &p.id == part_id)
                .ok_or_else(|| anyhow::anyhow!("Selected part no longer exists"))?;
            let amount = next.available(p).min(row.item.quantity);
            if amount > 0 {
                let allocation_id = id();
                let refs = references(&row.item.designators)?
                    .into_iter()
                    .take(amount as usize)
                    .collect::<Vec<_>>()
                    .join(", ");
                next.allocations.push(Allocation {
                    id: allocation_id.clone(),
                    part_id: part_id.clone(),
                    project: project.clone(),
                    quantity: amount,
                    designators: refs,
                });
                entry.allocation_id = Some(allocation_id);
            }
        }
        next.bom.push(entry);
    }
    next.validate()?;
    *data = next;
    Ok(())
}
pub fn write_requested(writer: impl Write, inventory: &Inventory, project: &str) -> Result<()> {
    let pending = inventory.requested_items();
    let entries = inventory
        .bom
        .iter()
        .filter(|e| {
            key(&e.project) == key(project) && pending.get(&e.id).is_some_and(|i| i.quantity > 0)
        })
        .collect::<Vec<_>>();
    let suppliers: BTreeSet<_> = entries
        .iter()
        .filter(|e| !e.item.supplier.is_empty())
        .map(|e| e.item.supplier.clone())
        .collect();
    let mut w = csv::Writer::from_writer(writer);
    let mut headers = vec![
        "Designator".into(),
        "Footprint".into(),
        "Quantity".into(),
        "Value".into(),
    ];
    headers.extend(suppliers.iter().cloned());
    w.write_record(headers)?;
    for e in entries {
        let refs = pending
            .get(&e.id)
            .map(|i| i.designators.clone())
            .unwrap_or_default();
        let mut row = vec![
            refs,
            e.item.package.clone(),
            pending[&e.id].quantity.to_string(),
            e.item.value.clone(),
        ];
        row.extend(suppliers.iter().map(|s| {
            if s == &e.item.supplier {
                e.item.spn.clone()
            } else {
                String::new()
            }
        }));
        w.write_record(row)?;
    }
    w.flush()?;
    Ok(())
}
fn package(raw: &str) -> String {
    let re = Regex::new(r"(?:01005|0201|0402|0603|0805|1206|1210|1812|2010|2512|3225)").unwrap();
    re.find(raw)
        .map(|m| m.as_str().into())
        .unwrap_or_else(|| header(raw.rsplit(':').next().unwrap_or(raw)))
}
pub fn magnitude(raw: &str) -> Option<f64> {
    let raw = raw
        .trim()
        .replace(['µ', 'μ'], "u")
        .replace(['Ω', 'Ω'], "Ohm");
    let embedded = Regex::new(r"^(\d+)([RrKkMm])(\d+)(?:[Oo][Hh][Mm])?$").unwrap();
    let scale = |s: &str| match s {
        "p" => 1e-12,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" | "K" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        _ => 1.0,
    };
    if let Some(c) = embedded.captures(&raw) {
        return Some(format!("{}.{}", &c[1], &c[3]).parse::<f64>().ok()? * scale(&c[2]));
    }
    let re =
        Regex::new(r"(?i)^(\d+(?:\.\d+)?|\.\d+)\s*([pnumkMG]?)\s*(?:ohms?|R|F|H|Hz)?$").unwrap();
    let c = re.captures(&raw)?;
    Some(c[1].parse::<f64>().ok()? * scale(&c[2]))
}
fn bom_matches(item: &BomItem, p: &Part) -> bool {
    if key(&item.value) == key(&p.number)
        || (!item.spn.is_empty()
            && key(&item.spn) == key(&p.spn)
            && key(&item.supplier) == key(&p.supplier))
    {
        return true;
    }
    let kind = item.designators.chars().next().unwrap_or(' ');
    let compatible = match kind.to_ascii_uppercase() {
        'R' => p.labels.iter().any(|s| key(s) == "resistor"),
        'C' => p.labels.iter().any(|s| key(s) == "capacitor"),
        'L' => p.labels.iter().any(|s| key(s) == "inductor"),
        'Y' | 'X' => p
            .labels
            .iter()
            .any(|s| ["crystal", "oscillator"].contains(&key(s).as_str())),
        _ => false,
    };
    if compatible
        && package(&item.package) == package(&p.package)
        && !item.package.is_empty()
        && let (Some(a), Some(b)) = (magnitude(&item.value), magnitude(&p.value))
    {
        return (a - b).abs() <= a.abs() * 1e-9;
    }
    item.value.len() >= 5 && key(&p.number).starts_with(&key(&item.value))
}
pub fn analyze(p: &mut Part) {
    crate::description_analysis::DescriptionAnalysis::analyze(&p.description, &p.value)
        .fill_missing(p);
}
