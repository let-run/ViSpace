//! Ephemeral link-state gossip, separate from inventory history.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const HEARTBEAT_MS: u64 = 20_000;
pub const EXPIRY_MS: u64 = 75_000;
const MAX_NODES: usize = 512;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Advertisement {
    pub id: String,
    pub title: String,
    /// SQLite-backed launch counter makes an earlier process's announcements obsolete.
    pub epoch: u64,
    pub sequence: u64,
    pub neighbors: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub title: String,
    pub local: bool,
    pub direct: bool,
    pub reachable: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Graph {
    pub local: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<[String; 2]>,
    /// Leader confirmed by the write authority, independent of topology gossip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    #[serde(default)]
    pub managed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Seen {
    ad: Advertisement,
    received_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Topology {
    local: String,
    title: String,
    epoch: u64,
    sequence: u64,
    direct: BTreeMap<String, usize>,
    known: BTreeMap<String, Seen>,
    last_heartbeat: u64,
}
fn valid_id(id: &str) -> bool {
    crate::network_identity::valid_node_id(id)
}
impl Topology {
    pub fn new(id: String, title: String, epoch: u64, now: u64) -> Self {
        let mut result = Self {
            local: id,
            title,
            epoch,
            sequence: 0,
            direct: BTreeMap::new(),
            known: BTreeMap::new(),
            last_heartbeat: now,
        };
        result.announce(now);
        result
    }
    fn announce(&mut self, now: u64) {
        self.sequence = self.sequence.saturating_add(1);
        let ad = Advertisement {
            id: self.local.clone(),
            title: self.title.clone(),
            epoch: self.epoch,
            sequence: self.sequence,
            neighbors: self.direct.keys().cloned().collect(),
        };
        self.known.insert(
            self.local.clone(),
            Seen {
                ad,
                received_ms: now,
            },
        );
        self.last_heartbeat = now;
    }
    pub fn connect(&mut self, id: &str, now: u64) -> Result<()> {
        ensure!(valid_id(id) && id != self.local, "Invalid topology peer ID");
        ensure!(
            self.direct.contains_key(id) || self.direct.len() < 64,
            "At most 64 direct peers are supported"
        );
        let count = self.direct.entry(id.into()).or_default();
        *count += 1;
        if *count == 1 {
            self.announce(now);
        }
        Ok(())
    }
    pub fn disconnect(&mut self, id: &str, now: u64) {
        if let Some(count) = self.direct.get_mut(id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.direct.remove(id);
                self.announce(now);
            }
        }
    }
    pub fn heartbeat(&mut self, now: u64) -> bool {
        if now.saturating_sub(self.last_heartbeat) >= HEARTBEAT_MS {
            self.announce(now);
            true
        } else {
            false
        }
    }
    fn prune(&mut self, now: u64) {
        self.known.retain(|id, seen| {
            id == &self.local || now.saturating_sub(seen.received_ms) < EXPIRY_MS
        });
    }
    pub fn receive(&mut self, ads: Vec<Advertisement>, now: u64) -> Result<bool> {
        ensure!(ads.len() <= MAX_NODES, "Topology is too large");
        // Validate the entire batch before changing the map.
        for ad in &ads {
            ensure!(
                valid_id(&ad.id)
                    && ad.title.len() <= 80
                    && !ad.title.chars().any(char::is_control)
                    && ad.epoch > 0
                    && ad.sequence > 0
                    && ad.neighbors.len() <= 64
                    && ad.neighbors.iter().all(|id| valid_id(id) && id != &ad.id),
                "Invalid topology announcement"
            );
            let unique: BTreeSet<_> = ad.neighbors.iter().collect();
            ensure!(
                unique.len() == ad.neighbors.len(),
                "Duplicate topology neighbor"
            );
        }
        self.prune(now);
        let mut changed = false;
        for ad in ads {
            if ad.id == self.local {
                continue;
            }
            if let Some(old) = self.known.get(&ad.id) {
                let version = (ad.epoch, ad.sequence);
                let prior = (old.ad.epoch, old.ad.sequence);
                if version <= prior {
                    ensure!(
                        version != prior || old.ad == ad,
                        "Topology version reused with different contents"
                    );
                    continue;
                }
            }
            if !self.known.contains_key(&ad.id) && self.known.len() >= MAX_NODES {
                continue;
            }
            self.known.insert(
                ad.id.clone(),
                Seen {
                    ad,
                    received_ms: now,
                },
            );
            changed = true;
        }
        Ok(changed)
    }
    pub fn advertisements(&mut self, now: u64) -> Vec<Advertisement> {
        self.prune(now);
        self.known.values().map(|v| v.ad.clone()).collect()
    }
    pub fn graph(&mut self, now: u64) -> Graph {
        self.prune(now);
        let mut edges = vec![];
        for (id, seen) in &self.known {
            for other in &seen.ad.neighbors {
                if id < other
                    && self
                        .known
                        .get(other)
                        .is_some_and(|s| s.ad.neighbors.contains(id))
                {
                    edges.push([id.clone(), other.clone()]);
                }
            }
        }
        let mut reachable = BTreeSet::from([self.local.clone()]);
        loop {
            let n = reachable.len();
            for [a, b] in &edges {
                if reachable.contains(a) {
                    reachable.insert(b.clone());
                }
                if reachable.contains(b) {
                    reachable.insert(a.clone());
                }
            }
            if n == reachable.len() {
                break;
            }
        }
        let mut nodes: Vec<_> = self
            .known
            .values()
            .map(|s| Node {
                id: s.ad.id.clone(),
                title: s.ad.title.clone(),
                local: s.ad.id == self.local,
                direct: self.direct.contains_key(&s.ad.id),
                reachable: reachable.contains(&s.ad.id),
            })
            .collect();
        // A direct peer is visible during the short interval before its updated announcement arrives.
        for id in self.direct.keys() {
            if !nodes.iter().any(|n| &n.id == id) {
                nodes.push(Node {
                    id: id.clone(),
                    title: format!("Node {}", &id[..8]),
                    local: false,
                    direct: true,
                    reachable: true,
                });
            }
        }
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        Graph {
            local: self.local.clone(),
            nodes,
            edges,
            leader: None,
            managed: false,
        }
    }
}
