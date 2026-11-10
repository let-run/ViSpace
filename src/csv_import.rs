//! Editable CSV document: preserve columns and defer part validation until Apply.
use crate::{csv_io, description_analysis::DescriptionAnalysis, model::*};
use anyhow::{Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ImportRow {
    pub selected: bool,
    pub values: Vec<String>,
    pub pending_label: String,
    detected: BTreeMap<String, String>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CsvImport {
    pub filename: String,
    pub headers: Vec<String>,
    pub rows: Vec<ImportRow>,
    pub ignored_custom: BTreeSet<usize>,
}
pub fn labels(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in text
        .split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !names.iter().any(|s| key(s) == key(name)) {
            names.push(name.to_owned());
        }
    }
    names
}
impl CsvImport {
    pub fn read(reader: impl Read, filename: String) -> Result<Self> {
        let mut csv = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(reader);
        let mut headers: Vec<_> = csv.headers()?.iter().map(str::to_owned).collect();
        ensure!(
            !headers.is_empty()
                && headers.iter().all(|s| !s.is_empty())
                && headers
                    .iter()
                    .map(|s| key(s))
                    .collect::<BTreeSet<_>>()
                    .len()
                    == headers.len(),
            "CSV column names must be nonempty and unique."
        );
        let mut rows = Vec::new();
        for (n, record) in csv.records().enumerate() {
            let record = record?;
            if record.iter().all(|v| v.trim().is_empty()) {
                continue;
            }
            ensure!(
                record.len() <= headers.len(),
                "CSV row {} has more values than column names.",
                n + 2
            );
            let mut values: Vec<_> = record.iter().map(str::to_owned).collect();
            values.resize(headers.len(), String::new());
            rows.push(ImportRow {
                selected: true,
                values,
                pending_label: String::new(),
                detected: BTreeMap::new(),
            });
        }
        ensure!(!rows.is_empty(), "No parts were found in the CSV.");
        for name in ["Value", "Tolerance", "Labels"] {
            if !headers
                .iter()
                .any(|h| csv_io::part_field(h) == Some(&name.to_lowercase()))
            {
                headers.push(name.into());
                for row in &mut rows {
                    row.values.push(String::new());
                }
            }
        }
        let mut doc = Self {
            filename,
            headers,
            rows,
            ignored_custom: BTreeSet::new(),
        };
        for row in 0..doc.rows.len() {
            doc.analyze(row);
        }
        Ok(doc)
    }
    fn column(&self, field: &str) -> Option<usize> {
        self.headers
            .iter()
            .position(|h| csv_io::part_field(h) == Some(field))
    }
    pub fn label_column(&self) -> usize {
        self.column("labels").unwrap()
    }
    pub fn set_value(&mut self, row: usize, column: usize, value: String) {
        self.rows[row].values[column] = value;
        match csv_io::part_field(&self.headers[column]) {
            Some("description" | "value") => self.analyze(row),
            Some(field) => {
                self.rows[row].detected.remove(field);
            }
            None => {}
        }
    }
    fn analyze(&mut self, row: usize) {
        let description = self
            .column("description")
            .map(|c| self.rows[row].values[c].clone())
            .unwrap_or_default();
        let value = self
            .column("value")
            .map(|c| self.rows[row].values[c].clone())
            .unwrap_or_default();
        let hint = if self.rows[row].detected.get("value") == Some(&value) {
            ""
        } else {
            &value
        };
        let hints = DescriptionAnalysis::analyze(&description, hint);
        let mut detected = BTreeMap::new();
        for (field, hint) in [
            ("value", hints.value),
            ("tolerance", hints.tolerance),
            ("labels", hints.label),
        ] {
            if let Some(c) = self.column(field) {
                let current = &self.rows[row].values[c];
                if current.is_empty() || self.rows[row].detected.get(field) == Some(current) {
                    self.rows[row].values[c] = hint.clone();
                    detected.insert(field.into(), hint);
                }
            }
        }
        self.rows[row].detected = detected;
    }
    pub fn suggestions(&self, existing: impl IntoIterator<Item = String>) -> Vec<String> {
        let column = self.label_column();
        let mut names: Vec<String> = Vec::new();
        for name in existing
            .into_iter()
            .chain(self.rows.iter().flat_map(|r| labels(&r.values[column])))
        {
            if !name.trim().is_empty() && !names.iter().any(|s| key(s) == key(&name)) {
                names.push(name);
            }
        }
        names
    }
    pub fn add_labels(&mut self, row: usize, text: &str, suggestions: &[String]) {
        let column = self.label_column();
        let mut names = labels(&self.rows[row].values[column]);
        for token in labels(text) {
            let name = suggestions
                .iter()
                .find(|s| key(s) == key(&token))
                .cloned()
                .unwrap_or(token);
            if !names.iter().any(|s| key(s) == key(&name)) {
                names.push(name);
            }
        }
        self.set_value(row, column, names.join(", "));
        self.rows[row].pending_label.clear();
    }
    pub fn selected_parts(&mut self, suggestions: &[String]) -> Result<Vec<Part>> {
        ensure!(
            self.rows.iter().any(|r| r.selected),
            "Select at least one row to import."
        );
        for i in 0..self.rows.len() {
            if self.rows[i].selected {
                let pending = self.rows[i].pending_label.clone();
                if !pending.trim().is_empty() {
                    self.add_labels(i, &pending, suggestions);
                }
            }
        }
        let columns: Vec<_> = (0..self.headers.len())
            .filter(|i| {
                csv_io::part_field(&self.headers[*i]).is_some() || !self.ignored_custom.contains(i)
            })
            .collect();
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer.write_record(columns.iter().map(|i| &self.headers[*i]))?;
        for row in self.rows.iter().filter(|r| r.selected) {
            writer.write_record(columns.iter().map(|i| &row.values[*i]))?;
        }
        let bytes = writer.into_inner()?;
        csv_io::read_preview_parts(bytes.as_slice())
    }
}
