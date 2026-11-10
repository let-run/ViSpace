//! Portable P-256 identities and signed network membership actions.
//! Signatures use canonical JSON, fixed-width IEEE P1363 ECDSA, and domain separation.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use p256::ecdsa::{
    Signature, SigningKey, VerifyingKey,
    signature::{Signer, Verifier},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const PEER_TTL_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Serialize)]
pub(crate) struct DiscoverPeer {
    pub id: String,
    pub public_key: String,
    pub trusted: bool,
    pub revoked: bool,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    secret: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proof {
    pub public_key: String,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Membership {
    pub id: String,
    pub issuer: String,
    pub subject: String,
    pub public_key: String,
    pub revoked: bool,
    pub timestamp_ms: u64,
    #[serde(default)]
    pub accepted_records: BTreeSet<String>,
    pub proof: Proof,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub actions: BTreeMap<String, Membership>,
}
fn normalized<T: Serialize>(value: &T) -> Result<serde_json::Value> {
    fn visit(v: &mut serde_json::Value) -> Result<()> {
        match v {
            serde_json::Value::Number(n) => {
                let f = n.as_f64().context("Invalid JSON number")?;
                if f.fract() == 0.0 {
                    ensure!(
                        f.abs() <= 9_007_199_254_740_991.0,
                        "Integer exceeds browser-safe signed record range"
                    );
                    *n = serde_json::Number::from(f as i64);
                }
            }
            serde_json::Value::Array(a) => {
                for v in a {
                    visit(v)?
                }
            }
            serde_json::Value::Object(o) => {
                for v in o.values_mut() {
                    visit(v)?
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut value = serde_json::to_value(value)?;
    visit(&mut value)?;
    Ok(value)
}
pub(crate) fn canonical<T: Serialize>(domain: &str, value: &T) -> Result<Vec<u8>> {
    let mut bytes = domain.as_bytes().to_vec();
    bytes.push(0);
    // Value maps use sorted keys on both native and WASM builds.
    bytes.extend(serde_json::to_vec(&normalized(value)?)?);
    Ok(bytes)
}
pub(crate) fn valid_node_id(id: &str) -> bool {
    id.len() == 24 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn node_id(public: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = B64.decode(public)?;
    let key = VerifyingKey::from_sec1_bytes(&bytes)?;
    let hash = Sha256::digest(key.to_encoded_point(false).as_bytes());
    Ok(hash[..12].iter().map(|byte| format!("{byte:02x}")).collect())
}
impl Identity {
    pub fn generate() -> Self {
        loop {
            let mut bytes = [0; 32];
            bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
            bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
            use sha2::{Digest, Sha256};
            let secret = Sha256::digest(bytes);
            if SigningKey::from_slice(&secret).is_ok() {
                return Self {
                    secret: B64.encode(secret),
                };
            }
        }
    }
    fn key(&self) -> Result<SigningKey> {
        Ok(SigningKey::from_slice(&B64.decode(&self.secret)?)?)
    }
    pub fn public_key(&self) -> Result<String> {
        Ok(B64.encode(
            self.key()?
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes(),
        ))
    }
    pub fn id(&self) -> Result<String> {
        node_id(&self.public_key()?)
    }
    pub fn sign<T: Serialize>(&self, domain: &str, value: &T) -> Result<Proof> {
        let signature: Signature = self.key()?.sign(&canonical(domain, value)?);
        Ok(Proof {
            public_key: self.public_key()?,
            signature: B64.encode(signature.to_bytes()),
        })
    }
    pub fn membership(&self, public_key: String, revoked: bool) -> Result<Membership> {
        let mut m = Membership {
            id: uuid::Uuid::new_v4().to_string(),
            issuer: self.id()?,
            subject: node_id(&public_key)?,
            public_key,
            revoked,
            timestamp_ms: crate::action_log::timestamp_now(),
            accepted_records: BTreeSet::new(),
            proof: Proof {
                public_key: String::new(),
                signature: String::new(),
            },
        };
        m.proof = self.sign("vinventory.membership.v1", &m.payload())?;
        Ok(m)
    }
    pub fn sign_membership(&self, m: &mut Membership) -> Result<()> {
        m.proof = self.sign("vinventory.membership.v1", &m.payload())?;
        Ok(())
    }
    pub fn sign_record(&self, record: &mut crate::action_log::Record) -> Result<()> {
        record.proof = None;
        *record = serde_json::from_value(normalized(record)?)?;
        record.proof = Some(self.sign("vinventory.action.v1", record)?);
        Ok(())
    }
}
pub fn verify<T: Serialize>(domain: &str, value: &T, proof: &Proof) -> Result<String> {
    let key = VerifyingKey::from_sec1_bytes(&B64.decode(&proof.public_key)?)?;
    let sig = Signature::from_slice(&B64.decode(&proof.signature)?)?;
    key.verify(&canonical(domain, value)?, &sig)
        .context("Invalid signature")?;
    node_id(&proof.public_key)
}
pub fn record_fingerprint(record: &crate::action_log::Record) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(
        Sha256::digest(canonical("vinventory.accepted-action.v1", record)?)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}
pub fn verify_record(record: &crate::action_log::Record) -> Result<String> {
    let mut payload = record.clone();
    let proof = payload.proof.take().context("Unsigned action rejected")?;
    let id = verify("vinventory.action.v1", &payload, &proof)?;
    ensure!(
        record.mesh.as_ref().is_some_and(|m| m.author() == id),
        "Action author does not match signing key"
    );
    Ok(id)
}
impl Membership {
    fn payload(&self) -> serde_json::Value {
        serde_json::json!({"id":self.id,"issuer":self.issuer,"subject":self.subject,"public_key":self.public_key,"revoked":self.revoked,"timestamp_ms":self.timestamp_ms,"accepted_records":self.accepted_records})
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.issuer == verify("vinventory.membership.v1", &self.payload(), &self.proof)?,
            "Invalid membership issuer"
        );
        ensure!(
            self.subject == node_id(&self.public_key)?,
            "Invalid membership subject"
        );
        uuid::Uuid::parse_str(&self.id)?;
        Ok(())
    }
}
impl Ledger {
    pub fn merge(&mut self, actions: Vec<Membership>) -> Result<bool> {
        ensure!(
            self.actions
                .keys()
                .chain(actions.iter().map(|a| &a.id))
                .collect::<BTreeSet<_>>()
                .len()
                <= 20000,
            "Membership log too large"
        );
        for a in &actions {
            a.validate()?;
            if let Some(old) = self.actions.get(&a.id) {
                ensure!(
                    serde_json::to_value(old)? == serde_json::to_value(a)?,
                    "Membership ID reused"
                );
            }
        }
        let mut changed = false;
        for a in actions {
            changed |= self.actions.insert(a.id.clone(), a).is_none();
        }
        Ok(changed)
    }
    fn historical_members(&self, root: &str) -> BTreeSet<String> {
        let mut ever = BTreeSet::from([root.to_owned()]);
        loop {
            let n = ever.len();
            for a in self.actions.values() {
                if !a.revoked && ever.contains(&a.issuer) {
                    ever.insert(a.subject.clone());
                }
            }
            if n == ever.len() {
                break;
            }
        }
        ever
    }
    fn denied(&self, root: &str) -> BTreeSet<String> {
        let ever = self.historical_members(root);
        let denied: BTreeSet<_> = self
            .actions
            .values()
            .filter(|a| a.revoked && ever.contains(&a.issuer))
            .map(|a| a.subject.clone())
            .collect();
        denied
    }
    pub fn revoked(&self, root: &str, id: &str) -> bool {
        self.denied(root).contains(id)
    }
    pub(crate) fn discover_peers(
        &self,
        root: &str,
        mut seen: BTreeMap<String, (String, u64)>,
    ) -> Vec<DiscoverPeer> {
        let approved = self.members(root);
        let historical_members = self.historical_members(root);
        let mut revoked = BTreeMap::<String, u64>::new();
        for action in self.actions.values() {
            for (id, key) in [
                (&action.subject, &action.public_key),
                (&action.issuer, &action.proof.public_key),
            ] {
                let peer = seen.entry(id.clone()).or_insert_with(|| (key.clone(), 0));
                peer.1 = peer.1.max(action.timestamp_ms);
            }
            if action.revoked && historical_members.contains(&action.issuer) {
                revoked.entry(action.subject.clone())
                    .and_modify(|time| *time = (*time).min(action.timestamp_ms))
                    .or_insert(action.timestamp_ms);
            }
        }
        seen.remove(root);
        seen.into_iter().map(|(id, (public_key, last_seen))| {
            let trusted = approved.contains(&id);
            let revoked_at = revoked.get(&id).copied();
            DiscoverPeer {
                id,
                public_key,
                trusted,
                revoked: revoked_at.is_some(),
                expires_at: (!trusted).then(|| revoked_at.unwrap_or(last_seen).saturating_add(PEER_TTL_MS)),
            }
        }).collect()
    }
    pub fn merge_for(&mut self, root: &str, actions: Vec<Membership>) -> Result<bool> {
        let denied = self.denied(root);
        ensure!(
            actions
                .iter()
                .all(|a| self.actions.contains_key(&a.id) || !denied.contains(&a.issuer)),
            "New membership action from a revoked identity rejected"
        );
        self.merge(actions)
    }
    pub fn members(&self, root: &str) -> BTreeSet<String> {
        let mut members = BTreeSet::from([root.to_owned()]);
        // Grants form a delegation graph. No self-signed remote root becomes trusted automatically.
        loop {
            let before = members.len();
            for a in self.actions.values() {
                if !a.revoked && members.contains(&a.issuer) {
                    members.insert(a.subject.clone());
                }
            }
            if members.len() == before {
                break;
            }
        }
        let revoked: BTreeSet<_> = self
            .actions
            .values()
            .filter(|a| a.revoked && members.contains(&a.issuer))
            .map(|a| a.subject.clone())
            .collect();
        // Recompute reachability so a revoked member cannot sponsor new nodes.
        members = BTreeSet::new();
        if !revoked.contains(root) {
            members.insert(root.into());
        }
        loop {
            let before = members.len();
            for a in self.actions.values() {
                if !a.revoked && !revoked.contains(&a.subject) && members.contains(&a.issuer) {
                    members.insert(a.subject.clone());
                }
            }
            if members.len() == before {
                break;
            }
        }
        members
    }
    pub fn trusted(&self, root: &str, id: &str) -> bool {
        self.members(root).contains(id)
    }
    pub fn allows_record(&self, root: &str, record: &crate::action_log::Record) -> Result<bool> {
        let author = verify_record(record)?;
        if self.trusted(root, &author) {
            return Ok(true);
        }
        let hash = record_fingerprint(record)?;
        // A revocation signs the exact already accepted records, not a forgeable timestamp/counter.
        Ok(self.actions.values().any(|a| {
            a.revoked
                && a.subject == author
                && self.trusted(root, &a.issuer)
                && a.accepted_records.contains(&hash)
        }))
    }
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub struct BrowserIdentity {
    identity: Identity,
    ledger: Ledger,
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
impl BrowserIdentity {
    #[wasm_bindgen::prelude::wasm_bindgen(constructor)]
    pub fn new(saved: &str, ledger: &str) -> Result<BrowserIdentity, wasm_bindgen::JsValue> {
        (|| -> Result<_> {
            let identity = if saved.is_empty() {
                Identity::generate()
            } else {
                serde_json::from_str(saved)?
            };
            identity.key()?;
            let ledger = if ledger.is_empty() {
                Ledger::default()
            } else {
                serde_json::from_str(ledger)?
            };
            Ok(Self { identity, ledger })
        })()
        .map_err(js_error)
    }
    pub fn saved(&self) -> String {
        serde_json::to_string(&self.identity).unwrap()
    }
    pub fn id(&self) -> String {
        self.identity.id().unwrap()
    }
    pub fn public_key(&self) -> String {
        self.identity.public_key().unwrap()
    }
    pub fn ledger(&self) -> String {
        serde_json::to_string(&self.ledger.actions.values().collect::<Vec<_>>()).unwrap()
    }
    pub fn storage(&self) -> String {
        serde_json::to_string(&self.ledger).unwrap()
    }
    pub fn sign(&self, domain: &str, payload: &str) -> Result<String, wasm_bindgen::JsValue> {
        (|| -> Result<_> {
            Ok(serde_json::to_string(&self.identity.sign(
                domain,
                &serde_json::from_str::<serde_json::Value>(payload)?,
            )?)?)
        })()
        .map_err(js_error)
    }
    pub fn verify(
        &self,
        domain: &str,
        payload: &str,
        proof: &str,
    ) -> Result<String, wasm_bindgen::JsValue> {
        (|| -> Result<_> {
            verify(
                domain,
                &serde_json::from_str::<serde_json::Value>(payload)?,
                &serde_json::from_str(proof)?,
            )
        })()
        .map_err(js_error)
    }
    pub fn trusted(&self, id: &str) -> bool {
        self.ledger.trusted(&self.id(), id)
    }
    pub fn merge(&mut self, actions: &str) -> Result<bool, wasm_bindgen::JsValue> {
        (|| -> Result<_> {
            self.ledger
                .merge_for(&self.id(), serde_json::from_str(actions)?)
        })()
        .map_err(js_error)
    }
    pub fn discover_peers(&self, seen: &str) -> Result<String, wasm_bindgen::JsValue> {
        (|| -> Result<_> {
            let peers = self.ledger.discover_peers(&self.id(), serde_json::from_str(seen)?);
            Ok(serde_json::to_string(&peers)?)
        })().map_err(js_error)
    }
    pub fn approve(
        &mut self,
        public: &str,
        revoked: bool,
        history: &str,
    ) -> Result<(), wasm_bindgen::JsValue> {
        (|| -> Result<_> {
            ensure!(self.trusted(&self.id()), "This identity has been revoked");
            let mut m = self.identity.membership(public.into(), revoked)?;
            ensure!(
                revoked || !self.ledger.revoked(&self.id(), &m.subject),
                "This key is permanently revoked; a new identity is required to rejoin"
            );
            if !revoked && self.trusted(&m.subject) {
                return Ok(());
            }
            if revoked {
                for r in serde_json::from_str::<Vec<crate::action_log::Record>>(history)? {
                    if verify_record(&r)? == m.subject {
                        m.accepted_records.insert(record_fingerprint(&r)?);
                    }
                }
                self.identity.sign_membership(&mut m)?;
            }
            self.ledger.merge(vec![m])?;
            Ok(())
        })()
        .map_err(js_error)
    }
    pub fn sign_record(&self, record: &str) -> Result<String, wasm_bindgen::JsValue> {
        (|| -> Result<_> {
            let mut r = serde_json::from_str(record)?;
            self.identity.sign_record(&mut r)?;
            Ok(serde_json::to_string(&r)?)
        })()
        .map_err(js_error)
    }
    pub fn verify_record(&self, record: &str) -> Result<(), wasm_bindgen::JsValue> {
        (|| -> Result<_> {
            ensure!(
                self.ledger
                    .allows_record(&self.id(), &serde_json::from_str(record)?)?,
                "Unapproved or revoked action author"
            );
            Ok(())
        })()
        .map_err(js_error)
    }
}
#[cfg(target_arch = "wasm32")]
fn js_error(e: anyhow::Error) -> wasm_bindgen::JsValue {
    wasm_bindgen::JsValue::from_str(&e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn discovery_expiry_keeps_approved_peers_and_preserves_revocation() -> Result<()> {
        use super::*;
        let owner = Identity::generate();
        let approved = Identity::generate();
        let revoked = Identity::generate();
        let unknown = Identity::generate();
        let mut revoke = owner.membership(revoked.public_key()?, true)?;
        revoke.timestamp_ms = 10;
        owner.sign_membership(&mut revoke)?;

        let mut ledger = Ledger::default();
        ledger.merge(vec![owner.membership(approved.public_key()?, false)?, revoke])?;
        let seen = BTreeMap::from([
            (revoked.id()?, (revoked.public_key()?, PEER_TTL_MS)),
            (unknown.id()?, (unknown.public_key()?, 20)),
        ]);
        let rows = ledger.discover_peers(&owner.id()?, seen);
        let by_id: BTreeMap<_, _> = rows.iter().map(|row| (row.id.as_str(), row)).collect();
        assert_eq!(by_id[approved.id()?.as_str()].expires_at, None);
        assert_eq!(by_id[revoked.id()?.as_str()].expires_at, Some(10 + PEER_TTL_MS));
        assert_eq!(by_id[unknown.id()?.as_str()].expires_at, Some(20 + PEER_TTL_MS));
        assert!(ledger.revoked(&owner.id()?, &revoked.id()?));
        Ok(())
    }

    use super::*;
    #[test]
    fn json_integer_float_roundtrip() -> Result<()> {
        let identity = Identity::generate();
        let value = serde_json::json!({"price":1.0,"zero":-0.0,"fraction":0.0374});
        let proof = identity.sign("test", &value)?;
        assert!(
            verify(
                "test",
                &serde_json::json!({"fraction":0.0374,"zero":0,"price":1}),
                &proof
            )
            .is_ok()
        );
        assert!(
            identity
                .sign("test", &serde_json::json!(9007199254740992u64))
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn signatures_and_delegation() -> Result<()> {
        let a = Identity::generate();
        let b = Identity::generate();
        let c = Identity::generate();
        let mut l = Ledger::default();
        l.merge(vec![b.membership(c.public_key()?, false)?])?;
        assert!(!l.trusted(&a.id()?, &c.id()?));
        l.merge(vec![a.membership(b.public_key()?, false)?])?;
        assert!(l.trusted(&a.id()?, &c.id()?));
        l.merge(vec![a.membership(b.public_key()?, true)?])?;
        assert!(!l.trusted(&a.id()?, &b.id()?));
        assert!(!l.trusted(&a.id()?, &c.id()?));
        let p = a.sign("hello", &"challenge")?;
        assert!(verify("hello", &"replay", &p).is_err());
        assert!(verify("other", &"challenge", &p).is_err());
        Ok(())
    }
}
