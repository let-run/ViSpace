//! Symmetric, multi-hop WebSocket event gossip with file-before-event delivery.
use crate::{
    action_log::{self, Record},
    mesh::Clock,
    store::{SharedStore, Store},
    sync_blob::{self, Blob},
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};
pub const SYNC_VERSION: u32 = 9;
pub(crate) const MESSAGE_LIMIT: usize = sync_blob::MAX_BLOB_SIZE + 16384;
const CONTROL_LIMIT: usize = 64 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transfer {
    pub kind: TransferKind,
    pub record: Record,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    Event,
    Proposal,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Hello {
        #[serde(default)]
        head: Option<crate::merge_review::Head>,
        #[serde(default)]
        topology: Vec<crate::topology::Advertisement>,
        version: u32,
        application_id: String,
        clock: Clock,
        #[serde(default)]
        cluster: Option<uuid::Uuid>,
        #[serde(default)]
        browser: bool,
        #[serde(default)]
        component: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_bundle_grant: Option<uuid::Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_bundle_port: Option<u16>,
        #[serde(default,skip_serializing_if="Option::is_none")]
        git_bundle_host:Option<String>,
        #[serde(default)]
        git_bundle_notifications: bool,
    },
    Membership { actions: Vec<crate::network_identity::Membership> },
    GitBundleReady {
        #[serde(default,skip_serializing_if="Option::is_none")]
        git_bundle_grant:Option<uuid::Uuid>,
    },
    Topology {
        advertisements: Vec<crate::topology::Advertisement>,
    },
    Ready {
        version: u32,
        clock: Clock,
    },
    Ack {
        id: String,
        applied: bool,
        clock: Clock,
    },
    Authority {
        leader: Option<String>,
    },
    SaveResult {
        id: String,
        outcome: crate::component_sync::SaveResult,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record: Option<Record>,
    },
    Error {
        message: String,
    },
}
pub(crate) fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MESSAGE_LIMIT))
        .max_frame_size(Some(MESSAGE_LIMIT))
}
type WeakStore = Weak<Mutex<Store>>;

