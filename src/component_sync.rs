//! Leader approval scoped to a connected component of the Sync mesh.
use crate::{
    action_log::{self, Record},
    store::{SharedStore, Store},
    sync_blob::Blob,
    sync_relay::Relay,
    topology::Graph,
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::{Bytes, Message};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result", content = "detail")]
pub enum SaveResult {
    Committed,
    Stale(Vec<(String, String, String)>),
    Invalid(String),
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draft {
    pub record: Record,
    pub outcome: SaveResult,
    pub choices: BTreeMap<String, serde_json::Value>,
}

impl Draft {
    pub fn new(record: Record) -> Self {
        Self {
            record,
            outcome: SaveResult::Unavailable,
            choices: BTreeMap::new(),
        }
    }

    pub fn ready(&self, reviews: &[crate::merge_review::Review]) -> bool {
        let SaveResult::Stale(fields) = &self.outcome else {
            return false;
        };
        fields.iter().all(|(entity, key, field)| {
            reviews.iter().any(|review| {
                &review.entity == entity
                    && &review.key == key
                    && (&review.field == field || review.field == "$entity")
            })
        })
    }
}

const SETTLE: Duration = Duration::from_millis(900);
const LEASE: Duration = Duration::from_secs(45);

#[derive(Debug)]
pub(crate) struct Queued;
impl std::fmt::Display for Queued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Edit queued for leader approval. Open Sync to see pending edits.")
    }
}
impl std::error::Error for Queued {}

pub(crate) struct State {
    pub relay: Arc<Relay>,
    pub pending: VecDeque<Pending>,
    pub origin: String,
    pub bridge: Option<Bridge>,
    election_view: Option<(Vec<String>, Vec<[String; 2]>)>,
    settle_until: Instant,
    lease: Option<(Uuid, Instant)>,
    pub running: bool,
}

impl State {
    pub fn new(local: String) -> Self {
        Self {
            relay: Arc::new(Relay::new(local)),
            pending: VecDeque::new(),
            origin: Uuid::new_v4().to_string().to_uppercase(),
            bridge: None,
            election_view: None,
            settle_until: Instant::now() + SETTLE,
            lease: None,
            running: false,
        }
    }

    pub fn leader(&mut self, graph: Graph) -> Option<String> {
        let members = graph
            .nodes
            .into_iter()
            .filter(|node| (node.reachable || node.direct) && !node.title.starts_with("Browser "))
            .map(|node| node.id)
            .collect::<Vec<_>>();
        let edges = graph
            .edges
            .into_iter()
            .filter(|[a, b]| members.contains(a) && members.contains(b))
            .collect();
        let view = (members, edges);
        if self.election_view.as_ref() != Some(&view) {
            self.settle_until = Instant::now() + SETTLE;
            self.election_view = Some(view);
        }
        if Instant::now() < self.settle_until {
            return None;
        }
        self.election_view.as_ref()?.0.iter().min().cloned()
    }

    pub fn locked(&self) -> bool {
        self.lease.is_some_and(|(_, until)| Instant::now() < until)
    }

    pub fn prepare(&mut self, id: Uuid) -> Result<()> {
        ensure!(
            !self.locked() || self.lease.is_some_and(|(owner, _)| owner == id),
            "Another reconnect is finalizing"
        );
        self.lease = Some((id, Instant::now() + LEASE));
        Ok(())
    }

    pub fn owns(&self, id: Uuid) -> bool {
        self.locked() && self.lease.is_some_and(|(owner, _)| owner == id)
    }

    pub fn release(&mut self, id: Uuid) {
        if self.lease.is_some_and(|(owner, _)| owner == id) {
            self.lease = None;
        }
    }
}

