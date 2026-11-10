use crate::model::*;
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
pub type SharedStore = Arc<Mutex<Store>>;
pub struct Store {
    pub data: Inventory,
    pub path: PathBuf,
    revision: u64,
    last_saved: Option<String>,
}
impl Store {
    pub fn open(_path: impl AsRef<Path>) -> Result<Self> {
        let saved = crate::web::host_inventory_snapshot();
        let data=match &saved {Some(value)=>serde_json::from_str(value).context("Saved browser inventory could not be read. Export it before resetting browser storage.")?,None=>Inventory::default()};
        let store = Self {
            data,
            path: PathBuf::from("/browser/inventory.sqlite"),
            revision: 0,
            last_saved: saved,
        };
        store.data.validate()?;
        Ok(store)
    }
    pub fn accept_persisted(&mut self, expected: &str, actual: String) -> Result<()> {
        if self.last_saved.as_deref() != Some(expected) {
            return Ok(());
        }
        let data: Inventory = serde_json::from_str(&actual)?;
        data.validate()?;
        self.data = data;
        self.last_saved = Some(actual);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
    pub fn accept_remote(&mut self, actual: String) -> Result<()> {
        if self.last_saved.as_deref() == Some(&actual) {
            return Ok(());
        }
        let data: Inventory = serde_json::from_str(&actual)?;
        data.validate()?;
        self.data = data;
        self.last_saved = Some(actual);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
    pub fn local_head_hash(&self) -> String {
        crate::web::host_local_head_hash()
    }
    pub fn network_graph(&mut self) -> crate::topology::Graph {
        serde_json::from_str(&crate::web::host_network_graph()).unwrap_or_default()
    }
    pub fn uses_write_approval(&self) -> bool {
        serde_json::from_str::<crate::topology::Graph>(&crate::web::host_network_graph())
            .is_ok_and(|graph| graph.managed)
    }
    pub fn recovery_required(&self) -> bool {
        false
    }
    /// UI reconciliation must wait until the latest host projection has reached
    /// the Rust store, including the interval after sync completes its promise.
    pub fn background_save_ready(&self) -> bool {
        !crate::web::host_inventory_save_busy()
            && crate::web::host_inventory_snapshot() == self.last_saved
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn change<T>(&mut self, op: impl FnOnce(&mut Inventory) -> Result<T>) -> Result<T> {
        let mut next = self.data.clone();
        let result = op(&mut next)?;
        self.commit(next)?;
        Ok(result)
    }
    fn commit(&mut self, mut next: Inventory) -> Result<()> {
        next.reconcile_designator_priority(&self.data)?;
        next.validate()?;
        let serialized = serde_json::to_string(&next)?;
        crate::web::persist_inventory(&serialized, self.last_saved.as_deref())?;
        self.last_saved = Some(serialized);
        self.data = next;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
    pub fn commit_if_revision(&mut self, expected: u64, next: Inventory) -> Result<()> {
        ensure!(
            self.revision == expected,
            "Inventory changed during this operation. Review it again."
        );
        self.commit(next)
    }
    pub fn backup(&self, _folder: &Path) -> Result<()> {
        crate::web::download(
            "inventory.json",
            &serde_json::to_vec_pretty(&self.data)?,
            "application/json",
        )
    }
    pub fn import_swift(&mut self, _source: &Path) -> Result<()> {
        anyhow::bail!("Export parts to CSV in the desktop app, then import that CSV in the browser")
    }
    pub fn attach(&self, source: &Path, _owner: &str) -> Result<PathBuf> {
        if source.to_string_lossy().starts_with("data:") {
            Ok(source.into())
        } else {
            anyhow::bail!("Choose an image in the browser first")
        }
    }
}
