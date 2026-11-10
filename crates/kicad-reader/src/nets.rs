//! Sheet connectivity derived from electrical anchors. Crossing wire interiors
//! are deliberately not joined unless a junction (or endpoint) is present.
use crate::Point;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Net {
    pub name: String,
    pub aliases: Vec<String>,
    pub ports: Vec<String>,
    pub primitives: Vec<usize>,
    pub at: Point,
}
#[derive(Clone, Debug)]
pub(crate) struct Connection {
    pub points: Vec<Point>,
    pub name: Option<String>,
    pub port: Option<String>,
    pub primitives: Vec<usize>,
}
fn coordinate(p: Point) -> (i64, i64) {
    (
        (p.x * 100_000.).round() as i64,
        (p.y * 100_000.).round() as i64,
    )
}
fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}
fn join(parent: &mut [usize], a: usize, b: usize) {
    let a = root(parent, a);
    let b = root(parent, b);
    if a != b {
        parent[b] = a;
    }
}
fn on_segment(p: Point, a: Point, b: Point) -> bool {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let length = dx.hypot(dy);
    length > 1e-8
        && ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() < 1e-5 * length
        && p.x >= a.x.min(b.x) - 1e-5
        && p.x <= a.x.max(b.x) + 1e-5
        && p.y >= a.y.min(b.y) - 1e-5
        && p.y <= a.y.max(b.y) + 1e-5
}
pub(crate) fn build(connections: &[Connection]) -> Vec<Net> {
    let mut parent: Vec<_> = (0..connections.len()).collect();
    let mut anchors = BTreeMap::new();
    let mut names = BTreeMap::new();
    for (i, connection) in connections.iter().enumerate() {
        for &point in &connection.points {
            if let Some(&previous) = anchors.get(&coordinate(point)) {
                join(&mut parent, previous, i);
            } else {
                anchors.insert(coordinate(point), i);
            }
        }
        if let Some(name) = connection.name.as_ref().filter(|n| !n.is_empty()) {
            if let Some(&previous) = names.get(name) {
                join(&mut parent, previous, i);
            } else {
                names.insert(name.clone(), i);
            }
        }
    }
    // A T junction joins at a wire's interior; an unmarked X crossing does not.
    for (i, connection) in connections.iter().enumerate() {
        for segment in connection.points.windows(2) {
            let min = coordinate(Point::new(segment[0].x.min(segment[1].x) - 0.00001, -1e9));
            let max = coordinate(Point::new(segment[0].x.max(segment[1].x) + 0.00001, 1e9));
            for (&(x, y), &other) in anchors.range(min..=max) {
                if on_segment(
                    Point::new(x as f64 / 100_000., y as f64 / 100_000.),
                    segment[0],
                    segment[1],
                ) {
                    join(&mut parent, i, other);
                }
            }
        }
    }
    type SheetGroup = (BTreeSet<String>, BTreeSet<usize>, Point, BTreeSet<String>);
    let mut groups: BTreeMap<usize, SheetGroup> = BTreeMap::new();
    for (i, connection) in connections.iter().enumerate() {
        let Some(&at) = connection.points.first() else {
            continue;
        };
        let group = groups
            .entry(root(&mut parent, i))
            .or_insert_with(|| (BTreeSet::new(), BTreeSet::new(), at, BTreeSet::new()));
        if let Some(name) = connection.name.as_ref().filter(|s| !s.is_empty()) {
            group.0.insert(name.clone());
        }
        group.1.extend(&connection.primitives);
        if let Some(port) = &connection.port {
            group.3.insert(port.clone());
        }
    }
    let mut nets: Vec<_> = groups
        .into_values()
        .enumerate()
        .map(|(i, (names, primitives, at, ports))| Net {
            name: names
                .first()
                .cloned()
                .unwrap_or_else(|| format!("Net-{}", i + 1)),
            aliases: names.into_iter().collect(),
            ports: ports.into_iter().collect(),
            primitives: primitives.into_iter().collect(),
            at,
        })
        .collect();
    nets.sort_by(|a, b| a.name.cmp(&b.name));
    nets
}