pub(crate) struct Pending {
    pub draft: Record,
    pub prepared: Option<Record>,
}
impl From<Record> for Pending {
    fn from(draft: Record) -> Self {
        Self {
            draft,
            prepared: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Approval {
    pub result: SaveResult,
    pub record: Option<Record>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Branch {
    pub leader: String,
    pub head: String,
    pub records: Vec<Record>,
}

#[derive(Clone)]
pub(crate) struct Bridge {
    pub address: String,
    pub branch: Branch,
    pub choices: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Plan {
    id: Uuid,
    expected: String,
}

#[derive(Serialize, Deserialize)]
struct Commit {
    plan: Plan,
    records: Vec<Record>,
}

type WeakStore = Weak<Mutex<Store>>;

pub(crate) async fn access<T: Send + 'static>(
    store: &WeakStore,
    work: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
) -> Result<T> {
    let store = store.upgrade().context("Inventory closed")?;
    crate::async_io::blocking(move || work(&mut store.lock().unwrap())).await
}

pub(crate) async fn request(store: WeakStore, method: String, body: Bytes) -> Result<Vec<u8>> {
    match method.as_str() {
        "component_blob" => {
            let root = access(&store, |s| Ok(s.attachment_root())).await?;
            crate::async_io::blocking(move || Blob::decode(&body)?.save(&root)).await?;
            Ok(b"null".to_vec())
        }
        "component_get_blob" => {
            let asset: String = serde_json::from_slice(&body)?;
            let root = access(&store, |s| Ok(s.attachment_root())).await?;
            crate::async_io::blocking(move || Blob::read(&root, &asset)?.encode()).await
        }
        "component_propose" => {
            let draft = serde_json::from_slice(&body)?;
            Ok(serde_json::to_vec(
                &access(&store, move |s| s.approve_component(&draft)).await?,
            )?)
        }
        "component_branch" => Ok(serde_json::to_vec(
            &access(&store, |s| s.component_branch()).await?,
        )?),
        "component_prepare" => {
            let Plan { id, expected } = serde_json::from_slice(&body)?;
            access(&store, move |s| s.prepare_component_merge(id, &expected)).await?;
            Ok(b"null".to_vec())
        }
        "component_commit" => {
            let Commit { plan, records } = serde_json::from_slice(&body)?;
            access(&store, move |s| {
                s.commit_component_merge(plan.id, &plan.expected, &records)
            })
            .await?;
            Ok(b"null".to_vec())
        }
        "component_release" => {
            let id = serde_json::from_slice(&body)?;
            access(&store, move |s| {
                s.release_component_merge(id);
                Ok(())
            })
            .await?;
            Ok(b"null".to_vec())
        }
        _ => bail!("Unknown component request"),
    }
}

async fn route(store: &WeakStore, leader: &str, method: &str, body: Bytes) -> Result<Bytes> {
    let (relay, local) = access(store, |s| {
        Ok((
            s.component_relay().context("Partition sync is disabled")?,
            s.application_id().to_owned(),
        ))
    })
    .await?;
    if leader == local {
        return Ok(request(store.clone(), method.to_owned(), body)
            .await?
            .into());
    }
    relay.call(leader, method, body).await
}

async fn route_json<Q: Serialize, R: DeserializeOwned>(
    store: &WeakStore,
    leader: &str,
    method: &str,
    body: &Q,
) -> Result<R> {
    Ok(serde_json::from_slice(
        &route(store, leader, method, serde_json::to_vec(body)?.into()).await?,
    )?)
}

fn assets(records: &[Record]) -> Result<Vec<String>> {
    let mut ids = std::collections::BTreeSet::new();
    for record in records {
        ids.extend(action_log::referenced_assets(record)?);
    }
    Ok(ids.into_iter().collect())
}

async fn send_assets(store: &WeakStore, leader: &str, records: &[Record]) -> Result<()> {
    let local = access(store, |s| Ok(s.application_id().to_owned())).await?;
    if leader == local {
        return Ok(());
    }
    for asset in assets(records)? {
        let encoded = request(
            store.clone(),
            "component_get_blob".into(),
            serde_json::to_vec(&asset)?.into(),
        )
        .await?;
        route(store, leader, "component_blob", encoded.into()).await?;
    }
    Ok(())
}

pub(crate) async fn submit(store: &WeakStore, draft: &Record) -> Result<Approval> {
    let Some(leader) = access(store, |s| Ok(s.component_leader())).await? else {
        return Ok(Approval {
            result: SaveResult::Unavailable,
            record: None,
        });
    };
    send_assets(store, &leader, std::slice::from_ref(draft)).await?;
    route_json(store, &leader, "component_propose", draft).await
}

pub(crate) async fn authorize_member(store: WeakStore, public: String, revoke: bool) -> Result<()> {
    let draft = access(&store, move |s| {
        if !s.uses_component() || s.component_leader().as_deref() == Some(s.application_id()) {
            s.network_authorize(public, revoke)?;
            return Ok(None);
        }
        let mut draft = s.make_membership_record()?;
        draft.actions.push(action_log::Action {
            kind: if revoke { "revoke" } else { "approve" }.into(),
            entity: "network_membership_request".into(),
            key: String::new(),
            label: String::new(),
            value: Some(public.into()),
            changes: vec![],
            position: None,
        });
        s.network_identity().sign_record(&mut draft)?;
        Ok(Some(draft))
    }).await?;
    if let Some(draft) = draft {
        match submit(&store, &draft).await?.result {
            SaveResult::Committed => {}
            SaveResult::Invalid(error) => bail!("{error}"),
            _ => bail!("The sync leader could not commit the membership change. Try again."),
        }
    }
    Ok(())
}

pub fn start(store: &SharedStore) -> Result<()> {
    {
        let mut store = store.lock().unwrap();
        if !store.start_component_worker() {
            return Ok(());
        }
    }
    let store = Arc::downgrade(store);
    crate::async_io::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_millis(350));
        while store.strong_count() > 0 {
            timer.tick().await;
            let Ok(Some((leader, draft))) = access(&store, |s| s.next_component_proposal()).await
            else {
                continue;
            };
            let result = async {
                send_assets(&store, &leader, std::slice::from_ref(&draft)).await?;
                route_json::<_, Approval>(&store, &leader, "component_propose", &draft).await
            }
            .await;
            if let Ok(outcome) = result {
                let _ = access(&store, move |s| s.finish_component_proposal(draft, outcome)).await;
            }
        }
    })
}

#[derive(Serialize, Deserialize)]
struct Wire {
    method: String,
    payload: serde_json::Value,
}

async fn remote(store: &WeakStore, address: &str, method: &str, payload: serde_json::Value) -> Result<Bytes> {
    let url = crate::sync::websocket_address(address, "/merge")?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut ws, _) = crate::network_tls::connect_peer(
            url.as_str(),
            Some(crate::sync::config()),
            false,
            crate::network_tls::peer_trust(store, &url, None)?,
        )
        .await?;
        crate::network_auth::authenticate(&mut ws,store.clone(),url.host_str().filter(|h|h.ends_with(".vispace.local")),false,None).await?;
        ws.send(Message::Text(
            serde_json::to_string(&Wire {
                method: method.into(),
                payload,
            })?
            .into(),
        ))
        .await?;
        match ws.next().await.context("No reconnect response")?? {
            Message::Binary(bytes) => Ok(bytes),
            Message::Text(error) => bail!("{error}"),
            _ => bail!("Invalid reconnect response"),
        }
    })
    .await
    .context("Reconnect request timed out")?
}

