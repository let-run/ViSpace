//! DNS-SD advertises routing hints. Sync still checks the authenticated identity.
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    time::{Duration, Instant},
};
const SERVICE: &str = "_vispace._tcp.local.";

#[derive(Clone, Debug, Serialize)]
pub struct NearbyNode {
    pub node_id: String,
    pub address: String,
}

pub fn hostname(node_id: &str) -> Option<String> {
    crate::network_identity::valid_node_id(node_id).then(|| format!("{node_id}.vispace.local"))
}

pub struct Discovery {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    nodes: BTreeMap<String, NearbyNode>,
    advertisement: Option<(SocketAddr, String)>,
}
impl Discovery {
    pub fn new() -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(SERVICE)?;
        Ok(Self {
            daemon,
            events,
            nodes: BTreeMap::new(),
            advertisement: None,
        })
    }
    pub fn advertise(&mut self, node_id: &str, address: Option<SocketAddr>) -> Result<()> {
        let address = address.filter(|a| !a.ip().is_loopback());
        if self.advertisement.as_ref().map(|a| a.0) == address {
            return Ok(());
        }
        if let Some((_, name)) = &self.advertisement {
            self.daemon.unregister(name)?;
            self.advertisement = None;
        }
        let Some(address) = address else {
            return Ok(());
        };
        let host = hostname(node_id).ok_or_else(|| anyhow::anyhow!("Invalid sync identity"))?;
        let protocol = crate::sync::SYNC_VERSION.to_string();
        let ips = if address.ip().is_unspecified() {
            String::new()
        } else {
            address.ip().to_string()
        };
        let mut info = ServiceInfo::new(
            SERVICE,
            node_id,
            &format!("{host}."),
            ips,
            address.port(),
            &[("node_id", node_id), ("version", protocol.as_str())][..],
        )?;
        if address.ip().is_unspecified() {
            info = info.enable_addr_auto();
            if address.is_ipv4() {
                info.set_interfaces(vec![mdns_sd::IfKind::IPv4]);
            }
        }
        let name = info.get_fullname().to_owned();
        self.daemon.register(info)?;
        self.advertisement = Some((address, name));
        Ok(())
    }
    fn event(&mut self, event: ServiceEvent) {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let Some(node_id) = info
                    .get_property_val_str("node_id")
                    .filter(|id| crate::network_identity::valid_node_id(id))
                else {
                    return;
                };
                let host = hostname(node_id).unwrap();
                if info.get_port() == 0
                    || !info
                        .get_hostname()
                        .trim_end_matches('.')
                        .eq_ignore_ascii_case(&host)
                    || info
                        .get_property_val_str("version")
                        .and_then(|s| s.parse::<u32>().ok())
                        != Some(crate::sync::SYNC_VERSION)
                {
                    return;
                }
                if self.nodes.len() >= 128 && !self.nodes.contains_key(info.get_fullname()) {
                    return;
                }
                self.nodes.insert(
                    info.get_fullname().into(),
                    NearbyNode {
                        node_id: node_id.to_owned(),
                        address: format!("wss://{host}:{}", info.get_port()),
                    },
                );
            }
            ServiceEvent::ServiceRemoved(_, name) => {
                self.nodes.remove(&name);
            }
            _ => {}
        }
    }
    pub fn nodes(&mut self) -> Vec<NearbyNode> {
        for _ in 0..256 {
            let Ok(event) = self.events.try_recv() else {
                break;
            };
            self.event(event);
        }
        self.nodes.values().cloned().collect()
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        if let Some((_, name)) = &self.advertisement {
            let _ = self.daemon.unregister(name);
        }
        let _ = self.daemon.shutdown();
    }
}

/// Bounded native-messaging request used by Chrome. No inventory or keys are read.
pub fn nearby() -> Result<Vec<NearbyNode>> {
    let mut discovery = Discovery::new()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while let Ok(event) = discovery
        .events
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        discovery.event(event);
        if Instant::now() >= deadline {
            break;
        }
    }
    Ok(discovery.nodes())
}

/// Resolve the service hostname on systems whose regular resolver does not browse mDNS.
pub async fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    struct Resolver(ServiceDaemon);
    impl Drop for Resolver {
        fn drop(&mut self) {
            let _ = self.0.shutdown();
        }
    }
    let resolver = Resolver(ServiceDaemon::new()?);
    let events = resolver
        .0
        .resolve_hostname(&format!("{host}."), Some(3000))?;
    let addresses = tokio::time::timeout(Duration::from_secs(4), async {
        while let Ok(event) = events.recv_async().await {
            if matches!(event, mdns_sd::HostnameResolutionEvent::SearchTimeout(_)) {
                break;
            }
            if let mdns_sd::HostnameResolutionEvent::AddressesFound(_, addresses) = event {
                let mut addresses = addresses
                    .into_iter()
                    .filter(|ip| !ip.is_loopback())
                    .filter_map(|ip| match ip {
                        mdns_sd::ScopedIp::V4(v4) => {
                            Some(SocketAddr::new((*v4.addr()).into(), port))
                        }
                        mdns_sd::ScopedIp::V6(v6) => Some(SocketAddr::V6(
                            std::net::SocketAddrV6::new(*v6.addr(), port, 0, v6.scope_id().index),
                        )),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                addresses.sort_by_key(|ip| ip.is_ipv6());
                if !addresses.is_empty() {
                    return Ok(addresses);
                }
            }
        }
        anyhow::bail!("No mDNS address found for {host}")
    })
    .await??;
    Ok(addresses)
}
