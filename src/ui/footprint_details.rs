//! Project-scoped footprint metadata and links to real inventory/purchase rows.
use super::*;
use inventory_kicad::{Bundle, Pcb};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PartTarget {
    pub id: String,
    pub number: String,
    pub purchase: bool,
}
#[derive(Clone, Debug, Default)]
pub(super) struct FootprintDetails {
    pub reference: String,
    pub number: String,
    pub value: String,
    pub tolerance: String,
    pub package: String,
    pub targets: Vec<PartTarget>,
}
fn property_key(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}
fn property(fields: &BTreeMap<String, String>, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| {
            fields
                .iter()
                .find(|(key, value)| {
                    !value.trim().is_empty() && property_key(key) == property_key(name)
                })
                .map(|(_, value)| value.clone())
        })
        .unwrap_or_default()
}
fn numbered_reference(value: &str) -> Option<(&str, u32)> {
    let index = value.find(|c: char| c.is_ascii_digit())?;
    let (prefix, digits) = value.split_at(index);
    if prefix.is_empty()
        || !prefix.chars().all(|c| c.is_ascii_alphabetic())
        || !digits.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((prefix, digits.parse().ok()?))
}
fn has_reference(raw: &str, reference: &str) -> bool {
    // Match only the selected reference: no regex compilation or range expansion
    // while the UI is resolving a footprint against many allocations.
    raw.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .any(|token| {
            if token.eq_ignore_ascii_case(reference) {
                return true;
            }
            let Some((a, b)) = token.split_once('-') else {
                return false;
            };
            let (Some((pa, a)), Some((pb, b)), Some((pr, n))) = (
                numbered_reference(a),
                numbered_reference(b),
                numbered_reference(reference),
            ) else {
                return false;
            };
            let digits = &reference[pr.len()..];
            pa.eq_ignore_ascii_case(pb)
                && pa.eq_ignore_ascii_case(pr)
                && !(digits.len() > 1 && digits.starts_with('0'))
                && b.checked_sub(a).is_some_and(|length| length <= 10_000)
                && (a..=b).contains(&n)
        })
}
impl FootprintDetails {
    pub fn build(
        data: &Inventory,
        bundle: &Bundle,
        board: &Pcb,
        project: &str,
        reference: &str,
    ) -> Self {
        Self::build_optional(data, bundle, Some(board), project, reference)
    }
    pub fn build_for_symbol(
        data: &Inventory,
        bundle: &Bundle,
        project: &str,
        reference: &str,
    ) -> Self {
        Self::build_optional(data, bundle, bundle.pcb.as_deref(), project, reference)
    }
    fn build_optional(
        data: &Inventory,
        bundle: &Bundle,
        board: Option<&Pcb>,
        project: &str,
        reference: &str,
    ) -> Self {
        let footprint = board.and_then(|board| {
            board
                .footprint_list
                .iter()
                .find(|f| f.reference == reference)
        });
        let mut fields = footprint.map(|f| f.properties.clone()).unwrap_or_default();
        for node in bundle.nodes() {
            if let Some(component) = bundle.schematics.get(&node.file).and_then(|scene| {
                scene.components.iter().find(|c| {
                    c.references
                        .get(&node.instance_path)
                        .unwrap_or(&c.reference)
                        .eq_ignore_ascii_case(reference)
                })
            }) {
                for (key, value) in &component.properties {
                    if !value.trim().is_empty() {
                        fields.retain(|old, _| property_key(old) != property_key(key));
                        fields.insert(key.clone(), value.clone());
                    }
                }
                break;
            }
        }
        let mut details = Self {
            reference: reference.into(),
            number: property(&fields, &["MPN", "Part Number", "Manufacturer Part Number"]),
            value: property(&fields, &["Value"]),
            tolerance: property(&fields, &["Tolerance"]),
            package: property(&fields, &["Package", "Footprint"]),
            targets: vec![],
        };
        if details.package.is_empty() {
            details.package = footprint.map(|f| f.library.clone()).unwrap_or_default();
        }
        let mut parts = Vec::new();
        for allocation in data
            .allocations
            .iter()
            .filter(|a| key(&a.project) == key(project) && has_reference(&a.designators, reference))
        {
            if let Some(part) = data.parts.iter().find(|p| p.id == allocation.part_id) {
                parts.push((part, false));
            }
        }
        for purchase in &data.purchases {
            if purchase.assignments.iter().any(|a| {
                key(&a.project) == key(project) && has_reference(&a.designators, reference)
            }) {
                parts.push((&purchase.part, true));
            }
        }
        if let Some((part, _)) = parts.first() {
            if !part.number.trim().is_empty() {
                details.number = part.number.clone();
            }
            if !part.value.trim().is_empty() {
                details.value = part.value.clone();
            }
            if !part.tolerance.trim().is_empty() {
                details.tolerance = part.tolerance.clone();
            }
            if !part.package.trim().is_empty() {
                details.package = part.package.clone();
            }
        }
        if let Some((_, name)) = details.package.rsplit_once(':') {
            details.package = name.into();
        }
        for (part, purchase) in parts {
            if !details
                .targets
                .iter()
                .any(|t| t.id == part.id && t.purchase == purchase)
            {
                details.targets.push(PartTarget {
                    id: part.id.clone(),
                    number: part.number.clone(),
                    purchase,
                });
            }
        }
        details
    }
}

