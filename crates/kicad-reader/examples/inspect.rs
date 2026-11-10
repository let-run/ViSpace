use inventory_kicad::*;
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let path = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("Pass a .kicad_pro or .kicad_sch file")?,
    );
    if path.extension().is_some_and(|s| s == "kicad_sch") {
        let scene = Schematic::parse(&std::fs::read_to_string(&path)?)?;
        println!(
            "{}: {} symbols, {} primitives, {} child sheets",
            path.display(),
            scene.symbol_count,
            scene.primitives.len(),
            scene.sheets.len()
        );
        for warning in &scene.warnings {
            println!("Warning: {warning}");
        }
    } else {
        let directory = path.parent().ok_or("No parent folder")?;
        let bundle = load_project(&path.file_name().unwrap().to_string_lossy(), |file| {
            std::fs::read_to_string(directory.join(file)).map_err(|e| Error::new(e.to_string()))
        })?;
        println!("{} project nets", bundle.nets.len());
        for node in bundle.nodes() {
            println!(
                "{}: {} {}",
                node.instance_path,
                node.file,
                node.error.as_deref().unwrap_or("OK")
            );
        }
        for (file, scene) in bundle.schematics {
            println!(
                "{file}: {} symbols, {} primitives",
                scene.symbol_count,
                scene.primitives.len()
            );
            for warning in &scene.warnings {
                println!("Warning: {warning}");
            }
        }
    }
    Ok(())
}
