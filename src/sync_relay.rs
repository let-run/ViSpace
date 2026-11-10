//! Sync RPC transport over the existing Sync links.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{Bytes, Message};
use uuid::Uuid;

pub(crate) const MAGIC: &[u8; 8] = b"BOMROUTE";
const HOPS: usize = 128;
const TIMEOUT: Duration = Duration::from_secs(4);
type Reply = oneshot::Sender<Result<Bytes>>;

#[derive(Serialize, Deserialize)]
struct Header {
    id: Uuid,
    target: String,
    path: Vec<String>,
    kind: Kind,
}

#[derive(Serialize, Deserialize)]
enum Kind {
    Request(String),
    Reply(Option<String>),
}

#[derive(Default)]
struct Routes {
    links: HashMap<String, Vec<mpsc::Sender<Message>>>,
    seen: HashMap<Uuid, Instant>,
    pending: HashMap<Uuid, Reply>,
}

pub(crate) struct Relay {
    local: String,
    routes: Mutex<Routes>,
}

impl Relay {
    pub(crate) fn new(local: String) -> Self {
        Self {
            local,
            routes: Mutex::new(Routes::default()),
        }
    }

    pub(crate) fn connect(self: &Arc<Self>, peer: &str, tx: mpsc::Sender<Message>) -> Connection {
        self.routes
            .lock()
            .unwrap()
            .links
            .entry(peer.to_owned())
            .or_default()
            .push(tx.clone());
        Connection {
            relay: self.clone(),
            peer: peer.to_owned(),
            tx,
        }
    }

    pub(crate) async fn call(&self, target: &str, method: &str, body: Bytes) -> Result<Bytes> {
        let id = Uuid::new_v4();
        let header = Header {
            id,
            target: target.to_owned(),
            path: vec![self.local.clone()],
            kind: Kind::Request(method.into()),
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut routes = self.routes.lock().unwrap();
            ensure!(routes.pending.len() < 256, "Too many pending Sync requests");
            routes.pending.insert(id, sender);
        }
        let _pending = Pending { relay: self, id };
        self.forward(&header, body)?;
        let timeout = if matches!(method, "component_commit" | "component_propose") {
            Duration::from_secs(25)
        } else {
            TIMEOUT
        };
        tokio::time::timeout(timeout, receiver)
            .await
            .context("Sync route timed out")?
            .context("Sync response dropped")?
    }

    pub(crate) async fn receive<F, Fut>(&self, peer: &str, bytes: Bytes, dispatch: F) -> Result<()>
    where
        F: FnOnce(String, Bytes) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>>>,
    {
        let (mut header, body) = decode(bytes)?;
        match &header.kind {
            Kind::Request(method) => {
                ensure!(
                    header.path.last().map(String::as_str) == Some(peer) && header.path.len() < HOPS,
                    "Invalid Sync request path"
                );
                if header.path.contains(&self.local) {
                    return Ok(());
                }
                {
                    let now = Instant::now();
                    let mut routes = self.routes.lock().unwrap();
                    routes
                        .seen
                        .retain(|_, timestamp| now.duration_since(*timestamp) < TIMEOUT * 2);
                    if routes.seen.contains_key(&header.id) || routes.seen.len() >= 8192 {
                        return Ok(());
                    }
                    routes.seen.insert(header.id, now);
                }
                if self.local != header.target {
                    header.path.push(self.local.clone());
                    if header.path.len() < HOPS {
                        let _ = self.forward(&header, body);
                    }
                    return Ok(());
                }
                let result = dispatch(method.clone(), body).await;
                let payload = match result {
                    Ok(bytes) => {
                        header.kind = Kind::Reply(None);
                        bytes.into()
                    }
                    Err(error) => {
                        header.kind = Kind::Reply(Some(error.to_string()));
                        Bytes::new()
                    }
                };
                self.reply(&header, payload)
            }
            Kind::Reply(error) => {
                ensure!(
                    header.path.pop().as_deref() == Some(self.local.as_str()),
                    "Invalid Sync reply path"
                );
                if header.path.is_empty() {
                    if let Some(sender) = self.routes.lock().unwrap().pending.remove(&header.id) {
                        let _ = sender.send(match error {
                            Some(error) => Err(anyhow::anyhow!("{error}")),
                            None => Ok(body),
                        });
                    }
                    Ok(())
                } else {
                    self.reply(&header, body)
                }
            }
        }
    }

    fn forward(&self, header: &Header, body: Bytes) -> Result<()> {
        let message = encode(header, body)?;
        let routes = self.routes.lock().unwrap();
        if routes
            .links
            .get(&header.target)
            .is_some_and(|links| send(links, &message))
        {
            return Ok(());
        }
        let mut sent = false;
        for (peer, links) in &routes.links {
            if !header.path.contains(peer) {
                sent |= send(links, &message);
            }
        }
        ensure!(sent, "No reachable Sync route");
        Ok(())
    }

    fn reply(&self, header: &Header, body: Bytes) -> Result<()> {
        let peer = header.path.last().context("Missing Sync reply route")?;
        let message = encode(header, body)?;
        if self
            .routes
            .lock()
            .unwrap()
            .links
            .get(peer)
            .is_some_and(|links| send(links, &message))
        {
            Ok(())
        } else {
            bail!("Sync reply route disconnected")
        }
    }
}

fn send(links: &[mpsc::Sender<Message>], message: &Message) -> bool {
    links.iter().any(|tx| tx.try_send(message.clone()).is_ok())
}

fn encode(header: &Header, body: Bytes) -> Result<Message> {
    let metadata = serde_json::to_vec(header)?;
    ensure!(
        metadata.len() + body.len() + 12 <= crate::sync::MESSAGE_LIMIT,
        "Sync frame too large"
    );
    let mut frame = Vec::with_capacity(metadata.len() + body.len() + 12);
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    frame.extend_from_slice(&metadata);
    frame.extend_from_slice(&body);
    Ok(Message::Binary(frame.into()))
}

fn decode(bytes: Bytes) -> Result<(Header, Bytes)> {
    let size = u32::from_le_bytes(
        bytes
            .get(8..12)
            .context("Missing Sync frame length")?
            .try_into()?,
    ) as usize;
    ensure!(
        size <= 8192 && bytes.len() >= 12 + size,
        "Invalid Sync frame length"
    );
    let header: Header = serde_json::from_slice(&bytes[12..12 + size])?;
    ensure!(
        !header.path.is_empty()
            && header.path.len() <= HOPS
            && crate::network_identity::valid_node_id(&header.target)
            && header.path.iter().all(|id| crate::network_identity::valid_node_id(id)),
        "Invalid Sync route"
    );
    Ok((header, bytes.slice(12 + size..)))
}

struct Pending<'a> {
    relay: &'a Relay,
    id: Uuid,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.relay.routes.lock().unwrap().pending.remove(&self.id);
    }
}

pub(crate) struct Connection {
    relay: Arc<Relay>,
    peer: String,
    tx: mpsc::Sender<Message>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        let mut routes = self.relay.routes.lock().unwrap();
        if let Some(links) = routes.links.get_mut(&self.peer) {
            links.retain(|tx| !tx.same_channel(&self.tx));
            if links.is_empty() {
                routes.links.remove(&self.peer);
            }
        }
    }
}
