use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::de::DeserializeOwned;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub type SharedStore = Arc<Mutex<Store>>;
pub struct Store {
    pub data: Inventory,
    pub path: PathBuf,
    pub(crate) connection: Connection,
    pub(crate) security: crate::network_security::Security,
    revision: u64,
    journal: crate::action_log::Journal,
    history: Vec<crate::action_log::Record>,
    pub(crate) changes: tokio::sync::watch::Sender<u64>,
    mesh: crate::mesh::Mesh,
    local_head: std::sync::OnceLock<crate::merge_review::Head>,
    applied_head: String,
    application_id: String,
    topology: crate::topology::Topology,
    pull_count: usize,
    auto_merging: bool,
    reviews: Vec<crate::merge_review::Review>,
    sync_leader: Option<String>,
    component: Option<crate::component_sync::State>,
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let journal = crate::action_log::Journal::open(path.with_extension("actions"))?;
        let connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS parts(id TEXT PRIMARY KEY, barcode TEXT NOT NULL UNIQUE, payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS allocations(id TEXT PRIMARY KEY, part_id TEXT NOT NULL REFERENCES parts(id) ON DELETE CASCADE, payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS projects(name TEXT PRIMARY KEY COLLATE NOCASE, description TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS project_git_tips(project TEXT PRIMARY KEY COLLATE NOCASE REFERENCES projects(name) ON UPDATE CASCADE ON DELETE CASCADE,payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS project_git_remote(project TEXT PRIMARY KEY COLLATE NOCASE REFERENCES projects(name) ON UPDATE CASCADE ON DELETE CASCADE,payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS project_kicad(project TEXT PRIMARY KEY COLLATE NOCASE REFERENCES projects(name) ON UPDATE CASCADE ON DELETE CASCADE,payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS project_notes(project TEXT PRIMARY KEY COLLATE NOCASE REFERENCES projects(name) ON UPDATE CASCADE ON DELETE CASCADE, attachment_id TEXT NOT NULL, payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS storage_descriptions(name TEXT PRIMARY KEY COLLATE NOCASE, description TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS purchases(id TEXT PRIMARY KEY, payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS project_bom_entries(id TEXT PRIMARY KEY, payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS labels(name TEXT PRIMARY KEY COLLATE NOCASE, last_used INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS field_values(field TEXT NOT NULL,name TEXT NOT NULL COLLATE NOCASE,last_used INTEGER NOT NULL,PRIMARY KEY(field,name));")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS action_log_applied(id TEXT PRIMARY KEY, chunk TEXT NOT NULL, byte_offset INTEGER NOT NULL, timestamp_ms INTEGER NOT NULL);")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS sync_merge_drafts(id TEXT PRIMARY KEY,text TEXT NOT NULL); CREATE TABLE IF NOT EXISTS sync_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),application_id TEXT NOT NULL); CREATE TABLE IF NOT EXISTS sync_events(id TEXT PRIMARY KEY,origin TEXT NOT NULL,sequence INTEGER NOT NULL,lamport INTEGER NOT NULL,timestamp_ms INTEGER NOT NULL,payload TEXT NOT NULL,UNIQUE(origin,sequence)); CREATE TABLE IF NOT EXISTS sync_peers(id TEXT PRIMARY KEY,last_received_ms INTEGER NOT NULL,clock TEXT NOT NULL); CREATE TABLE IF NOT EXISTS project_ids(name TEXT PRIMARY KEY COLLATE NOCASE,id TEXT NOT NULL UNIQUE);")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS sync_component_mode(enabled INTEGER PRIMARY KEY CHECK(enabled=1));")?;
        let component = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_component_mode)",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        let legacy_path = path.with_extension("consensus.sqlite");
        let legacy_db = if !component && legacy_path.exists() {
            Some(Connection::open_with_flags(
                legacy_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?)
        } else {
            None
        };
        let security = crate::network_security::Security::open(&connection)?;
        let application_id = security.identity.id()?;
        connection.execute(
            "INSERT INTO sync_identity VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET application_id=excluded.application_id",
            [&application_id],
        )?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS sync_topology_epoch(singleton INTEGER PRIMARY KEY,epoch INTEGER NOT NULL); INSERT INTO sync_topology_epoch VALUES(1,1) ON CONFLICT(singleton) DO UPDATE SET epoch=epoch+1;")?;
        let epoch = connection.query_row(
            "SELECT epoch FROM sync_topology_epoch WHERE singleton=1",
            [],
            |r| r.get::<_, u64>(0),
        )?;
        let topology = crate::topology::Topology::new(
            application_id.clone(),
            format!(
                "Desktop {}",
                application_id.chars().take(8).collect::<String>()
            ),
            epoch,
            topology_now(),
        );
        connection.execute_batch("CREATE TABLE IF NOT EXISTS inventory_applied_head(singleton INTEGER PRIMARY KEY, hash TEXT NOT NULL);")?;
        let applied_head = connection
            .query_row(
                "SELECT hash FROM inventory_applied_head WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default();
        let mut data = load(&connection)?;
        prepare_project_ids(&mut data);
        let mut store = Self {
            data,
            path,
            connection,
            security,
            revision: 0,
            journal,
            history: Vec::new(),
            changes: tokio::sync::watch::channel(0).0,
            mesh: crate::mesh::Mesh::default(),
            local_head: std::sync::OnceLock::new(),
            applied_head,
            application_id,
            topology,
            pull_count: 0,
            auto_merging: false,
            reviews: vec![],
            sync_leader: None,
            component: None,
        };
        let indexed: Vec<crate::action_log::Record> = payloads(&store.connection, "sync_events")?;
        let entries = store.journal.entries()?;
        let present: std::collections::BTreeSet<_> = entries
            .iter()
            .map(|entry| entry.record.id.clone())
            .collect();
        for record in &indexed {
            if !present.contains(&record.id) {
                store.journal.append(record)?;
            }
            store.mesh.insert(record.clone())?;
        }
        if entries.is_empty() && indexed.is_empty() {
            let record = crate::action_log::diff(
                &Inventory::default(),
                &store.data,
                &store.attachment_root(),
                true,
            )?;
            let (chunk, offset) = store.journal.append(&record)?;
            let data = store.data.clone();
            store.persist(&data, Some((&record, &chunk, offset)), None)?;
        }
        let entries = store.journal.entries()?;
        let mut legacy = Inventory::default();
        let mut previous_legacy: Option<String> = None;
        let mut legacy_lamport = 0u64;
        for entry in &entries {
            if entry.record.mesh.is_some() {
                store.mesh.insert(entry.record.clone())?;
            } else if !store.mesh.events.contains_key(&entry.record.id) {
                let mut next =
                    crate::action_log::apply(&legacy, &entry.record, &store.attachment_root())?;
                crate::mesh::prepare_ids(&mut legacy);
                crate::mesh::prepare_ids(&mut next);
                if !entry.record.actions.is_empty() {
                    let mut record = store.mesh.make(
                        &legacy,
                        &next,
                        &store.attachment_root(),
                        &entry.record.id,
                        1,
                    )?;
                    legacy_lamport += 1;
                    record.id = entry.record.id.clone();
                    record.timestamp_ms = entry.record.timestamp_ms;
                    record.initial = entry.record.initial;
                    let meta = record.mesh.as_mut().unwrap();
                    meta.lamport = legacy_lamport;
                    meta.context.clear();
                    if let Some(previous) = previous_legacy.as_ref() {
                        meta.context.insert(previous.clone(), 1);
                    }
                    previous_legacy = Some(record.mesh.as_ref().unwrap().origin.clone());
                    store.mesh.insert(record)?;
                }
                legacy = next;
            }
        }
        if !store.mesh.events.is_empty() {
            let mut next = store.mesh.project(&store.attachment_root())?;
            next.restore_local_project_data(&store.data);
            store.reviews =
                crate::merge_review::conflicts(&store.mesh.events, &store.application_id);
            if store.merge_reviews().is_empty() {
                let head = store.mesh.applied_tip_hash()?;
                store.persist(&next, None, Some(&head))?;
                store.applied_head = head;
                store.data = next;
            }
        }
        // Rebuild this index from the durable chunks, including records flushed before a crash.
        let tx = store.connection.transaction()?;
        tx.execute("DELETE FROM sync_events", [])?;
        for record in store.mesh.events.values() {
            let m = record.mesh.as_ref().unwrap();
            tx.execute(
                "INSERT INTO sync_events VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    record.id,
                    m.origin,
                    m.sequence,
                    m.lamport,
                    record.timestamp_ms,
                    serde_json::to_string(record)?
                ],
            )?;
        }
        for entry in &entries {
            tx.execute(
                "INSERT OR IGNORE INTO action_log_applied VALUES(?1,?2,?3,?4)",
                params![
                    entry.record.id,
                    entry.chunk,
                    entry.offset,
                    entry.record.timestamp_ms
                ],
            )?;
        }
        tx.commit()?;
        store.history = store.mesh.missing(&crate::mesh::Clock::new());
        let membership = store.history.iter()
            .map(crate::network_identity::membership_record)
            .collect::<Result<Vec<_>>>()?;
        store.network_merge(membership.into_iter().flatten().collect())?;
        store.connection.execute_batch("CREATE TABLE IF NOT EXISTS sync_pending_proposal(singleton INTEGER PRIMARY KEY CHECK(singleton=1), payload TEXT NOT NULL);")?;
        let migrate = !component
            && (legacy_db.is_some() || store.path.with_extension("consensus.json").exists());
        if component || migrate {
            store.component = Some(crate::component_sync::State::new(
                store.application_id.clone(),
            ));
        }
        store.journal.start_chunk()?;
        if migrate {
            if let Some(legacy) = legacy_db {
                #[derive(serde::Deserialize)]
                struct AppliedState {
                    records: Vec<crate::action_log::Record>,
                }
                if let Some(state) = legacy_metadata::<AppliedState>(&legacy, "state")? {
                    let missing = state
                        .records
                        .into_iter()
                        .filter(|record| !store.mesh.events.contains_key(&record.id))
                        .collect::<Vec<_>>();
                    store.replay_component_merge(&missing).context(
                        "Could not recover committed history from the retired Raft workspace",
                    )?;
                }
            }
            if store
                .pending_proposal()?
                .is_some_and(|draft| store.mesh.events.contains_key(&draft.record.id))
            {
                store.save_pending_proposal(None)?;
            }
            store
                .connection
                .execute("INSERT INTO sync_component_mode VALUES(1)", [])?;
        }
        Ok(store)
    }
    /// Clone, validate, and persist a complete operation before exposing it to the UI/API.
    pub fn change<T>(&mut self, op: impl FnOnce(&mut Inventory) -> Result<T>) -> Result<T> {
        self.ensure_writable()?;
        let mut next = self.data.clone();
        let result = op(&mut next)?;
        prepare_project_ids(&mut next);
        next.reconcile_designator_priority(&self.data)?;
        next.validate()?;
        self.persist_logged(&next)?;
        self.revision = self.revision.wrapping_add(1);
        self.changes.send_replace(self.revision);
        Ok(result)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Commit a worker-computed snapshot only if no UI/API operation changed its base.
    pub fn commit_if_revision(&mut self, revision: u64, mut next: Inventory) -> Result<()> {
        self.ensure_writable()?;
        ensure!(
            self.revision == revision,
            "Inventory changed during the background operation. Review the latest data before applying again."
        );
        prepare_project_ids(&mut next);
        next.reconcile_designator_priority(&self.data)?;
        next.validate()?;
        self.persist_logged(&next)?;
        self.revision = self.revision.wrapping_add(1);
        self.changes.send_replace(self.revision);
        Ok(())
    }
    pub fn recovery_required(&self) -> bool {
        self.journal.failed
    }
    pub fn action_log(&self) -> Result<Vec<crate::action_log::LogEntry>> {
        self.journal.entries()
    }
    pub fn action_log_page(
        &self,
        before: Option<&crate::action_log::HistoryCursor>,
        limit: usize,
    ) -> Result<crate::action_log::HistoryPage> {
        self.journal.page(before, limit)
    }
    fn ensure_writable(&self) -> Result<()> {
        ensure!(self.network_trusted(self.application_id()), "This node has been revoked");
        ensure!(
            self.pending_proposal()?.is_none(),
            "Review the pending draft in Sync before saving another change"
        );
        ensure!(
            self.merge_reviews().is_empty(),
            "Resolve pending merge conflicts in Sync before editing or publishing"
        );
        ensure!(
            !self.recovery_required(),
            "Restart the application to recover its action log"
        );
        Ok(())
    }
    pub fn sync_head(&self) -> Option<String> {
        self.history.last().map(|record| record.id.clone())
    }
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
    pub fn records_after(&self, after: Option<&str>) -> Result<Vec<crate::action_log::Record>> {
        ensure!(
            !self.recovery_required(),
            "Restart the application to recover its action log"
        );
        let start = match after {
            None => 0,
            Some(id) => {
                self.history
                    .iter()
                    .position(|record| record.id == id)
                    .context("Unknown sync cursor; the inventories have different histories")?
                    + 1
            }
        };
        Ok(self.history[start..].to_vec())
    }
    /// Preserve the received ID and payload; never generate a replacement local event.
    /// Per-origin sequence and causal context replace the legacy global predecessor.
    pub fn replay_record(
        &mut self,
        record: &crate::action_log::Record,
        previous: Option<&str>,
    ) -> Result<bool> {
        self.network_verify_record(record)?;
        let membership = crate::network_identity::membership_record(record)?;
        ensure!(
            !self.recovery_required(),
            "Restart the application to recover its action log"
        );
        if let Some(existing) = self.history.iter().find(|entry| entry.id == record.id) {
            ensure!(
                serde_json::to_value(existing)? == serde_json::to_value(record)?,
                "Record ID reused with different contents"
            );
            return Ok(false);
        }
        ensure!(
            !record.id.is_empty() && uuid::Uuid::parse_str(&record.id).is_ok(),
            "Invalid record UUID"
        );
        let _ = previous; // Kept for older Rust callers; mesh events have per-origin causal clocks.
        let mut mesh = self.mesh.clone();
        if !mesh.insert(record.clone())? {
            return Ok(false);
        }
        let mut candidate = mesh.project(&self.attachment_root())?;
        candidate.restore_local_project_data(&self.data);
        let reviews = crate::merge_review::conflicts(&mesh.events, &self.application_id);
        let next = if reviews.is_empty() {
            candidate
        } else {
            self.data.clone()
        };
        let head = reviews
            .is_empty()
            .then(|| mesh.applied_tip_hash())
            .transpose()?;
        let (chunk, offset) = self.journal.append(record)?;
        if let Err(error) = self.persist(&next, Some((record, &chunk, offset)), head.as_deref()) {
            self.journal.failed = true;
            return Err(error.context("Incoming event is logged; restart to recover it"));
        }
        self.mesh = mesh;
        self.local_head.take();
        self.reviews = reviews;
        if let Some(head) = head {
            self.applied_head = head;
        }
        self.data = next;
        self.history.push(record.clone());
        if let Some(action) = membership {
            if let Err(error) = self.network_merge(vec![action]) {
                self.journal.failed = true;
                return Err(error.context("Membership change is logged; restart to recover it"));
            }
        }
        self.revision = self.revision.wrapping_add(1);
        self.changes.send_replace(self.revision);
        if !self.auto_merging && !self.uses_component() {
            self.auto_merging = true;
            let result = (|| -> Result<()> {
                loop {
                    let auto = self
                        .merge_reviews()
                        .into_iter()
                        .find_map(|r| crate::merge_review::auto_notes(&r).map(|v| (r.id, v)));
                    let Some((id, value)) = auto else {
                        break;
                    };
                    self.resolve_merge(&id, value)?;
                }
                Ok(())
            })();
            self.auto_merging = false;
            result?;
        }
        Ok(true)
    }
    pub(crate) fn begin_pull(&mut self) {
        self.pull_count += 1;
    }
    pub(crate) fn end_pull(&mut self) {
        self.pull_count = self.pull_count.saturating_sub(1);
        self.changes.send_replace(self.revision);
    }
    pub fn shared_head(&self) -> crate::merge_review::Head {
        let events = self
            .events_after(&Default::default())
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();
        crate::merge_review::head(&events, self.mesh.clock.clone())
    }
    pub fn merge_draft(&self, id: &str) -> Option<String> {
        self.connection
            .query_row(
                "SELECT text FROM sync_merge_drafts WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .ok()
    }
    pub fn save_merge_draft(&self, id: &str, text: &str) -> Result<()> {
        self.connection.execute("INSERT INTO sync_merge_drafts VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET text=excluded.text",params![id,text])?;
        Ok(())
    }
    fn current_head(&self) -> &crate::merge_review::Head {
        self.local_head
            .get_or_init(|| crate::merge_review::head(&self.mesh.events, self.mesh.clock.clone()))
    }
    pub fn merge_head(&self) -> crate::merge_review::Head {
        self.current_head().clone()
    }
    pub fn local_head_hash(&self) -> String {
        self.applied_head.clone()
    }
    pub fn merge_reviews(&self) -> Vec<crate::merge_review::Review> {
        self.reviews.clone()
    }
    pub fn resolve_merge(&mut self, id: &str, value: serde_json::Value) -> Result<()> {
        ensure!(
            !self.uses_write_approval(),
            "Resolve your pending draft through the write authority"
        );
        let mut record = crate::merge_review::resolve(
            &self.mesh,
            &self.attachment_root(),
            &self.application_id,
            id,
            value,
        )?;
        self.security.identity.sign_record(&mut record)?;
        self.replay_record(&record, None)?;
        Ok(())
    }
    pub fn network_graph(&mut self) -> crate::topology::Graph {
        let mut graph = self.topology.graph(topology_now());
        graph.leader.clone_from(&self.sync_leader);
        graph.managed = self.uses_write_approval();
        graph
    }
    pub fn uses_write_approval(&self) -> bool {
        self.uses_component()
    }

    pub fn uses_component(&self) -> bool {
        self.component.is_some()
    }
    pub(crate) fn component_locked(&self) -> bool {
        self.component.as_ref().is_some_and(|state| state.locked()) || !self.reviews.is_empty()
    }
    pub(crate) fn make_membership_record(&self) -> Result<crate::action_log::Record> {
        // Membership commits have a separate origin from queued component drafts.
        let origin = self.application_id.as_str();
        let sequence = self.mesh.clock.get(origin).copied().unwrap_or(0)
            .checked_add(1).context("Origin sequence exhausted")?;
        self.mesh.make(&self.data, &self.data, &self.attachment_root(), origin, sequence)
    }

    pub fn enable_component(store: &SharedStore) -> Result<()> {
        {
            let mut store = store.lock().unwrap();
            ensure!(
                store.reviews.is_empty(),
                "Resolve the pending comparisons before changing sync modes"
            );
            ensure!(
                !store
                    .topology
                    .graph(topology_now())
                    .nodes
                    .iter()
                    .any(|node| node.direct),
                "Disconnect existing peers before enabling partition sync"
            );
            if store.component.is_none() {
                let local = store.application_id.clone();
                store
                    .connection
                    .execute("INSERT INTO sync_component_mode VALUES(1)", [])?;
                store.component = Some(crate::component_sync::State::new(local));
                store.changes.send_replace(store.revision);
            }
        }
        crate::component_sync::start(store)
    }

    pub(crate) fn component_relay(&self) -> Option<Arc<crate::sync_relay::Relay>> {
        Some(self.component.as_ref()?.relay.clone())
    }

    pub(crate) fn start_component_worker(&mut self) -> bool {
        let Some(component) = &mut self.component else {
            return false;
        };
        !std::mem::replace(&mut component.running, true)
    }

    pub(crate) fn component_leader(&mut self) -> Option<String> {
        let graph = self.topology.graph(topology_now());
        let leader = self.component.as_mut()?.leader(graph);
        if self.sync_leader != leader {
            self.sync_leader.clone_from(&leader);
            self.changes.send_replace(self.revision);
        }
        leader
    }

    pub(crate) fn next_component_proposal(
        &mut self,
    ) -> Result<Option<(String, crate::action_log::Record)>> {
        let Some(leader) = self.component_leader() else {
            return Ok(None);
        };
        if self.pending_proposal()?.is_some()
            || self.component.as_ref().is_some_and(|state| state.locked())
        {
            return Ok(None);
        }
        let Some(pending) = self.component.as_ref().unwrap().pending.front() else {
            return Ok(None);
        };
        if let Some(prepared) = &pending.prepared {
            return Ok(Some((leader, prepared.clone())));
        }
        let draft = pending.draft.clone();
        let meta = draft.mesh.as_ref().context("Missing proposal context")?;
        let mut prepared =
            if meta.sequence == self.mesh.clock.get(&meta.origin).copied().unwrap_or(0) + 1 {
                draft.clone()
            } else {
                let stale = crate::merge_review::stale_fields(
                    &meta.context,
                    &meta.mutations,
                    self.mesh.events.values(),
                );
                if !stale.is_empty() {
                    let mut review = crate::component_sync::Draft::new(draft);
                    review.outcome =
                        crate::component_sync::SaveResult::Stale(stale.into_iter().collect());
                    self.save_pending_proposal(Some(&review))?;
                    self.component.as_mut().unwrap().pending.pop_front();
                    return Ok(None);
                }
                let mut record = crate::merge_review::rebase_draft(
                    &mut self.mesh.clone(),
                    &draft,
                    &Default::default(),
                    &self.attachment_root(),
                )?;
                if record.mesh.as_ref().unwrap().mutations.is_empty() {
                    self.component.as_mut().unwrap().pending.pop_front();
                    self.changes.send_replace(self.revision);
                    return Ok(None);
                }
                record.id = draft.id;
                record
            };
        self.security.identity.sign_record(&mut prepared)?;
        self.component
            .as_mut()
            .unwrap()
            .pending
            .front_mut()
            .unwrap()
            .prepared = Some(prepared.clone());
        Ok(Some((leader, prepared)))
    }

    pub(crate) fn approve_component(
        &mut self,
        draft: &crate::action_log::Record,
    ) -> Result<crate::component_sync::Approval> {
        let author = crate::network_identity::verify_record(draft)?;
        ensure!(self.network_trusted(&author), "The proposal author is unapproved or revoked");
        if let [action] = draft.actions.as_slice()
            && action.entity == "network_membership_request"
        {
            use crate::component_sync::{Approval, SaveResult};
            ensure!(draft.mesh.as_ref().is_some_and(|meta| meta.mutations.is_empty()
                && meta.resolutions.is_empty() && meta.receipts.is_empty()), "Invalid membership proposal");
            let public = action.value.as_ref().and_then(serde_json::Value::as_str)
                .context("Missing proposed member key")?;
            ensure!(crate::network_identity::node_id(public)? != author, "Cannot change your own membership");
            let revoke = match action.kind.as_str() {
                "revoke" => true,
                "approve" => false,
                _ => anyhow::bail!("Invalid membership proposal"),
            };
            if let Some(existing) = self.mesh.events.get(&draft.id) {
                let membership = crate::network_identity::membership_record(existing)?
                    .context("Membership proposal ID was reused")?;
                ensure!(membership.public_key == public && membership.revoked == revoke, "Membership proposal ID was reused");
                return Ok(Approval { result: SaveResult::Committed, record: Some(existing.clone()) });
            }
            let record = self.commit_network_membership(public.to_owned(), revoke, Some(&draft.id))?;
            return Ok(Approval { result: SaveResult::Committed, record: Some(record) });
        }
        let approval = self.validate_component_proposal(draft)?;
        if let Some(record) = &approval.record {
            self.replay_record(record, None)?;
        }
        Ok(approval)
    }

    fn validate_component_proposal(
        &mut self,
        draft: &crate::action_log::Record,
    ) -> Result<crate::component_sync::Approval> {
        use crate::{component_sync::Approval, component_sync::SaveResult};
        let outcome = |result| Approval {
            result,
            record: None,
        };
        if self.component_leader().as_deref() != Some(&self.application_id)
            || self.component.as_ref().is_some_and(|state| state.locked())
            || !self.reviews.is_empty()
        {
            return Ok(outcome(SaveResult::Unavailable));
        }
        if let Some(record) = self.mesh.events.get(&draft.id) {
            ensure!(
                serde_json::to_vec(record)? == serde_json::to_vec(draft)?,
                "Proposal ID was reused with different content"
            );
            return Ok(Approval {
                result: SaveResult::Committed,
                record: Some(record.clone()),
            });
        }
        let meta = draft.mesh.as_ref().context("Missing proposal context")?;
        ensure!(self.network_trusted(meta.author()), "The proposal author is unapproved or revoked");
        // A draft from an earlier membership period must not be revived by reapproval.
        if self.history.iter().any(|record| {
            record.actions.iter().any(|action| {
                action.entity == "network_membership" && action.key == meta.author()
            }) && record.mesh.as_ref().is_some_and(|membership| {
                meta.context.get(&membership.origin).copied().unwrap_or(0) < membership.sequence
            })
        }) {
            return Ok(outcome(SaveResult::Invalid(
                "The draft predates a membership change. Sync before editing again.".into(),
            )));
        }
        if !meta
            .context
            .iter()
            .all(|(author, n)| self.mesh.clock.get(author).copied().unwrap_or(0) >= *n)
        {
            return Ok(outcome(SaveResult::Unavailable));
        }
        let stale = crate::merge_review::stale_fields(
            &meta.context,
            &meta.mutations,
            self.mesh.events.values(),
        );
        if !stale.is_empty() {
            return Ok(outcome(SaveResult::Stale(stale.into_iter().collect())));
        }
        let approved = (|| -> Result<_> {
            let root = self.attachment_root();
            let mut mesh = self.mesh.clone();
            ensure!(
                !meta.mutations.is_empty() && meta.resolutions.is_empty(),
                "Invalid inventory proposal"
            );
            crate::sync_blob::require_assets(&root, draft)?;
            mesh.insert(draft.clone())?;
            let candidate = mesh.project(&root)?;
            ensure!(
                mesh.clock.get(&meta.origin) == Some(&meta.sequence),
                "Proposal could not be applied"
            );
            ensure!(
                crate::merge_review::conflicts(&mesh.events, &self.application_id).is_empty(),
                "Proposal conflicts with an approved change"
            );
            candidate.validate()?;
            Ok(Some(draft.clone()))
        })();
        match approved {
            Ok(record) => Ok(Approval {
                result: SaveResult::Committed,
                record,
            }),
            Err(error) => Ok(outcome(SaveResult::Invalid(error.to_string()))),
        }
    }

    pub(crate) fn finish_component_proposal(
        &mut self,
        draft: crate::action_log::Record,
        approval: crate::component_sync::Approval,
    ) -> Result<()> {
        use crate::component_sync::{Draft, SaveResult};
        if approval.result == SaveResult::Unavailable {
            return Ok(());
        }
        if !self
            .component
            .as_ref()
            .and_then(|state| state.pending.front())
            .is_some_and(|pending| pending.draft.id == draft.id)
        {
            return Ok(());
        }
        if let Some(record) = approval.record {
            self.replay_record(&record, None)?;
        }
        if approval.result != SaveResult::Committed {
            let mut review = Draft::new(draft);
            review.outcome = approval.result;
            self.save_pending_proposal(Some(&review))?;
        }
        self.component.as_mut().unwrap().pending.pop_front();
        self.changes.send_replace(self.revision);
        Ok(())
    }

    pub fn component_pending_count(&self) -> usize {
        self.component
            .as_ref()
            .map_or(0, |state| state.pending.len())
    }

    pub(crate) fn component_branch(&mut self) -> Result<crate::component_sync::Branch> {
        ensure!(
            self.component_leader().as_deref() == Some(&self.application_id),
            "Component leader changed"
        );
        ensure!(
            self.reviews.is_empty(),
            "Resolve local conflicts before reconnecting"
        );
        ensure!(
            self.mesh.events.values().all(|record| {
                let meta = record.mesh.as_ref().unwrap();
                self.mesh.clock.get(&meta.origin).copied().unwrap_or(0) >= meta.sequence
            }),
            "Waiting for the branch history to catch up"
        );
        Ok(crate::component_sync::Branch {
            leader: self.application_id.clone(),
            head: self.merge_head().hash,
            records: self.mesh.missing(&Default::default()),
        })
    }

    pub(crate) fn component_bridge(&self) -> Option<crate::component_sync::Bridge> {
        self.component.as_ref()?.bridge.clone()
    }

    pub(crate) fn set_component_bridge(
        &mut self,
        address: String,
        branch: crate::component_sync::Branch,
    ) -> Result<()> {
        let state = self
            .component
            .as_mut()
            .context("Enable partition sync first")?;
        let choices = state
            .bridge
            .take()
            .filter(|old| old.address == address)
            .map_or_else(Default::default, |old| old.choices);
        state.bridge = Some(crate::component_sync::Bridge {
            address,
            branch,
            choices,
        });
        self.changes.send_replace(self.revision);
        Ok(())
    }

    pub(crate) fn clear_component_bridge(&mut self) {
        if let Some(state) = &mut self.component {
            state.bridge = None;
        }
        self.changes.send_replace(self.revision);
    }

    fn merged_component_mesh(
        &self,
        local: &[crate::action_log::Record],
        remote: &[crate::action_log::Record],
    ) -> Result<crate::mesh::Mesh> {
        let mut mesh = crate::mesh::Mesh::default();
        for record in local.iter().chain(remote) {
            self.network_verify_record(record)?;
            mesh.insert(record.clone())?;
        }
        mesh.project(&self.attachment_root())?;
        Ok(mesh)
    }

    pub(crate) fn component_bridge_reviews(&self) -> Result<Vec<crate::merge_review::Review>> {
        let Some(bridge) = self.component_bridge() else {
            return Ok(vec![]);
        };
        let mesh = self.merged_component_mesh(&self.history, &bridge.branch.records)?;
        let local = self.history.iter().map(|r| r.id.as_str()).collect();
        Ok(
            crate::merge_review::conflicts_for_branch(&mesh.events, &self.application_id, &local)
                .into_iter()
                .filter(|review| !bridge.choices.contains_key(&review.id))
                .collect(),
        )
    }

    pub(crate) fn choose_component_merge(
        &mut self,
        id: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        ensure!(
            self.component_bridge_reviews()?
                .iter()
                .any(|review| review.id == id),
            "Refresh the reconnect comparison"
        );
        self.component
            .as_mut()
            .unwrap()
            .bridge
            .as_mut()
            .unwrap()
            .choices
            .insert(id.into(), value);
        Ok(())
    }

    pub(crate) fn build_component_merge(
        &self,
        local: &crate::component_sync::Branch,
        bridge: &crate::component_sync::Bridge,
    ) -> Result<Vec<crate::action_log::Record>> {
        let mut mesh = self.merged_component_mesh(&local.records, &bridge.branch.records)?;
        let root = self.attachment_root();
        let merge_origin = id();
        let reviews = crate::merge_review::conflicts(&mesh.events, &self.application_id);
        for review in reviews {
            let value = bridge
                .choices
                .get(&review.id)
                .cloned()
                .context("Review new overlaps before reconnecting")?;
            let mut record =
                crate::merge_review::resolve(&mesh, &root, &merge_origin, &review.id, value)?;
            record.mesh.as_mut().unwrap().author = Some(self.application_id.clone());
            self.security.identity.sign_record(&mut record)?;
            mesh.insert(record)?;
            mesh.project(&root)?;
        }
        ensure!(
            crate::merge_review::conflicts(&mesh.events, &self.application_id).is_empty(),
            "Reconnect still has unresolved changes"
        );
        Ok(mesh.missing(&Default::default()))
    }

    pub(crate) fn prepare_component_merge(&mut self, id: uuid::Uuid, expected: &str) -> Result<()> {
        ensure!(
            self.component_branch()?.head == expected,
            "Inventory changed. Refresh the reconnect review."
        );
        self.component.as_mut().unwrap().prepare(id)
    }

    pub(crate) fn release_component_merge(&mut self, id: uuid::Uuid) {
        if let Some(state) = &mut self.component {
            state.release(id);
        }
    }

    pub(crate) fn commit_component_merge(
        &mut self,
        id: uuid::Uuid,
        expected: &str,
        records: &[crate::action_log::Record],
    ) -> Result<()> {
        ensure!(
            self.component.as_ref().is_some_and(|state| state.owns(id)),
            "Reconnect approval expired"
        );
        ensure!(
            self.component_branch()?.head == expected,
            "Inventory changed during reconnect"
        );
        self.replay_component_merge(records)
    }

    pub(crate) fn replay_component_merge(
        &mut self,
        records: &[crate::action_log::Record],
    ) -> Result<()> {
        let mesh = self.merged_component_mesh(&self.history, records)?;
        ensure!(
            crate::merge_review::conflicts(&mesh.events, &self.application_id).is_empty(),
            "Inventory changed. Review the new overlaps."
        );
        for record in records {
            crate::sync_blob::require_assets(&self.attachment_root(), record)?;
        }
        let previous = std::mem::replace(&mut self.auto_merging, true);
        let result = records
            .iter()
            .try_for_each(|record| self.replay_record(record, None).map(|_| ()));
        self.auto_merging = previous;
        result
    }

    pub(crate) fn sync_leader(&self) -> Option<String> {
        self.sync_leader.clone()
    }

    pub fn pending_proposal(&self) -> Result<Option<crate::component_sync::Draft>> {
        let payload: Option<String> = self
            .connection
            .query_row(
                "SELECT payload FROM sync_pending_proposal WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(payload
            .map(|text| serde_json::from_str(&text))
            .transpose()?)
    }
    fn save_pending_proposal(&self, draft: Option<&crate::component_sync::Draft>) -> Result<()> {
        if let Some(draft) = draft {
            self.connection.execute(
                "INSERT OR REPLACE INTO sync_pending_proposal VALUES(1,?1)",
                [serde_json::to_string(draft)?],
            )?;
        } else {
            self.connection
                .execute("DELETE FROM sync_pending_proposal", [])?;
        }
        self.changes.send_replace(self.revision);
        Ok(())
    }
    pub fn retry_proposal(&mut self) -> Result<()> {
        let draft = self.pending_proposal()?.context("No draft is pending")?;
        ensure!(
            draft.outcome == crate::component_sync::SaveResult::Unavailable,
            "This draft needs review"
        );
        self.component
            .as_mut()
            .context("Enable partition sync first")?
            .pending
            .push_front(draft.record.into());
        self.save_pending_proposal(None)
    }
    pub fn discard_proposal(&self) -> Result<()> {
        let draft = self.pending_proposal()?.context("No draft is pending")?;
        ensure!(
            draft.outcome != crate::component_sync::SaveResult::Unavailable,
            "Retry first to check whether the draft already committed"
        );
        self.save_pending_proposal(None)
    }
    pub fn proposal_reviews(
        &self,
        draft: &crate::component_sync::Draft,
    ) -> Result<Vec<crate::merge_review::Review>> {
        let reviews = crate::merge_review::draft_reviews(
            &mut self.mesh.clone(),
            &draft.record,
            &self.attachment_root(),
        )?;
        Ok(if draft.ready(&reviews) {
            reviews
        } else {
            vec![]
        })
    }
    pub fn resolve_proposal(&mut self, id: &str, value: serde_json::Value) -> Result<()> {
        let mut draft = self.pending_proposal()?.context("No draft is pending")?;
        let reviews = self.proposal_reviews(&draft)?;
        ensure!(
            reviews.iter().any(|review| review.id == id),
            "Review the latest shared values first"
        );
        draft.choices.insert(id.to_owned(), value);
        if reviews
            .iter()
            .any(|review| !draft.choices.contains_key(&review.id))
        {
            return self.save_pending_proposal(Some(&draft));
        }
        let record = crate::merge_review::rebase_draft(
            &mut self.mesh.clone(),
            &draft.record,
            &draft.choices,
            &self.attachment_root(),
        )?;
        if !record.mesh.as_ref().unwrap().mutations.is_empty() {
            self.component
                .as_mut()
                .context("Enable partition sync first")?
                .pending
                .push_front(record.into());
        }
        self.save_pending_proposal(None)
    }
    pub(crate) fn topology_ads(&mut self) -> Vec<crate::topology::Advertisement> {
        self.topology.heartbeat(topology_now());
        self.topology.advertisements(topology_now())
    }
    pub(crate) fn topology_receive(
        &mut self,
        ads: Vec<crate::topology::Advertisement>,
    ) -> Result<()> {
        if self.topology.receive(ads, topology_now())? {
            self.changes.send_replace(self.revision);
        }
        Ok(())
    }
    pub(crate) fn topology_connect(&mut self, id: &str) -> Result<()> {
        self.topology.connect(id, topology_now())?;
        self.changes.send_replace(self.revision);
        Ok(())
    }
    pub(crate) fn topology_disconnect(&mut self, id: &str) {
        self.topology.disconnect(id, topology_now());
        self.changes.send_replace(self.revision);
    }
    pub fn application_id(&self) -> &str {
        &self.application_id
    }
    pub fn sync_clock(&self) -> crate::mesh::Clock {
        self.mesh.clock.clone()
    }
    pub fn sync_conflicts(&self) -> &[crate::mesh::Conflict] {
        &self.mesh.conflicts
    }
    pub fn events_after(
        &self,
        clock: &crate::mesh::Clock,
    ) -> Result<Vec<crate::action_log::Record>> {
        ensure!(
            !self.recovery_required(),
            "Restart to recover the action log"
        );
        let blocked: Vec<_> = self
            .merge_reviews()
            .iter()
            .flat_map(|c| c.events.iter())
            .filter_map(|id| self.mesh.events.get(id).and_then(|r| r.mesh.as_ref()))
            .cloned()
            .collect();
        Ok(self
            .mesh
            .missing(clock)
            .into_iter()
            .filter(|r| {
                let m = r.mesh.as_ref().unwrap();
                (self.pull_count == 0 || m.origin != self.application_id)
                    && !blocked.iter().any(|b| {
                        (m.origin == b.origin && m.sequence >= b.sequence)
                            || m.context.get(&b.origin).copied().unwrap_or(0) >= b.sequence
                    })
            })
            .collect())
    }
    pub fn note_peer(&mut self, peer: &str) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        self.connection.execute("INSERT INTO sync_peers VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET last_received_ms=excluded.last_received_ms,clock=excluded.clock",params![peer,now,serde_json::to_string(&self.mesh.clock)?])?;
        Ok(())
    }
    pub(crate) fn attachment_root(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("Attachments")
    }
    fn persist_logged(&mut self, data: &Inventory) -> Result<()> {
        ensure!(
            !self.journal.failed,
            "Action log needs recovery; restart the application before making changes"
        );
        let origin = self
            .component
            .as_ref()
            .map_or(self.application_id.as_str(), |state| &state.origin);
        let sequence = self
            .mesh
            .events
            .values()
            .filter_map(|record| record.mesh.as_ref())
            .filter(|meta| meta.origin == origin)
            .map(|meta| meta.sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("Origin sequence exhausted")?;
        let mut record =
            self.mesh
                .make(&self.data, data, &self.attachment_root(), origin, sequence)?;
        if self.uses_component() {
            record.mesh.as_mut().unwrap().author = Some(self.application_id.clone());
        }
        self.security.identity.sign_record(&mut record)?;
        if record.mesh.as_ref().unwrap().mutations.is_empty() {
            self.persist(data, None, None)?;
            self.data = data.clone();
            return Ok(());
        }
        if self.uses_component() {
            use crate::component_sync::SaveResult;
            // Preserve queued edit order. A settled leader can validate its own
            // next edit immediately and persist the local KiCad data with it.
            let outcome = if self.component_pending_count() == 0 {
                self.validate_component_proposal(&record)?.result
            } else {
                SaveResult::Unavailable
            };
            match outcome {
                SaveResult::Committed => {}
                SaveResult::Invalid(message) => anyhow::bail!("{message}"),
                SaveResult::Stale(_) => {
                    anyhow::bail!("The inventory changed. Reopen the editor and try again.")
                }
                SaveResult::Unavailable => {
                    let state = self.component.as_mut().unwrap();
                    ensure!(state.pending.len() < 128, "The pending edit queue is full");
                    state.pending.push_back(record.into());
                    self.changes.send_replace(self.revision);
                    return Err(crate::component_sync::Queued.into());
                }
            }
        }
        let mut mesh = self.mesh.clone();
        mesh.insert(record.clone())?;
        let mut next = mesh.project(&self.attachment_root())?;
        next.restore_local_project_data(data);
        let head = mesh.applied_tip_hash()?;
        let (chunk, offset) = self.journal.append(&record)?;
        if let Err(error) = self.persist(&next, Some((&record, &chunk, offset)), Some(&head)) {
            self.journal.failed = true;
            return Err(error.context("Operation is logged; restart to recover it"));
        }
        self.mesh = mesh;
        self.local_head.take();
        self.data = next;
        self.history.push(record);
        self.applied_head = head;
        Ok(())
    }

    fn persist(
        &mut self,
        data: &Inventory,
        applied: Option<(&crate::action_log::Record, &str, u64)>,
        head: Option<&str>,
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        tx.execute_batch("DELETE FROM project_ids; DELETE FROM project_git_remote; DELETE FROM project_git_tips; DELETE FROM project_kicad; DELETE FROM project_notes; DELETE FROM allocations; DELETE FROM parts; DELETE FROM projects; DELETE FROM purchases; DELETE FROM project_bom_entries; DELETE FROM storage_descriptions; DELETE FROM labels; DELETE FROM field_values;")?;
        for p in &data.projects {
            tx.execute(
                "INSERT INTO project_ids VALUES(?1,?2)",
                params![p.name, p.id],
            )?;
            tx.execute(
                "INSERT INTO projects VALUES (?1,?2)",
                params![p.name, p.description],
            )?;
            if !p.git_tips.is_empty() {
                tx.execute(
                    "INSERT INTO project_git_tips VALUES (?1,?2)",
                    params![p.name, serde_json::to_string(&p.git_tips)?],
                )?;
            }
            if let Some(remote) = &p.git_remote {
                tx.execute(
                    "INSERT INTO project_git_remote VALUES (?1,?2)",
                    params![p.name, serde_json::to_string(remote)?],
                )?;
            }
            if let Some(kicad) = &p.kicad {
                tx.execute(
                    "INSERT INTO project_kicad VALUES (?1,?2)",
                    params![p.name, serde_json::to_string(kicad)?],
                )?;
            }
            tx.execute(
                "INSERT INTO project_notes VALUES (?1,?2,?3)",
                params![
                    p.name,
                    p.notes_attachment_id
                        .clone()
                        .unwrap_or_else(|| format!("project-{}", id())),
                    serde_json::to_string(&p.notes)?
                ],
            )?;
        }
        for (name, description) in &data.storage {
            tx.execute(
                "INSERT INTO storage_descriptions VALUES (?1,?2)",
                params![name, description],
            )?;
        }
        for p in &data.parts {
            tx.execute(
                "INSERT INTO parts VALUES (?1,?2,?3)",
                params![p.id, p.barcode, serde_json::to_string(p)?],
            )?;
            for label in &p.labels {
                tx.execute("INSERT OR IGNORE INTO labels VALUES (?1,0)", [label])?;
            }
            for (field, value) in [
                ("manufacturer", &p.manufacturer),
                ("package", &p.package),
                ("storage", &p.storage),
            ] {
                if !value.is_empty() {
                    tx.execute(
                        "INSERT OR IGNORE INTO field_values VALUES (?1,?2,0)",
                        params![field, value],
                    )?;
                }
            }
        }
        for label in [
            "Inductor",
            "Capacitor",
            "Resistor",
            "Crystal",
            "Oscillator",
            "IC",
        ] {
            tx.execute("INSERT OR IGNORE INTO labels VALUES (?1,0)", [label])?;
        }
        let mut stored = data.clone();
        stored.convert_project_references(true);
        for a in &stored.allocations {
            tx.execute(
                "INSERT INTO allocations VALUES (?1,?2,?3)",
                params![a.id, a.part_id, serde_json::to_string(a)?],
            )?;
        }
        for p in &stored.purchases {
            tx.execute(
                "INSERT INTO purchases VALUES (?1,?2)",
                params![p.part.id, serde_json::to_string(p)?],
            )?;
        }
        for e in &stored.bom {
            tx.execute(
                "INSERT INTO project_bom_entries VALUES (?1,?2)",
                params![e.id, serde_json::to_string(e)?],
            )?;
        }
        if let Some((record, chunk, offset)) = applied {
            if let Some(m) = record.mesh.as_ref() {
                tx.execute(
                    "INSERT INTO sync_events VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        record.id,
                        m.origin,
                        m.sequence,
                        m.lamport,
                        record.timestamp_ms,
                        serde_json::to_string(record)?
                    ],
                )?;
            }
            tx.execute(
                "INSERT INTO action_log_applied VALUES (?1,?2,?3,?4)",
                params![record.id, chunk, offset, record.timestamp_ms],
            )?;
        }
        if let Some(hash) = head {
            tx.execute("INSERT INTO inventory_applied_head VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET hash=excluded.hash", [hash])?;
        }
        tx.pragma_update(None, "user_version", 11)?;
        tx.commit()?;
        Ok(())
    }
    pub fn backup(&self, folder: &Path) -> Result<()> {
        ensure!(!folder.exists(), "Choose a new backup folder");
        std::fs::create_dir_all(folder)?;
        self.connection.backup(
            rusqlite::DatabaseName::Main,
            folder.join("inventory.sqlite"),
            None,
        )?;
        copy_tree(&self.journal.directory, &folder.join("inventory.actions"))?;
        let attachments = self.path.parent().unwrap().join("Attachments");
        if attachments.exists() {
            copy_tree(&attachments, &folder.join("Attachments"))?;
        }
        Ok(())
    }
    pub fn import_swift(&mut self, source: &Path) -> Result<()> {
        self.ensure_writable()?;
        ensure!(
            self.data.parts.is_empty()
                && self.data.purchases.is_empty()
                && self.data.projects.is_empty()
                && self.data.storage.is_empty(),
            "Import requires an empty Rust inventory"
        );
        ensure!(
            source.canonicalize()? != self.path.canonicalize()?,
            "Cannot import the active database"
        );
        let conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.execute_batch("BEGIN")?;
        let mut data = load(&conn)?;
        let old_root = source
            .parent()
            .context("Database has no parent directory")?
            .join("Attachments");
        let new_root = self
            .path
            .parent()
            .unwrap()
            .join("Attachments")
            .join(format!("import-{}", id()));
        if old_root.exists() {
            copy_tree(&old_root, &new_root)?;
            let mut value = serde_json::to_value(&data)?;
            relocate(&mut value, &old_root, &new_root);
            data = serde_json::from_value(value)?;
        }
        conn.execute_batch("COMMIT")?;
        prepare_project_ids(&mut data);
        data.validate()?;
        self.persist_logged(&data)?;
        self.revision = self.revision.wrapping_add(1);
        self.changes.send_replace(self.revision);
        Ok(())
    }
    pub fn attach(&self, source: &Path, owner: &str) -> Result<PathBuf> {
        ensure!(source.is_file(), "Attachment must be a file");
        ensure!(
            !owner.contains('/') && !owner.contains('\\') && owner != "..",
            "Invalid attachment owner"
        );
        let dir = self.path.parent().unwrap().join("Attachments").join(owner);
        std::fs::create_dir_all(&dir)?;
        let destination = dir.join(format!(
            "{}-{}",
            id(),
            source
                .file_name()
                .context("Invalid filename")?
                .to_string_lossy()
        ));
        if let Err(error) = std::fs::copy(source, &destination) {
            let _ = std::fs::remove_file(&destination);
            let _ = std::fs::remove_dir(&dir);
            return Err(error.into());
        }
        Ok(destination)
    }
}
fn prepare_project_ids(data: &mut Inventory) {
    for project in &mut data.projects {
        if project.id.is_empty() {
            project.id = project
                .notes_attachment_id
                .as_deref()
                .map(crate::mesh::stable_id)
                .unwrap_or_else(id);
        }
        if project.notes_attachment_id.is_none() {
            project.notes_attachment_id = Some(format!("project-{}", id()));
        }
    }
}
fn exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get::<_, i64>(0),
    )? > 0)
}
fn payloads<T: DeserializeOwned>(conn: &Connection, table: &str) -> Result<Vec<T>> {
    if !exists(conn, table)? {
        return Ok(vec![]);
    }
    let mut statement = conn.prepare(&format!("SELECT payload FROM {table} ORDER BY rowid"))?;
    let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
    rows.map(|r| serde_json::from_str(&r?).with_context(|| format!("Invalid record in {table}")))
        .collect()
}
fn load(conn: &Connection) -> Result<Inventory> {
    let mut data = Inventory {
        parts: payloads(conn, "parts")?,
        allocations: payloads(conn, "allocations")?,
        purchases: payloads(conn, "purchases")?,
        bom: payloads(conn, "project_bom_entries")?,
        ..Default::default()
    };
    for p in &mut data.parts {
        p.migrate();
    }
    for p in &mut data.purchases {
        p.part.migrate();
    }
    for e in &mut data.bom {
        if e.item.spn.is_empty()
            && let Some(v) = e
                .item
                .extra
                .remove("lcsc")
                .and_then(|v| v.as_str().map(str::to_owned))
            && !v.is_empty()
        {
            e.item.spn = v;
            e.item.supplier = "LCSC".into();
        }
    }
    if exists(conn, "projects")? {
        let mut s = conn.prepare("SELECT name,description FROM projects ORDER BY rowid")?;
        data.projects = s
            .query_map([], |r| {
                Ok(Project {
                    name: r.get(0)?,
                    description: r.get(1)?,
                    ..Default::default()
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    if exists(conn, "project_ids")? {
        let mut statement = conn.prepare("SELECT name,id FROM project_ids")?;
        for row in
            statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            let (name, id) = row?;
            if let Some(project) = data
                .projects
                .iter_mut()
                .find(|p| key(&p.name) == key(&name))
            {
                project.id = id;
            }
        }
    }
    if exists(conn, "project_git_tips")? {
        let mut statement = conn.prepare("SELECT project,payload FROM project_git_tips")?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (name, json) = row?;
            if let Some(project) = data
                .projects
                .iter_mut()
                .find(|p| key(&p.name) == key(&name))
            {
                project.git_tips = serde_json::from_str(&json)?;
            }
        }
    }
    if exists(conn, "project_git_remote")? {
        let mut statement = conn.prepare("SELECT project,payload FROM project_git_remote")?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (name, json) = row?;
            if let Some(project) = data
                .projects
                .iter_mut()
                .find(|p| key(&p.name) == key(&name))
            {
                project.git_remote = Some(serde_json::from_str(&json)?);
            }
        }
    }
    if exists(conn, "project_kicad")? {
        let mut statement = conn.prepare("SELECT project,payload FROM project_kicad")?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (name, json) = row?;
            if let Some(project) = data
                .projects
                .iter_mut()
                .find(|p| key(&p.name) == key(&name))
            {
                project.kicad = Some(serde_json::from_str(&json)?);
            }
        }
    }
    if exists(conn, "project_notes")? {
        let mut s = conn.prepare("SELECT project,attachment_id,payload FROM project_notes")?;
        for row in s.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (name, id, json) = row?;
            if let Some(p) = data
                .projects
                .iter_mut()
                .find(|p| key(&p.name) == key(&name))
            {
                p.notes = serde_json::from_str(&json)?;
                p.notes_attachment_id = Some(id);
            }
        }
    }
    if exists(conn, "storage_descriptions")? {
        let mut s = conn.prepare("SELECT name,description FROM storage_descriptions")?;
        data.storage = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
    }
    data.convert_project_references(false);
    for a in data.allocations.clone() {
        data.ensure_project(&a.project)?;
    }
    for p in &data.parts {
        if !p.storage.is_empty() {
            data.storage.entry(p.storage.clone()).or_default();
        }
    }
    Ok(data)
}
pub fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        ensure!(
            !ty.is_symlink(),
            "Attachment backups do not follow symlinks"
        );
        if ty.is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}
fn relocate(value: &mut serde_json::Value, old: &Path, new: &Path) {
    match value {
        serde_json::Value::String(s) => {
            if let Ok(relative) = Path::new(s).strip_prefix(old) {
                *s = new.join(relative).to_string_lossy().into();
            }
        }
        serde_json::Value::Array(a) => {
            for v in a {
                relocate(v, old, new);
            }
        }
        serde_json::Value::Object(o) => {
            for v in o.values_mut() {
                relocate(v, old, new);
            }
        }
        _ => {}
    }
}

fn topology_now() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

fn legacy_metadata<T: DeserializeOwned>(db: &Connection, key: &str) -> Result<Option<T>> {
    let bytes: Option<Vec<u8>> = db
        .query_row("SELECT value FROM metadata WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(bytes
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()?)
}
