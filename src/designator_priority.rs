//! Project-scoped designator coverage; requested BOM sources remain intact.
use crate::model::*;
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
type Claims = BTreeMap<String, BTreeSet<String>>;
impl Inventory {
    fn allocation_claims(&self) -> Result<Claims> {
        let mut claims = Claims::new();
        for a in &self.allocations {
            claims.entry(key(&a.project)).or_default().extend(
                references(&a.designators)?
                    .into_iter()
                    .take(a.quantity.max(0) as usize),
            );
        }
        Ok(claims)
    }
    /// Newly allocated references take precedence, but purchase edits may not
    /// claim references that were already allocated. Purchase quantities stay intact.
    pub fn reconcile_designator_priority(&mut self, previous: &Self) -> Result<()> {
        let before = previous.allocation_claims()?;
        let after = self.allocation_claims()?;
        for p in &mut self.purchases {
            for a in &mut p.assignments {
                let refs = references(&a.designators)?;
                let Some(allocated) = after.get(&key(&a.project)) else {
                    continue;
                };
                let overlap: BTreeSet<_> = refs.intersection(allocated).cloned().collect();
                if overlap.is_empty() {
                    continue;
                }
                let old = before.get(&key(&a.project));
                let forbidden: Vec<_> = overlap
                    .iter()
                    .filter(|r| old.is_some_and(|s| s.contains(*r)))
                    .cloned()
                    .collect();
                ensure!(
                    forbidden.is_empty(),
                    "Designators already in Allocated for {}: {}. Release them before assigning them to Purchase.",
                    a.project,
                    forbidden.join(", ")
                );
                a.designators = refs
                    .difference(&overlap)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
            }
        }
        Ok(())
    }
    /// Remaining quantities and references, accounting for allocations and purchases.
    /// Unnamed linked reservations retain their shared quantity-budget behaviour.
    pub fn requested_items(&self) -> BTreeMap<String, BomItem> {
        let mut covered = self.allocation_claims().unwrap_or_default();
        for p in &self.purchases {
            for a in &p.assignments {
                covered.entry(key(&a.project)).or_default().extend(
                    references(&a.designators)
                        .unwrap_or_default()
                        .into_iter()
                        .take(a.quantity.max(0) as usize),
                );
            }
        }
        let mut budgets: BTreeMap<_, _> = self
            .allocations
            .iter()
            .map(|a| (a.id.as_str(), a.quantity))
            .collect();
        let allocations: BTreeMap<_, _> = self
            .allocations
            .iter()
            .map(|a| (a.id.as_str(), a))
            .collect();
        let empty = BTreeSet::new();
        self.bom
            .iter()
            .map(|e| {
                let refs = references(&e.item.designators).unwrap_or_default();
                let claimed = covered.get(&key(&e.project)).unwrap_or(&empty);
                let matching = refs.intersection(claimed).count() as i64;
                let linked = e
                    .allocation_id
                    .as_deref()
                    .and_then(|id| budgets.get_mut(id))
                    .map_or(0, |b| {
                        let used = e.item.quantity.min((*b).max(0));
                        *b -= used;
                        used
                    });
                let linked_refs = e
                    .allocation_id
                    .as_deref()
                    .and_then(|id| allocations.get(id))
                    .map(|a| {
                        references(&a.designators)
                            .unwrap_or_default()
                            .into_iter()
                            .take(a.quantity.max(0) as usize)
                            .collect::<BTreeSet<_>>()
                    })
                    .unwrap_or_default();
                let named_linked = refs.intersection(&linked_refs).count() as i64;
                let unnamed_linked = (linked - named_linked).max(0);
                let mut item = e.item.clone();
                item.quantity = (item.quantity - matching - unnamed_linked).max(0);
                item.designators = refs
                    .difference(claimed)
                    .skip(unnamed_linked as usize)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                (e.id.clone(), item)
            })
            .collect()
    }
}