#[cfg(test)]
mod tests {
    use super::*;
    fn connection(points: &[(f64, f64)], name: Option<&str>, primitive: usize) -> Connection {
        Connection {
            points: points.iter().map(|&(x, y)| Point::new(x, y)).collect(),
            name: name.map(str::to_owned),
            port: None,
            primitives: vec![primitive],
        }
    }
    #[test]
    fn crossings_need_junctions_but_t_connections_and_named_islands_join() {
        let mut c = vec![
            connection(&[(0., 0.), (10., 0.)], None, 0),
            connection(&[(5., -5.), (5., 5.)], None, 1),
            connection(&[(2., 0.), (2., 3.)], None, 2),
            connection(&[(0., 0.)], Some("+3V3"), 3),
            connection(&[(20., 20.)], Some("+3V3"), 4),
        ];
        let nets = build(&c);
        assert_eq!(nets.len(), 2);
        assert_eq!(
            nets.iter().find(|n| n.name == "+3V3").unwrap().primitives,
            vec![0, 2, 3, 4]
        );
        c.push(connection(&[(5., 0.)], None, 5));
        assert_eq!(build(&c).len(), 1);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetMember {
    pub instance: String,
    pub primitives: Vec<usize>,
    pub at: Point,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectNet {
    pub name: String,
    pub members: Vec<NetMember>,
}
/// Resolve local, global/power, and hierarchical sheet ports per sheet instance.
/// Repeated instances of a sheet retain independent local nets.
pub(crate) fn project_nets(bundle: &crate::Bundle) -> Vec<ProjectNet> {
    let nodes = bundle.nodes();
    let mut sheet_paths = BTreeMap::new();
    for node in &nodes {
        let parent = node
            .instance_path
            .rsplit_once('/')
            .map(|(p, _)| p)
            .unwrap_or("");
        let prefix = sheet_paths.get(parent).map(String::as_str).unwrap_or("");
        let path = if node.instance_path == bundle.root.instance_path {
            String::new()
        } else {
            format!("{prefix}/{}", node.name)
        };
        sheet_paths.insert(node.instance_path.clone(), path);
    }
    let entries: Vec<_> = nodes
        .iter()
        .flat_map(|node| {
            bundle
                .schematics
                .get(&node.file)
                .into_iter()
                .flat_map(move |scene| scene.nets.iter().map(move |net| (*node, net)))
        })
        .collect();
    let mut parent: Vec<_> = (0..entries.len()).collect();
    let mut ports = BTreeMap::new();
    for (i, (node, net)) in entries.iter().enumerate() {
        for port in &net.ports {
            let key = if let Some(global) = port.strip_prefix("g:") {
                format!("g:{global}")
            } else if let Some(name) = port.strip_prefix("h:") {
                format!("{}:{name}", node.instance_path)
            } else if let Some(sheet) = port.strip_prefix("s:") {
                let (uuid, name) = sheet.split_once(':').unwrap_or((sheet, ""));
                format!("{}/{}:{name}", node.instance_path, uuid)
            } else {
                continue;
            };
            if let Some(&previous) = ports.get(&key) {
                join(&mut parent, previous, i);
            } else {
                ports.insert(key, i);
            }
        }
    }
    type ProjectGroup = (BTreeSet<(u8, String)>, Vec<NetMember>);
    let mut groups: BTreeMap<usize, ProjectGroup> = BTreeMap::new();
    for (i, (node, net)) in entries.iter().enumerate() {
        let group = groups.entry(root(&mut parent, i)).or_default();
        for port in &net.ports {
            if let Some(name) = port.strip_prefix("g:") {
                group.0.insert((0, name.into()));
            }
        }
        let named = !net.aliases.is_empty();
        let path = &sheet_paths[&node.instance_path];
        let name = if path.is_empty() {
            net.name.clone()
        } else {
            format!("{path}/{}", net.name)
        };
        group.0.insert((
            if !named {
                3
            } else if node.instance_path == bundle.root.instance_path {
                1
            } else {
                2
            },
            name,
        ));
        group.1.push(NetMember {
            instance: node.instance_path.clone(),
            primitives: net.primitives.clone(),
            at: net.at,
        });
    }
    let mut result: Vec<_> = groups
        .into_values()
        .filter_map(|(names, members)| {
            Some(ProjectNet {
                name: names.first()?.1.clone(),
                members,
            })
        })
        .collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

/// Name-based differential-pair grouping for navigation; electrical connectivity
/// and the original names remain unchanged. A positive member is required.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetNameGroup {
    Single(String),
    Pair { name: String, nets: Vec<String> },
}
pub fn group_net_names(names: impl IntoIterator<Item = String>) -> Vec<NetNameGroup> {
    let names: BTreeSet<String> = names.into_iter().collect();
    let mut pairs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut paired = BTreeSet::new();
    for positive in &names {
        let Some(base) = positive.strip_suffix("_P").filter(|b| !b.is_empty()) else {
            continue;
        };
        let mut members: Vec<_> = [format!("{base}_N"), format!("{base}_M")]
            .into_iter()
            .filter(|n| names.contains(n))
            .collect();
        if members.is_empty() {
            continue;
        }
        members.push(positive.clone());
        members.sort();
        paired.extend(members.iter().cloned());
        pairs.insert(base.into(), members);
    }
    let mut result: Vec<_> = names
        .into_iter()
        .filter(|n| !paired.contains(n))
        .map(NetNameGroup::Single)
        .chain(
            pairs
                .into_iter()
                .map(|(name, nets)| NetNameGroup::Pair { name, nets }),
        )
        .collect();
    let label = |g: &NetNameGroup| match g {
        NetNameGroup::Single(n) => n.clone(),
        NetNameGroup::Pair { name, .. } => name.clone(),
    };
    result.sort_by_key(label);
    result
}
#[cfg(test)]
mod grouping_tests {
    use super::*;
    #[test]
    fn groups_only_matching_complementary_suffixes_and_preserves_sheet_paths() {
        let groups = group_net_names(
            [
                "USB_N",
                "USB_P",
                "USB1_N",
                "USB1_P",
                "USB_WIFI_M",
                "USB_WIFI_P",
                "RESET",
                "LONE_N",
                "/A/DATA_N",
                "/B/DATA_P",
                "USB_N",
            ]
            .map(str::to_owned),
        );
        assert!(groups.contains(&NetNameGroup::Pair {
            name: "USB".into(),
            nets: vec!["USB_N".into(), "USB_P".into()]
        }));
        assert!(groups.contains(&NetNameGroup::Pair {
            name: "USB_WIFI".into(),
            nets: vec!["USB_WIFI_M".into(), "USB_WIFI_P".into()]
        }));
        for name in ["RESET", "LONE_N", "/A/DATA_N", "/B/DATA_P"] {
            assert!(groups.contains(&NetNameGroup::Single(name.into())));
        }
        assert_eq!(
            groups
                .iter()
                .map(|g| match g {
                    NetNameGroup::Single(_) => 1,
                    NetNameGroup::Pair { nets, .. } => nets.len(),
                })
                .sum::<usize>(),
            10
        );
    }
}
