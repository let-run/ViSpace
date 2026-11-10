//! Independent parsed-PCB and prepared-3D sections in one disposable project file.
use anyhow::{Result, ensure};
pub use inventory_kicad_renderer::cache::CACHE_NAME as NAME;
pub const LIMIT: usize = 128 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"VINVPK01";
pub const HEADER: usize = 24;
#[derive(Clone, Copy)]
pub enum Section {
    Pcb,
    Scene,
}
pub fn lengths(header: &[u8], total: usize) -> Result<Option<[usize; 2]>> {
    ensure!(
        total <= 2 * LIMIT + HEADER,
        "Project cache exceeds size limit"
    );
    if header.get(..8) != Some(MAGIC) {
        return Ok(None);
    }
    ensure!(header.len() >= HEADER, "Truncated project cache");
    let a = u64::from_le_bytes(header[8..16].try_into()?) as usize;
    let b = u64::from_le_bytes(header[16..24].try_into()?) as usize;
    ensure!(
        a <= LIMIT && b <= LIMIT && HEADER + a + b == total,
        "Invalid project cache lengths"
    );
    Ok(Some([a, b]))
}
pub fn split(bytes: &[u8], section: Section) -> Result<&[u8]> {
    match lengths(bytes, bytes.len())? {
        Some([a, b]) => Ok(match section {
            Section::Pcb => &bytes[HEADER..HEADER + a],
            Section::Scene => &bytes[HEADER + a..HEADER + a + b],
        }),
        // Existing files contained only the compressed 3D section.
        None => {
            ensure!(bytes.len() <= LIMIT, "Legacy cache exceeds size limit");
            Ok(match section {
                Section::Pcb => &[],
                Section::Scene => bytes,
            })
        }
    }
}
pub fn join(pcb: &[u8], scene: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        pcb.len() <= LIMIT && scene.len() <= LIMIT,
        "Project cache section exceeds size limit"
    );
    let mut bytes = Vec::with_capacity(HEADER + pcb.len() + scene.len());
    bytes.extend(MAGIC);
    bytes.extend((pcb.len() as u64).to_le_bytes());
    bytes.extend((scene.len() as u64).to_le_bytes());
    bytes.extend(pcb);
    bytes.extend(scene);
    Ok(bytes)
}
#[cfg(not(target_arch = "wasm32"))]
pub fn read(path: &std::path::Path, section: Section) -> Result<Vec<u8>> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len() as usize;
    let mut header = [0; HEADER];
    let n = file.read(&mut header)?;
    let (start, len) = match lengths(&header[..n], size)? {
        Some([a, b]) => match section {
            Section::Pcb => (HEADER, a),
            Section::Scene => (HEADER + a, b),
        },
        None => {
            ensure!(size <= LIMIT, "Legacy cache exceeds size limit");
            match section {
                Section::Pcb => (0, 0),
                Section::Scene => (0, size),
            }
        }
    };
    file.seek(std::io::SeekFrom::Start(start as u64))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}
#[cfg(not(target_arch = "wasm32"))]
pub fn write(path: &std::path::Path, section: Section, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITER
        .lock()
        .map_err(|_| anyhow::anyhow!("Project cache writer unavailable"))?;
    let other = read(
        path,
        match section {
            Section::Pcb => Section::Scene,
            Section::Scene => Section::Pcb,
        },
    )
    .unwrap_or_default();
    let packed = match section {
        Section::Pcb => join(bytes, &other)?,
        Section::Scene => join(&other, bytes)?,
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&packed)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn independent_sections_and_legacy_cache_survive_updates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(NAME);
        std::fs::write(&path, b"legacy 3d").unwrap();
        write(&path, Section::Pcb, b"parsed board").unwrap();
        assert_eq!(read(&path, Section::Scene).unwrap(), b"legacy 3d");
        write(&path, Section::Scene, b"new 3d").unwrap();
        assert_eq!(read(&path, Section::Pcb).unwrap(), b"parsed board");
        write(&path, Section::Pcb, b"new board").unwrap();
        assert_eq!(read(&path, Section::Scene).unwrap(), b"new 3d");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(split(&bytes, Section::Pcb).unwrap(), b"new board");
        assert!(split(&bytes[..bytes.len() - 1], Section::Pcb).is_err());
    }
}