async fn remote_json<Q: Serialize, R: DeserializeOwned>(
    store:&WeakStore,
    address: &str,
    method: &str,
    input: &Q,
) -> Result<R> {
    Ok(serde_json::from_slice(
        &remote(store,address, method, serde_json::to_value(input)?).await?,
    )?)
}

pub(crate) async fn serve_merge<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send>(
    mut ws: tokio_tungstenite::WebSocketStream<S>,
    store: WeakStore,
) -> Result<()> {
    let result = tokio::time::timeout(Duration::from_secs(40), async {
        let Message::Text(text) = ws.next().await.context("Missing reconnect request")?? else {
            bail!("Invalid reconnect request")
        };
        let Wire { method, payload } = serde_json::from_str(&text)?;
        if method == "release" {
            let target = payload["leader"]
                .as_str()
                .context("Missing branch leader")?;
            return route(
                &store,
                target,
                "component_release",
                serde_json::to_vec(&payload["body"])?.into(),
            )
            .await;
        }
        let leader = access(&store, |s| {
            ensure!(
                s.uses_component(),
                "This desktop does not use Partition sync"
            );
            s.component_leader()
                .context("Waiting for a component leader")
        })
        .await?;
        match method.as_str() {
            "branch" => route(&store, &leader, "component_branch", b"null".to_vec().into()).await,
            "get_blob" => {
                route(
                    &store,
                    &leader,
                    "component_get_blob",
                    serde_json::to_vec(&payload)?.into(),
                )
                .await
            }
            "blob" => {
                let Message::Binary(bytes) =
                    ws.next().await.context("Missing reconnect attachment")??
                else {
                    bail!("Invalid reconnect attachment")
                };
                route(&store, &leader, "component_blob", bytes).await
            }
            "prepare" | "commit" => {
                let target = payload["leader"]
                    .as_str()
                    .context("Missing branch leader")?;
                ensure!(
                    target == leader,
                    "Remote leader changed. Refresh the reconnect review."
                );
                let method = format!("component_{method}");
                let response = route(
                    &store,
                    target,
                    &method,
                    serde_json::to_vec(&payload["body"])?.into(),
                )
                .await?;
                if method == "component_commit" {
                    let records: Commit = serde_json::from_value(payload["body"].clone())?;
                    // Wait until the receiving gateway has the merge before opening the Sync link.
                    let local = access(&store, |s| Ok(s.application_id().to_owned())).await?;
                    if local != target {
                        for asset in assets(&records.records)? {
                            let blob = route(
                                &store,
                                target,
                                "component_get_blob",
                                serde_json::to_vec(&asset)?.into(),
                            )
                            .await?;
                            request(store.clone(), "component_blob".into(), blob).await?;
                        }
                        access(&store, move |s| s.replay_component_merge(&records.records)).await?;
                    }
                }
                Ok(response)
            }
            _ => bail!("Unknown reconnect request"),
        }
    })
    .await;
    let message = match result {
        Ok(Ok(bytes)) if bytes.len() <= crate::sync::MESSAGE_LIMIT => Message::Binary(bytes),
        Ok(Ok(_)) => Message::Text("Reconnect history is too large for one exchange".into()),
        Ok(Err(error)) => Message::Text(error.to_string().into()),
        Err(_) => Message::Text("Reconnect request timed out".into()),
    };
    ws.send(message).await?;
    let _ = ws.close(None).await;
    Ok(())
}

