use crate::network_identity::{Identity, Ledger, Membership};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
type PairingPeers = Arc<Mutex<BTreeMap<String, usize>>>;

pub(crate) struct PairingConnection {
    peers: PairingPeers,
    id: String,
    changes: tokio::sync::watch::Sender<u64>,
}
impl Drop for PairingConnection {
    fn drop(&mut self) {
        let mut peers = self.peers.lock().unwrap();
        if let Some(count) = peers.get_mut(&self.id) {
            *count -= 1;
            if *count == 0 {
                peers.remove(&self.id);
            }
        }
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }
}
type CodeRequests = Arc<Mutex<BTreeMap<String, (String, tokio::sync::watch::Sender<bool>)>>>;

pub(crate) struct CodeRequest {
    peers: CodeRequests,
    id: String,
}
impl Drop for CodeRequest {
    fn drop(&mut self) {
        self.peers.lock().unwrap().remove(&self.id);
    }
}

pub struct Security {
    pub identity: Identity,
    pub ledger: Ledger,
    pub pending: BTreeMap<String, (String, u64)>,
    pairing: PairingPeers,
    codes: CodeRequests,
}
impl Security {
    pub fn open(db: &rusqlite::Connection) -> Result<Self> {
        db.execute_batch("CREATE TABLE IF NOT EXISTS network_identity(singleton INTEGER PRIMARY KEY,secret TEXT NOT NULL); CREATE TABLE IF NOT EXISTS network_actions(id TEXT PRIMARY KEY,payload TEXT NOT NULL); CREATE TABLE IF NOT EXISTS network_tls_pins(node TEXT PRIMARY KEY,origin TEXT UNIQUE,pin TEXT NOT NULL);")?;
        use rusqlite::OptionalExtension;
        let saved: Option<String> = db
            .query_row(
                "SELECT secret FROM network_identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let identity = if let Some(s) = saved {
            serde_json::from_str(&s)?
        } else {
            let i = Identity::generate();
            db.execute(
                "INSERT INTO network_identity VALUES(1,?1)",
                [serde_json::to_string(&i)?],
            )?;
            i
        };
        let mut stmt = db.prepare("SELECT payload FROM network_actions")?;
        let actions = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut ledger = Ledger::default();
        ledger.merge(
            actions
                .into_iter()
                .map(|s| serde_json::from_str(&s))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        )?;
        Ok(Self {
            identity,
            ledger,
            pending: BTreeMap::new(),
            pairing: PairingPeers::default(),
            codes: CodeRequests::default(),
        })
    }
}
impl crate::store::Store {
    pub fn network_identity(&self) -> Identity {
        self.security.identity.clone()
    }
    pub fn network_actions(&self) -> Vec<Membership> {
        self.security.ledger.actions.values().cloned().collect()
    }
    pub fn network_trusted(&self, id: &str) -> bool {
        self.security.ledger.trusted(self.application_id(), id)
    }
    pub fn network_pending(&self) -> Vec<(String, String)> {
        let now = crate::action_log::timestamp_now();
        self.network_discover_peers().into_iter()
            .filter(|peer| peer.expires_at.is_none_or(|expires| expires > now))
            .map(|peer| (peer.id, peer.public_key))
            .collect()
    }
    pub(crate) fn network_discover_peers(&self) -> Vec<crate::network_identity::DiscoverPeer> {
        self.security.ledger.discover_peers(self.application_id(), self.security.pending.clone())
    }
    pub(crate) fn network_pairing_ids(&self) -> BTreeSet<String> {
        self.security.pairing.lock().unwrap().keys().cloned().collect()
    }
    pub(crate) fn begin_network_pairing(&self, id: String) -> PairingConnection {
        let peers = self.security.pairing.clone();
        *peers.lock().unwrap().entry(id.clone()).or_default() += 1;
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
        PairingConnection { peers, id, changes: self.changes.clone() }
    }
    pub(crate) fn begin_pairing_code(&self, id: &str, code: String) -> Result<(CodeRequest, tokio::sync::watch::Receiver<bool>)> {
        let mut peers = self.security.codes.lock().unwrap();
        ensure!(!peers.contains_key(id), "This node already has a pairing request");
        let (approval, receiver) = tokio::sync::watch::channel(false);
        peers.insert(id.to_owned(), (code, approval));
        Ok((CodeRequest { peers: self.security.codes.clone(), id: id.to_owned() }, receiver))
    }

    pub(crate) fn pairing_code(&self, id: &str) -> Option<Option<String>> {
        self.security.codes.lock().unwrap().get(id).map(|(code, approval)| {
            (*approval.borrow()).then(|| code.clone())
        })
    }

    pub(crate) fn show_pairing_code(&self, id: &str) -> bool {
        self.security.codes.lock().unwrap().get(id).is_some_and(|(_, approval)| {
            approval.send_replace(true);
            true
        })
    }

