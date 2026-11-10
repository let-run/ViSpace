//! Parsed PCB snapshots validated using cheap source metadata and a small header.
use anyhow::{Result, ensure};
use inventory_kicad::Pcb;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read};
pub const PARSER_VERSION: u32 = 2;
const MAGIC: &[u8; 8] = b"VINVPCB1";
const RAW_LIMIT: usize = 256 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceStamp {
    pub path: String,
    pub size: u64,
    pub modified: String,
    pub header: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
struct Metadata {
    version: u32,
    sources: Vec<SourceStamp>,
}
pub fn pack(board: &Pcb, sources: &[SourceStamp]) -> Result<Vec<u8>> {
    let metadata = serde_json::to_vec(&Metadata {
        version: PARSER_VERSION,
        sources: sources.into(),
    })?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend((metadata.len() as u32).to_le_bytes());
    bytes.extend(metadata);
    let mut encoder = flate2::write::ZlibEncoder::new(bytes, flate2::Compression::fast());
    bincode::serde::encode_into_std_write(board, &mut encoder, bincode::config::standard())?;
    let bytes = encoder.finish()?;
    ensure!(
        bytes.len() <= crate::project_cache::LIMIT,
        "Parsed PCB cache exceeds size limit"
    );
    Ok(bytes)
}
pub fn unpack(bytes: &[u8], sources: &[SourceStamp]) -> Result<Pcb> {
    ensure!(
        !sources.is_empty() && bytes.len() <= crate::project_cache::LIMIT,
        "PCB cache has no source metadata"
    );
    ensure!(
        bytes.get(..8) == Some(MAGIC),
        "Unsupported parsed PCB cache"
    );
    let len = u32::from_le_bytes(
        bytes
            .get(8..12)
            .ok_or_else(|| anyhow::anyhow!("Truncated PCB cache"))?
            .try_into()?,
    ) as usize;
    ensure!(len <= 64 * 1024, "Invalid PCB cache metadata");
    let metadata: Metadata = serde_json::from_slice(
        bytes
            .get(12..12 + len)
            .ok_or_else(|| anyhow::anyhow!("Truncated PCB metadata"))?,
    )?;
    ensure!(
        metadata.version == PARSER_VERSION && metadata.sources == sources,
        "PCB source files changed"
    );
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&bytes[12 + len..])
        .take(RAW_LIMIT as u64 + 1)
        .read_to_end(&mut raw)?;
    ensure!(
        raw.len() <= RAW_LIMIT,
        "Parsed PCB cache exceeds size limit"
    );
    let (board, read): (Pcb, _) = bincode::serde::decode_from_slice(
        &raw,
        bincode::config::standard().with_limit::<RAW_LIMIT>(),
    )?;
    ensure!(read == raw.len(), "Trailing PCB cache data");
    Ok(board)
}
pub fn sources(project: &str) -> Vec<String> {
    let stem = project.strip_suffix(".kicad_pro").unwrap_or(project);
    vec![
        project.into(),
        format!("{stem}.kicad_pcb"),
        format!("{stem}.kicad_dru"),
    ]
}
#[cfg(not(target_arch = "wasm32"))]
fn stamps(project: &std::path::Path) -> Result<Vec<SourceStamp>> {
    let parent = project
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let name = project
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Project filename is missing"))?
        .to_string_lossy();
    sources(&name)
        .into_iter()
        .map(|name| {
            let path = parent.join(&name);
            let mut file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e)
                    if e.kind() == std::io::ErrorKind::NotFound && name.ends_with(".kicad_dru") =>
                {
                    return Ok(SourceStamp {
                        path: name,
                        size: 0,
                        modified: "missing".into(),
                        header: vec![],
                    });
                }
                Err(e) => return Err(e.into()),
            };
            let meta = file.metadata()?;
            ensure!(
                meta.len() <= inventory_kicad::MAX_FILE_BYTES as u64,
                "KiCad file exceeds 32 MB"
            );
            let modified = meta
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
                .to_string();
            let mut header = Vec::new();
            (&mut file).take(512).read_to_end(&mut header)?;
            Ok(SourceStamp {
                path: name,
                size: meta.len(),
                modified,
                header,
            })
        })
        .collect()
}
#[cfg(not(target_arch = "wasm32"))]
pub fn load(project: &std::path::Path, cancel: &std::sync::atomic::AtomicBool) -> Result<Pcb> {
    let parent = project
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let cache = parent.join(crate::project_cache::NAME);
    let before = stamps(project)?;
    ensure!(
        !cancel.load(std::sync::atomic::Ordering::Relaxed),
        "Loading cancelled"
    );
    if let Ok(bytes) = crate::project_cache::read(&cache, crate::project_cache::Section::Pcb)
        && let Ok(board) = unpack(&bytes, &before)
        && stamps(project)? == before
    {
        return Ok(board);
    }
    let name = project
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Project filename is missing"))?
        .to_string_lossy();
    let board = inventory_kicad::load_board(&name, |name| {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(inventory_kicad::Error::new("Loading cancelled"));
        }
        let path = parent.join(name);
        let meta =
            std::fs::metadata(&path).map_err(|e| inventory_kicad::Error::new(e.to_string()))?;
        if meta.len() > inventory_kicad::MAX_FILE_BYTES as u64 {
            return Err(inventory_kicad::Error::new("KiCad file exceeds 32 MB"));
        }
        std::fs::read_to_string(path).map_err(|e| inventory_kicad::Error::new(e.to_string()))
    })?;
    if !cancel.load(std::sync::atomic::Ordering::Relaxed)
        && stamps(project).is_ok_and(|after| after == before)
        && let Ok(bytes) = pack(&board, &before)
    {
        let _ = crate::project_cache::write(&cache, crate::project_cache::Section::Pcb, &bytes);
    }
    Ok(board)
}
/// Worker path: source bytes are only supplied on a cache miss (or corrupt hit).
pub fn worker(
    path: &str,
    files: &BTreeMap<String, String>,
    stamps: &[SourceStamp],
    cache: Option<&str>,
) -> Result<serde_json::Value> {
    use base64::Engine;
    if let Some(encoded) = cache
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded)
        && let Ok(board) = unpack(&bytes, stamps)
    {
        return Ok(serde_json::json!({"board":board}));
    }
    let board = inventory_kicad::load_board(path, |name| {
        files
            .get(name)
            .cloned()
            .ok_or_else(|| inventory_kicad::Error::new(format!("File not supplied: {name}")))
    })?;
    if stamps.is_empty() {
        return Ok(serde_json::to_value(board)?);
    }
    let cache = pack(&board, stamps)
        .ok()
        .map(|b| base64::engine::general_purpose::STANDARD.encode(b));
    Ok(serde_json::json!({"board":board,"pcb_cache":cache}))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::project_cache::{self, Section};
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("demo.kicad_pro");
        std::fs::write(&project, "{}").unwrap();
        std::fs::write(project.with_extension("kicad_pcb"),r#"(kicad_pcb (version 20260206)
            (layers (0 "F.Cu" signal) (44 "Edge.Cuts" user))
            (gr_rect (start 0 0) (end 10 10) (stroke (width 0.05) (type default)) (fill none) (layer "Edge.Cuts")))"#).unwrap();
        (dir, project)
    }
    #[test]
    fn parsed_cache_skips_parser_and_invalidates_for_each_source_stamp_change() {
        let (dir, project) = fixture();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut board = load(&project, &cancel).unwrap();
        let initial = stamps(&project).unwrap();
        let path = dir.path().join(project_cache::NAME);
        board.warnings.push("cached marker".into());
        project_cache::write(&path, Section::Pcb, &pack(&board, &initial).unwrap()).unwrap();
        project_cache::write(&path, Section::Scene, b"unchanged 3d section").unwrap();
        assert!(
            load(&project, &cancel)
                .unwrap()
                .warnings
                .contains(&"cached marker".into())
        );
        let bytes = project_cache::read(&path, Section::Pcb).unwrap();
        for field in 0..4 {
            let mut modified = initial.clone();
            match field {
                0 => modified[1].size += 1,
                1 => modified[1].modified = "older mtime".into(),
                2 => modified[1].header.push(b' '),
                _ => modified[0].modified = "project edited".into(),
            };
            assert!(unpack(&bytes, &modified).is_err());
        }
        std::fs::File::open(project.with_extension("kicad_pcb"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(123))
            .unwrap();
        assert!(
            !load(&project, &cancel)
                .unwrap()
                .warnings
                .contains(&"cached marker".into())
        );
        assert_eq!(
            project_cache::read(&path, Section::Scene).unwrap(),
            b"unchanged 3d section"
        );
        std::fs::write(project.with_extension("kicad_dru"), "(version 1)").unwrap();
        assert!(
            load(&project, &cancel)
                .unwrap()
                .warnings
                .iter()
                .any(|s| s.contains("Custom .kicad_dru"))
        );
    }
    #[test]
    fn worker_uses_valid_cache_without_source_text_and_bad_cache_rebuilds() {
        use base64::Engine;
        let (_dir, project) = fixture();
        let meta = stamps(&project).unwrap();
        let paths = sources("demo.kicad_pro");
        let files: BTreeMap<_, _> = paths
            .into_iter()
            .filter_map(|name| {
                std::fs::read_to_string(project.parent().unwrap().join(&name))
                    .ok()
                    .map(|text| (name, text))
            })
            .collect();
        let first = worker("demo.kicad_pro", &files, &meta, None).unwrap();
        let encoded = first["pcb_cache"].as_str().unwrap();
        let hit = worker("demo.kicad_pro", &BTreeMap::new(), &meta, Some(encoded)).unwrap();
        assert_eq!(first["board"], hit["board"]);
        let bad = base64::engine::general_purpose::STANDARD.encode(b"broken cache");
        assert_eq!(
            worker("demo.kicad_pro", &files, &meta, Some(&bad)).unwrap()["board"],
            first["board"]
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let mut metadata: serde_json::Value = serde_json::from_slice(&bytes[12..12 + len]).unwrap();
        metadata["version"] = 999.into();
        let metadata = serde_json::to_vec(&metadata).unwrap();
        let mut wrong_version = bytes[..8].to_vec();
        wrong_version.extend((metadata.len() as u32).to_le_bytes());
        wrong_version.extend(metadata);
        wrong_version.extend(&bytes[12 + len..]);
        assert!(unpack(&wrong_version, &meta).is_err());
    }
    #[test]
    fn corrupt_cache_is_rebuilt_and_cancellation_does_not_overwrite_it() {
        let (dir, project) = fixture();
        let path = dir.path().join(project_cache::NAME);
        std::fs::write(&path, b"corrupt").unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(true);
        assert!(load(&project, &cancel).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
        cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        load(&project, &cancel).unwrap();
        assert!(
            unpack(
                &project_cache::read(&path, Section::Pcb).unwrap(),
                &stamps(&project).unwrap()
            )
            .is_ok()
        );
    }
    #[test]
    #[ignore = "Real PCB cache benchmark; set KICAD_PCB_PROJECT. Writes only a temporary copy."]
    fn benchmark_parsed_board_cache() {
        let source = std::path::PathBuf::from(std::env::var("KICAD_PCB_PROJECT").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let name = source.file_name().unwrap();
        let project = dir.path().join(name);
        for path in sources(&name.to_string_lossy()) {
            let original = source.parent().unwrap().join(&path);
            if original.exists() {
                std::fs::copy(original, dir.path().join(path)).unwrap();
            }
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let start = std::time::Instant::now();
        let first = load(&project, &cancel).unwrap();
        let cold = start.elapsed();
        let start = std::time::Instant::now();
        let cached = load(&project, &cancel).unwrap();
        let warm = start.elapsed();
        assert_eq!(first.items.len(), cached.items.len());
        assert_eq!(first.pads.len(), cached.pads.len());
        eprintln!(
            "PCB cold parse + save: {cold:?}; cached load: {warm:?}; {} items; {} pads",
            first.items.len(),
            first.pads.len()
        );
    }
}
