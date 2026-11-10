//! KiCad project net-class definitions, explicit assignments, and wildcard rules.
use crate::{Result, json};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NetClasses {
    classes: BTreeSet<String>,
    assignments: BTreeMap<String, BTreeSet<String>>,
    patterns: Vec<(String, String)>,
}
impl NetClasses {
    pub fn parse(source: &str) -> Result<Self> {
        let project = json::parse(source)?;
        let settings = project.get("net_settings");
        let mut result = Self::default();
        result.classes.insert("Default".into());
        if let Some(classes) = settings.and_then(|s| s.get("classes")) {
            for class in classes.array() {
                if let Some(name) = class.get("name").and_then(json::Value::string) {
                    result.classes.insert(name.into());
                    if let Some(nets) = class.get("nets") {
                        for net in nets.array().iter().filter_map(json::Value::string) {
                            result
                                .assignments
                                .entry(net.into())
                                .or_default()
                                .insert(name.into());
                        }
                    }
                }
            }
        }
        if let Some(assignments) = settings
            .and_then(|s| s.get("netclass_assignments"))
            .and_then(json::Value::object)
        {
            for (net, value) in assignments {
                let names = value
                    .string()
                    .into_iter()
                    .chain(value.array().iter().filter_map(json::Value::string));
                for name in names {
                    result.classes.insert(name.into());
                    result
                        .assignments
                        .entry(net.into())
                        .or_default()
                        .insert(name.into());
                }
            }
        }
        if let Some(patterns) = settings.and_then(|s| s.get("netclass_patterns")) {
            for pattern in patterns.array() {
                if let (Some(pattern), Some(name)) = (
                    pattern.get("pattern").and_then(json::Value::string),
                    pattern.get("netclass").and_then(json::Value::string),
                ) {
                    result.classes.insert(name.into());
                    result.patterns.push((pattern.into(), name.into()));
                }
            }
        }
        Ok(result)
    }
    pub fn resolve(
        &self,
        nets: impl IntoIterator<Item = String>,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut classes: BTreeMap<_, _> = self
            .classes
            .iter()
            .map(|c| (c.clone(), BTreeSet::new()))
            .collect();
        classes.entry("Default".into()).or_default();
        for net in nets {
            if net.is_empty() {
                continue;
            }
            let mut assigned = self.assignments.get(&net).cloned().unwrap_or_default();
            for (pattern, class) in &self.patterns {
                if crate::pcb::wildcard(pattern, &net) {
                    assigned.insert(class.clone());
                }
            }
            if assigned.is_empty() {
                assigned.insert("Default".into());
            }
            for class in assigned {
                classes.entry(class).or_default().insert(net.clone());
            }
        }
        classes
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_legacy_patterns_composites_and_default_are_resolved() {
        let classes=NetClasses::parse(r#"{"net_settings":{"classes":[{"name":"Default"},{"name":"Power","nets":["+5V"]},{"name":"Empty"}],"netclass_assignments":{"USB_P":["USB","Fast"],"GND":"Power"},"netclass_patterns":[{"netclass":"USB","pattern":"USB_?"},{"netclass":"Power","pattern":"+*"}]}}"#).unwrap();
        let found =
            classes.resolve(["+5V", "+3V3", "GND", "USB_P", "USB_N", "RESET"].map(str::to_owned));
        assert_eq!(
            found["Power"],
            BTreeSet::from(["+5V".into(), "+3V3".into(), "GND".into()])
        );
        assert_eq!(
            found["USB"],
            BTreeSet::from(["USB_P".into(), "USB_N".into()])
        );
        assert!(found["Fast"].contains("USB_P"));
        assert!(found["Empty"].is_empty());
        assert_eq!(found["Default"], BTreeSet::from(["RESET".into()]));
    }
}
