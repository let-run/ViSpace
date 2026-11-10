//! File/transport envelope shared by desktop and browser prepared-scene caches.
use inventory_kicad_renderer::cache::{MAX_CACHE_BYTES, PreparedCache};
use std::io::{Read, Write};
const MAX_COMPRESSED: usize = 128 * 1024 * 1024;
pub fn pack(cache: &PreparedCache) -> anyhow::Result<Vec<u8>> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&cache.encode().map_err(anyhow::Error::msg)?)?;
    let bytes = encoder.finish()?;
    anyhow::ensure!(
        bytes.len() <= MAX_COMPRESSED,
        "3D cache exceeds 128 MB compressed"
    );
    Ok(bytes)
}
pub fn unpack(bytes: &[u8]) -> anyhow::Result<PreparedCache> {
    anyhow::ensure!(
        bytes.len() <= MAX_COMPRESSED,
        "3D cache exceeds 128 MB compressed"
    );
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .take(MAX_CACHE_BYTES as u64 + 1)
        .read_to_end(&mut raw)?;
    PreparedCache::decode(&raw).map_err(anyhow::Error::msg)
}
#[cfg(not(target_arch = "wasm32"))]
pub fn read(path: &std::path::Path) -> anyhow::Result<PreparedCache> {
    unpack(&crate::project_cache::read(
        path,
        crate::project_cache::Section::Scene,
    )?)
}
#[cfg(not(target_arch = "wasm32"))]
pub fn write(path: &std::path::Path, cache: &PreparedCache) -> anyhow::Result<()> {
    crate::project_cache::write(path, crate::project_cache::Section::Scene, &pack(cache)?)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn cache_file_is_replaceable_and_corruption_is_a_recoverable_miss() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join(inventory_kicad_renderer::cache::CACHE_NAME);
        assert!(read(&path).is_err());
        let cache = PreparedCache::default();
        write(&path, &cache).unwrap();
        assert_eq!(
            read(&path).unwrap().encode().unwrap(),
            cache.encode().unwrap()
        );
        std::fs::write(&path, b"interrupted or old cache").unwrap();
        assert!(read(&path).is_err());
        write(&path, &cache).unwrap();
        assert!(read(&path).is_ok());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        assert!(write(&temp.path().join("absent/.vspace.cache"), &cache).is_err());
        assert!(read(&path).is_ok());
    }
}
