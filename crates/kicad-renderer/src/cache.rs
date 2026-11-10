//! Portable prepared-scene cache. The application owns persistence and compression.
use crate::{Geometry, ModelAsset, Scene};
use inventory_kicad::Pcb;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
pub const CACHE_NAME: &str = ".vspace.cache";
pub const MAX_CACHE_BYTES: usize = 256 * 1024 * 1024;
// Bump whenever scene generation, model meshing, fonts, or serialized types change.
const VERSION: u32 = 2;
const MAGIC: &[u8; 8] = b"VINV3D\0\0";
type Key = [u64; 2];
#[derive(Default, Serialize, Deserialize)]
pub struct PreparedCache {
    key: Option<Key>,
    scene: Option<Arc<Scene>>,
    // Indices into scene.meshes avoid storing component geometry twice.
    models: BTreeMap<Key, usize>,
}
pub(crate) fn content_key(bytes: &[u8]) -> Key {
    // Stable content fingerprint, independent of file timestamps and platform.
    let mut a = 0xcbf29ce484222325_u64;
    let mut b = 0x9e3779b97f4a7c15_u64;
    for chunk in bytes.chunks(8) {
        let mut word = [0; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        let n = u64::from_le_bytes(word);
        a = (a ^ n).wrapping_mul(0x100000001b3);
        b = (b ^ n).rotate_left(27).wrapping_mul(0x9ddfea08eb382d69);
    }
    [
        a ^ bytes.len() as u64,
        b ^ (bytes.len() as u64).rotate_left(17),
    ]
}
fn scene_key(board: &Pcb, assets: &BTreeMap<String, ModelAsset>) -> Result<Key, String> {
    let board = bincode::serde::encode_to_vec(board, bincode::config::standard())
        .map_err(|e| e.to_string())?;
    let mut manifest = vec![content_key(&board)];
    for (path, asset) in assets {
        manifest.extend([content_key(path.as_bytes()), content_key(&asset.bytes)]);
    }
    let bytes: Vec<_> = manifest
        .into_iter()
        .flatten()
        .flat_map(u64::to_le_bytes)
        .collect();
    Ok(content_key(&bytes))
}
impl PreparedCache {
    pub fn scene(&self) -> Option<Arc<Scene>> {
        self.scene.clone()
    }
    pub fn matching_scene(
        &self,
        board: &Pcb,
        assets: &BTreeMap<String, ModelAsset>,
    ) -> Option<Arc<Scene>> {
        (self.key == Some(scene_key(board, assets).ok()?))
            .then(|| self.scene.clone())
            .flatten()
    }
    pub(crate) fn model(&self, key: &Key) -> Option<&Geometry> {
        self.scene.as_ref()?.meshes.get(*self.models.get(key)?)
    }
    pub fn prepare(
        &mut self,
        board: &Pcb,
        assets: &BTreeMap<String, ModelAsset>,
        cancel: impl Fn() -> bool,
        progress: impl Fn(&str),
    ) -> Result<Arc<Scene>, String> {
        if cancel() {
            return Err("3D preparation cancelled".into());
        }
        let key = scene_key(board, assets)?;
        if self.key == Some(key)
            && let Some(scene) = &self.scene
        {
            progress("Using .vspace.cache");
            return Ok(scene.clone());
        }
        let mut models = BTreeMap::new();
        let scene = crate::scene::build_internal(
            board,
            assets,
            cancel,
            progress,
            true,
            Some(self),
            &mut models,
        )?;
        self.key = Some(key);
        self.scene = Some(scene.clone());
        self.models = models;
        Ok(scene)
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(VERSION.to_le_bytes());
        bincode::serde::encode_into_std_write(self, &mut bytes, bincode::config::standard())
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Err("3D cache exceeds 256 MB".into());
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_CACHE_BYTES
            || bytes.get(..8) != Some(MAGIC)
            || bytes.get(8..12) != Some(VERSION.to_le_bytes().as_slice())
        {
            return Err("Unsupported 3D cache".into());
        }
        let (cache, read): (Self, _) = bincode::serde::decode_from_slice(
            &bytes[12..],
            bincode::config::standard().with_limit::<MAX_CACHE_BYTES>(),
        )
        .map_err(|e| e.to_string())?;
        if read != bytes.len() - 12 || !cache.valid() {
            return Err("Invalid 3D cache geometry".into());
        }
        Ok(cache)
    }
    fn valid(&self) -> bool {
        let Some(scene) = &self.scene else {
            return self.key.is_none() && self.models.is_empty();
        };
        scene.radius.is_finite()
            && scene.radius > 0.
            && scene.center.iter().all(|n| n.is_finite())
            && scene.meshes.iter().map(|m| m.vertices.len()).sum::<usize>() <= 4_500_000
            && scene.meshes.iter().all(|m| {
                m.vertices.iter().all(|v| {
                    v.position
                        .iter()
                        .chain(&v.normal)
                        .chain(&v.color)
                        .all(|n| n.is_finite())
                }) && m.indices.len() % 3 == 0
                    && m.indices.iter().all(|i| (*i as usize) < m.vertices.len())
            })
            && scene
                .instances
                .iter()
                .all(|i| i.mesh < scene.meshes.len() && i.transform.iter().all(|n| n.is_finite()))
            && self.models.values().all(|i| *i < scene.meshes.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    fn fixture() -> (Pcb, BTreeMap<String, ModelAsset>) {
        let board=Pcb::parse(r#"(kicad_pcb (general (thickness 1.6))
            (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (44 "Edge.Cuts" user))
            (gr_rect (start 0 0) (end 10 10) (stroke (width 0.05) (type default)) (fill none) (layer "Edge.Cuts"))
            (footprint "LED" (layer "F.Cu") (at 2 2) (property "Reference" "D1") (model "one.wrl"))
            (footprint "JST" (layer "F.Cu") (at 5 5) (property "Reference" "J1") (model "two.wrl")))"#).unwrap();
        let assets=[("one.wrl", "1 0 0"),("two.wrl","0 1 0")].into_iter().map(|(path,color)| (path.into(),ModelAsset {name:path.into(),bytes:format!("#VRML V2.0 utf8\nShape {{ appearance Appearance {{ material Material {{ diffuseColor {color} }} }} geometry IndexedFaceSet {{ coord Coordinate {{ point [0 0 0,1 0 0,0 1 0] }} coordIndex [0,1,2,-1] }} }}").into_bytes()})).collect();
        (board, assets)
    }
    #[test]
    fn roundtrip_hits_whole_scene_and_board_changes_reuse_models() {
        let (mut board, mut assets) = fixture();
        let mut cache = PreparedCache::default();
        let original = cache.prepare(&board, &assets, || false, |_| {}).unwrap();
        assert_eq!(cache.models.len(), 2);
        assets.get_mut("one.wrl").unwrap().name = "/relocated/project/one.wrl".into();
        assert!(
            cache.matching_scene(&board, &assets).is_some(),
            "relocation does not change model contents"
        );
        let bytes = cache.encode().unwrap();
        let mut cache = PreparedCache::decode(&bytes).unwrap();
        let loaded = cache.matching_scene(&board, &assets).unwrap();
        assert_ne!(loaded.id(), original.id());
        assert_eq!(loaded.statistics.models, 2);
        assert_eq!(loaded.statistics.triangles, original.statistics.triangles);
        let stages = RefCell::new(Vec::new());
        let hit = cache
            .prepare(
                &board,
                &assets,
                || false,
                |s| stages.borrow_mut().push(s.to_owned()),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&loaded, &hit));
        assert_eq!(stages.borrow().as_slice(), ["Using .vspace.cache"]);
        board.thickness = 2.;
        assert!(cache.matching_scene(&board, &assets).is_none());
        stages.borrow_mut().clear();
        cache
            .prepare(
                &board,
                &assets,
                || false,
                |s| stages.borrow_mut().push(s.to_owned()),
            )
            .unwrap();
        assert_eq!(
            stages
                .borrow()
                .iter()
                .filter(|s| s.starts_with("Reusing model:"))
                .count(),
            2
        );
        assert!(!stages.borrow().iter().any(|s| s.starts_with("Meshing")));
        assets.get_mut("one.wrl").unwrap().bytes.push(b'\n');
        assert!(cache.matching_scene(&board, &assets).is_none());
        stages.borrow_mut().clear();
        cache
            .prepare(
                &board,
                &assets,
                || false,
                |s| stages.borrow_mut().push(s.to_owned()),
            )
            .unwrap();
        assert_eq!(
            stages
                .borrow()
                .iter()
                .filter(|s| s.starts_with("Reusing model:"))
                .count(),
            1
        );
        assert_eq!(
            stages
                .borrow()
                .iter()
                .filter(|s| s.starts_with("Meshing"))
                .count(),
            1
        );
    }
    #[test]
    fn missing_new_models_and_cancellation_do_not_return_stale_geometry() {
        let (board, mut assets) = fixture();
        let removed = assets.remove("two.wrl").unwrap();
        let mut cache = PreparedCache::default();
        assert_eq!(
            cache
                .prepare(&board, &assets, || false, |_| {})
                .unwrap()
                .statistics
                .missing,
            1
        );
        assets.insert("two.wrl".into(), removed);
        assert!(cache.matching_scene(&board, &assets).is_none());
        let before = cache.encode().unwrap();
        assert!(
            cache
                .prepare(&board, &assets, || true, |_| panic!("cancelled work ran"))
                .is_err()
        );
        assert_eq!(before, cache.encode().unwrap());
        assert_eq!(
            cache
                .prepare(&board, &assets, || false, |_| {})
                .unwrap()
                .statistics
                .missing,
            0
        );
        assets.clear();
        assert!(cache.matching_scene(&board, &assets).is_none());
    }
    #[test]
    fn invalid_version_truncation_and_invalid_gpu_indices_are_rejected() {
        let (board, assets) = fixture();
        let mut cache = PreparedCache::default();
        cache.prepare(&board, &assets, || false, |_| {}).unwrap();
        let mut bytes = cache.encode().unwrap();
        assert!(PreparedCache::decode(&bytes[..bytes.len() / 2]).is_err());
        bytes[8] = 255;
        assert!(PreparedCache::decode(&bytes).is_err());
        let scene = Arc::get_mut(cache.scene.as_mut().unwrap()).unwrap();
        scene.meshes[0].indices[0] = u32::MAX;
        assert!(PreparedCache::decode(&cache.encode().unwrap()).is_err());
    }
}