async fn access<T: Send + 'static>(
    store: &WeakStore,
    op: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
) -> Result<T> {
    let store = store.upgrade().context("Inventory closed")?;
    crate::async_io::blocking(move || {
        let mut locked = store
            .lock()
            .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?;
        op(&mut locked)
    })
    .await
}
async fn enqueue(tx: &mpsc::Sender<Message>, message: Message) -> Result<()> {
    tokio::time::timeout(IO_TIMEOUT, tx.send(message))
        .await
        .context("Sync send queue timed out")?
        .map_err(|_| anyhow::anyhow!("Sync connection closed"))
}
async fn control(tx: &mpsc::Sender<Message>, value: Control) -> Result<()> {
    enqueue(tx, Message::Text(serde_json::to_string(&value)?.into())).await
}
async fn assets(
    tx: &mpsc::Sender<Message>,
    store: &WeakStore,
    record: &Record,
    sent: &mut BTreeSet<String>,
    repair: bool,
) -> Result<()> {
    let root = access(store, |s| Ok(s.attachment_root())).await?;
    for asset in action_log::referenced_assets(record)? {
        if sent.contains(&asset) {
            continue;
        }
        let root = root.clone();
        let id = asset.clone();
        let bytes = crate::async_io::blocking(move || match Blob::read(&root, &id) {
            Ok(blob) => Ok(Some(blob.encode()?)),
            Err(error)
                if repair
                    && error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        })
        .await?;
        let Some(bytes) = bytes else {
            continue;
        };
        enqueue(tx, Message::Binary(bytes.into())).await?;
        sent.insert(asset);
    }
    Ok(())
}
async fn receive_blob(store: &WeakStore, bytes: Vec<u8>) -> Result<()> {
    let root = access(store, |s| Ok(s.attachment_root())).await?;
    crate::async_io::blocking(move || Blob::decode(&bytes)?.save(&root)).await
}
async fn peer<S: AsyncRead + AsyncWrite + Unpin + Send>(
    mut ws: WebSocketStream<S>,
    store: WeakStore,
    status: Option<Arc<Mutex<ClientStatus>>>,
    pull: Option<Arc<PullGuard>>,
    bundle_source: Option<url::Url>,
    incoming_ip: Option<std::net::IpAddr>,
    expected: Option<String>,
    approve_remote: bool,
    pairing: Option<crate::network_auth::CodePairing>,
) -> Result<()> {
    let authenticated = crate::network_auth::authenticate(&mut ws, store.clone(), expected.as_deref(), approve_remote, pairing).await?;
    let git_bundle_port = crate::git_bundle::Host::port(&store);
    let git_bundle_host=crate::git_bundle::Host::hostname(&store);
    let git_grant = git_bundle_port.map(|_| crate::git_bundle::Grant::new());
    let git_bundle_grant = git_grant.as_ref().map(|grant| grant.0);
    let (hello, changes) = access(&store, move |s| {
        Ok((
            Control::Hello {
                version: SYNC_VERSION,
                application_id: s.application_id().to_owned(),
                head: Some(if s.uses_component() {
                    s.merge_head()
                } else {
                    s.shared_head()
                }),
                topology: s.topology_ads(),
                clock: s.sync_clock(),
                cluster: None,
                browser: false,
                component: s.uses_component(),
                git_bundle_grant,
                git_bundle_port,
                git_bundle_host,
                git_bundle_notifications: true,
            },
            s.subscribe_changes(),
        ))
    })
    .await?;
    let Control::Hello {
        application_id: id,
        component,
        clock: local_clock,
        head: local_head,
        ..
    } = &hello
    else {
        unreachable!()
    };
    let (id, component, local_clock, local_head) = (
        id.clone(),
        *component,
        local_clock.clone(),
        local_head.clone(),
    );
    let (mut sink, mut stream) = ws.split();
    tokio::time::timeout(
        IO_TIMEOUT,
        sink.send(Message::Text(serde_json::to_string(&hello)?.into())),
    )
    .await
    .context("Sync handshake send timed out")??;
    let (tx, mut rx) = mpsc::channel::<Message>(4);
    let writer = async {
        while let Some(message) = rx.recv().await {
            tokio::time::timeout(IO_TIMEOUT, sink.send(message))
                .await
                .context("Sync send timed out")??;
        }
        Ok::<(), anyhow::Error>(())
    };
    let receiver = async {
        let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await?
            .context("Missing peer handshake")??;
        let Message::Text(text) = first else {
            bail!("Expected peer hello");
        };
        ensure!(text.len() <= CONTROL_LIMIT, "Handshake too large");
        let Control::Hello {
            version,
            application_id: peer_id,
            clock,
            topology,
            cluster,
            browser,
            component: remote_component,
            git_bundle_grant: remote_bundle_grant,
            git_bundle_port: remote_bundle_port,
            git_bundle_host:remote_bundle_host,
            git_bundle_notifications,
            head,
            ..
        } = serde_json::from_str(&text)?
        else {
            bail!("Expected peer hello");
        };
        ensure!(peer_id == authenticated, "Hello identity differs from authenticated peer");
        ensure!(cluster.is_none(), "{RAFT_REMOVED}");
        ensure!(
            version == SYNC_VERSION,
            "Sync requires protocol version 9 on both apps"
        );
        ensure!(
            component == remote_component
                || (component && browser && cluster.is_none() && clock.is_empty()),
            "Enable the same sync mode on both desktops"
        );
        if component {
            let covers = |a: &Clock, b: &Clock| {
                b.iter()
                    .all(|(author, n)| a.get(author).copied().unwrap_or(0) >= *n)
            };
            let same = clock == local_clock;
            ensure!(
                if same {
                    head.as_ref()
                        .zip(local_head.as_ref())
                        .is_some_and(|(a, b)| a.hash == b.hash)
                } else {
                    covers(&local_clock, &clock) || (!browser && covers(&clock, &local_clock))
                },
                "Branches have diverged. Choose Reconnect branches in Sync."
            );
        }
        ensure!(peer_id != id, "Peer has the same application ID");
        if let Some(grant) = &git_grant {
            grant.activate(store.clone(),peer_id.clone());
        }
        let component_relay = if component && !browser {
            access(&store, |s| Ok(s.component_relay())).await?
        } else {
            None
        };
        let _component_route = component_relay
            .as_ref()
            .map(|relay| relay.connect(&peer_id, tx.clone()));
        let relay_tasks = Arc::new(tokio::sync::Semaphore::new(32));
        let peer_for_db = peer_id.clone();
        access(&store, move |s| {
            s.note_peer(&peer_for_db)?;
            s.topology_receive(topology)?;
            s.topology_connect(&peer_for_db)
        })
        .await?;
        let _topology = TopologyGuard {
            store: store.clone(),
            peer: peer_id.clone(),
        };
        if let Some(status) = status.as_ref() {
            *status.lock().unwrap() = ClientStatus::Connected {
                peer_id: peer_id.clone(),
            };
        }
        let (remote_tx, remote_rx) = watch::channel(clock);
        let (ready_tx, ready_rx) = watch::channel(pull.is_none());
        let bundle_wake = Arc::new(tokio::sync::Notify::new());
        let producer = produce(&tx, &store, remote_rx, changes, ready_rx, component, git_bundle_notifications, git_bundle_grant, peer_id.clone());
        let reader = async {
            loop {
                let message = tokio::time::timeout(
                    Duration::from_secs(if component { 12 } else { 60 }),
                    stream.next(),
                )
                .await
                .context("Sync peer stopped responding")?
                .context("Sync peer disconnected")??;
                let authorized=peer_id.clone();
                access(&store,move|s|{ensure!(s.network_trusted(&authorized),"Peer authorization revoked");Ok(())}).await?;
                match message {
                    Message::Binary(bytes) if bytes.starts_with(crate::sync_relay::MAGIC) => {
                        if let Ok(permit) = relay_tasks.clone().try_acquire_owned() {
                            let relay = component_relay
                                .as_ref()
                                .context("Unexpected routed frame")?
                                .clone();
                            let store = store.clone();
                            let peer_id = peer_id.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                let _ = relay
                                    .receive(&peer_id, bytes, |method, body| {
                                        crate::component_sync::request(store, method, body)
                                    })
                                    .await;
                            });
                        }
                    }
                    Message::Binary(bytes) if bytes.starts_with(sync_blob::MAGIC) => {
                        receive_blob(&store, bytes.to_vec()).await?
                    }
                    Message::Binary(bytes) => {
                        let transfer: Transfer =
                            crate::async_io::blocking(move || action_log::decode_frame(&bytes))
                                .await?;
                        let verified=transfer.record.clone();
                        access(&store,move |s|s.network_verify_record(&verified)).await?;
                        let event_id = transfer.record.id.clone();
                        if component && browser {
                            ensure!(
                                transfer.kind == TransferKind::Proposal
                                    && transfer
                                        .record
                                        .mesh
                                        .as_ref()
                                        .is_some_and(|meta| meta.author() == peer_id),
                                "Browser clients send proposals for their own edits"
                            );
                            let approval = crate::component_sync::submit(&store, &transfer.record)
                                .await
                                .unwrap_or(crate::component_sync::Approval {
                                    result: crate::component_sync::SaveResult::Unavailable,
                                    record: None,
                                });
                            control(
                                &tx,
                                Control::SaveResult {
                                    id: event_id,
                                    outcome: approval.result,
                                    record: approval.record,
                                },
                            )
                            .await?;
                            continue;
                        }
                        ensure!(
                            transfer.kind == TransferKind::Event,
                            "This peer has no write authority"
                        );
                        let p = peer_id.clone();
                        let (applied, clock) = access(&store, move |s| {
                            ensure!(
                                s.uses_component() == component,
                                "Inventory changed sync mode. Reconnect this peer."
                            );
                            sync_blob::require_assets(&s.attachment_root(), &transfer.record)?;
                            let applied = s.replay_record(&transfer.record, None)?;
                            s.note_peer(&p)?;
                            Ok((applied, s.sync_clock()))
                        })
                        .await?;
                        control(
                            &tx,
                            Control::Ack {
                                id: event_id,
                                applied,
                                clock,
                            },
                        )
                        .await?;
                    }
                    Message::Text(text) => {
                        ensure!(text.len() <= CONTROL_LIMIT, "Control message too large");
                        match serde_json::from_str(&text)? {
                            Control::Membership { actions } => { access(&store,move|s|s.network_merge(actions)).await?; }
                            Control::Topology { advertisements } => {
                                if component {
                                    if let Ok(permit) = relay_tasks.clone().try_acquire_owned() {
                                        let store = store.clone();
                                        tokio::spawn(async move {
                                            let _permit = permit;
                                            let _ = access(&store, move |s| {
                                                s.topology_receive(advertisements)
                                            })
                                            .await;
                                        });
                                    }
                                } else {
                                    access(&store, move |s| s.topology_receive(advertisements))
                                        .await?;
                                }
                            }
                            Control::GitBundleReady { .. } => { bundle_wake.notify_one(); }
                            Control::Ack { clock, .. } => {
                                remote_tx.send_replace(clock);
                            }
                            Control::Ready { clock, .. } => {
                                if let Some(pull) = pull.as_ref() {
                                    pull.finish();
                                }
                                ready_tx.send_replace(true);
                                remote_tx.send_replace(clock);
                            }
                            Control::Authority { .. } if component => {}
                            Control::Error { message } => bail!("Peer: {message}"),
                            _ => bail!("Unexpected peer control message"),
                        }
                    }
                    Message::Ping(bytes) => enqueue(&tx, Message::Pong(bytes)).await?,
                    Message::Pong(_) => {}
                    Message::Close(_) => return Ok(()),
                    _ => bail!("Unexpected WebSocket message"),
                }
            }
        };
        let bundles = async {
            let source = bundle_source.or_else(|| {
                let port = remote_bundle_port.filter(|port| *port != 0)?;
                if let Some(host)=remote_bundle_host.filter(|host|crate::discovery::hostname(&peer_id).as_ref()==Some(host)){
                    return url::Url::parse(&format!("wss://{host}:{port}")).ok();
                }
                let address = SocketAddr::new(incoming_ip?, port);
                url::Url::parse(&format!("wss://{address}")).ok()
            });
            if let (Some(url), Some(grant)) = (source, remote_bundle_grant) {
                crate::git_bundle::follow(store.clone(), url, grant, bundle_wake.clone(), peer_id.clone()).await;
            }
            std::future::pending::<Result<()>>().await
        };
        tokio::select! {result=producer=>result,result=reader=>result,result=bundles=>result}
    };
    let result = tokio::select! {result=writer=>result,result=receiver=>result};
    if let Err(error) = &result {
        // Share actionable protocol rejections without exposing local storage errors.
        let detail = error.to_string();
        let message = match detail.as_str() {
            RAFT_REMOVED
            | "Sync requires protocol version 9 on both apps"
            | "Enable the same sync mode on both desktops"
            | "Branches have diverged. Choose Reconnect branches in Sync."
            | "Peer authorization revoked" => detail,
            _ => "Desktop sync failed. Check the desktop Sync log for details.".into(),
        };
        let frame = Message::Text(serde_json::to_string(&Control::Error { message })?.into());
        let _ = tokio::time::timeout(IO_TIMEOUT, sink.send(frame)).await;
        let _ = tokio::time::timeout(IO_TIMEOUT, sink.close()).await;
    }
    result
}
async fn produce(
    tx: &mpsc::Sender<Message>,
    store: &WeakStore,
    remote: watch::Receiver<Clock>,
    mut changes: watch::Receiver<u64>,
    mut ready: watch::Receiver<bool>,
    component: bool,
    bundle_notifications: bool,
    git_bundle_grant:Option<uuid::Uuid>,
    peer_id:String,
) -> Result<()> {
    let mut bundle_ready = crate::git_bundle::subscribe_ready();
    let mut sent_topology = std::collections::BTreeMap::new();
    let mut sent = BTreeSet::new();
    let mut sent_assets = BTreeSet::new();
    let mut initial = true;
    let mut prior_authority = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(if component { 3 } else { 20 }));
    heartbeat.tick().await;
    loop {
        let remote_id=peer_id.clone();
        let (actions,trusted)=access(store,move|s|Ok((s.network_actions(),s.network_trusted(&remote_id)))).await?;
        control(tx,Control::Membership{actions}).await?;
        ensure!(trusted,"Peer authorization revoked");
        if component {
            let current = access(store, |s| Ok(s.sync_leader())).await?;
            if prior_authority.as_ref() != Some(&current) {
                control(
                    tx,
                    Control::Authority {
                        leader: current.clone(),
                    },
                )
                .await?;
                prior_authority = Some(current);
            }
        }
        let ads = access(store, move |s| {
            ensure!(
                s.uses_component() == component,
                "Inventory authority changed. Reconnect this peer."
            );
            Ok(s.topology_ads())
        })
        .await?;
        let updates: Vec<_> = ads
            .into_iter()
            .filter(|ad| sent_topology.get(&ad.id) != Some(&(ad.epoch, ad.sequence)))
            .collect();
        if !updates.is_empty() {
            for ad in &updates {
                sent_topology.insert(ad.id.clone(), (ad.epoch, ad.sequence));
            }
            control(
                tx,
                Control::Topology {
                    advertisements: updates,
                },
            )
            .await?;
        }
        // Connection discovery stays live while the initial inventory pull is in progress.
        let publish_ready = *ready.borrow();
        if publish_ready {
            let clock = remote.borrow().clone();
            let records = access(store, move |s| s.events_after(&clock)).await?;
            for record in records {
                if sent.contains(&record.id) {
                    continue;
                }
                assets(tx, store, &record, &mut sent_assets, false).await?;
                let id = record.id.clone();
                let transfer = Transfer {
                    kind: TransferKind::Event,
                    record,
                };
                let bytes =
                    crate::async_io::blocking(move || action_log::encode_frame(&transfer)).await?;
                enqueue(tx, Message::Binary(bytes.into())).await?;
                sent.insert(id);
            }
            if initial {
                let current = access(store, |s| {
                    action_log::diff(
                        &crate::model::Inventory::default(),
                        &s.data,
                        &s.attachment_root(),
                        true,
                    )
                })
                .await?;
                assets(tx, store, &current, &mut sent_assets, true).await?;
                let clock = access(store, |s| Ok(s.sync_clock())).await?;
                control(
                    tx,
                    Control::Ready {
                        version: SYNC_VERSION,
                        clock,
                    },
                )
                .await?;
                initial = false;
            }
        }
        tokio::select! {notice=bundle_ready.recv()=>{if bundle_notifications && notice.is_ok_and(|id| id == store.as_ptr() as usize) {control(tx, Control::GitBundleReady {git_bundle_grant}).await?;}},changed=changes.changed()=>{changed?;if bundle_notifications {control(tx, Control::GitBundleReady {git_bundle_grant}).await?;}},changed=ready.changed()=>{changed?;},_=heartbeat.tick()=>{prior_authority=None;if bundle_notifications && git_bundle_grant.is_some(){control(tx,Control::GitBundleReady{git_bundle_grant}).await?;}enqueue(tx,Message::Ping(Vec::new().into())).await?;}}
    }
}

