//! Component hints ported from Swift's Models/DescriptionAnalysis.swift.
use crate::model::{Part, key};
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DescriptionAnalysis {
    pub value: String,
    pub tolerance: String,
    pub label: String,
}
const NUMBER: &str = r"(?:\d+(?:\.\d+)?|\.\d+)";
fn letter_or_number(c: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\p{L}\p{N}]$").unwrap())
        .is_match(&c.to_string())
}
// Rust regexes omit lookaround. Check Swift's unit boundaries against each match
// explicitly so package codes and fragments of supplier numbers cannot become values.
fn measurement(text: &str, pattern: &str) -> Option<Vec<String>> {
    Regex::new(pattern)
        .unwrap()
        .captures_iter(text)
        .find_map(|c| {
            let span = c.get(0).unwrap();
            if text[..span.start()]
                .chars()
                .next_back()
                .is_some_and(|c| letter_or_number(c) || c == '.')
                || text[span.end()..]
                    .chars()
                    .next()
                    .is_some_and(letter_or_number)
            {
                return None;
            }
            Some(
                c.iter()
                    .map(|m| m.map_or("", |m| m.as_str()).to_owned())
                    .collect(),
            )
        })
}
fn resistance(text: &str) -> String {
    if let Some(m) = measurement(text, r"(\d+)([kKmM])(\d+)\s*(?:[oO][hH][mM][sS]?|[ΩΩ])") {
        return format!(
            "{}.{}{}Ω",
            m[1],
            m[3],
            if m[2].eq_ignore_ascii_case("k") {
                "k"
            } else {
                &m[2]
            }
        );
    }
    measurement(
        text,
        &format!(r"({NUMBER})\s*([kKmM]?)\s*(?:[oO][hH][mM][sS]?|[ΩΩ])"),
    )
    .map(|m| format!("{}{}Ω", m[1], if m[2] == "K" { "k" } else { &m[2] }))
    .unwrap_or_default()
}
fn reactive(text: &str, unit: &str) -> String {
    measurement(text, &format!(r"({NUMBER})\s*([pPnNuUµμmM]?){unit}"))
        .map(|m| {
            let prefix = match m[2].as_str() {
                "µ" | "μ" | "U" => "u",
                "P" => "p",
                "N" => "n",
                other => other,
            };
            format!("{}{prefix}{unit}", m[1])
        })
        .unwrap_or_default()
}
fn frequency(text: &str) -> String {
    measurement(text, &format!(r"({NUMBER})\s*([kKMG]?)\s*[hH][zZ]"))
        .map(|m| format!("{}{}Hz", m[1], if m[2] == "K" { "k" } else { &m[2] }))
        .unwrap_or_default()
}
impl DescriptionAnalysis {
    pub fn analyze(description: &str, value_hint: &str) -> Self {
        let source = if value_hint.is_empty() {
            description
        } else {
            value_hint
        };
        let inductance = reactive(source, "H");
        let capacitance = reactive(source, "F");
        let ohms = resistance(source);
        let hz = frequency(source);
        let has = |pattern: &str| Regex::new(pattern).unwrap().is_match(description);
        let (label, value) = if has(r"(?i)\bcrystal\b") {
            ("Crystal", hz)
        } else if has(r"(?i)\boscillator\b") {
            ("Oscillator", hz)
        } else if has(r"(?i)\binductors?\b") {
            ("Inductor", inductance)
        } else if has(r"(?i)\bcapacitors?\b") {
            ("Capacitor", capacitance)
        } else if has(r"(?i)\bresistors?\b") {
            ("Resistor", ohms)
        } else if !inductance.is_empty() {
            ("Inductor", inductance)
        } else if !capacitance.is_empty() {
            ("Capacitor", capacitance)
        } else if !ohms.is_empty() {
            ("Resistor", ohms)
        } else if has(r"(?i)\bIC\b|\bintegrated\s+circuit\b") {
            ("IC", String::new())
        } else {
            ("", String::new())
        };
        let sign = format!(r"(?:±|\+\s*/\s*-)\s*({NUMBER})\s*");
        let tolerance = if let Some(m) = Regex::new(&format!("{sign}%"))
            .unwrap()
            .captures(description)
        {
            format!("±{}%", &m[1])
        } else {
            Regex::new(&format!("{sign}(?i:ppm)"))
                .unwrap()
                .captures_iter(description)
                .find_map(|m| {
                    let tail = &description[m.get(0).unwrap().end()..];
                    if tail.chars().next().is_some_and(letter_or_number)
                        || tail.trim_start().starts_with('/')
                        || Regex::new(r"^\s*per\b").unwrap().is_match(tail)
                    {
                        None
                    } else {
                        Some(format!("±{}ppm", &m[1]))
                    }
                })
                .unwrap_or_default()
        };
        Self {
            value,
            tolerance,
            label: label.into(),
        }
    }
    pub fn fill_missing(&self, part: &mut Part) {
        if part.value.trim().is_empty() {
            part.value = self.value.clone();
        }
        if part.tolerance.trim().is_empty() {
            part.tolerance = self.tolerance.clone();
        }
        if !self.label.is_empty() && !part.labels.iter().any(|s| key(s) == key(&self.label)) {
            part.labels.push(self.label.clone());
        }
    }
}