impl FootprintDetails {
    pub fn show(&self, ui: &mut egui::Ui) -> Option<PartTarget> {
        let details = self;
        let mut navigation = None;
        ui.separator();
        egui::Grid::new(("footprint_metadata", &details.reference))
            .num_columns(2)
            .spacing(egui::vec2(8., 3.))
            .min_row_height(16.)
            .show(ui, |ui| {
                for (label, value) in [
                    ("Part number", details.number.as_str()),
                    ("Value", details.value.as_str()),
                    ("Tolerance", details.tolerance.as_str()),
                    ("Package", details.package.as_str()),
                ] {
                    if label == "Tolerance" && value.trim().is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(label).size(11.).color(theme::MUTED));
                    ui.add(
                        egui::Label::new(
                            RichText::new(if value.trim().is_empty() {
                                "—"
                            } else {
                                value
                            })
                            .size(12.),
                        )
                        .wrap(),
                    );
                    ui.end_row();
                }
            });
        for target in &details.targets {
            let label = if details.targets.len() > 1 {
                format!(
                    "{}: {}",
                    if target.purchase { "Purchase" } else { "Part" },
                    target.number
                )
            } else if target.purchase {
                "Go to Purchase".into()
            } else {
                "Go to Part".into()
            };
            if ui.link(RichText::new(label).color(theme::YELLOW)).clicked() {
                navigation = Some(target.clone());
            }
        }
        navigation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Allocation, Assignment, Purchase};
    fn fixture() -> Bundle {
        inventory_kicad::load_project("demo.kicad_pro",|path| match path {
            "demo.kicad_pro" => Ok("{}".into()),
            "demo.kicad_sch" => Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (symbol (lib_id "Missing") (at 10 10 0) (uuid d6)
                    (property "Reference" "D6") (property "Value" "LED")
                    (property "mpn" "SCHEM-MPN") (property "Tolerance" "5%")
                    (property "Footprint" "LED_SMD:LED_0603")))"#.into()),
            "demo.kicad_pcb" => Ok(r#"(kicad_pcb (version 20250114) (layers (0 "F.Cu" signal))
                (footprint "BoardLib:BoardPackage" (layer "F.Cu") (at 1 1)
                    (property "Reference" "D6") (property "Value" "Old value") (property "MPN" "OLD-MPN")
                    (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu"))))"#.into()),
            _ => Err(inventory_kicad::Error::new("Missing")),
        }).unwrap()
    }
    #[test]
    fn designator_lookup_matches_ranges_and_custom_names_without_prefix_collisions() {
        for (raw, reference, matched) in [
            ("D5-D7", "D6", true),
            ("D60", "D6", false),
            ("r1; R3-R5", "R4", true),
            ("TP_3V1", "TP_3V1", true),
            ("R1-R3", "R01", false),
            ("R3-R1", "R2", false),
            ("D5-D7", "R6", false),
        ] {
            assert_eq!(has_reference(raw, reference), matched, "{raw}: {reference}");
        }
    }
    #[test]
    fn metadata_falls_back_to_schematic_and_board_without_inventing_links() {
        let bundle = fixture();
        let board = bundle.pcb.as_ref().unwrap();
        let info = FootprintDetails::build(&Inventory::default(), &bundle, board, "Demo", "D6");
        assert_eq!(info.number, "SCHEM-MPN");
        assert_eq!(info.value, "LED");
        assert_eq!(info.tolerance, "5%");
        assert_eq!(info.package, "LED_0603");
        assert!(info.targets.is_empty());
    }
    #[test]
    fn links_match_project_designators_and_keep_inventory_and_purchase_destinations_distinct() {
        let bundle = fixture();
        let board = bundle.pcb.as_ref().unwrap();
        let mut data = Inventory::default();
        let part = Part {
            id: "allocated".into(),
            number: "ACTUAL-MPN".into(),
            value: "Red LED".into(),
            tolerance: "1%".into(),
            package: "0603".into(),
            ..Default::default()
        };
        data.parts.push(part);
        data.allocations.push(Allocation {
            id: "a1".into(),
            part_id: "allocated".into(),
            project: "Demo".into(),
            quantity: 1,
            designators: "D6".into(),
        });
        // The same reference in a different project must not produce a link.
        data.parts.push(Part {
            id: "other".into(),
            number: "OTHER".into(),
            ..Default::default()
        });
        data.allocations.push(Allocation {
            id: "a2".into(),
            part_id: "other".into(),
            project: "Other project".into(),
            quantity: 1,
            designators: "D6".into(),
        });
        let allocated = FootprintDetails::build(&data, &bundle, board, "Demo", "D6");
        assert_eq!(allocated.number, "ACTUAL-MPN");
        assert_eq!(allocated.value, "Red LED");
        assert_eq!(allocated.tolerance, "1%");
        assert_eq!(allocated.package, "0603");
        assert_eq!(allocated.targets.len(), 1);
        assert!(!allocated.targets[0].purchase);
        assert_eq!(allocated.targets[0].id, "allocated");
        data.purchases.push(Purchase {
            part: Part {
                id: "purchase".into(),
                number: "BUY-MPN".into(),
                value: "Green LED".into(),
                ..Default::default()
            },
            assignments: vec![Assignment {
                id: "p1".into(),
                project: "Demo".into(),
                quantity: 3,
                designators: "D5-D7".into(),
            }],
        });
        let both = FootprintDetails::build(&data, &bundle, board, "Demo", "D6");
        assert_eq!(both.targets.len(), 2);
        assert_eq!(both.number, "ACTUAL-MPN");
        assert!(both.targets[1].purchase);
        assert_eq!(both.targets[1].id, "purchase");
        data.allocations.clear();
        let purchase = FootprintDetails::build(&data, &bundle, board, "Demo", "D6");
        assert_eq!(purchase.number, "BUY-MPN");
        assert_eq!(purchase.value, "Green LED");
        assert_eq!(purchase.targets.len(), 1);
        assert!(purchase.targets[0].purchase);
        data.purchases[0].assignments[0].designators = "D60".into();
        assert!(
            FootprintDetails::build(&data, &bundle, board, "Demo", "D6")
                .targets
                .is_empty()
        );
    }
}