struct TopologyGuard {
    store: WeakStore,
    peer: String,
}
impl Drop for TopologyGuard {
    fn drop(&mut self) {
        let Some(store) = self.store.upgrade() else {
            return;
        };
        if let Ok(mut s) = store.try_lock() {
            s.topology_disconnect(&self.peer);
        } else if let Ok(runtime) = crate::async_io::runtime() {
            // Cancellation must not wait for a UI operation holding the inventory lock.
            let store = self.store.clone();
            let peer = self.peer.clone();
            runtime.spawn_blocking(move || {
                if let Some(store) = store.upgrade()
                    && let Ok(mut s) = store.lock()
                {
                    s.topology_disconnect(&peer);
                }
            });
        }
    }
}

pub struct SyncServer {
    pub address: SocketAddr,
    pub discovery_hostname:Option<String>,
    listener: tokio::task::JoinHandle<()>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}
pub(crate) fn bind_listener(address: SocketAddr) -> Result<tokio::net::TcpListener> {
    let socket = if address.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    #[cfg(unix)]
    socket.set_reuseaddr(true)?;
    socket.bind(address)?;
    Ok(socket.listen(128)?)
}
impl SyncServer {
    pub fn start(store: SharedStore, address: SocketAddr) -> Result<Self> {
        let runtime = crate::async_io::runtime()?;
        let _entered = runtime.enter();
        let (tls, discovery_hostname) = {
            let locked = store.lock().unwrap();
            let hostname = crate::discovery::hostname(locked.application_id());
            let (tls, covers_hostname) = crate::network_tls::listener(&locked, hostname.as_deref(), address.ip())?;
            (tls, hostname.filter(|_| !address.ip().is_loopback() && covers_hostname))
        };
        let listener = bind_listener(address)?;
        let address = listener.local_addr()?;
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let bundle_host = crate::git_bundle::Host::register(&store, address.port(),discovery_hostname.clone());
        let task = runtime.spawn(async move {
            let _bundle_host = bundle_host;
            let permits = Arc::new(tokio::sync::Semaphore::new(32));
            let mut connections = tokio::task::JoinSet::new();
            connections.spawn(crate::git_bundle::watch(store.clone()));
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    _ = connections.join_next(), if !connections.is_empty() => {},
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else {break;};
                        let stream_ip=stream.peer_addr().ok().map(|a|a.ip());
                        let Ok(permit) = permits.clone().try_acquire_owned() else {continue;};
                        let store = store.clone();
                        let tls=tls.clone();
                        connections.spawn(async move {
                            let _permit = permit;
                            if let Ok(Ok(secure))=tokio::time::timeout(Duration::from_secs(10),tls.accept(stream)).await {
                                if let Err(error)=serve_transport(secure, Arc::downgrade(&store),stream_ip).await {eprintln!("Secure Sync connection: {error}");}
                            }
                        });
                    }
                }
            }
            drop(listener);
            connections.shutdown().await;
        });
        Ok(Self {
            address,
            discovery_hostname,
            listener: task,
            stop: Some(stop),
        })
    }
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.listener).await;
    }
}
impl Drop for SyncServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn serve_transport(stream:tokio_rustls::server::TlsStream<tokio::net::TcpStream>,store:WeakStore,incoming:Option<std::net::IpAddr>)->Result<()> {
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    let mut merge_route = false;
    let mut bundle_route = false;
    let mut extension_route = false;
    let mut pairing_route = false;
    let binding = stream.get_ref().1.export_keying_material([0; 32], crate::network_tls::PAIRING_EXPORTER, None)?;
    let incoming_ip = incoming.context("Missing peer address")?;
    let loopback = incoming_ip.is_loopback();
    let callback =
        |request: &Request, response: Response| -> std::result::Result<Response, ErrorResponse> {
            match request.uri().path() {
                "/raft" | "/raft/" => {
                    return Err(Response::builder()
                        .status(410)
                        .body(Some(RAFT_REMOVED.into()))
                        .unwrap());
                }
                "/extension" if store.strong_count() > 0 => {
                    let valid_origin = request
                        .headers()
                        .get("origin")
                        .and_then(|origin| origin.to_str().ok())
                        .and_then(|origin| origin.strip_prefix("chrome-extension://"))
                        .is_some_and(|id| {
                            id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
                        });
                    if !loopback || !valid_origin {
                        return Err(Response::builder()
                            .status(403)
                            .body(Some("Local Chrome extension connection required".into()))
                            .unwrap());
                    }
                    extension_route = true;
                }
                "/git-bundle" if store.strong_count() > 0 => bundle_route = true,
                "/merge" if store.strong_count() > 0 => merge_route = true,
                "/sync" if store.strong_count() > 0 => {}
                "/pair" if store.strong_count() > 0 && !request.headers().contains_key("origin") => pairing_route = true,
                _ => {
                    return Err(Response::builder()
                        .status(404)
                        .body(Some("Endpoint unavailable".into()))
                        .unwrap());
                }
            }
            Ok(response)
        };
    let mut ws = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(config())),
    )
    .await??;
    if extension_route {
        let store = store.upgrade().context("Inventory closed")?;
        return tokio::time::timeout(
            Duration::from_secs(30),
            crate::api::transport::serve(ws, store),
        )
        .await?;
    }
    if bundle_route {
        return crate::git_bundle::serve(ws, store).await;
    }
    if merge_route {
        crate::network_auth::authenticate(&mut ws,store.clone(),None,false,None).await?;
        return crate::component_sync::serve_merge(ws, store).await;
    }
    let pairing = pairing_route.then_some(crate::network_auth::CodePairing::Host { binding });
    peer(ws, store, None, None, None, Some(incoming_ip), None, false, pairing).await
}

