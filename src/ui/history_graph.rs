use crate::action_log::HistoryEntry;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Graph {
    pub rows: Vec<Row>,
    pub lanes: usize,
}

pub(super) struct Row {
    pub entry: usize,
    pub lane: usize,
    pub incoming: bool,
    pub through: Vec<usize>,
    pub parents: Vec<usize>,
    pub conflict: bool,
    pub resolution: bool,
}

impl Graph {
    pub fn build(entries: &[HistoryEntry], pending_conflicts: &[String]) -> Self {
        let mut order: Vec<_> = (0..entries.len()).collect();
        // Receipt order can put a relayed parent above its child. Display causal order.
        order.sort_by_key(|&index| {
            let record = &entries[index].entry.record;
            (
                record.mesh.as_ref().map(|meta| meta.lamport),
                std::cmp::Reverse(index),
            )
        });
        order.reverse();

        let mut by_id = BTreeMap::new();
        let mut by_origin = BTreeMap::new();
        let mut conflict_ids: BTreeSet<&str> =
            pending_conflicts.iter().map(String::as_str).collect();
        for (index, item) in entries.iter().enumerate() {
            let record = &item.entry.record;
            by_id.entry(record.id.as_str()).or_insert(index);
            if let Some(meta) = &record.mesh {
                by_origin
                    .entry((meta.origin.as_str(), meta.sequence))
                    .or_insert(index);
                for resolution in &meta.resolutions {
                    conflict_ids.extend(resolution.parents.iter().map(String::as_str));
                }
            } else if !record.actions.is_empty() {
                // Migrated legacy records use the original event ID as their origin.
                by_origin.entry((record.id.as_str(), 1)).or_insert(index);
            }
        }

        let mut rank = vec![0; entries.len()];
        for (row, &index) in order.iter().enumerate() {
            rank[index] = row;
        }
        let mut lanes = Vec::new();
        let mut graph = Self::default();
        for (row, &index) in order.iter().enumerate() {
            let record = &entries[index].entry.record;
            let mut parents = Vec::new();
            let mut explicit = BTreeSet::new();
            if let Some(meta) = &record.mesh {
                parents.extend(meta.context.iter().filter_map(|(origin, &sequence)| {
                    by_origin.get(&(origin.as_str(), sequence)).copied()
                }));
                explicit.extend(meta.resolutions.iter().flat_map(|resolution| {
                    resolution
                        .parents
                        .iter()
                        .filter_map(|id| by_id.get(id.as_str()).copied())
                }));
                // Keep the frontier of observed events and any explicit conflict edges.
                let observed = parents.clone();
                parents.retain(|&parent| {
                    let candidate = &entries[parent].entry.record;
                    let (origin, sequence) = candidate
                        .mesh
                        .as_ref()
                        .map_or((candidate.id.as_str(), 1), |m| {
                            (m.origin.as_str(), m.sequence)
                        });
                    !observed.iter().any(|&other| {
                        other != parent
                            && entries[other].entry.record.mesh.as_ref().is_some_and(|m| {
                                m.context.get(origin).is_some_and(|&seen| seen >= sequence)
                            })
                    })
                });
                parents.extend(explicit);
            } else if let Some(&older) = order.get(row + 1)
                && entries[older].entry.record.mesh.is_none()
            {
                // Legacy records were replayed in journal order.
                parents.push(older);
            }
            parents.retain(|&parent| rank[parent] > row);
            parents.sort_unstable_by_key(|&parent| rank[parent]);
            parents.dedup();

            let incoming = lanes.iter().position(|target| *target == Some(index));
            let lane = incoming.unwrap_or_else(|| free_lane(&mut lanes));
            lanes[lane] = None;
            let through = lanes
                .iter()
                .enumerate()
                .filter_map(|(lane, target)| target.map(|_| lane))
                .collect();
            let parent_lanes = parents
                .into_iter()
                .map(|parent| {
                    let next = lanes
                        .iter()
                        .position(|target| *target == Some(parent))
                        .unwrap_or_else(|| {
                            if lanes[lane].is_none() {
                                lane
                            } else {
                                free_lane(&mut lanes)
                            }
                        });
                    lanes[next] = Some(parent);
                    next
                })
                .collect();
            graph.rows.push(Row {
                entry: index,
                lane,
                incoming: incoming.is_some(),
                through,
                parents: parent_lanes,
                conflict: conflict_ids.contains(record.id.as_str()),
                resolution: record
                    .mesh
                    .as_ref()
                    .is_some_and(|m| !m.resolutions.is_empty()),
            });
        }
        graph.lanes = lanes.len();
        graph
    }
}

fn free_lane(lanes: &mut Vec<Option<usize>>) -> usize {
    if let Some(index) = lanes.iter().position(Option::is_none) {
        index
    } else {
        lanes.push(None);
        lanes.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_log::LogEntry;
    use crate::merge_review::Resolution;
    use crate::mesh::Meta;

    #[test]
    fn resolution_joins_both_branches_even_when_received_before_a_parent() {
        let entry =
            |id: &str, origin: &str, sequence, lamport, context, resolutions| HistoryEntry {
                hash: String::new(),
                entry: LogEntry {
                    chunk: String::new(),
                    offset: 0,
                    record: crate::action_log::Record {
                        version: 2,
                        id: id.into(),
                        timestamp_ms: 0,
                        initial: false,
                        actions: vec![],
                        proof: None,
                        mesh: Some(Meta {
                            author: None,
                            origin: origin.into(),
                            sequence,
                            lamport,
                            context,
                            resolutions,
                            mutations: vec![],
                            receipts: vec![],
                            parents: vec![],
                        }),
                    },
                },
            };
        let entries = [
            entry(
                "right",
                "B",
                1,
                2,
                BTreeMap::from([("A".into(), 1)]),
                vec![],
            ),
            entry(
                "merge",
                "A",
                3,
                3,
                BTreeMap::from([("A".into(), 2), ("B".into(), 1)]),
                vec![Resolution {
                    conflict: "conflict".into(),
                    entity: "part".into(),
                    key: "part".into(),
                    field: "description".into(),
                    value: "chosen".into(),
                    parents: vec!["left".into(), "right".into()],
                }],
            ),
            entry("left", "A", 2, 2, BTreeMap::from([("A".into(), 1)]), vec![]),
            entry("base", "A", 1, 1, BTreeMap::new(), vec![]),
        ];
        let graph = Graph::build(&entries, &[]);
        let [merge, first, second, base] = graph.rows.as_slice() else {
            panic!("Expected four rows");
        };
        assert_eq!(entries[merge.entry].entry.record.id, "merge");
        assert!(merge.resolution);
        assert_eq!(merge.parents, vec![first.lane, second.lane]);
        assert_ne!(first.lane, second.lane);
        assert!(first.conflict && second.conflict && first.incoming && second.incoming);
        assert_eq!(first.parents, vec![base.lane]);
        assert_eq!(second.parents, vec![base.lane]);
    }
}
