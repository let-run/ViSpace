//! Compare initial schematic-only loading with eager project+PCB loading.
use inventory_kicad::{Error, load_project, load_schematics};
use std::{path::PathBuf, time::Instant};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).ok_or("Pass a .kicad_pro file")?);
    let directory = path.parent().ok_or("No project folder")?;
    let filename = path.file_name().unwrap().to_string_lossy();
    // Warm filesystem caches before comparing the parser paths.
    for mode in [true, false, true, false] {
        let start = Instant::now();
        let read = |file: &str| {
            std::fs::read_to_string(directory.join(file)).map_err(|e| Error::new(e.to_string()))
        };
        let bundle = if mode {
            load_project(&filename, read)?
        } else {
            load_schematics(&filename, read)?
        };
        println!(
            "{}: {:?}; {} sheets, {} nets, {} PCB items",
            if mode { "eager" } else { "schematics" },
            start.elapsed(),
            bundle.schematics.len(),
            bundle.nets.len(),
            bundle.pcb.as_ref().map_or(0, |b| b.items.len())
        );
    }
    Ok(())
}
