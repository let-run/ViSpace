#[cfg(not(target_arch = "wasm32"))]
mod desktop {
    use anyhow::{Context, Result};
    use vispace::{store::Store, ui::InventoryApp};
    use std::sync::{Arc, Mutex};
    pub fn main() -> Result<()> {
        let mut args = std::env::args().skip(1);
        let mut path = None;
        let mut check = false;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--data" => {
                    path = Some(std::path::PathBuf::from(
                        args.next().context("--data requires a path")?,
                    ))
                }
                "--check" => check = true,
                "--help" | "-h" => {
                    println!(
                        "ViSpace\n\n  --data PATH   Use a separate SQLite database\n  --check       Validate database without opening a window"
                    );
                    return Ok(());
                }
                _ => anyhow::bail!("Unknown argument: {arg}"),
            }
        }
        let path = match path {
            Some(path) => path,
            None => vispace::data_location::configured_folder()?
                .unwrap_or_else(|| {
                    directories::ProjectDirs::from("", "", "BOMInventoryRust")
                        .expect("No application data directory")
                        .data_dir()
                        .to_owned()
                })
                .join("inventory.sqlite"),
        };
        let store = Store::open(&path)
            .with_context(|| format!("Could not open inventory at {}", path.display()))?;
        if check {
            store.data.validate()?;
            println!(
                "Inventory OK: {} parts, {} projects, {} purchases\n{}",
                store.data.parts.len(),
                store.data.projects.len(),
                store.data.purchases.len(),
                path.display()
            );
            return Ok(());
        }
        let store = Arc::new(Mutex::new(store));
        if store.lock().unwrap().merge_reviews().is_empty() {
            Store::enable_component(&store)?;
        } else {
            vispace::component_sync::start(&store)?;
        }
        let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/app-icon.png"))
            .context("Could not decode application icon")?;
        let options = eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: egui_wgpu::WgpuConfiguration {
                present_mode: egui_wgpu::wgpu::PresentMode::AutoVsync,
                desired_maximum_frame_latency: Some(2),
                ..Default::default()
            },
            viewport: eframe::egui::ViewportBuilder::default()
                .with_title("ViSpace")
                .with_icon(icon)
                .with_fullsize_content_view(true)
                .with_title_shown(false)
                .with_titlebar_shown(false)
                .with_inner_size([1380., 860.])
                .with_min_inner_size([1100., 600.]),
            ..Default::default()
        };
        eframe::run_native(
            "ViSpace",
            options,
            Box::new(move |cc| {
                if let Some(render_state) = &cc.wgpu_render_state {
                    let info = render_state.adapter.get_info();
                    eprintln!("Renderer: egui-wgpu / {:?} / {}", info.backend, info.name);
                }
                {
                    let mut changes = store.lock().unwrap().subscribe_changes();
                    let ctx = cc.egui_ctx.clone();
                    vispace::async_io::spawn(async move {
                        while changes.changed().await.is_ok() {
                            ctx.request_repaint();
                        }
                    })?;
                }
                Ok(Box::new(InventoryApp::new(cc, store)))
            }),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    desktop::main()
}
#[cfg(target_arch = "wasm32")]
fn main() {}