async fn remote_blob(store: &WeakStore,address: &str, bytes: Bytes) -> Result<()> {
    let url = crate::sync::websocket_address(address, "/merge")?;
    tokio::time::timeout(Duration::from_secs(8), async {
        let (mut ws, _) = crate::network_tls::connect_peer(
            url.as_str(),
            Some(crate::sync::config()),
            false,
            crate::network_tls::peer_trust(store, &url, None)?,
        )
        .await?;
        crate::network_auth::authenticate(&mut ws,store.clone(),url.host_str().filter(|h|h.ends_with(".vispace.local")),false,None).await?;
        ws.send(Message::Text(r#"{"method":"blob","payload":null}"#.into()))
            .await?;
        ws.send(Message::Binary(bytes)).await?;
        match ws.next().await.context("No attachment response")?? {
            Message::Binary(_) => Ok(()),
            Message::Text(error) => bail!("{error}"),
            _ => bail!("Invalid attachment response"),
        }
    })
    .await
    .context("Reconnect attachment timed out")?
}

pub async fn fetch_branch(store: SharedStore, address: String) -> Result<()> {
    let store = Arc::downgrade(&store);
    let branch: Branch = remote_json(&store,&address, "branch", &()).await?;
    let root = access(&store, |s| Ok(s.attachment_root())).await?;
    for asset in assets(&branch.records)? {
        if Blob::read(&root, &asset).is_ok() {
            continue;
        }
        let bytes = remote(&store,&address, "get_blob", serde_json::to_value(&asset)?).await?;
        let blob = Blob::decode(&bytes)?;
        ensure!(
            blob.asset == asset,
            "Peer returned an unexpected attachment"
        );
        blob.save(&root)?;
    }
    access(&store, move |s| s.set_component_bridge(address, branch)).await
}

pub async fn finish_bridge(store: SharedStore) -> Result<(String, crate::sync::SyncClient)> {
    let store = Arc::downgrade(&store);
    let (leader, bridge) = access(&store, |s| {
        Ok((
            s.component_leader()
                .context("Waiting for a component leader")?,
            s.component_bridge()
                .context("Open a reconnect review first")?,
        ))
    })
    .await?;
    let local: Branch = route_json(&store, &leader, "component_branch", &()).await?;
    let expected = local.head.clone();
    let remote_leader = bridge.branch.leader.clone();
    let remote_head = bridge.branch.head.clone();
    let root = access(&store, |s| Ok(s.attachment_root())).await?;
    for asset in assets(&local.records)? {
        if Blob::read(&root, &asset).is_ok() {
            continue;
        }
        let bytes = route(
            &store,
            &leader,
            "component_get_blob",
            serde_json::to_vec(&asset)?.into(),
        )
        .await?;
        Blob::decode(&bytes)?.save(&root)?;
    }
    let snapshot = local.clone();
    let records = access(&store, move |s| s.build_component_merge(&snapshot, &bridge)).await?;
    let address = access(&store, |s| {
        Ok(s.component_bridge()
            .context("Reconnect review was closed")?
            .address)
    })
    .await?;

    // Stage files on both branches while they can still accept saves.
    send_assets(&store, &leader, &records).await?;
    for asset in assets(&records)? {
        let bytes = request(
            store.clone(),
            "component_get_blob".into(),
            serde_json::to_vec(&asset)?.into(),
        )
        .await?;
        remote_blob(&store,&address, bytes.into()).await?;
    }
    let id = Uuid::new_v4();
    let local_plan = Plan { id, expected };
    let remote_plan = Plan {
        id,
        expected: remote_head,
    };
    route_json::<_, ()>(&store, &leader, "component_prepare", &local_plan).await?;
    let outcome = async {
        remote_json::<_, ()>(
            &store, &address,
            "prepare",
            &serde_json::json!({"leader": remote_leader, "body": remote_plan}),
        )
        .await?;
        let commit = Commit {
            plan: remote_plan,
            records: records.clone(),
        };
        remote_json::<_, ()>(
            &store, &address,
            "commit",
            &serde_json::json!({"leader": remote_leader, "body": commit}),
        )
        .await?;
        route_json::<_, ()>(
            &store,
            &leader,
            "component_commit",
            &Commit {
                plan: local_plan,
                records: records.clone(),
            },
        )
        .await?;
        access(&store, move |s| s.replay_component_merge(&records)).await?;
        let client =
            crate::sync::SyncClient::start(store.upgrade().context("Inventory closed")?, &address)?;
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                match client.status() {
                    crate::sync::ClientStatus::Connected { .. } => return Ok(()),
                    crate::sync::ClientStatus::Failed(error) => bail!("{error}"),
                    crate::sync::ClientStatus::WaitingForApproval | crate::sync::ClientStatus::PairingCode { .. } => {
                        bail!("Complete pairing in Sync before reconnecting");
                    }
                    crate::sync::ClientStatus::Connecting => {
                        tokio::time::sleep(Duration::from_millis(60)).await
                    }
                }
            }
        })
        .await
        .context("Reconnected histories, but the Sync link timed out")??;
        access(&store, |s| {
            s.clear_component_bridge();
            Ok(())
        })
        .await?;
        Ok::<_, anyhow::Error>(client)
    }
    .await;
    let _ = route_json::<_, ()>(&store, &leader, "component_release", &id).await;
    let _ = remote_json::<_, ()>(
        &store,&address,
        "release",
        &serde_json::json!({"leader": remote_leader, "body": id}),
    )
    .await;
    Ok((address, outcome?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::Part,
        sync::{SyncClient, SyncServer},
    };

    async fn wait_for(label: &str, check: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(12), async {
            while !check() {
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{label}"));
    }

    fn node(root: &std::path::Path, name: &str) -> SharedStore {
        let store = Arc::new(Mutex::new(
            Store::open(root.join(name).join("inventory.sqlite")).unwrap(),
        ));
        Store::enable_component(&store).unwrap();
        store
    }

    fn endpoint(store: &SharedStore) -> SyncServer {
        SyncServer::start(store.clone(), "127.0.0.1:0".parse().unwrap()).unwrap()
    }

    fn leader(store: &SharedStore) -> Option<String> {
        store.lock().unwrap().component_leader()
    }

    #[tokio::test]
    async fn revocation_is_ordered_after_accepted_edits_and_blocks_the_next_proposal() {
        let root = tempfile::tempdir().unwrap();
        let owner = node(root.path(), "owner");
        let owner_id = owner.lock().unwrap().application_id().to_owned();
        wait_for("owner elected", || leader(&owner).as_deref() == Some(&owner_id)).await;
        let author = crate::network_identity::Identity::generate();
        let author_id = author.id().unwrap();
        let key = author.public_key().unwrap();
        let mut owner = owner.lock().unwrap();
        owner.network_authorize(key.clone(), false).unwrap();

        let edit = |owner: &Store, description: &str| {
            let mut mesh = crate::mesh::Mesh::default();
            for event in owner.records_after(None).unwrap() {
                mesh.insert(event).unwrap();
            }
            let attachments = owner.attachment_root();
            let before = mesh.project(&attachments).unwrap();
            let mut after = before.clone();
            if after.parts.is_empty() {
                after.save_part(Part { number: "R1".into(), ..Default::default() }).unwrap();
            }
            after.parts[0].description = description.into();
            let sequence = mesh.clock.get(&author_id).copied().unwrap_or(0) + 1;
            let mut record = mesh.make(&before, &after, &attachments, &author_id, sequence).unwrap();
            author.sign_record(&mut record).unwrap();
            record
        };
        for description in ["created", "modified"] {
            let proposal = edit(&owner, description);
            assert_eq!(owner.approve_component(&proposal).unwrap().result, SaveResult::Committed);
        }
        owner.network_authorize(key.clone(), true).unwrap();
        let rejected = edit(&owner, "after revocation");
        assert!(owner.approve_component(&rejected).is_err());
        let events = owner.events_after(&Default::default()).unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].actions[0].kind, "node_approved");
        assert_eq!(events[3].actions[0].kind, "node_revoked");
        assert_eq!(events[3].mesh.as_ref().unwrap().context.get(&author_id), Some(&2));

        let mut joiner = Store::open(root.path().join("joiner/inventory.sqlite")).unwrap();
        joiner.network_bootstrap_peer(owner.network_identity().public_key().unwrap()).unwrap();
        joiner.network_merge(owner.network_actions()).unwrap();
        for event in events {
            joiner.replay_record(&event, None).unwrap();
        }
        assert_eq!(joiner.data.parts[0].description, "modified");
        assert!(joiner.network_revoked(&author_id));
        owner.network_authorize(key, false).unwrap();
        assert!(matches!(owner.approve_component(&rejected).unwrap().result, SaveResult::Invalid(_)));
        let fresh = edit(&owner, "after reapproval");
        assert_eq!(owner.approve_component(&fresh).unwrap().result, SaveResult::Committed);
    }

    fn description(store: &SharedStore) -> String {
        store
            .lock()
            .unwrap()
            .data
            .parts
            .first()
            .map(|part| part.description.clone())
            .unwrap_or_default()
    }

    fn edit(store: &SharedStore, desc: &str) {
        let result = store.lock().unwrap().change(|data| {
            data.parts[0].description = desc.into();
            Ok(())
        });
        if let Err(error) = result {
            assert!(error.is::<Queued>(), "{error}");
        }
    }

    #[tokio::test]
    async fn settled_leader_saves_project_edit_with_local_kicad_link() {
        let root = tempfile::tempdir().unwrap();
        let store = node(root.path(), "leader");
        wait_for("single desktop becomes leader", || leader(&store).is_some()).await;
        let mut store = store.lock().unwrap();
        store
            .change(|data| data.ensure_project("Board").map(|_| ()))
            .unwrap();
        let before = store.records_after(None).unwrap().len();
        store
            .change(|data| {
                let project = &mut data.projects[0];
                project.description = "Updated".into();
                project.kicad = Some(crate::model::KicadProject {
                    path: "board.kicad_pro".into(),
                    ..Default::default()
                });
                Ok(())
            })
            .unwrap();
        assert_eq!(store.component_pending_count(), 0);
        assert_eq!(
            store.data.projects[0].kicad.as_ref().unwrap().path,
            "board.kicad_pro"
        );
        let records = store.records_after(None).unwrap();
        assert_eq!(records.len(), before + 1);
        assert!(
            !serde_json::to_string(records.last().unwrap())
                .unwrap()
                .contains("board.kicad_pro")
        );
    }

    #[tokio::test]
    async fn branches_elect_approve_and_rejoin_through_the_initiator() {
        let root = tempfile::tempdir().unwrap();
        let a = node(root.path(), "a");
        let b = node(root.path(), "b");
        let c = node(root.path(), "c");
        for (x,y) in [(&a,&b),(&b,&a),(&b,&c),(&c,&b)]{let key=y.lock().unwrap().network_identity().public_key().unwrap();x.lock().unwrap().network_bootstrap_peer(key).unwrap();}
        let host_b = endpoint(&b);
        let host_c = endpoint(&c);
        let address_c = format!("wss://{}", host_c.address);
        let _a_b = SyncClient::start(a.clone(), &format!("wss://{}", host_b.address)).unwrap();
        let b_c = SyncClient::start(b.clone(), &address_c).unwrap();
        wait_for("chain shares a leader", || {
            leader(&a).is_some_and(|id| leader(&b) == Some(id.clone()) && leader(&c) == Some(id))
        })
        .await;
        let result = a.lock().unwrap().change(|data| {
            data.save_part(Part {
                number: "R1".into(),
                description: "base".into(),
                ..Default::default()
            })
        });
        if let Err(error) = result {
            assert!(error.is::<Queued>(), "{error}");
        }
        wait_for("approved add reached both ends", || {
            [&a, &b, &c].iter().all(|node| description(node) == "base")
        })
        .await;

        b_c.shutdown().await;
        edit(&a, "left");
        edit(&c, "right");
        wait_for("both sides approve independently", || {
            description(&a) == "left" && description(&b) == "left" && description(&c) == "right"
        })
        .await;
        assert_eq!(leader(&c).unwrap(), c.lock().unwrap().application_id());

        let plain = SyncClient::start(b.clone(), &address_c).unwrap();
        wait_for("direct reconnect refused divergent histories", || {
            plain.is_finished()
        })
        .await;
        fetch_branch(b.clone(), address_c.clone()).await.unwrap();
        let comparison = b
            .lock()
            .unwrap()
            .component_bridge_reviews()
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(comparison.base, "base");
        assert!(
            a.lock()
                .unwrap()
                .component_bridge_reviews()
                .unwrap()
                .is_empty()
        );
        assert!(
            c.lock()
                .unwrap()
                .component_bridge_reviews()
                .unwrap()
                .is_empty()
        );
        assert_eq!(description(&b), "left");
        edit(&c, "new right");
        wait_for("remote kept working during review", || {
            description(&c) == "new right"
        })
        .await;
        b.lock()
            .unwrap()
            .choose_component_merge(&comparison.id, "merged".into())
            .unwrap();
        assert!(finish_bridge(b.clone()).await.is_err());
        assert_eq!(description(&c), "new right");

        fetch_branch(b.clone(), address_c.clone()).await.unwrap();
        let mut reconnecting = b.lock().unwrap();
        for comparison in reconnecting.component_bridge_reviews().unwrap() {
            reconnecting
                .choose_component_merge(&comparison.id, "merged".into())
                .unwrap();
        }
        drop(reconnecting);
        wait_for("both branches have settled leaders", || {
            leader(&b).is_some_and(|id| leader(&a) == Some(id)) && leader(&c).is_some()
        })
        .await;
        let (_, _reconnected) = finish_bridge(b.clone()).await.unwrap();
        wait_for("approved merge reached the whole chain", || {
            [&a, &b, &c]
                .iter()
                .all(|node| description(node) == "merged")
        })
        .await;
        wait_for("rejoined network elects one leader", || {
            leader(&a).is_some_and(|id| leader(&b) == Some(id.clone()) && leader(&c) == Some(id))
        })
        .await;
    }
}
