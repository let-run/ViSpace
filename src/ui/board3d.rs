//! Application boundary for the reusable renderer: I/O, background jobs and project settings.
#[path = "board3d_cache.rs"]
mod persisted;
use super::*;
use inventory_kicad_renderer::{ModelAsset, Scene, Selection};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Loading {
    preview: Option<Arc<Scene>>,
    message: String,
    cache_status: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct DisplayPreferences {
    context: f32,
    brightness: f32,
}
impl Default for DisplayPreferences {
    fn default() -> Self {
        Self {
            context: 0.6,
            brightness: 2.0,
        }
    }
}
pub(super) struct Board3d {
    loading: Arc<Mutex<Loading>>,
    pub project: String,
    board: usize,
    settings: (Option<String>, Option<String>),
    task: Option<crate::tasks::Task<Arc<Scene>>>,
    scene: Option<Arc<Scene>>,
    camera: Option<inventory_kicad_renderer::Camera>,
    brightness: f32,
    preferences_path: Option<std::path::PathBuf>,
    preferences_error: Option<String>,
    pub selected: Selection,
    pub part_navigation: Option<super::footprint_details::PartTarget>,
    overlay: Option<Arc<Scene>>,
    overlay_task: Option<crate::tasks::Task<Arc<Scene>>>,
    overlay_stamp: Option<(usize, Selection)>,
    pick_nets: bool,
    context: f32,
    error: Option<String>,
    ctx: egui::Context,
}
impl Drop for Board3d {
    fn drop(&mut self) {
        if let Some(scene) = &self.scene {
            inventory_kicad_renderer::release(&self.ctx, scene);
        }
        if let Some(scene) = &self.overlay {
            inventory_kicad_renderer::release(&self.ctx, scene);
        }
    }
}
impl Board3d {
    pub fn new(project: &str, ctx: &egui::Context) -> Self {
        Self {
            loading: Arc::new(Mutex::new(Loading::default())),
            project: project.into(),
            board: 0,
            settings: (None, None),
            task: None,
            scene: None,
            camera: None,
            brightness: DisplayPreferences::default().brightness,
            preferences_path: None,
            preferences_error: None,
            selected: Selection::default(),
            part_navigation: None,
            overlay: None,
            overlay_task: None,
            overlay_stamp: None,
            pick_nets: false,
            context: DisplayPreferences::default().context,
            error: None,
            ctx: ctx.clone(),
        }
    }
    pub(super) fn load_preferences(&mut self, path: std::path::PathBuf) {
        let result = match crate::platform::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<DisplayPreferences>(&bytes).map_err(anyhow::Error::from)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(DisplayPreferences::default())
            }
            Err(error) => Err(error.into()),
        };
        self.preferences_path = Some(path);
        match result {
            Ok(settings) => {
                self.context = if settings.context.is_finite() {
                    settings.context.clamp(0.05, 1.)
                } else {
                    0.6
                };
                self.brightness = if settings.brightness.is_finite() {
                    settings.brightness.clamp(0.5, 3.)
                } else {
                    2.0
                };
            }
            Err(error) => {
                self.preferences_error =
                    Some(format!("Could not load 3D display preferences: {error}"))
            }
        }
    }
    fn save_preferences(&mut self) {
        let Some(path) = &self.preferences_path else {
            return;
        };
        let settings = DisplayPreferences {
            context: self.context,
            brightness: self.brightness,
        };
        let result = serde_json::to_vec(&settings)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| crate::platform::fs::write(path, bytes).map_err(anyhow::Error::from));
        self.preferences_error = result
            .err()
            .map(|error| format!("Could not save 3D display preferences: {error}"));
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        link: &KicadProject,
        board: Arc<inventory_kicad::Pcb>,
        incoming: Selection,
        details: Option<super::footprint_details::FootprintDetails>,
    ) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        let effective_link = {
            let mut link = link.clone();
            if link.models_directory.is_none() {
                link.models_directory = crate::model_library::configured_folder();
            }
            link
        };
        #[cfg(not(target_arch = "wasm32"))]
        let link = &effective_link;
        let previous_display = (self.context, self.brightness);
        self.selected = incoming;
        let stamp = Arc::as_ptr(&board) as usize;
        let settings = (
            link.models_directory.clone(),
            link.browser_models_folder.clone(),
        );
        if self.board != stamp || self.settings != settings {
            self.task = None;
            if let Some(scene) = self.scene.take() {
                inventory_kicad_renderer::release(ui.ctx(), &scene);
            }
            self.error = None;
            self.board = stamp;
            self.settings = settings;
            self.start(board.clone(), link, ui.ctx());
        }
        if let Some(preview) = self.loading.lock().unwrap().preview.take() {
            self.camera = Some(inventory_kicad_renderer::Camera::fit(&preview));
            self.scene = Some(preview);
        }
        if let Some(result) = self.task.as_ref().and_then(crate::tasks::Task::poll) {
            self.task = None;
            match result {
                Ok(scene) => {
                    if self.camera.is_none() {
                        self.camera = Some(inventory_kicad_renderer::Camera::fit(&scene));
                    }
                    if let Some(preview) = self.scene.replace(scene) {
                        inventory_kicad_renderer::release(ui.ctx(), &preview);
                    }
                }
                Err(error) => self.error = Some(error),
            }
        }
        self.update_overlay(board.clone(), ui.ctx());
        if !ui.ctx().wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selected = Selection::default();
        }
        let rect = if let (Some(scene), Some(camera)) = (&self.scene, &mut self.camera) {
            let (rect, ray) = inventory_kicad_renderer::draw_selected(
                ui,
                scene,
                camera,
                self.brightness,
                &self.selected,
                self.overlay.as_ref(),
                self.context,
            );
            if let Some((origin, direction)) = ray {
                self.selected = inventory_kicad_renderer::pick(
                    scene,
                    &board,
                    origin,
                    direction,
                    self.pick_nets,
                );
            }
            rect
        } else {
            let rect = ui.available_rect_before_wrap();
            ui.allocate_ui_with_layout(
                rect.size(),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.add_space((rect.height() * 0.5 - 40.).max(0.));
                    if let Some(error) = &self.error {
                        ui.colored_label(Color32::LIGHT_RED, "Could not prepare 3D board");
                        ui.add(egui::Label::new(error).wrap());
                        if ui.button("Retry").clicked() {
                            self.start(board.clone(), link, ui.ctx());
                        }
                    } else {
                        ui.add(egui::Spinner::new().size(28.));
                        ui.label("Preparing 3D board and component models…");
                    }
                },
            );
            rect
        };
        let mut choose = false;
        egui::Area::new(egui::Id::new(("board_3d_controls", &self.project)))
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(rect.right_top() + egui::vec2(-12., 12.))
            .constrain_to(rect)
            .movable(false)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .corner_radius(10)
                    .stroke(egui::Stroke::new(1., theme::LINE))
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_max_width(310.);
                        ui.spacing_mut().item_spacing = egui::vec2(6., 4.);
                        ui.spacing_mut().interact_size.y = 22.;
                        ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("3D board").color(theme::YELLOW));
                            if let Some(scene) = &self.scene {
                                if ui.small_button("Fit").clicked() {
                                    self.camera = Some(inventory_kicad_renderer::Camera::fit(scene));
                                }
                                for (label, pitch) in [("Top", 1.56), ("Bottom", -1.56)] {
                                    if ui.small_button(label).clicked()
                                        && let Some(camera) = &mut self.camera
                                    {
                                        camera.pitch = pitch;
                                        camera.yaw = -std::f32::consts::FRAC_PI_2;
                                    }
                                }
                            }
                        });
                        if let Some(error) = &self.preferences_error { ui.colored_label(Color32::LIGHT_RED, error); }
                        ui.checkbox(&mut self.pick_nets,"Pick nets through models");
                        if !self.selected.is_empty(){
                            ui.add(egui::Slider::new(&mut self.context,0.05..=1.).text("Context strength"));
                            if self.overlay_task.is_some(){ui.spinner();}
                            if let Some(overlay)=&self.overlay {for instance in &overlay.instances {if !instance.reference.is_empty(){ui.label(RichText::new(&instance.reference).color(Color32::from_rgb((inventory_kicad_renderer::layer_color(&instance.reference)[0]*255.) as u8,(inventory_kicad_renderer::layer_color(&instance.reference)[1]*255.) as u8,(inventory_kicad_renderer::layer_color(&instance.reference)[2]*255.) as u8)));}}}
                        }
                        ui.add(egui::Slider::new(&mut self.brightness, 0.5..=3.).text("Brightness").fixed_decimals(1));
                        choose = ui.button("Models folder…")
                            .on_hover_text("Select the KiCad 3D model library root, containing the .3dshapes folders")
                            .clicked();
                        if self.task.is_some() {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.add(egui::Label::new(self.loading.lock().unwrap().message.clone()).wrap());
                            });
                        }
                        if let Some(scene) = &self.scene {
                            ui.label(RichText::new(format!("{} footprints · {} models", scene.statistics.footprints, scene.statistics.models)).color(theme::MUTED));
                            if self.task.is_none() && scene.statistics.missing > 0 {
                                ui.label(RichText::new(format!("{} unavailable models (not shown)", scene.statistics.missing)).color(theme::YELLOW));
                            }
                            if self.task.is_none() && !scene.warnings.is_empty() {
                                ui.label(RichText::new(format!("{} model / board notices", scene.warnings.len())).color(theme::MUTED))
                                    .on_hover_text(scene.warnings.join("\n"));
                            }
                        }
                        let status = self.loading.lock().unwrap().cache_status.clone();
                        if !status.is_empty() { ui.add(egui::Label::new(RichText::new(&status).size(10.).color(theme::MUTED)).truncate()).on_hover_text(status); }
                        ui.label(RichText::new("Drag: orbit · Shift/right-drag: pan · Wheel: zoom").size(10.).color(theme::MUTED));
                    });
            });
        if !self.selected.is_empty() {
            egui::Area::new(egui::Id::new(("board_3d_selection", &self.project)))
                .fixed_pos(rect.left_top() + egui::vec2(12., 12.))
                .movable(false)
                .constrain_to(rect)
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .corner_radius(10)
                        .stroke(egui::Stroke::new(1., theme::LINE))
                        .inner_margin(8)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6., 3.);
                            ui.spacing_mut().interact_size.y = 20.;
                            ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.));
                            ui.set_max_width((rect.width() - 350.).clamp(120., 280.));
                            let label = self
                                .selected
                                .net
                                .as_ref()
                                .map(|n| {
                                    if n.starts_with("Net class: ") || n.starts_with("Net group: ")
                                    {
                                        n.clone()
                                    } else {
                                        format!("Net: {n}")
                                    }
                                })
                                .unwrap_or_else(|| {
                                    format!(
                                        "{}: {}",
                                        if self.selected.references.len() > 1 {
                                            "Footprints"
                                        } else {
                                            "Footprint"
                                        },
                                        self.selected
                                            .references
                                            .iter()
                                            .cloned()
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    )
                                });
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(label).color(theme::YELLOW))
                                        .truncate(),
                                );
                                if ui.small_button("Clear").clicked() {
                                    self.selected = Selection::default();
                                }
                            });
                            if self.selected.net.is_none()
                                && self.selected.references.len() == 1
                                && let Some(details) = &details
                                && self.selected.references.contains(&details.reference)
                                && let Some(target) = details.show(ui)
                            {
                                self.part_navigation = Some(target);
                            }
                        });
                });
        }
        if previous_display != (self.context, self.brightness) {
            self.save_preferences();
        }
        choose
    }
    fn update_overlay(&mut self, board: Arc<inventory_kicad::Pcb>, ctx: &egui::Context) {
        let stamp = (
            Arc::as_ptr(&board) as usize ^ self.scene.as_ref().map_or(0, |s| s.id()),
            self.selected.clone(),
        );
        if self.overlay_stamp.as_ref() != Some(&stamp) {
            self.overlay_task = None;
            if let Some(scene) = self.overlay.take() {
                inventory_kicad_renderer::release(ctx, &scene);
            }
            self.overlay_stamp = Some(stamp);
            let mut selected = self.selected.clone();
            if selected.net.is_none()
                && let Some(scene) = &self.scene
            {
                selected
                    .references
                    .retain(|r| !scene.instances.iter().any(|i| &i.reference == r));
            }
            if !selected.is_empty() {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let wake = ctx.clone();
                    self.overlay_task = Some(crate::tasks::Task::spawn_shared(
                        move |cancel| {
                            inventory_kicad_renderer::build_selection(&board, &selected, || {
                                cancel.load(std::sync::atomic::Ordering::Relaxed)
                            })
                            .map_err(anyhow::Error::msg)
                        },
                        Box::new(move || wake.request_repaint()),
                    ));
                }
                #[cfg(target_arch = "wasm32")]
                {
                    self.overlay_task = Some(crate::tasks::Task::browser(
                        crate::web::jobs::Input::Board3dSelection {
                            board,
                            selection: selected,
                        },
                        decode_scene,
                        ctx.clone(),
                    ));
                }
            }
        }
        if let Some(result) = self
            .overlay_task
            .as_ref()
            .and_then(crate::tasks::Task::poll)
        {
            self.overlay_task = None;
            match result {
                Ok(scene) => self.overlay = Some(scene),
                Err(error) => self.error = Some(error),
            }
        }
    }
    fn start(
        &mut self,
        board: Arc<inventory_kicad::Pcb>,
        link: &KicadProject,
        ctx: &egui::Context,
    ) {
        self.error = None;
        self.loading = Arc::new(Mutex::new(Loading {
            preview: None,
            message: "Preparing board…".into(),
            cache_status: String::new(),
        }));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let link = link.clone();
            let wake = ctx.clone();
            let progress_ctx = ctx.clone();
            let loading = self.loading.clone();
            self.task = Some(crate::tasks::Task::spawn_shared(
                move |cancel| {
                    // Truck parallelizes faces internally. Keep this work off the shared
                    // task pool and leave CPU capacity available for the interface.
                    static POOL: std::sync::OnceLock<rayon::ThreadPool> =
                        std::sync::OnceLock::new();
                    POOL.get_or_init(|| {
                        rayon::ThreadPoolBuilder::new()
                            .num_threads(2)
                            .thread_name(|i| format!("inventory-3d-{i}"))
                            .build()
                            .expect("3D worker pool")
                    })
                    .install(|| {
                        let cancelled = || cancel.load(std::sync::atomic::Ordering::Relaxed);
                        loading.lock().unwrap().message = "Checking .vspace.cache…".into();
                        progress_ctx.request_repaint();
                        let assets = native_assets(&board, &link, &cancel)?;
                        let cache_path = std::path::Path::new(&link.path)
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .join(inventory_kicad_renderer::cache::CACHE_NAME);
                        let mut cache = persisted::read(&cache_path).unwrap_or_default();
                        if cancelled() {
                            anyhow::bail!("3D loading cancelled");
                        }
                        if let Some(scene) = cache.matching_scene(&board, &assets) {
                            loading.lock().unwrap().cache_status = "Using .vspace.cache".into();
                            return Ok(scene);
                        }
                        let preview = inventory_kicad_renderer::build_preview(&board, cancelled)
                            .map_err(anyhow::Error::msg)?;
                        loading.lock().unwrap().preview = Some(preview);
                        progress_ctx.request_repaint();
                        let scene = cache
                            .prepare(&board, &assets, cancelled, |stage| {
                                loading.lock().unwrap().message = stage.into();
                                progress_ctx.request_repaint();
                            })
                            .map_err(anyhow::Error::msg)?;
                        if !cancelled() {
                            loading.lock().unwrap().message = "Saving .vspace.cache…".into();
                            progress_ctx.request_repaint();
                            let status = match persisted::write(&cache_path, &cache) {
                                Ok(()) => "Saved .vspace.cache".into(),
                                Err(error) => format!("3D cache not saved: {error}"),
                            };
                            loading.lock().unwrap().cache_status = status;
                        }
                        Ok(scene)
                    })
                },
                Box::new(move || wake.request_repaint()),
            ));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let loading = self.loading.clone();
            self.task = Some(crate::tasks::Task::browser_updates(
                crate::web::jobs::Input::Board3d {
                    board,
                    path: link.path.clone(),
                    folder: link.browser_folder.clone(),
                    models_folder: link.browser_models_folder.clone(),
                    demo: crate::web::is_browser_demo(),
                    assets: BTreeMap::new(),
                    cache: None,
                },
                decode_scene,
                move |update| {
                    if let Some(message) = update["message"].as_str() {
                        loading.lock().unwrap().message = message.into();
                    }
                    if let Some(status) = update["cache_status"].as_str() {
                        loading.lock().unwrap().cache_status = status.into();
                    }
                    if let Some(preview) = update.get("preview")
                        && let Ok(scene) = decode_scene(preview.clone())
                    {
                        loading.lock().unwrap().preview = Some(scene);
                    }
                },
                ctx.clone(),
            ));
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn native_assets(
    board: &inventory_kicad::Pcb,
    link: &KicadProject,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<BTreeMap<String, ModelAsset>> {
    let project = std::path::Path::new(&link.path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let project = project.to_string_lossy();
    let variables: BTreeMap<_, _> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    let mut libraries = vec![];
    if let Some(path) = &link.models_directory {
        libraries.push(path.clone());
    }
    for version in (6..=11).rev() {
        if let Ok(path) = std::env::var(format!("KICAD{version}_3DMODEL_DIR")) {
            libraries.push(path);
        }
    }
    libraries.extend(
        [
            "/Applications/KiCad/KiCad.app/Contents/SharedSupport/3dmodels",
            "/usr/share/kicad/3dmodels",
            "/usr/local/share/kicad/3dmodels",
            "/usr/share/kicad/packages3d",
        ]
        .map(str::to_owned),
    );
    if let Ok(root) = std::env::var("ProgramFiles") {
        for version in (6..=11).rev() {
            libraries.push(format!("{root}/KiCad/{version}.0/share/kicad/3dmodels"));
        }
    }
    libraries.retain(|path| std::path::Path::new(path).is_dir());
    let footprint_libraries =
        std::fs::read_to_string(std::path::Path::new(project.as_ref()).join("fp-lib-table"))
            .ok()
            .and_then(|source| inventory_kicad::footprint_model_libraries(&source).ok())
            .unwrap_or_default();
    let mut assets = embedded_assets(board);
    let mut total = assets.values().map(|a| a.bytes.len()).sum::<usize>();
    let paths: BTreeSet<_> = board
        .footprint_list
        .iter()
        .flat_map(|f| &f.models)
        .filter(|m| !m.hidden)
        .map(|m| m.path.as_str())
        .collect();
    for path in paths {
        anyhow::ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "3D loading cancelled"
        );
        if assets.contains_key(path) || path.starts_with("kicad-embed://") {
            continue;
        }
        let mut options = vec![];
        for library in libraries
            .iter()
            .map(|s| Some(s.as_str()))
            .chain(std::iter::once(None))
        {
            options.extend(inventory_kicad_models::candidates(
                path, &project, library, &variables,
            ));
        }
        if !path.contains("${") && !std::path::Path::new(path).is_absolute() {
            for footprint in &board.footprint_list {
                if !footprint.models.iter().any(|m| m.path == path) {
                    continue;
                }
                let nickname = footprint.library.split(':').next().unwrap_or("");
                if let Some(uri) = footprint_libraries.get(nickname)
                    && let Some(directory) =
                        inventory_kicad_models::candidates(uri, &project, None, &variables).first()
                {
                    options.extend(inventory_kicad_models::candidates(
                        path, directory, None, &variables,
                    ));
                }
            }
        }
        for candidate in options {
            let Ok(metadata) = std::fs::metadata(&candidate) else {
                continue;
            };
            if metadata.len() > inventory_kicad_models::MAX_MODEL_BYTES as u64
                || total + metadata.len() as usize > 128 * 1024 * 1024
            {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&candidate) {
                total += bytes.len();
                assets.insert(
                    path.into(),
                    ModelAsset {
                        name: candidate,
                        bytes,
                    },
                );
                break;
            }
        }
    }
    Ok(assets)
}
fn embedded_assets(board: &inventory_kicad::Pcb) -> BTreeMap<String, ModelAsset> {
    let mut assets = BTreeMap::new();
    for (name, encoded) in &board.embedded_files {
        if let Ok(bytes) = inventory_kicad_models::embedded(encoded) {
            assets.insert(
                format!("kicad-embed://{name}"),
                ModelAsset {
                    name: name.clone(),
                    bytes,
                },
            );
        }
    }
    assets
}
pub(crate) fn worker_scene_cached(
    board: Arc<inventory_kicad::Pcb>,
    assets: BTreeMap<String, (String, String)>,
    saved: Option<String>,
) -> Result<serde_json::Value> {
    use base64::Engine;
    let mut decoded = embedded_assets(&board);
    for (path, (name, encoded)) in assets {
        decoded.insert(
            path,
            ModelAsset {
                name,
                bytes: base64::engine::general_purpose::STANDARD.decode(encoded)?,
            },
        );
    }
    let mut cache = saved
        .as_deref()
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .and_then(|b| persisted::unpack(&b).ok())
        .unwrap_or_default();
    if cache.matching_scene(&board, &decoded).is_some() {
        #[cfg(target_arch = "wasm32")]
        publish_update(serde_json::json!({"cache_status":"Using .vspace.cache"}));
        // Reuse original packed bytes on a hit; no geometry/serialization work.
        if let Some(saved) = saved {
            return Ok(serde_json::json!({"prepared":saved}));
        }
    } else {
        #[cfg(target_arch = "wasm32")]
        {
            let preview = inventory_kicad_renderer::build_preview(&board, || false)
                .map_err(anyhow::Error::msg)?;
            publish_update(serde_json::json!({"preview":encode_scene(&preview)?}));
        }
        cache
            .prepare(
                &board,
                &decoded,
                || false,
                |stage| {
                    #[cfg(target_arch = "wasm32")]
                    publish_update(serde_json::json!({"message":stage}));
                    #[cfg(not(target_arch = "wasm32"))]
                    let _ = stage;
                },
            )
            .map_err(anyhow::Error::msg)?;
    }
    Ok(
        serde_json::json!({"prepared":base64::engine::general_purpose::STANDARD.encode(persisted::pack(&cache)?)}),
    )
}
#[cfg(target_arch = "wasm32")]
fn publish_update(update: serde_json::Value) {
    use wasm_bindgen::JsCast;
    if let Ok(worker) = js_sys::global().dyn_into::<web_sys::DedicatedWorkerGlobalScope>() {
        let _ = worker.post_message(&serde_json::json!({"update":update}).to_string().into());
    }
}
pub(crate) fn worker_selection(
    board: Arc<inventory_kicad::Pcb>,
    selection: Selection,
) -> Result<serde_json::Value> {
    let scene = inventory_kicad_renderer::build_selection(&board, &selection, || false)
        .map_err(anyhow::Error::msg)?;
    encode_scene(&scene)
}
fn encode_scene(scene: &Scene) -> Result<serde_json::Value> {
    use base64::Engine;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    bincode::serde::encode_into_std_write(scene, &mut encoder, bincode::config::standard())?;
    Ok(serde_json::json!(
        base64::engine::general_purpose::STANDARD.encode(encoder.finish()?)
    ))
}
#[cfg(target_arch = "wasm32")]
fn decode_scene(value: serde_json::Value) -> Result<Arc<Scene>> {
    use base64::Engine;
    if let Some(encoded) = value["prepared"].as_str() {
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        return persisted::unpack(&bytes)?
            .scene()
            .ok_or_else(|| anyhow::anyhow!("3D cache has no scene"));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(
        value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid 3D worker result"))?,
    )?;
    let mut reader = flate2::read::ZlibDecoder::new(bytes.as_slice());
    Ok(Arc::new(bincode::serde::decode_from_std_read(
        &mut reader,
        bincode::config::standard().with_limit::<{ 192 * 1024 * 1024 }>(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn display_defaults_and_saved_adjustments_survive_view_recreation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("board3d-display.json");
        let ctx = egui::Context::default();
        let mut viewer = Board3d::new("first", &ctx);
        viewer.load_preferences(path.clone());
        assert_eq!((viewer.context, viewer.brightness), (0.6, 2.0));
        viewer.context = 0.75;
        viewer.brightness = 1.8;
        viewer.save_preferences();
        assert!(viewer.preferences_error.is_none());
        let mut reopened = Board3d::new("another project", &ctx);
        reopened.load_preferences(path);
        assert_eq!((reopened.context, reopened.brightness), (0.75, 1.8));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[ignore = "Requires USB Hub board fixture; set KICAD_3D_BOARD"]
    fn usb_hub_embedded_and_footprint_relative_models_render() {
        let path = std::env::var("KICAD_3D_BOARD").unwrap();
        let board = inventory_kicad::Pcb::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let link = KicadProject {
            path,
            ..Default::default()
        };
        let assets =
            native_assets(&board, &link, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        assert!(
            assets.contains_key("kicad-embed://CY7C65632-28LTXCT.step"),
            "U2 embedded model must decode"
        );
        assert!(
            assets["TPS25751DREFR.stp"]
                .name
                .ends_with("Lib/TPS25751DREFR/TPS25751DREFR.stp")
        );
        let scene = inventory_kicad_renderer::build(&board, &assets, || false).unwrap();
        for reference in ["U2", "IC2"] {
            for instance in scene.instances.iter().filter(|i| i.reference == reference) {
                let mut min = [f32::INFINITY; 3];
                let mut max = [f32::NEG_INFINITY; 3];
                for vertex in &scene.meshes[instance.mesh].vertices {
                    let p = vertex.position;
                    let m = instance.transform;
                    for a in 0..3 {
                        let v = m[a] * p[0] + m[4 + a] * p[1] + m[8 + a] * p[2] + m[12 + a];
                        min[a] = min[a].min(v);
                        max[a] = max[a].max(v);
                    }
                }
                assert!(
                    max[2] > board.thickness as f32 * 0.5 + 0.5,
                    "{reference} must sit above the board: {min:?} .. {max:?}"
                );
                eprintln!(
                    "{reference}: board bounds {min:?} .. {max:?}, models {:?}",
                    board
                        .footprint_list
                        .iter()
                        .find(|f| f.reference == reference)
                        .unwrap()
                        .models
                );
            }
            assert!(
                scene.instances.iter().any(|i| i.reference == reference),
                "missing {reference}: {:?}",
                scene.warnings
            );
        }
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[ignore = "Manual real-board progressive background loading; set KICAD_3D_BOARD"]
    fn background_loading_publishes_preview_before_models_finish() {
        let path = std::env::var("KICAD_3D_BOARD").unwrap();
        let board = Arc::new(
            inventory_kicad::Pcb::parse(&std::fs::read_to_string(&path).unwrap()).unwrap(),
        );
        let link = KicadProject {
            path,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut view = Board3d::new("fixture", &ctx);
        let start = std::time::Instant::now();
        view.start(board, &link, &ctx);
        let mut saw_preview = false;
        loop {
            if view.loading.lock().unwrap().preview.is_some() && !saw_preview {
                saw_preview = true;
                eprintln!("Board preview available at {:?}", start.elapsed());
                assert!(view.task.as_ref().unwrap().poll().is_none());
            }
            if let Some(scene) = view.task.as_ref().unwrap().poll() {
                let scene = scene.unwrap();
                assert!(saw_preview);
                assert!(scene.statistics.models > 0);
                eprintln!("Full model scene available at {:?}", start.elapsed());
                if let Ok(path) = std::env::var("KICAD_3D_SCENE") {
                    let bytes = bincode::serde::encode_to_vec(&*scene, bincode::config::standard())
                        .unwrap();
                    std::fs::write(path, bytes).unwrap();
                }
                break;
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(45),
                "3D loading exceeded fixture budget"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[ignore = "Manual real KiCad board/model fixture; set KICAD_3D_BOARD and optionally KICAD_3D_SCENE"]
    fn prepare_real_board_3d_fixture() {
        let path = std::env::var("KICAD_3D_BOARD").expect("Set KICAD_3D_BOARD");
        let board = inventory_kicad::Pcb::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let link = KicadProject {
            path: std::path::Path::new(&path)
                .with_extension("kicad_pro")
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        };
        let start = std::time::Instant::now();
        let assets =
            native_assets(&board, &link, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        eprintln!(
            "Loaded {} unique model references in {:?}",
            assets.len(),
            start.elapsed()
        );
        let scene = inventory_kicad_renderer::build_with_progress(
            &board,
            &assets,
            || false,
            |stage| eprintln!("{:?}: {stage}", start.elapsed()),
        )
        .unwrap();
        eprintln!(
            "3D prepared in {:?}: {} model placements, {} missing, {} triangles, {} shared meshes",
            start.elapsed(),
            scene.statistics.models,
            scene.statistics.missing,
            scene.statistics.triangles,
            scene.meshes.len()
        );
        for warning in &scene.warnings {
            eprintln!("{warning}");
        }
        assert!(scene.statistics.models > 0);
        if let Ok(output) = std::env::var("KICAD_3D_SCENE") {
            let mut file = std::fs::File::create(output).unwrap();
            bincode::serde::encode_into_std_write(&*scene, &mut file, bincode::config::standard())
                .unwrap();
        }
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn opening_3d_saves_beside_project_and_second_open_skips_preparation() {
        let temp = tempfile::tempdir().unwrap();
        let board = Arc::new(inventory_kicad::Pcb::parse(r#"(kicad_pcb
            (layers (0 "F.Cu" signal) (44 "Edge.Cuts" user))
            (gr_rect (start 0 0) (end 10 10) (stroke (width 0.05) (type default)) (fill none) (layer "Edge.Cuts")))"#).unwrap());
        let link = KicadProject {
            path: temp
                .path()
                .join("demo.kicad_pro")
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let wait = |view: &Board3d| {
            let start = std::time::Instant::now();
            loop {
                if let Some(result) = view.task.as_ref().unwrap().poll() {
                    return result.unwrap();
                }
                assert!(start.elapsed() < std::time::Duration::from_secs(10));
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let mut first = Board3d::new("demo", &ctx);
        first.start(board.clone(), &link, &ctx);
        let scene = wait(&first);
        let path = temp.path().join(".vspace.cache");
        assert!(path.is_file());
        let mut second = Board3d::new("demo", &ctx);
        second.start(board.clone(), &link, &ctx);
        assert_eq!(
            wait(&second).statistics.triangles,
            scene.statistics.triangles
        );
        assert!(
            second.loading.lock().unwrap().preview.is_none(),
            "cache hit must skip preview geometry too"
        );
        assert_eq!(
            second.loading.lock().unwrap().cache_status,
            "Using .vspace.cache"
        );
        std::fs::write(&path, b"truncated cache").unwrap();
        let mut recovered = Board3d::new("demo", &ctx);
        recovered.start(board, &link, &ctx);
        assert_eq!(
            wait(&recovered).statistics.triangles,
            scene.statistics.triangles
        );
        assert!(persisted::read(&path).is_ok());
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[ignore = "Real-board cache benchmark; set KICAD_3D_BOARD. Writes only a temporary cache."]
    fn benchmark_real_board_cache() {
        let path = std::env::var("KICAD_3D_BOARD").unwrap();
        let board = inventory_kicad::Pcb::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let link = KicadProject {
            path,
            ..Default::default()
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".vspace.cache");
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let cold = std::time::Instant::now();
        let assets = native_assets(&board, &link, &cancel).unwrap();
        let mut cache = inventory_kicad_renderer::cache::PreparedCache::default();
        let scene = pool
            .install(|| cache.prepare(&board, &assets, || false, |_| {}))
            .unwrap();
        persisted::write(&path, &cache).unwrap();
        let cold = cold.elapsed();
        let model_count = scene.statistics.models;
        assert!(model_count > 0);
        drop(scene);
        drop(cache);
        drop(assets);
        let warm = std::time::Instant::now();
        let assets = native_assets(&board, &link, &cancel).unwrap();
        let mut cache = persisted::read(&path).unwrap();
        assert!(cache.matching_scene(&board, &assets).is_some());
        let scene = cache
            .prepare(
                &board,
                &assets,
                || false,
                |stage| assert_eq!(stage, "Using .vspace.cache"),
            )
            .unwrap();
        assert_eq!(scene.statistics.models, model_count);
        eprintln!(
            "Cold including cache save: {cold:?}; cached read + validation: {:?}; {} model placements; {} bytes on disk",
            warm.elapsed(),
            model_count,
            std::fs::metadata(&path).unwrap().len()
        );
    }
    #[test]
    fn browser_worker_prepares_board_without_native_file_access() {
        let board=Arc::new(inventory_kicad::Pcb::parse(r#"(kicad_pcb
            (layers (0 "F.Cu" signal) (44 "Edge.Cuts" user))
            (gr_rect (start 0 0) (end 10 10) (stroke (width 0.05) (type default)) (fill none) (layer "Edge.Cuts")))"#).unwrap());
        use base64::Engine;
        let value = worker_scene_cached(board.clone(), BTreeMap::new(), None).unwrap();
        let encoded = value["prepared"].as_str().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(
            persisted::unpack(&bytes)
                .unwrap()
                .matching_scene(&board, &BTreeMap::new())
                .is_some()
        );
        let hit = worker_scene_cached(board, BTreeMap::new(), Some(encoded.into())).unwrap();
        assert_eq!(hit, value);
    }
}