    pub(crate) fn save_network_tls_pin(&self, id: &str, origin: Option<&str>, pin: &str) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        if let Some(origin) = origin {
            transaction.execute(
                "UPDATE network_tls_pins SET origin=NULL WHERE origin=?1 AND node<>?2",
                rusqlite::params![origin, id],
            )?;
        }
        transaction.execute(
            "INSERT INTO network_tls_pins(node, origin, pin) VALUES(?1, ?2, ?3)
             ON CONFLICT(node) DO UPDATE SET origin=COALESCE(excluded.origin, origin), pin=excluded.pin",
            rusqlite::params![id, origin, pin],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn network_tls_pin(&self, origin: &str, peer: Option<&str>) -> Result<Option<(String, String)>> {
        use rusqlite::OptionalExtension;
        let pin: Option<(String, String)> = self.connection.query_row(
            "SELECT node, pin FROM network_tls_pins WHERE
             CASE WHEN ?1 IS NULL THEN origin=?2 ELSE node=?1 END",
            rusqlite::params![peer, origin],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        Ok(pin.filter(|(id, _)| self.network_trusted(id)))
    }

    pub fn network_seen(&mut self, public: String) -> Result<()> {
        let id = crate::network_identity::node_id(&public)?;
        let now = crate::action_log::timestamp_now();
        self.security.pending.retain(|_, (_, seen)| {
            now.saturating_sub(*seen) < crate::network_identity::PEER_TTL_MS
        });
        self.security.pending.insert(id, (public, now));
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
        Ok(())
    }
    pub fn network_merge(&mut self, actions: Vec<Membership>) -> Result<()> {
        let mut next = self.security.ledger.clone();
        if !next.merge_for(self.application_id(), actions.clone())? {
            return Ok(());
        }
        let tx = self.connection.transaction()?;
        for a in actions {
            tx.execute(
                "INSERT OR IGNORE INTO network_actions VALUES(?1,?2)",
                rusqlite::params![a.id, serde_json::to_string(&a)?],
            )?;
        }
        tx.commit()?;
        self.security.ledger = next;
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
        Ok(())
    }
    pub fn network_revoked(&self, id: &str) -> bool {
        self.security.ledger.revoked(self.application_id(), id)
    }
    pub fn network_authorize(&mut self, public: String, revoke: bool) -> Result<()> {
        if !revoke && self.network_trusted(&crate::network_identity::node_id(&public)?) {
            return Ok(());
        }
        self.commit_network_membership(public, revoke, None).map(|_| ())
    }

    // An outbound connection needs a local trust anchor before a shared history exists.
    pub(crate) fn network_bootstrap_peer(&mut self, public: String) -> Result<()> {
        let action = self.security.identity.membership(public, false)?;
        if self.network_trusted(self.application_id())
            && !self.network_trusted(&action.subject)
            && !self.network_revoked(&action.subject)
        {
            self.network_merge(vec![action])?;
        }
        Ok(())
    }

    pub(crate) fn commit_network_membership(
        &mut self,
        public: String,
        revoke: bool,
        request_id: Option<&str>,
    ) -> Result<crate::action_log::Record> {
        if self.uses_component() {
            let leader = self.component_leader();
            ensure!(
                leader.as_deref() == Some(self.application_id()) && !self.component_locked(),
                "The current sync leader must commit membership changes"
            );
        }
        ensure!(
            self.network_trusted(self.application_id()),
            "This identity has been revoked"
        );
        let mut action = self.security.identity.membership(public, revoke)?;
        if revoke {
            for r in self.records_after(None)? {
                if crate::network_identity::verify_record(&r)? == action.subject {
                    action
                        .accepted_records
                        .insert(crate::network_identity::record_fingerprint(&r)?);
                }
            }
        } else {
            self.security.ledger.reinstate(self.application_id(), &mut action);
        }
        self.security.identity.sign_membership(&mut action)?;
        ensure!(
            action.subject != self.application_id(),
            "Cannot change your own membership"
        );
        let mut record = self.make_membership_record()?;
        if let Some(id) = request_id {
            record.id = id.to_owned();
        }
        record.actions.push(crate::action_log::Action {
            kind: if revoke { "node_revoked" } else { "node_approved" }.into(),
            entity: "network_membership".into(),
            key: action.subject.clone(),
            label: action.subject.clone(),
            value: Some(serde_json::to_value(action)?),
            changes: vec![],
            position: None,
        });
        self.security.identity.sign_record(&mut record)?;
        self.replay_record(&record, None)?;
        Ok(record)
    }
    pub fn network_verify_record(&self, record: &crate::action_log::Record) -> Result<()> {
        ensure!(
            self.security
                .ledger
                .allows_record(self.application_id(), record)?,
            "Unapproved or revoked action author"
        );
        Ok(())
    }
}