const RAFT_REMOVED: &str =
    "Raft sync has been removed. Use the WebSocket base address in Sync (/sync).";

pub(crate) fn websocket_address(address: &str, endpoint: &str) -> Result<url::Url> {
    let mut url = url::Url::parse(address.trim())
        .context("Enter a WebSocket base address, such as wss://localhost:8824")?;
    ensure!(!matches!(url.path(), "/raft" | "/raft/"), "{RAFT_REMOVED}");
    ensure!(
        url.scheme() == "wss"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && (url.path().is_empty() || url.path() == "/" || url.path() == endpoint),
        "Enter a WebSocket base address, such as wss://localhost:8824"
    );
    url.set_path(endpoint);
    Ok(url)
}

#[derive(Clone, Debug)]
pub enum ClientStatus {
    Connecting,
    WaitingForApproval,
    PairingCode { remaining: usize },
    Connected { peer_id: String },
    Failed(String),
}
struct PullGuard {
    store: SharedStore,
    active: std::sync::atomic::AtomicBool,
}
impl PullGuard {
    fn finish(&self) {
        if self.active.swap(false, std::sync::atomic::Ordering::SeqCst) {
            if let Ok(mut store) = self.store.try_lock() {
                store.end_pull();
            } else if let Ok(runtime) = crate::async_io::runtime() {
                let store = self.store.clone();
                runtime.spawn_blocking(move || {
                    if let Ok(mut store) = store.lock() {
                        store.end_pull();
                    }
                });
            }
        }
    }
}
impl Drop for PullGuard {
    fn drop(&mut self) {
        self.finish();
    }
}
pub struct SyncClient {
    task: tokio::task::JoinHandle<()>,
    status: Arc<Mutex<ClientStatus>>,
    codes: mpsc::Sender<String>,
}
impl SyncClient {
    pub fn start(store: SharedStore, url: &str) -> Result<Self> {
        Self::start_expected(store,url,None)
    }
    pub fn start_expected(store:SharedStore,url:&str,expected:Option<String>)->Result<Self>{
        Self::start_connection(store, url, expected, false, false, false)
    }
    #[cfg(test)]
    pub fn start_pairing(store: SharedStore, url: &str, expected: Option<String>) -> Result<Self> {
        Self::start_connection(store, url, expected, true, false, false)
    }
    pub fn start_code_pairing(store: SharedStore, url: &str, expected: Option<String>, replace_pin: bool) -> Result<Self> {
        Self::start_connection(store, url, expected, false, true, replace_pin)
    }
    fn start_connection(store: SharedStore, url: &str, expected: Option<String>, approve_remote: bool, allow_pairing: bool, replace_pin: bool) -> Result<Self> {
        let mut url = websocket_address(url, "/sync")?;
        let expected=expected.or_else(||url.host_str().filter(|host|host.ends_with(".vispace.local")).map(str::to_owned));
        let (expected, trust) = {
            let inventory = store.lock().unwrap();
            let pin = inventory.network_tls_pin(&url.origin().ascii_serialization(), expected.as_deref())?;
            match pin {
                Some((id, _)) if replace_pin => {
                    url.set_path("/pair");
                    (Some(id), crate::network_tls::PeerTls::Pairing)
                }
                Some((id, pin)) => (Some(id), crate::network_tls::PeerTls::Pinned(pin)),
                None if allow_pairing => {
                    url.set_path("/pair");
                    (expected, crate::network_tls::PeerTls::Pairing)
                }
                None => (expected, crate::network_tls::PeerTls::System),
            }
        };
        let (codes, input) = mpsc::channel(1);
        store
            .lock()
            .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?
            .begin_pull();
        let pull = Arc::new(PullGuard {
            store: store.clone(),
            active: std::sync::atomic::AtomicBool::new(true),
        });
        let status = Arc::new(Mutex::new(ClientStatus::Connecting));
        let output = status.clone();
        let store = Arc::downgrade(&store);
        let task = crate::async_io::runtime()?.spawn(async move {
            let result = async {
                let (ws, _) = tokio::time::timeout(
                    IO_TIMEOUT,
                    crate::network_tls::connect_peer(url.as_str(), Some(config()), false, trust),
                )
                .await??;
                let pairing = if url.path() == "/pair" {
                    let (binding, server_pin) = crate::network_tls::client_pairing_binding(&ws)?;
                    Some(crate::network_auth::CodePairing::Client {
                        binding, server_pin, origin: url.origin().ascii_serialization(),
                        status: output.clone(), codes: input,
                    })
                } else { None };
                peer(
                    ws,
                    store,
                    Some(output.clone()),
                    Some(pull.clone()),
                    Some(url),
                    None,
                    expected,
                    approve_remote,
                    pairing,
                )
                .await
            }
            .await;
            *output.lock().unwrap() = ClientStatus::Failed(
                result
                    .err()
                    .map(|e: anyhow::Error| e.to_string())
                    .unwrap_or_else(|| "Peer disconnected; reconnect to resume".into()),
            );
        });
        Ok(Self { task, status, codes })
    }
    pub fn enter_pairing_code(&self, code: String) {
        if matches!(self.status(), ClientStatus::PairingCode { .. }) {
            let _ = self.codes.try_send(code);
        }
    }
    pub fn status(&self) -> ClientStatus {
        self.status.lock().unwrap().clone()
    }
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
    pub async fn shutdown(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}
impl Drop for SyncClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}
