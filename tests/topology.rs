use vispace::topology::{EXPIRY_MS, Topology};
fn id(n: u8) -> String {
    format!("{n:024x}")
}
fn node(n: u8) -> Topology {
    Topology::new(id(n), format!("Node {n}"), 1, 0)
}
fn gossip(a: &mut Topology, b: &mut Topology, now: u64) {
    let ads = a.advertisements(now);
    b.receive(ads, now).unwrap();
}
#[test]
fn chain_announcements_reconstruct_real_edges_and_retransmissions_expire() {
    let (mut a, mut b, mut c) = (node(1), node(2), node(3));
    a.connect(&id(2), 0).unwrap();
    b.connect(&id(1), 0).unwrap();
    b.connect(&id(3), 0).unwrap();
    c.connect(&id(2), 0).unwrap();
    gossip(&mut c, &mut b, 1);
    gossip(&mut a, &mut b, 1);
    gossip(&mut b, &mut a, 2);
    gossip(&mut b, &mut c, 2);
    let graph = a.graph(2);
    assert_eq!(graph.nodes.len(), 3);
    assert_eq!(graph.edges, vec![[id(1), id(2)], [id(2), id(3)]]);
    assert!(graph.nodes.iter().all(|n| n.reachable));
    assert!(!graph.nodes.iter().find(|n| n.id == id(3)).unwrap().direct);
    let duplicate = b.advertisements(2);
    assert!(!a.receive(duplicate.clone(), 30_000).unwrap());
    assert!(!a.receive(duplicate, 60_000).unwrap());
    assert_eq!(a.graph(EXPIRY_MS + 3).nodes.len(), 2); // only this app and its directly connected peer placeholder
}
#[test]
fn disconnects_remove_edges_without_waiting_for_expiry_and_parallel_sessions_are_counted() {
    let (mut a, mut b) = (node(1), node(2));
    a.connect(&id(2), 0).unwrap();
    a.connect(&id(2), 0).unwrap();
    b.connect(&id(1), 0).unwrap();
    gossip(&mut a, &mut b, 1);
    gossip(&mut b, &mut a, 1);
    assert_eq!(a.graph(1).edges.len(), 1);
    a.disconnect(&id(2), 2);
    gossip(&mut a, &mut b, 2);
    assert_eq!(b.graph(2).edges.len(), 1);
    a.disconnect(&id(2), 3);
    gossip(&mut a, &mut b, 3);
    assert!(b.graph(3).edges.is_empty());
    let mut restarted = Topology::new(id(1), "Restarted".into(), 2, 4);
    restarted.connect(&id(2), 4).unwrap();
    gossip(&mut restarted, &mut b, 4);
    assert_eq!(b.graph(4).edges.len(), 1);
    gossip(&mut a, &mut b, 5);
    assert_eq!(b.graph(5).edges.len(), 1);
}
#[test]
fn cycles_deduplicate_and_unilateral_or_invalid_announcements_do_not_invent_edges() {
    let (mut a, mut b, mut c) = (node(1), node(2), node(3));
    a.connect(&id(2), 0).unwrap();
    gossip(&mut a, &mut b, 1);
    assert!(b.graph(1).edges.is_empty());
    b.connect(&id(1), 1).unwrap();
    b.connect(&id(3), 1).unwrap();
    c.connect(&id(2), 1).unwrap();
    c.connect(&id(1), 1).unwrap();
    a.connect(&id(3), 1).unwrap();
    for _ in 0..8 {
        gossip(&mut a, &mut b, 2);
        gossip(&mut b, &mut c, 2);
        gossip(&mut c, &mut a, 2);
    }
    assert_eq!(a.graph(2).edges.len(), 3);
    assert_eq!(a.graph(2).nodes.len(), 3);
    let mut ads = c.advertisements(2);
    ads[0].neighbors.push("../bad".into());
    assert!(a.receive(ads, 3).is_err());
    assert_eq!(a.graph(3).edges.len(), 3);
}
