#[path = "cad_search.rs"]
mod cad_search;
use super::*;
use crate::tasks::Task;
use inventory_kicad::nets::{NetMember, ProjectNet};
use inventory_kicad::{Bundle, Ink, Point, Primitive, SheetNode};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Default)]
pub(super) struct KicadState {
    cad_search: cad_search::CadSearch,
    board3d: Option<super::board3d::Board3d>,
    open_3d: Option<(String, String)>,
    opened_3d: bool,
    slots: BTreeMap<String, Slot>,
    viewer: Option<Viewer>,
    pcb_viewer: Option<super::pcb::PcbViewer>,
    opened_pcb: bool,
    last_schematic_selection: Option<SchematicSelectionStamp>,
    opened_project: Option<String>,
    synced: BTreeMap<String, (String, u64)>,
    sync_tasks: BTreeMap<String, Task<()>>,
    monitored: BTreeSet<String>,
    designator_navigation: Option<DesignatorNavigation>,
    navigation_error: Option<String>,
    footprint_details: Option<FootprintDetailsCache>,
    class_tree: Option<ClassTreeCache>,
    class_style_path: Option<std::path::PathBuf>,
    class_styles: BTreeMap<String, BTreeMap<String, super::net_class_style::ClassStyle>>,
}
type ClassTreeCache = (String, usize, usize, BTreeMap<String, BTreeSet<String>>);
#[derive(PartialEq)]
struct FootprintDetailsKey {
    project: String,
    reference: String,
    revision: u64,
    board: usize,
    generation: u64,
}
struct FootprintDetailsCache {
    key: FootprintDetailsKey,
    details: super::footprint_details::FootprintDetails,
}
struct DesignatorNavigation {
    key: String,
    project: String,
    references: BTreeSet<String>,
    pcb: bool,
}
#[derive(PartialEq)]
struct SchematicSelectionStamp {
    key: String,
    net: Option<String>,
    nets: BTreeSet<String>,
    component: Option<(String, String)>,
    components: BTreeSet<(String, String)>,
    board: usize,
}
struct Slot {
    link: KicadProject,
    generation: u64,
    dirty: bool,
    load_error: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    watcher: Option<crate::kicad_watch::Watch>,
    task: Option<Task<Bundle>>,
    pcb_task: Option<Task<inventory_kicad::Pcb>>,
    pcb_requested: Option<String>,
    pcb_open_requested: bool,
    result: Option<Result<Arc<Bundle>, String>>,
}
fn cache_key(link: &KicadProject) -> String {
    if let Some(project)=link.revision.strip_prefix("git-snapshot:").and_then(|rest|rest.split(':').next()) {
        // A received commit changes content, not the identity of an open CAD view.
        return format!("git-snapshot:{project}");
    }
    format!("{}:{}", link.path, link.revision)
}
impl KicadState {
    pub(super) fn new(style_path: std::path::PathBuf) -> Self {
        let class_styles = crate::platform::fs::read(&style_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            class_style_path: Some(style_path),
            class_styles,
            ..Default::default()
        }
    }
    fn save_class_styles(&mut self) {
        if let Some(path) = &self.class_style_path {
            let result = serde_json::to_vec(&self.class_styles)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| {
                    crate::platform::fs::write(path, bytes).map_err(anyhow::Error::from)
                });
            if let Err(error) = result {
                self.navigation_error = Some(format!("Could not save net class colors: {error}"));
            }
        }
    }

    pub(super) fn ensure(&mut self, link: &KicadProject, ctx: &egui::Context) {
        let key = cache_key(link);
        if self.slots.contains_key(&key) {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let task = {
            let path = link.path.clone();
            let wake = ctx.clone();
            Task::spawn(
                move |cancel| {
                    let path = std::path::PathBuf::from(path);
                    let directory = path
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("Project folder is missing"))?;
                    let name = path
                        .file_name()
                        .ok_or_else(|| anyhow::anyhow!("Project filename is missing"))?
                        .to_string_lossy();
                    Ok(inventory_kicad::load_schematics(&name, |name| {
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err(inventory_kicad::Error::new("Loading cancelled"));
                        }
                        let file = directory.join(name);
                        let metadata = std::fs::metadata(&file).map_err(|e| {
                            inventory_kicad::Error::new(format!("{}: {e}", file.display()))
                        })?;
                        if metadata.len() > inventory_kicad::MAX_FILE_BYTES as u64 {
                            return Err(inventory_kicad::Error::new("KiCad file exceeds 32 MB"));
                        }
                        // Discovery needs only existence, not board contents or geometry.
                        if name.ends_with(".kicad_pcb") {
                            return Ok(String::new());
                        }
                        std::fs::read_to_string(&file).map_err(|e| {
                            inventory_kicad::Error::new(format!("{}: {e}", file.display()))
                        })
                    })?)
                },
                Box::new(move || wake.request_repaint()),
            )
        };
        #[cfg(target_arch = "wasm32")]
        let task = Task::browser(
            crate::web::jobs::Input::Kicad {
                path: link.path.clone(),
                files: link
                    .files
                    .iter()
                    .map(|(name, source)| {
                        (
                            name.clone(),
                            if name.ends_with(".kicad_pcb") {
                                String::new()
                            } else {
                                source.clone()
                            },
                        )
                    })
                    .collect(),
                folder: link.browser_folder.clone(),
            },
            |value| Ok(serde_json::from_value(value)?),
            ctx.clone(),
        );
        self.slots.insert(
            key,
            Slot {
                link: link.clone(),
                generation: 0,
                dirty: false,
                load_error: None,
                #[cfg(not(target_arch = "wasm32"))]
                watcher: None,
                task: Some(task),
                pcb_task: None,
                pcb_requested: None,
                pcb_open_requested: false,
                result: None,
            },
        );
    }
    pub(super) fn refresh(&mut self, link: &KicadProject, ctx: &egui::Context) {
        let key = cache_key(link);
        if let Some(slot) = self.slots.get_mut(&key)
            && slot.task.is_some()
        {
            slot.link = link.clone();
            slot.dirty = true;
            return;
        }
        let old = self.slots.remove(&key);
        self.ensure(link, ctx);
        if let (Some(old), Some(slot)) = (old, self.slots.get_mut(&key)) {
            slot.result = old.result;
            slot.generation = old.generation;
            slot.pcb_requested = old.pcb_requested;
            slot.pcb_open_requested = old.pcb_open_requested;
            #[cfg(not(target_arch = "wasm32"))]
            {
                slot.watcher = old.watcher;
            }
        }
    }
    pub(super) fn refresh_managed_snapshots(&mut self, data: &Inventory, ctx: &egui::Context) {
        for link in data.projects.iter().filter_map(|project| project.kicad.as_ref()) {
            if link.revision.starts_with("git-snapshot:")
                && self.slots.get(&cache_key(link)).is_some_and(|slot| {
                    slot.link.browser_folder != link.browser_folder || slot.link.path != link.path || slot.link.revision != link.revision
                })
            {
                // Keep the slot key and open tabs while replacing the parsed content.
                self.refresh(link, ctx);
            }
        }
    }
    pub(super) fn retain_links(
        &mut self,
        data: &Inventory,
        editor: Option<&CatalogEditor>,
        selected: Option<&str>,
    ) {
        let mut keys: std::collections::BTreeSet<String> = data
            .projects
            .iter()
            .filter_map(|p| p.kicad.as_ref())
            .map(cache_key)
            .collect();
        if let Some(link) = editor.and_then(|e| e.kicad.as_ref()) {
            keys.insert(cache_key(link));
        }
        if self.viewer.as_ref().is_some_and(|v| !keys.contains(&v.key)) {
            self.viewer = None;
        }
        if self
            .pcb_viewer
            .as_ref()
            .is_some_and(|v| !keys.contains(&v.key))
        {
            self.pcb_viewer = None;
        }
        self.monitored = data
            .projects
            .iter()
            .filter(|p| selected.is_some_and(|name| key(name) == key(&p.name)))
            .filter_map(|p| p.kicad.as_ref())
            .map(cache_key)
            .collect();
        #[cfg(target_arch = "wasm32")]
        crate::web::host_set_kicad_watch(
            data.projects
                .iter()
                .find(|p| selected.is_some_and(|name| key(name) == key(&p.name)))
                .and_then(|p| p.kicad.as_ref())
                .and_then(|l| l.browser_folder.as_deref()),
        );
        #[cfg(not(target_arch = "wasm32"))]
        for (key, slot) in &mut self.slots {
            if let Some(watcher) = &slot.watcher {
                watcher.set_active(self.monitored.contains(key));
            }
        }
        if self
            .designator_navigation
            .as_ref()
            .is_some_and(|request| !self.monitored.contains(&request.key))
        {
            self.designator_navigation = None;
        }
        if self.open_3d.as_ref().is_some_and(|(project, key)| {
            !keys.contains(key) || !data.projects.iter().any(|p| &p.name == project)
        }) {
            self.open_3d = None;
            self.board3d = None;
        }
        self.slots.retain(|key, _| keys.contains(key));
    }
    pub(super) fn poll(&mut self, ctx: &egui::Context) {
        let mut refresh = Vec::new();
        for (key, slot) in &mut self.slots {
            #[cfg(not(target_arch = "wasm32"))]
            if self.monitored.contains(key) && slot.watcher.as_ref().is_some_and(|w| w.changed()) {
                slot.dirty = true;
            }
            if let Some(result) = slot.task.as_ref().and_then(Task::poll) {
                slot.task = None;
                if let Ok(bundle) = result {
                    slot.load_error = None;
                    slot.generation = slot.generation.wrapping_add(1);
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        let directory = std::path::Path::new(&slot.link.path)
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new("."));
                        let mut paths = vec![std::path::PathBuf::from(&slot.link.path)];
                        paths.extend(bundle.nodes().iter().map(|n| directory.join(&n.file)));
                        paths.push(directory.join(&bundle.pcb_file));
                        paths.push(directory.join(format!(
                            "{}.kicad_dru",
                            bundle.project.file.trim_end_matches(".kicad_pro")
                        )));
                        paths.sort();
                        paths.dedup();
                        let wake = ctx.clone();
                        if let Ok(watcher) =
                            crate::kicad_watch::Watch::new(paths, move || wake.request_repaint())
                        {
                            watcher.set_active(self.monitored.contains(key));
                            slot.watcher = Some(watcher);
                        }
                    }
                    slot.result = Some(Ok(Arc::new(bundle)));
                    slot.pcb_task = None;
                } else {
                    let error = result.unwrap_err();
                    slot.load_error = Some(error.clone());
                    if slot.result.as_ref().is_none_or(|r| r.is_err()) {
                        slot.result = Some(Err(error));
                    }
                }

                if let Some(viewer) = &mut self.viewer
                    && viewer.key == *key
                    && slot.load_error.is_none()
                    && let Some(Ok(bundle)) = &slot.result
                {
                    viewer.prepare_bundle_refresh(bundle.clone(),ctx);
                }
                if let Some(viewer) = &mut self.pcb_viewer
                    && viewer.key == *key
                    && slot.load_error.is_none()
                    && let Some(Ok(bundle)) = &slot.result
                    && let Some(board) = &bundle.pcb
                {
                    viewer.replace_board(board.clone());
                }
            }
            if let Some(result) = slot.pcb_task.as_ref().and_then(Task::poll) {
                slot.pcb_task = None;
                if let Some(Ok(bundle)) = &mut slot.result {
                    let bundle = Arc::make_mut(bundle);
                    match result {
                        Ok(board) => {
                            let board = Arc::new(board);
                            bundle.pcb = Some(board.clone());
                            bundle.pcb_error = None;
                            if let Some(project) = &slot.pcb_requested {
                                if let Some(viewer) = &mut self.pcb_viewer
                                    && viewer.key == *key
                                {
                                    viewer.replace_board(board);
                                } else {
                                    self.pcb_viewer =
                                        Some(super::pcb::PcbViewer::new(project, key, board));
                                }
                                if std::mem::take(&mut slot.pcb_open_requested) {
                                    self.opened_project = Some(project.clone());
                                    self.opened_3d = false;
                                    self.opened_pcb = true;
                                }
                            }
                        }
                        Err(error) => bundle.pcb_error = Some(error),
                    }
                }
            }
            if slot.task.is_none() && slot.dirty {
                slot.dirty = false;
                refresh.push(slot.link.clone());
            }
        }
        for link in refresh {
            self.refresh(&link, ctx);
        }
        if let Some(viewer)=&mut self.viewer{viewer.poll_bundle_refresh(ctx);}
        let boards: Vec<_> = self
            .slots
            .iter()
            .filter(|(key, slot)| {
                self.monitored.contains(*key)
                    && slot.task.is_none()
                    && slot.pcb_task.is_none()
                    && slot.pcb_requested.is_some()
                    && slot.result.as_ref().is_some_and(|r| {
                        r.as_ref().is_ok_and(|b| {
                            b.pcb_available && b.pcb.is_none() && b.pcb_error.is_none()
                        })
                    })
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in boards {
            self.request_pcb(&key, ctx);
        }
        self.poll_designator_navigation(ctx);
        self.sync_schematic_selection();
        if self
            .slots
            .values()
            .any(|s| s.task.is_some() || s.pcb_task.is_some())
            || !self.sync_tasks.is_empty()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    fn request_pcb(&mut self, key: &str, ctx: &egui::Context) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        if slot.pcb_task.is_some() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let task = {
            let path = std::path::PathBuf::from(&slot.link.path);
            let wake = ctx.clone();
            Task::spawn(
                move |cancel| crate::pcb_cache::load(&path, cancel),
                Box::new(move || wake.request_repaint()),
            )
        };
        #[cfg(target_arch = "wasm32")]
        let task = Task::browser(
            crate::web::jobs::Input::KicadBoard {
                path: slot.link.path.clone(),
                files: slot
                    .link
                    .files
                    .iter()
                    .filter(|(name, _)| !name.ends_with(".kicad_sch"))
                    .map(|(name, source)| (name.clone(), source.clone()))
                    .collect(),
                folder: slot.link.browser_folder.clone(),
                stamps: vec![],
                cache: None,
            },
            |value| {
                Ok(serde_json::from_value(
                    value.get("board").cloned().unwrap_or(value),
                )?)
            },
            ctx.clone(),
        );
        slot.pcb_task = Some(task);
    }
    pub(super) fn sync_requests(
        &mut self,
        data: &Inventory,
        store: &SharedStore,
        ctx: &egui::Context,
    ) -> Vec<String> {
        let mut errors: Vec<_> = self.navigation_error.take().into_iter().collect();
        self.sync_tasks.retain(|_, task| {
            if let Some(result) = task.poll() {
                if let Err(error) = result {
                    errors.push(error);
                }
                false
            } else {
                true
            }
        });
        self.synced
            .retain(|name, _| data.projects.iter().any(|p| key(&p.name) == *name));
        for project in &data.projects {
            let name_key = key(&project.name);
            let Some(link) = &project.kicad else {
                continue;
            };
            if !self.monitored.contains(&cache_key(link)) {
                continue;
            }
            self.ensure(link, ctx);
            let slot = &self.slots[&cache_key(link)];
            let stamp = (cache_key(link), slot.generation);
            if self.sync_tasks.contains_key(&name_key) || self.synced.get(&name_key) == Some(&stamp)
            {
                continue;
            }
            let Some(Ok(bundle)) = &slot.result else {
                continue;
            };
            if slot.task.is_some() {
                continue;
            }
            let bundle = bundle.clone();
            let name = project.name.clone();
            let expected = link.clone();
            #[cfg(target_arch = "wasm32")]
            if !store.lock().is_ok_and(|store| store.background_save_ready()) {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                continue;
            }
            self.synced.insert(name_key.clone(), stamp);
            #[cfg(not(target_arch = "wasm32"))]
            {
                let store = store.clone();
                let wake = ctx.clone();
                let task = Task::spawn(
                    move |_| {
                        let items = crate::kicad_sync::requested_items(&bundle)?;
                        let mut store = store
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?;
                        // A stale load must never reattach rows after detach, relink, or delete.
                        if !store.data.projects.iter().any(|p| {
                            key(&p.name) == key(&name) && p.kicad.as_ref() == Some(&expected)
                        }) {
                            return Ok(());
                        }
                        let mut next = store.data.clone();
                        if next.sync_kicad_requests(&name, &items)? {
                            let revision = store.revision();
                            store.commit_if_revision(revision, next)?;
                        }
                        Ok(())
                    },
                    Box::new(move || wake.request_repaint()),
                );
                self.sync_tasks.insert(name_key, task);
            }
            #[cfg(target_arch = "wasm32")]
            {
                let result =
                    (|| -> Result<()> {
                        let items = crate::kicad_sync::requested_items(&bundle)?;
                        let mut store = store
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?;
                        if !store.data.projects.iter().any(|p| {
                            key(&p.name) == key(&name) && p.kicad.as_ref() == Some(&expected)
                        }) {
                            return Ok(());
                        }
                        let mut next = store.data.clone();
                        if next.sync_kicad_requests(&name, &items)? {
                            let revision = store.revision();
                            store.commit_if_revision(revision, next)?;
                        }
                        Ok(())
                    })();
                if let Err(error) = result {
                    errors.push(error.to_string());
                }
            }
        }
        errors
    }
    pub(super) fn link_controls(&mut self, ui: &mut egui::Ui, editor: &mut CatalogEditor) {
        surfaces::row(ui, "KiCad project", |ui| {
            ui.horizontal(|ui| {
                let name = editor
                    .kicad
                    .as_ref()
                    .map(|link| link.path.as_str())
                    .unwrap_or("No project attached");
                ui.add_sized(
                    [ui.available_width() - 64., 26.],
                    egui::Label::new(name).truncate().halign(egui::Align::Min),
                )
                .on_hover_text(name);
                if theme::tool_sized(
                    ui,
                    Icon::Folder,
                    "Choose KiCad project",
                    egui::vec2(26., 26.),
                    false,
                )
                .clicked()
                {
                    #[cfg(not(target_arch="wasm32"))]
                    if editor.git_destination.is_some() && editor.kicad.is_none(){
                        editor.error=Some("Wait for the repository bundle to arrive before relocating it.".into());
                    }else if editor.kicad.as_ref().is_some_and(|link|link.revision.starts_with("git-snapshot:")) || editor.git_destination.is_some() {
                        if editor.relocation.is_none() && let Some(target)=crate::platform::dialogs::FileDialog::new().pick_folder(){
                            let mut link=editor.kicad.clone().unwrap();
                            if !link.revision.starts_with("git-snapshot:") && let Some((project,tip))=&editor.git_destination{link.revision=format!("git-snapshot:{project}:{tip}");}
                            let wake=ui.ctx().clone();
                            editor.relocation=Some(Task::spawn(move |_|crate::git_bundle::relocate_managed(&link,&target),Box::new(move||wake.request_repaint())));
                        }
                    }else {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(path) = crate::platform::dialogs::FileDialog::new()
                        .add_filter("KiCad project", &["kicad_pro"])
                        .pick_file()
                    {
                        editor.kicad = Some(KicadProject {
                            path: path.to_string_lossy().into(),
                            revision: id(),
                            files: BTreeMap::new(),
                            browser_folder: None,
                            ..Default::default()
                        });
                        editor.error = None;
                    }
                    }
                    #[cfg(target_arch="wasm32")]
                    if let Some(link)=editor.kicad.as_ref().filter(|link|link.revision.starts_with("git-snapshot:")){
                        let revision=editor.git_destination.as_ref().map(|(project,tip)|format!("git-snapshot:{project}:{tip}")).unwrap_or_else(||link.revision.clone());
                        let parts=revision.split(':').collect::<Vec<_>>();
                        if let (Some(project),Some(tip))=(parts.get(1),parts.get(2)){
                            crate::web::pick_managed_folder(editor.token.clone(),project,tip);
                        }else{editor.error=Some("Wait for the latest Git snapshot, then choose its destination folder.".into());}
                    }else if let Some((project,tip))=&editor.git_destination{
                        crate::web::pick_managed_folder(editor.token.clone(),project,tip);
                    }else{crate::web::pick_kicad_folder(editor.token.clone());}
                }
                if editor.kicad.is_some()
                    && theme::tool_sized(
                        ui,
                        Icon::Close,
                        "Detach KiCad project",
                        egui::vec2(24., 26.),
                        false,
                    )
                    .clicked()
                {
                    editor.kicad = None;
                    editor.error = None;
                }
            });
            #[cfg(target_arch = "wasm32")]
            ui.label(
                RichText::new("Choose its folder so the browser can read the schematic sheets.")
                    .size(10.)
                    .color(theme::MUTED),
            );
        });
        #[cfg(target_arch = "wasm32")]
        if editor.kicad_choices.len() > 1
            && let Some(link) = &mut editor.kicad
        {
            let old = link.path.clone();
            egui::ComboBox::from_id_salt("kicad_project_choice")
                .selected_text(&link.path)
                .show_ui(ui, |ui| {
                    for path in &editor.kicad_choices {
                        ui.selectable_value(&mut link.path, path.clone(), path);
                    }
                });
            if link.path != old {
                link.revision = id();
            }
        }
        if let Some(link) = &editor.kicad
            && let Some(slot) = self.slots.get(&cache_key(link))
        {
            if slot.task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading KiCad project…");
                });
            }
            if let Some(result) = &slot.result {
                match result {
                    Ok(bundle) => {
                        ui.label(
                            RichText::new(format!(
                                "{} schematic sheet(s) · source files are read-only",
                                bundle.nodes().len()
                            ))
                            .size(11.)
                            .color(theme::MUTED),
                        );
                    }
                    Err(error) => {
                        ui.colored_label(Color32::LIGHT_RED, error);
                        if ui.button("Retry project").clicked() {
                            self.slots.remove(&cache_key(link));
                        }
                    }
                }
            }
        }
    }
    fn select_tree_net(
        &mut self,
        project: &str,
        key: &str,
        bundle: Arc<Bundle>,
        name: &str,
        active_tab: usize,
        now: f64,
    ) {
        let Some(net) = bundle.nets.iter().find(|net| net.name == name) else {
            return;
        };
        let current = self
            .viewer
            .as_ref()
            .filter(|v| v.key == key)
            .and_then(|v| net.members.iter().find(|m| m.instance == v.instance));
        if let Some(member) = current.or_else(|| net.members.first()) {
            if self.viewer.as_ref().is_none_or(|v| v.key != key) {
                self.viewer = Some(Viewer::new(project, key, bundle.clone(), &member.instance));
            }
            let viewer = self.viewer.as_mut().unwrap();
            viewer.navigate(member.instance.clone());
            viewer.select_net(name.into(), now);
        }
        self.opened_3d = active_tab == 5 && self.open_3d.as_ref().is_some_and(|(_, k)| k == key);
        self.opened_pcb = active_tab == 4 && self.pcb_viewer.as_ref().is_some_and(|v| v.key == key);
        if (self.opened_pcb || self.opened_3d)
            && let Some(pcb) = &mut self.pcb_viewer
        {
            pcb.select_schematic_net(schematic_pcb_net(&bundle, &pcb.board, name));
            pcb.blink_selection(now);
        }
        self.last_schematic_selection = self.schematic_selection_stamp();
        self.opened_project = Some(project.into());
    }
    fn select_tree_class(
        &mut self,
        project: &str,
        key: &str,
        bundle: Arc<Bundle>,
        (class, names): (&str, BTreeSet<String>),
        active_tab: usize,
        now: f64,
    ) {
        self.select_tree_group(
            project,
            key,
            bundle,
            (&format!("Net class: {class}"), names),
            active_tab,
            now,
        );
    }
    fn select_tree_group(
        &mut self,
        project: &str,
        key: &str,
        bundle: Arc<Bundle>,
        (label, names): (&str, BTreeSet<String>),
        active_tab: usize,
        now: f64,
    ) {
        let label = label.to_owned();
        let board = bundle.pcb.as_ref();
        let mut members: BTreeMap<String, NetMember> = BTreeMap::new();
        for net in &bundle.nets {
            if !names.contains(&net.name)
                && !board
                    .and_then(|b| schematic_pcb_net(&bundle, b, &net.name))
                    .is_some_and(|n| names.contains(&n))
            {
                continue;
            }
            for member in &net.members {
                let entry = members
                    .entry(member.instance.clone())
                    .or_insert_with(|| NetMember {
                        instance: member.instance.clone(),
                        primitives: vec![],
                        at: member.at,
                    });
                entry.primitives.extend(&member.primitives);
            }
        }
        for member in members.values_mut() {
            member.primitives.sort_unstable();
            member.primitives.dedup();
        }
        let instance = self
            .viewer
            .as_ref()
            .filter(|v| v.key == key && members.contains_key(&v.instance))
            .map(|v| v.instance.clone())
            .or_else(|| members.keys().next().cloned())
            .unwrap_or_else(|| bundle.root.instance_path.clone());
        if self.viewer.as_ref().is_none_or(|v| v.key != key) {
            self.viewer = Some(Viewer::new(project, key, bundle.clone(), &instance));
        }
        let viewer = self.viewer.as_mut().unwrap();
        let repeated = viewer.selected_net.as_ref() == Some(&label) && viewer.group_names == names;
        viewer.navigate(instance);
        viewer.select_net(label.clone(), now);
        viewer.group_names = names.clone();
        viewer.group_net = Some(ProjectNet {
            name: label.clone(),
            members: members.into_values().collect(),
        });
        viewer.reveal_net = false;
        if !repeated {
            viewer.focus_target = Some(FocusTarget::Net);
            viewer.blink_after_transition = true;
            viewer.blink_start = None;
        }
        if repeated {
            viewer.focus_target = None;
            viewer.transition = None;
            viewer.blink_start = Some(now);
            viewer.blink_after_transition = false;
        }
        self.opened_3d = active_tab == 5 && self.open_3d.as_ref().is_some_and(|(_, k)| k == key);
        self.opened_pcb = active_tab == 4 && self.pcb_viewer.as_ref().is_some_and(|v| v.key == key);
        self.opened_project = Some(project.into());
        if let Some(pcb) = self.pcb_viewer.as_mut().filter(|v| v.key == key) {
            let nets = names
                .iter()
                .map(|n| schematic_pcb_net(&bundle, &pcb.board, n).unwrap_or_else(|| n.clone()))
                .collect();
            pcb.select_net_group(label, nets);
            pcb.blink_selection(now);
        }
        self.last_schematic_selection = self.schematic_selection_stamp();
    }
    fn net_leaf(
        &mut self,
        ui: &mut egui::Ui,
        (project, key): (&str, &str),
        bundle: &Arc<Bundle>,
        net: &str,
        active_tab: usize,
    ) -> egui::Response {
        let selected = self.viewer.as_ref().is_some_and(|v| {
            v.key == key && (v.selected_net.as_deref() == Some(net) || v.group_names.contains(net))
        }) || self
            .pcb_viewer
            .as_ref()
            .is_some_and(|v| v.key == key && v.selected_nets().contains(net));
        let response = super::net_class_style::class_button(
            ui,
            net.into(),
            selected,
            true,
            ui.available_width(),
        );
        if response.clicked() {
            let schematic = bundle.nets.iter().find(|n| {
                n.name == net
                    || bundle
                        .pcb
                        .as_ref()
                        .and_then(|b| schematic_pcb_net(bundle, b, &n.name))
                        .as_deref()
                        == Some(net)
            });
            if let Some(schematic) = schematic {
                self.select_tree_net(
                    project,
                    key,
                    bundle.clone(),
                    &schematic.name,
                    active_tab,
                    ui.input(|i| i.time),
                );
            } else if let Some(pcb) = self.pcb_viewer.as_mut().filter(|v| v.key == key) {
                pcb.select_schematic_net(Some(net.into()));
                pcb.blink_selection(ui.input(|i| i.time));
                if let Some(viewer) = &mut self.viewer {
                    viewer.clear_selection();
                }
                self.last_schematic_selection = self.schematic_selection_stamp();
            }
        }
        response
    }
    fn net_rows(
        &mut self,
        ui: &mut egui::Ui,
        (project, key, scope): (&str, &str, &str),
        bundle: &Arc<Bundle>,
        names: impl IntoIterator<Item = String>,
        active_tab: usize,
        reveal: Option<&str>,
    ) {
        use inventory_kicad::nets::{NetNameGroup, group_net_names};
        for group in group_net_names(names) {
            match group {
                NetNameGroup::Single(net) => {
                    let response = self.net_leaf(ui, (project, key), bundle, &net, active_tab);
                    if reveal == Some(net.as_str()) {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                }
                NetNameGroup::Pair { name, nets } => {
                    let label = format!("Net group: {name}");
                    let members: BTreeSet<_> = nets.iter().cloned().collect();
                    let selected = self.viewer.as_ref().is_some_and(|v| {
                        v.key == key
                            && v.selected_net.as_ref() == Some(&label)
                            && v.group_names == members
                    });
                    egui::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        ui.make_persistent_id((project, scope, "net_pair", &name)),
                        false,
                    )
                    .show_header(ui, |ui| {
                        if super::net_class_style::class_button(
                            ui,
                            name.clone(),
                            selected,
                            true,
                            ui.available_width(),
                        )
                        .clicked()
                        {
                            self.select_tree_group(
                                project,
                                key,
                                bundle.clone(),
                                (&label, members),
                                active_tab,
                                ui.input(|i| i.time),
                            );
                        }
                    })
                    .body(|ui| {
                        for net in nets {
                            let response =
                                self.net_leaf(ui, (project, key), bundle, &net, active_tab);
                            if reveal == Some(net.as_str()) {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
                }
            }
        }
    }
    fn net_classes_tree(
        &mut self,
        ui: &mut egui::Ui,
        project: &str,
        key: &str,
        bundle: &Arc<Bundle>,
        active_tab: usize,
    ) {
        let stamp = (
            key.to_owned(),
            Arc::as_ptr(bundle) as usize,
            bundle.pcb.as_ref().map_or(0, |b| Arc::as_ptr(b) as usize),
        );
        if self
            .class_tree
            .as_ref()
            .is_none_or(|(k, b, p, _)| (&stamp.0, stamp.1, stamp.2) != (k, *b, *p))
        {
            let mut names: BTreeSet<_> = bundle.nets.iter().map(|n| n.name.clone()).collect();
            if let Some(board) = &bundle.pcb {
                names = bundle
                    .nets
                    .iter()
                    .map(|n| {
                        schematic_pcb_net(bundle, board, &n.name).unwrap_or_else(|| n.name.clone())
                    })
                    .collect();
                names.extend(board.net_names.values().filter(|n| !n.is_empty()).cloned());
            }
            self.class_tree = Some((
                stamp.0,
                stamp.1,
                stamp.2,
                bundle.project.net_classes.resolve(names),
            ));
        }
        let classes = self.class_tree.as_ref().unwrap().3.clone();
        egui::CollapsingHeader::new(RichText::new("Net Classes").size(11.).color(theme::MUTED))
            .id_salt((project, "net_classes"))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt((project, "class_list"))
                    .auto_shrink([false, true])
                    .max_height(320.)
                    .show(ui, |ui| {
                        for (class, nets) in classes {
                            let label = format!("Net class: {class}");
                            let selected = self.viewer.as_ref().is_some_and(|v| {
                                v.key == key
                                    && v.group_net.is_some()
                                    && v.selected_net.as_ref() == Some(&label)
                            });
                            let collapse_id = ui.make_persistent_id((project, "net_class", &class));
                            egui::collapsing_header::CollapsingState::load_with_default_open(
                                ui.ctx(),
                                collapse_id,
                                false,
                            )
                            .show_header(ui, |ui| {
                                let style = self
                                    .class_styles
                                    .entry(key.into())
                                    .or_default()
                                    .entry(class.clone())
                                    .or_default();
                                let previous = *style;
                                let button = super::net_class_style::row(
                                    ui,
                                    format!("{class} ({})", nets.len()),
                                    selected,
                                    !nets.is_empty(),
                                    style,
                                );
                                let changed = previous != *style;
                                if button.clicked() {
                                    self.select_tree_class(
                                        project,
                                        key,
                                        bundle.clone(),
                                        (&class, nets.clone()),
                                        active_tab,
                                        ui.input(|i| i.time),
                                    );
                                }
                                if changed {
                                    self.save_class_styles();
                                }
                            })
                            .body(|ui| {
                                if nets.is_empty() {
                                    ui.label(
                                        RichText::new("No assigned nets")
                                            .size(11.)
                                            .color(theme::MUTED),
                                    );
                                }
                                self.net_rows(
                                    ui,
                                    (project, key, &class),
                                    bundle,
                                    nets.iter().cloned(),
                                    active_tab,
                                    None,
                                );
                            });
                        }
                    });
            });
    }
    pub(super) fn project_tree(
        &mut self,
        ui: &mut egui::Ui,
        project: &str,
        link: &KicadProject,
        active_tab: usize,
    ) {
        self.ensure(link, ui.ctx());
        let key = cache_key(link);
        let result = self.slots.get(&key).and_then(|s| s.result.clone());
        ui.indent(("kicad_project_tree", project), |ui| {
            ui.add_space(4.);
            if self
                .slots
                .get(&key)
                .is_some_and(|s| s.task.is_some() && s.result.is_some())
            {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Refreshing schematics…")
                            .size(10.)
                            .color(theme::MUTED),
                    );
                });
            }
            if let Some(error) = self.slots.get(&key).and_then(|s| s.load_error.as_ref()) {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    "Schematic update failed; showing last valid version",
                )
                .on_hover_text(error);
                #[cfg(target_arch = "wasm32")]
                if let Some(folder) = &link.browser_folder
                    && ui.small_button("Allow folder access").clicked()
                {
                    crate::web::authorize_kicad_folder(folder.clone());
                }
            }
            match result {
                Some(Ok(bundle)) => {
                    self.tree_node(ui, project, &key, &bundle, &bundle.root, 0);
                    if (bundle.pcb_available || bundle.pcb.is_some())
                        && ui
                            .add(
                                egui::Button::image_and_text(
                                    egui::Image::new(egui::include_image!(
                                        "../../assets/icons/pcb.png"
                                    ))
                                    .fit_to_exact_size(egui::vec2(20., 20.)),
                                    RichText::new(format!("PCB · {}", bundle.pcb_file))
                                        .size(11.)
                                        .color(theme::MUTED),
                                )
                                .selected(
                                    self.pcb_viewer.as_ref().is_some_and(|v| v.key == key)
                                        || self
                                            .slots
                                            .get(&key)
                                            .is_some_and(|slot| slot.pcb_requested.is_some()),
                                )
                                .truncate(),
                            )
                            .clicked()
                    {
                        if let Some(slot) = self.slots.get_mut(&key) {
                            slot.pcb_requested = Some(project.into());
                            slot.pcb_open_requested = false;
                            if let Some(Ok(bundle)) = &mut slot.result {
                                Arc::make_mut(bundle).pcb_error = None;
                            }
                        }
                        if let Some(board) = &bundle.pcb {
                            if self.pcb_viewer.as_ref().is_none_or(|v| v.key != key) {
                                self.pcb_viewer =
                                    Some(super::pcb::PcbViewer::new(project, &key, board.clone()));
                            }
                            self.opened_project = Some(project.into());
                            self.opened_3d = false;
                            self.opened_pcb = true;
                        } else {
                            self.request_pcb(&key, ui.ctx());
                        }
                        self.opened_project = Some(project.into());
                        self.opened_3d = false;
                        self.opened_pcb = true;
                    }
                    if let Some(error) = &bundle.pcb_error {
                        ui.colored_label(Color32::LIGHT_RED, "Could not read PCB")
                            .on_hover_text(error);
                    }
                    if (bundle.pcb_available || bundle.pcb.is_some())
                        && ui
                            .add(
                                egui::Button::image_and_text(
                                    egui::Image::new(egui::include_image!(
                                        "../../assets/icons/3d-board.png"
                                    ))
                                    .fit_to_exact_size(egui::vec2(20., 20.)),
                                    RichText::new("3D board").size(11.).color(theme::MUTED),
                                )
                                .selected(self.has_3d(project))
                                .truncate(),
                            )
                            .clicked()
                    {
                        self.open_3d = Some((project.into(), key.clone()));
                        self.opened_project = Some(project.into());
                        self.opened_3d = true;
                        if bundle.pcb.is_none() {
                            self.request_pcb(&key, ui.ctx());
                        }
                    }
                    let reveal = self
                        .viewer
                        .as_mut()
                        .filter(|v| v.key == key && v.reveal_net)
                        .and_then(|v| {
                            v.reveal_net = false;
                            v.selected_net.clone()
                        });
                    egui::CollapsingHeader::new(
                        RichText::new("Nets").size(11.).color(theme::MUTED),
                    )
                    .id_salt((project, "nets"))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt((project, "net_list"))
                            .auto_shrink([false, true])
                            .max_height(320.)
                            .show(ui, |ui| {
                                self.net_rows(
                                    ui,
                                    (project, &key, "all_nets"),
                                    &bundle,
                                    bundle.nets.iter().map(|n| n.name.clone()),
                                    active_tab,
                                    reveal.as_deref(),
                                );
                            });
                    });
                    self.net_classes_tree(ui, project, &key, &bundle, active_tab);
                }
                Some(Err(error)) => {
                    ui.colored_label(Color32::LIGHT_RED, "Could not read KiCad project")
                        .on_hover_text(error);
                    if ui.small_button("Retry").clicked() {
                        self.slots.remove(&key);
                    }
                    #[cfg(target_arch = "wasm32")]
                    if let Some(folder) = &link.browser_folder
                        && ui.small_button("Allow folder access").clicked()
                    {
                        crate::web::authorize_kicad_folder(folder.clone());
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Reading schematics…")
                                .size(11.)
                                .color(theme::MUTED),
                        );
                    });
                }
            }
            ui.add_space(5.);
        });
    }
    fn tree_node(
        &mut self,
        ui: &mut egui::Ui,
        project: &str,
        key: &str,
        bundle: &Arc<Bundle>,
        node: &SheetNode,
        depth: usize,
    ) {
        let filename = node.file.rsplit('/').next().unwrap_or(&node.file);
        let label = if depth == 0 {
            filename.to_owned()
        } else {
            format!("{} · {filename}", node.name)
        };
        let tooltip = node.error.as_deref().unwrap_or(&node.file);
        let selected = self
            .viewer
            .as_ref()
            .is_some_and(|v| v.key == key && v.instance == node.instance_path);
        let text = RichText::new(label)
            .size(11.)
            .color(if node.error.is_some() {
                Color32::LIGHT_RED
            } else {
                theme::MUTED
            });
        let clicked = if node.children.is_empty() {
            let button = if depth == 0 {
                egui::Button::image_and_text(
                    egui::Image::new(egui::include_image!("../../assets/icons/schematics.png"))
                        .fit_to_exact_size(egui::vec2(20., 20.)),
                    text,
                )
                .selected(selected)
                .truncate()
            } else {
                egui::Button::selectable(selected, text)
            };
            ui.add_enabled(bundle.schematics.contains_key(&node.file), button)
                .on_hover_text(tooltip)
                .clicked()
        } else if depth == 0 {
            egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                ui.make_persistent_id((project, &node.instance_path)),
                true,
            )
            .show_header(ui, |ui| {
                ui.add_enabled(
                    bundle.schematics.contains_key(&node.file),
                    egui::Button::image_and_text(
                        egui::Image::new(egui::include_image!("../../assets/icons/schematics.png"))
                            .fit_to_exact_size(egui::vec2(20., 20.)),
                        text,
                    )
                    .selected(selected)
                    .truncate(),
                )
                .on_hover_text(tooltip)
                .clicked()
            })
            .body(|ui| {
                for child in &node.children {
                    self.tree_node(ui, project, key, bundle, child, depth + 1);
                }
            })
            .1
            .inner
        } else {
            egui::CollapsingHeader::new(text)
                .id_salt((project, &node.instance_path))
                .show(ui, |ui| {
                    for child in &node.children {
                        self.tree_node(ui, project, key, bundle, child, depth + 1);
                    }
                })
                .header_response
                .on_hover_text(tooltip)
                .clicked()
        };
        if clicked {
            self.opened_project = Some(project.into());
            self.opened_3d = false;
            self.opened_pcb = false;
            if let Some(viewer) = self.viewer.as_mut().filter(|v| v.key == key) {
                viewer.navigate(node.instance_path.clone());
            } else {
                self.viewer = Some(Viewer::new(
                    project,
                    key,
                    bundle.clone(),
                    &node.instance_path,
                ));
            }
        }
    }
    pub(super) fn navigation_available(&self, link: &KicadProject, pcb: bool) -> bool {
        self.slots
            .get(&cache_key(link))
            .and_then(|slot| slot.result.as_ref())
            .is_none_or(|result| {
                result
                    .as_ref()
                    .is_ok_and(|bundle| !pcb || bundle.pcb_available || bundle.pcb.is_some())
            })
    }
    pub(super) fn navigate_designators(
        &mut self,
        project: &str,
        link: &KicadProject,
        raw: &str,
        pcb: bool,
        ctx: &egui::Context,
    ) -> anyhow::Result<()> {
        let references = crate::model::references(raw)?;
        anyhow::ensure!(!references.is_empty(), "This request has no designators");
        self.ensure(link, ctx);
        self.designator_navigation = Some(DesignatorNavigation {
            key: cache_key(link),
            project: project.into(),
            references,
            pcb,
        });
        self.poll_designator_navigation(ctx);
        ctx.request_repaint();
        Ok(())
    }
    fn poll_designator_navigation(&mut self, ctx: &egui::Context) {
        let Some(request) = self.designator_navigation.take() else {
            return;
        };
        let Some(slot) = self.slots.get(&request.key) else {
            return;
        };
        let Some(result) = &slot.result else {
            self.designator_navigation = Some(request);
            return;
        };
        let bundle = match result {
            Ok(bundle) => bundle.clone(),
            Err(error) => {
                self.navigation_error = Some(error.clone());
                return;
            }
        };
        if request.pcb && bundle.pcb.is_none() {
            if !bundle.pcb_available {
                self.navigation_error = Some("This KiCad project has no PCB file".into());
                return;
            }
            if let Some(error) = &bundle.pcb_error {
                self.navigation_error = Some(error.clone());
                return;
            }
            let slot = self.slots.get_mut(&request.key).unwrap();
            slot.pcb_requested = Some(request.project.clone());
            slot.pcb_open_requested = false;
            self.request_pcb(&request.key, ctx);
            self.designator_navigation = Some(request);
            return;
        }
        let selections = designator_components(&bundle, &request.references);
        if !request.pcb && selections.is_empty() {
            self.navigation_error = Some(format!(
                "No schematic symbols found for {}",
                request
                    .references
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        }
        if !selections.is_empty() {
            let current = self
                .viewer
                .as_ref()
                .filter(|v| v.key == request.key)
                .map(|v| v.instance.clone());
            let instance = current
                .filter(|i| selections.iter().any(|(instance, _)| instance == i))
                .unwrap_or_else(|| selections.first().unwrap().0.clone());
            if self.viewer.as_ref().is_none_or(|v| v.key != request.key) {
                self.viewer = Some(Viewer::new(
                    &request.project,
                    &request.key,
                    bundle.clone(),
                    &instance,
                ));
            }
            self.viewer
                .as_mut()
                .unwrap()
                .select_components(selections, instance, true);
        }
        if request.pcb
            && let Some(board) = &bundle.pcb
        {
            let references: BTreeSet<_> = board
                .footprint_list
                .iter()
                .filter(|f| {
                    request
                        .references
                        .contains(&f.reference.to_ascii_uppercase())
                })
                .map(|f| f.reference.clone())
                .collect();
            if references.is_empty() {
                self.navigation_error =
                    Some("No PCB footprints found for these designators".into());
                return;
            }
            if self
                .pcb_viewer
                .as_ref()
                .is_none_or(|v| v.key != request.key)
            {
                self.pcb_viewer = Some(super::pcb::PcbViewer::new(
                    &request.project,
                    &request.key,
                    board.clone(),
                ));
            }
            self.pcb_viewer
                .as_mut()
                .unwrap()
                .select_schematic_footprints(references, true);
            self.last_schematic_selection = self.schematic_selection_stamp();
        } else {
            self.sync_schematic_selection();
        }
        self.opened_project = Some(request.project);
        self.opened_3d = false;
        self.opened_pcb = request.pcb;
        ctx.request_repaint();
    }
    pub(super) fn take_opened_project(&mut self) -> Option<String> {
        self.opened_project.take()
    }
    pub(super) fn opened_document_tab(&self) -> usize {
        if self.opened_3d {
            5
        } else if self.opened_pcb {
            4
        } else {
            3
        }
    }
    pub(super) fn cad_shortcuts(
        &mut self,
        ctx: &egui::Context,
        project: &str,
        tab: usize,
    ) -> usize {
        let editing_text = ctx
            .memory(|m| m.focused())
            .is_some_and(|id| egui::text_edit::TextEditState::load(ctx, id).is_some());
        if editing_text {
            return tab;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            if let Some(viewer) = &mut self.viewer
                && key(&viewer.project) == key(project)
            {
                viewer.clear_selection();
            }
            if let Some(viewer) = &mut self.pcb_viewer
                && key(&viewer.project) == key(project)
            {
                viewer.select_schematic_net(None);
            }
            self.designator_navigation = None;
            self.last_schematic_selection = self.schematic_selection_stamp();
            ctx.request_repaint();
        }
        let other = match tab {
            3 if self.has_pcb(project) => Some(4),
            4 if self.has_viewer(project) => Some(3),
            _ => None,
        };
        if let Some(other) = other
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
        {
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            ctx.request_repaint();
            return other;
        }
        tab
    }
    pub(super) fn has_3d(&self, project: &str) -> bool {
        self.open_3d
            .as_ref()
            .is_some_and(|(name, _)| key(name) == key(project))
    }
    pub(super) fn close_3d(&mut self) {
        self.open_3d = None;
        self.board3d = None;
        self.opened_3d = false;
    }
    pub(super) fn take_3d_part_navigation(
        &mut self,
    ) -> Option<super::footprint_details::PartTarget> {
        self.board3d.as_mut().and_then(|v| v.part_navigation.take())
    }
    pub(super) fn board3d_panel(
        &mut self,
        ui: &mut egui::Ui,
        data: &Inventory,
        project: &str,
        revision: u64,
    ) -> bool {
        let Some((_, slot_key)) = self
            .open_3d
            .clone()
            .filter(|(name, _)| key(name) == key(project))
        else {
            return false;
        };
        let Some(link) = data
            .projects
            .iter()
            .find(|p| p.name == project)
            .and_then(|p| p.kicad.as_ref())
        else {
            return false;
        };
        let bundle = self
            .slots
            .get(&slot_key)
            .and_then(|slot| slot.result.as_ref())
            .and_then(|r| r.as_ref().ok())
            .cloned();
        if let Some(board) = bundle.as_ref().and_then(|b| b.pcb.clone()) {
            if self
                .pcb_viewer
                .as_ref()
                .is_none_or(|v| v.key != slot_key || !Arc::ptr_eq(&v.board, &board))
            {
                self.pcb_viewer = Some(super::pcb::PcbViewer::new(
                    project,
                    &slot_key,
                    board.clone(),
                ));
            }
            if self.board3d.as_ref().is_none_or(|v| v.project != project) {
                let mut viewer = super::board3d::Board3d::new(project, ui.ctx());
                if let Some(path) = &self.class_style_path {
                    viewer.load_preferences(path.with_file_name("board3d-display.json"));
                }
                self.board3d = Some(viewer);
            }
            self.sync_schematic_selection();
            let mut incoming = inventory_kicad_renderer::Selection::default();
            if let Some(pcb) = self.pcb_viewer.as_ref().filter(|v| v.key == slot_key) {
                incoming.net = pcb.selected_net().map(str::to_owned);
                incoming.nets = pcb.selected_nets().clone();
                incoming.references = pcb.selected_footprints().clone();
            }
            self.update_footprint_details(data, revision);
            let details = self.pcb_viewer.as_ref().and_then(|v| v.details());
            let choose =
                self.board3d
                    .as_mut()
                    .unwrap()
                    .show(ui, link, board, incoming.clone(), details);
            let selected = self.board3d.as_ref().unwrap().selected.clone();
            if selected != incoming {
                if let Some(pcb) = self.pcb_viewer.as_mut().filter(|v| v.key == slot_key) {
                    if selected.references.is_empty() {
                        pcb.select_schematic_net(selected.net.clone());
                    } else {
                        pcb.select_schematic_footprints(selected.references.clone(), false);
                    }
                }
                if let Some(reference) = selected.references.iter().next() {
                    self.select_pcb_footprint(&slot_key, project, Some(reference));
                } else {
                    self.select_pcb_net(
                        &slot_key,
                        project,
                        selected.net.as_deref(),
                        ui.ctx().input(|i| i.time),
                    );
                }
            }
            return choose;
        }
        let error = bundle.as_ref().and_then(|b| b.pcb_error.clone());
        if error.is_none() {
            self.request_pcb(&slot_key, ui.ctx());
        }
        let size = ui.available_size();
        ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.add_space((size.y * 0.5 - 40.).max(0.));
            if let Some(error) = error {
                ui.colored_label(Color32::LIGHT_RED, error);
                if ui.button("Retry").clicked() {
                    if let Some(slot) = self.slots.get_mut(&slot_key)
                        && let Some(Ok(bundle)) = &mut slot.result
                    {
                        Arc::make_mut(bundle).pcb_error = None;
                    }
                    self.request_pcb(&slot_key, ui.ctx());
                }
            } else {
                ui.add(egui::Spinner::new().size(28.));
                ui.label("Loading 3D board…");
            }
        });
        false
    }
    pub(super) fn has_pcb(&self, project: &str) -> bool {
        self.pcb_viewer
            .as_ref()
            .is_some_and(|v| key(&v.project) == key(project))
            || self.slots.values().any(|slot| {
                slot.pcb_requested
                    .as_ref()
                    .is_some_and(|name| key(name) == key(project))
            })
    }
    pub(super) fn close_pcb(&mut self) {
        self.pcb_viewer = None;
        self.last_schematic_selection = None;
        for slot in self.slots.values_mut() {
            slot.pcb_requested = None;
            slot.pcb_open_requested = false;
            slot.pcb_task = None;
        }
    }
    pub(super) fn pcb_panel(
        &mut self,
        ui: &mut egui::Ui,
        data: &Inventory,
        revision: u64,
        project: &str,
    ) -> Option<super::footprint_details::PartTarget> {
        if self
            .pcb_viewer
            .as_ref()
            .is_none_or(|v| key(&v.project) != key(project))
        {
            let pending = self.slots.iter().find(|(_, slot)| {
                slot.pcb_requested
                    .as_ref()
                    .is_some_and(|name| key(name) == key(project))
            });
            let pending_key = pending.map(|(key, _)| key.clone());
            let error = pending
                .and_then(|(_, slot)| slot.result.as_ref())
                .and_then(|result| result.as_ref().ok())
                .and_then(|bundle| bundle.pcb_error.clone());
            let size = ui.available_size();
            let mut retry = false;
            ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.add_space((size.y * 0.5 - 40.).max(0.));
                if let Some(error) = error {
                    ui.colored_label(Color32::LIGHT_RED, "Could not load PCB");
                    ui.add(egui::Label::new(error).wrap());
                    retry = ui.button("Retry").clicked();
                } else {
                    ui.add(egui::Spinner::new().size(28.));
                    ui.label("Loading PCB…");
                }
            });
            if retry && let Some(key) = pending_key {
                if let Some(slot) = self.slots.get_mut(&key)
                    && let Some(Ok(bundle)) = &mut slot.result
                {
                    Arc::make_mut(bundle).pcb_error = None;
                }
                self.request_pcb(&key, ui.ctx());
            }
            return None;
        }
        self.sync_schematic_selection();
        self.update_footprint_details(data, revision);
        if let Some(viewer) = &mut self.pcb_viewer {
            let colors = self
                .slots
                .get(&viewer.key)
                .and_then(|s| s.result.as_ref())
                .and_then(|r| r.as_ref().ok())
                .map(|bundle| {
                    let classes = self
                        .class_tree
                        .as_ref()
                        .filter(|(key, stamp, _, _)| {
                            key == &viewer.key && *stamp == Arc::as_ptr(bundle) as usize
                        })
                        .map(|cached| cached.3.clone())
                        .unwrap_or_else(|| {
                            bundle
                                .project
                                .net_classes
                                .resolve(viewer.board.net_names.values().cloned())
                        });
                    let mut colors = BTreeMap::new();
                    if let Some(styles) = self.class_styles.get(&viewer.key) {
                        for (class, nets) in classes {
                            if let Some(style) = styles.get(&class).filter(|s| s.enabled) {
                                for net in nets {
                                    colors.insert(net, style.color);
                                }
                            }
                        }
                    }
                    colors
                })
                .unwrap_or_default();
            viewer.set_class_colors(colors);
        }
        let change = self.pcb_viewer.as_mut().and_then(|viewer| {
            viewer.ui(ui);
            let footprint = viewer.take_footprint_change();
            let net = viewer.take_net_change();
            footprint
                .map(|name| (true, viewer.key.clone(), viewer.project.clone(), name))
                .or_else(|| {
                    net.map(|name| (false, viewer.key.clone(), viewer.project.clone(), name))
                })
        });
        if let Some((footprint, key, project, name)) = change {
            if footprint {
                self.select_pcb_footprint(&key, &project, name.as_deref());
            } else {
                self.select_pcb_net(&key, &project, name.as_deref(), ui.ctx().input(|i| i.time));
            }
        }
        self.pcb_viewer
            .as_mut()
            .and_then(|viewer| viewer.take_part_navigation())
    }
    fn update_footprint_details(&mut self, data: &Inventory, revision: u64) {
        let Some(viewer) = &mut self.pcb_viewer else {
            return;
        };
        let Some(reference) = viewer.single_footprint().map(str::to_owned) else {
            viewer.set_details(None);
            return;
        };
        let Some(slot) = self.slots.get(&viewer.key) else {
            return;
        };
        let Some(Ok(bundle)) = &slot.result else {
            return;
        };
        let key = FootprintDetailsKey {
            project: viewer.key.clone(),
            reference: reference.clone(),
            revision,
            board: Arc::as_ptr(&viewer.board) as usize,
            generation: slot.generation,
        };
        if self
            .footprint_details
            .as_ref()
            .is_none_or(|cached| cached.key != key)
        {
            let details = super::footprint_details::FootprintDetails::build(
                data,
                bundle,
                &viewer.board,
                &viewer.project,
                &reference,
            );
            self.footprint_details = Some(FootprintDetailsCache { key, details });
        }
        viewer.set_details(
            self.footprint_details
                .as_ref()
                .map(|cached| cached.details.clone()),
        );
    }
    fn select_pcb_footprint(&mut self, key: &str, project: &str, reference: Option<&str>) {
        let bundle = self
            .slots
            .get(key)
            .and_then(|s| s.result.as_ref())
            .and_then(|r| r.as_ref().ok())
            .cloned();
        if let (Some(bundle), Some(reference)) = (bundle, reference) {
            let current = self
                .viewer
                .as_ref()
                .filter(|v| v.key == key)
                .map(|v| v.instance.clone());
            let mut matches = bundle
                .nodes()
                .into_iter()
                .flat_map(|node| {
                    bundle
                        .schematics
                        .get(&node.file)
                        .into_iter()
                        .flat_map(move |scene| {
                            scene
                                .components
                                .iter()
                                .filter(move |c| {
                                    c.references
                                        .get(&node.instance_path)
                                        .unwrap_or(&c.reference)
                                        == reference
                                })
                                .map(move |c| (node.instance_path.clone(), c.uuid.clone(), c.at))
                        })
                })
                .collect::<Vec<_>>();
            matches.sort_by_key(|m| current.as_deref() != Some(m.0.as_str()));
            if let Some((instance, id, point)) = matches.into_iter().next() {
                if self.viewer.as_ref().is_none_or(|v| v.key != key) {
                    self.viewer = Some(Viewer::new(project, key, bundle.clone(), &instance));
                }
                self.viewer
                    .as_mut()
                    .unwrap()
                    .select_component(instance, id, point, true);
            } else if let Some(viewer) = &mut self.viewer
                && viewer.key == key
            {
                viewer.clear_selection();
            }
        } else if let Some(viewer) = &mut self.viewer
            && viewer.key == key
        {
            viewer.clear_selection();
        }
        self.last_schematic_selection = self.schematic_selection_stamp();
    }
    fn schematic_selection_stamp(&self) -> Option<SchematicSelectionStamp> {
        let schematic = self.viewer.as_ref()?;
        let pcb = self
            .pcb_viewer
            .as_ref()
            .filter(|pcb| pcb.key == schematic.key)?;
        Some(SchematicSelectionStamp {
            key: schematic.key.clone(),
            net: schematic.selected_net.clone(),
            nets: schematic.group_names.clone(),
            component: schematic.selected_component.clone(),
            components: schematic.selected_components.clone(),
            board: Arc::as_ptr(&pcb.board) as usize,
        })
    }
    fn sync_schematic_selection(&mut self) {
        let Some(stamp) = self.schematic_selection_stamp() else {
            return;
        };
        if self.last_schematic_selection.as_ref() == Some(&stamp) {
            return;
        }
        let Some(Ok(bundle)) = self.slots.get(&stamp.key).and_then(|s| s.result.as_ref()) else {
            return;
        };
        let pcb = self.pcb_viewer.as_mut().unwrap();
        if let Some((instance, id)) = &stamp.component {
            let references: BTreeSet<_> = stamp
                .components
                .iter()
                .chain(std::iter::once(&(instance.clone(), id.clone())))
                .filter_map(|(i, id)| component_reference(bundle, i, id))
                .filter(|name| {
                    pcb.board
                        .footprint_list
                        .iter()
                        .any(|f| &f.reference == name)
                })
                .collect();
            pcb.select_schematic_footprints(references, false);
        } else if !stamp.nets.is_empty() {
            let nets = stamp
                .nets
                .iter()
                .map(|n| schematic_pcb_net(bundle, &pcb.board, n).unwrap_or_else(|| n.clone()))
                .collect();
            pcb.select_net_group(stamp.net.clone().unwrap_or_default(), nets);
        } else {
            let net = stamp
                .net
                .as_deref()
                .and_then(|name| schematic_pcb_net(bundle, &pcb.board, name));
            pcb.select_schematic_net(net);
        }
        self.last_schematic_selection = Some(stamp);
    }
    fn select_pcb_net(&mut self, key: &str, project: &str, name: Option<&str>, now: f64) {
        self.select_pcb_net_inner(key, project, name, now);
        self.last_schematic_selection = self.schematic_selection_stamp();
    }
    fn select_pcb_net_inner(&mut self, key: &str, project: &str, name: Option<&str>, now: f64) {
        let Some(Ok(bundle)) = self.slots.get(key).and_then(|slot| slot.result.as_ref()) else {
            return;
        };
        let bundle = bundle.clone();
        let net = name.and_then(|name| {
            bundle
                .nets
                .iter()
                .find(|net| net.name == name)
                .or_else(|| {
                    bundle.nets.iter().find(|net| {
                        net.name.trim_start_matches('/') == name.trim_start_matches('/')
                    })
                })
                .or_else(|| {
                    self.pcb_viewer
                        .as_ref()
                        .filter(|v| v.key == key)
                        .and_then(|v| pcb_net_by_pins(&bundle, &v.board, name))
                })
        });
        let Some(net) = net else {
            if let Some(viewer) = &mut self.viewer
                && viewer.key == key
            {
                viewer.selected_net = None;
                viewer.group_net = None;
                viewer.group_names.clear();
                viewer.selected_component = None;
                viewer.selected_components.clear();
                viewer.focus_target = None;
                viewer.blink_start = None;
                viewer.blink_after_transition = false;
                viewer.gpu_net = None;
            }
            return;
        };
        let current = self
            .viewer
            .as_ref()
            .filter(|v| v.key == key)
            .and_then(|v| net.members.iter().find(|m| m.instance == v.instance));
        let Some(member) = current.or_else(|| net.members.first()) else {
            return;
        };
        if self.viewer.as_ref().is_none_or(|v| v.key != key) {
            self.viewer = Some(Viewer::new(project, key, bundle.clone(), &member.instance));
        }
        if let Some(viewer) = &mut self.viewer {
            viewer.navigate(member.instance.clone());
            viewer.select_net(net.name.clone(), now);
            // The schematic is hidden behind PCB: start its blink only when rendered.
            viewer.blink_start = None;
            viewer.blink_after_transition = true;
        }
    }
    pub(super) fn has_viewer(&self, project: &str) -> bool {
        self.viewer
            .as_ref()
            .is_some_and(|v| key(&v.project) == key(project))
    }
    pub(super) fn close_viewer(&mut self) {
        self.viewer = None;
        self.opened_project = None;
    }
    pub(super) fn viewer_panel(
        &mut self,
        ui: &mut egui::Ui,
        data: &Inventory,
        revision: u64,
    ) -> Option<super::footprint_details::PartTarget> {
        let mut viewer = self.viewer.take()?;
        if let Some(reference) = viewer.single_symbol() {
            let stamp = (
                reference.clone(),
                revision,
                Arc::as_ptr(&viewer.bundle) as usize,
            );
            if viewer.details_stamp.as_ref() != Some(&stamp) {
                viewer.details = Some(
                    super::footprint_details::FootprintDetails::build_for_symbol(
                        data,
                        &viewer.bundle,
                        &viewer.project,
                        &reference,
                    ),
                );
                viewer.details_stamp = Some(stamp);
            }
        } else {
            viewer.details = None;
            viewer.details_stamp = None;
        }
        egui::Frame::new()
            .inner_margin(8)
            .show(ui, |ui| viewer.ui(ui));
        let navigation = viewer.part_navigation.take();
        self.viewer = Some(viewer);
        self.sync_schematic_selection();
        navigation
    }
}
fn designator_components(
    bundle: &Bundle,
    references: &BTreeSet<String>,
) -> BTreeSet<(String, String)> {
    bundle
        .nodes()
        .into_iter()
        .flat_map(|node| {
            bundle
                .schematics
                .get(&node.file)
                .into_iter()
                .flat_map(move |scene| {
                    scene
                        .components
                        .iter()
                        .filter(|component| {
                            references.contains(
                                &component
                                    .references
                                    .get(&node.instance_path)
                                    .unwrap_or(&component.reference)
                                    .to_ascii_uppercase(),
                            )
                        })
                        .map(move |component| (node.instance_path.clone(), component.uuid.clone()))
                })
        })
        .collect()
}
fn component_reference(bundle: &Bundle, instance: &str, id: &str) -> Option<String> {
    let node = bundle
        .nodes()
        .into_iter()
        .find(|node| node.instance_path == instance)?;
    let component = bundle
        .schematics
        .get(&node.file)?
        .components
        .iter()
        .find(|c| c.uuid == id)?;
    Some(
        component
            .references
            .get(instance)
            .unwrap_or(&component.reference)
            .clone(),
    )
}
// Resolve the reverse direction using names first, then physical pin endpoints.
fn schematic_pcb_net(bundle: &Bundle, board: &inventory_kicad::Pcb, name: &str) -> Option<String> {
    let names: BTreeSet<&str> = board
        .net_names
        .values()
        .map(String::as_str)
        .chain(board.items.iter().map(|item| item.net.as_str()))
        .chain(board.pads.iter().map(|pad| pad.net.as_str()))
        .chain(board.vias.iter().map(|via| via.net.as_str()))
        .filter(|name| !name.is_empty())
        .collect();
    if names.contains(name) {
        return Some(name.into());
    }
    let normalized: Vec<_> = names
        .iter()
        .filter(|n| n.trim_start_matches('/') == name.trim_start_matches('/'))
        .collect();
    if normalized.len() == 1 {
        return Some((**normalized[0]).into());
    }
    let net = bundle.nets.iter().find(|net| net.name == name)?;
    let nodes = bundle.nodes();
    let mut endpoints: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for member in &net.members {
        let Some(scene) = nodes
            .iter()
            .find(|node| node.instance_path == member.instance)
            .and_then(|node| bundle.schematics.get(&node.file))
        else {
            continue;
        };
        for component in &scene.components {
            let reference = component
                .references
                .get(&member.instance)
                .unwrap_or(&component.reference);
            for (number, primitive) in &component.pins {
                if member.primitives.contains(primitive) {
                    endpoints.entry(reference).or_default().insert(number);
                }
            }
        }
    }
    let mut matches = board
        .pads
        .iter()
        .filter(|pad| {
            !pad.net.is_empty()
                && endpoints
                    .get(pad.reference.as_str())
                    .is_some_and(|numbers| numbers.contains(pad.number.as_str()))
        })
        .map(|pad| pad.net.as_str())
        .collect::<BTreeSet<_>>();
    (matches.len() == 1).then(|| matches.pop_first().unwrap().to_owned())
}
// KiCad generates different names for unlabeled schematic/PCB nets. Match their
// electrical endpoints rather than guessing from the generated name.
fn pcb_net_by_pins<'a>(
    bundle: &'a Bundle,
    board: &inventory_kicad::Pcb,
    name: &str,
) -> Option<&'a inventory_kicad::nets::ProjectNet> {
    let mut endpoints: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for pad in board
        .pads
        .iter()
        .filter(|pad| pad.net == name && !pad.reference.is_empty())
    {
        endpoints
            .entry(&pad.reference)
            .or_default()
            .insert(&pad.number);
    }
    let nodes = bundle.nodes();
    let mut matched = None;
    for net in &bundle.nets {
        let matches = net.members.iter().any(|member| {
            nodes
                .iter()
                .find(|node| node.instance_path == member.instance)
                .and_then(|node| bundle.schematics.get(&node.file))
                .is_some_and(|scene| {
                    scene.components.iter().any(|component| {
                        let reference = component
                            .references
                            .get(&member.instance)
                            .unwrap_or(&component.reference);
                        endpoints.get(reference.as_str()).is_some_and(|numbers| {
                            component.pins.iter().any(|(number, primitive)| {
                                numbers.contains(number.as_str())
                                    && member.primitives.contains(primitive)
                            })
                        })
                    })
                })
        });
        if matches {
            if matched.is_some() {
                return None;
            } // Stale/mismatched files: never choose arbitrarily.
            matched = Some(net);
        }
    }
    matched
}
struct CameraTransition {
    start: f64,
    from: Point,
    to: Point,
    from_scale: f64,
    to_scale: f64,
}
enum FocusTarget {
    Components,
    Part(Point),
    Net,
}
#[derive(Clone)]
struct SchematicMatch {
    component: Option<String>,
    title: String,
    value: String,
    package: String,
    instance: String,
    point: Option<Point>,
    net: Option<String>,
}
struct Viewer {
    details: Option<super::footprint_details::FootprintDetails>,
    details_stamp: Option<(String, u64, usize)>,
    part_navigation: Option<super::footprint_details::PartTarget>,
    project: String,
    key: String,
    bundle: Arc<Bundle>,
    instance: String,
    center: Point,
    scale: f64,
    fit: bool,
    selected_net: Option<String>,
    group_net: Option<ProjectNet>,
    group_names: BTreeSet<String>,
    selected_component: Option<(String, String)>,
    selected_components: BTreeSet<(String, String)>,
    blink_start: Option<f64>,
    blink_after_transition: bool,
    highlight_ink: Option<Color32>,
    focus_target: Option<FocusTarget>,
    transition: Option<CameraTransition>,
    reveal_net: bool,
    message: Option<String>,
    images: super::cad_bitmap::BitmapCache,
    primitive_bounds: BTreeMap<String, Vec<egui::Rect>>,
    gpu_base: BTreeMap<String, Arc<super::cad_gpu::Scene>>,
    gpu_task: Option<(String, Task<Arc<super::cad_gpu::Scene>>)>,
    pending_bundle:Option<(Arc<Bundle>,String,Task<Arc<super::cad_gpu::Scene>>)>,
    gpu_error: Option<String>,
    gpu_lru: std::collections::VecDeque<String>,
    gpu_net: Option<(String, BTreeSet<usize>, Arc<super::cad_gpu::Scene>, Arc<super::cad_gpu::Scene>)>,
}
impl Viewer {
    fn prepare_bundle_refresh(&mut self,bundle:Arc<Bundle>,ctx:&egui::Context){
        let nodes=bundle.nodes();
        let node=nodes.iter().find(|node|node.instance_path==self.instance).copied().unwrap_or(&bundle.root);
        let instance=node.instance_path.clone();
        let Some(source)=bundle.schematics.get(&node.file).cloned() else {return};
        let task=super::cad_jobs::spawn(super::cad_jobs::Request::Schematic{source,variables:bundle.project.text_variables.clone(),instance:instance.clone()},ctx);
        self.pending_bundle=Some((bundle,instance,task));
    }
    fn poll_bundle_refresh(&mut self,ctx:&egui::Context){
        if let Some((bundle,instance,_))=&self.pending_bundle {
            if instance!=&self.instance && bundle.nodes().iter().any(|node|node.instance_path==self.instance){
                let bundle=bundle.clone();self.prepare_bundle_refresh(bundle,ctx);
            }
        }
        let Some(result)=self.pending_bundle.as_ref().and_then(|(_,_,task)|task.poll()) else{return};
        let (bundle,instance,_)=self.pending_bundle.take().unwrap();
        match result {
            Ok(scene)=>self.apply_bundle_refresh(bundle,instance,scene),
            Err(error)=>self.message=Some(format!("Schematic update failed; showing previous render: {error}")),
        }
    }
    fn apply_bundle_refresh(&mut self,bundle:Arc<Bundle>,instance:String,scene:Arc<super::cad_gpu::Scene>){
        self.images.clear();self.primitive_bounds.clear();self.gpu_base.clear();self.gpu_task=None;self.gpu_error=None;self.gpu_lru.clear();self.gpu_net=None;
        self.gpu_base.insert(instance.clone(),scene);self.gpu_lru.push_back(instance.clone());
        if self.instance!=instance{self.instance=instance;self.fit=true;}
        if let Some(group)=&mut self.group_net {
            let mut members:BTreeMap<String,NetMember>=BTreeMap::new();
            for net in bundle.nets.iter().filter(|net|self.group_names.contains(&net.name)) {
                for member in &net.members {
                    let entry=members.entry(member.instance.clone()).or_insert_with(||NetMember{instance:member.instance.clone(),primitives:vec![],at:member.at});
                    entry.primitives.extend(&member.primitives);
                }
            }
            group.members=members.into_values().collect();
        }
        self.details_stamp=None;self.bundle=bundle;
    }
    fn new(project: &str, key: &str, bundle: Arc<Bundle>, instance: &str) -> Self {
        Self {
            details: None,
            details_stamp: None,
            part_navigation: None,
            project: project.into(),
            key: key.into(),
            bundle,
            instance: instance.into(),
            center: Point::default(),
            scale: 3.,
            fit: true,
            selected_net: None,
            group_net: None,
            group_names: BTreeSet::new(),
            selected_component: None,
            selected_components: BTreeSet::new(),
            blink_start: None,
            blink_after_transition: false,
            highlight_ink: Some(Color32::from_rgb(220, 0, 220)),
            focus_target: None,
            transition: None,
            reveal_net: false,
            message: None,
            images: super::cad_bitmap::BitmapCache::default(),
            primitive_bounds: BTreeMap::new(),
            gpu_base: BTreeMap::new(),
            gpu_task: None,
            pending_bundle:None,
            gpu_error: None,
            gpu_lru: Default::default(),
            gpu_net: None,
        }
    }
    fn single_symbol(&self) -> Option<String> {
        let references: BTreeSet<_> = self
            .selected_components
            .iter()
            .chain(self.selected_component.iter())
            .filter_map(|(instance, id)| component_reference(&self.bundle, instance, id))
            .collect();
        if references.len() == 1 {
            references.into_iter().next()
        } else {
            None
        }
    }
    fn selected_in<'a>(
        &self,
        scene: &'a inventory_kicad::Schematic,
    ) -> Vec<&'a inventory_kicad::Component> {
        let ids: BTreeSet<_> = self
            .selected_components
            .iter()
            .filter(|(i, _)| i == &self.instance)
            .map(|(_, id)| id.as_str())
            .collect();
        scene
            .components
            .iter()
            .filter(|c| {
                ids.contains(c.uuid.as_str())
                    || (self.selected_components.is_empty()
                        && self
                            .selected_component
                            .as_ref()
                            .is_some_and(|(instance, id)| {
                                instance == &self.instance && id == &c.uuid
                            }))
            })
            .collect()
    }
    fn select_components(
        &mut self,
        selections: BTreeSet<(String, String)>,
        instance: String,
        focus: bool,
    ) {
        self.navigate(instance.clone());
        self.selected_component = selections
            .iter()
            .find(|(i, _)| i == &instance)
            .cloned()
            .or_else(|| selections.iter().next().cloned());
        self.selected_components = selections;
        self.group_net = None;
        self.group_names.clear();
        self.selected_net = None;
        self.blink_after_transition = true;
        self.blink_start = None;
        self.focus_target = focus.then_some(FocusTarget::Components);
    }
    fn clear_selection(&mut self) {
        self.group_net = None;
        self.group_names.clear();
        self.selected_net = None;
        self.selected_component = None;
        self.selected_components.clear();
        self.blink_start = None;
        self.blink_after_transition = false;
        self.focus_target = None;
        self.transition = None;
        self.gpu_net = None;
    }
    fn select_component(&mut self, instance: String, id: String, point: Point, focus: bool) {
        self.navigate(instance.clone());
        self.selected_components = BTreeSet::from([(instance.clone(), id.clone())]);
        self.selected_component = Some((instance, id));
        self.group_net = None;
        self.group_names.clear();
        self.selected_net = None;
        self.blink_after_transition = true;
        self.blink_start = None;
        self.focus_target = focus.then_some(FocusTarget::Part(point));
    }
    fn select_net(&mut self, name: String, now: f64) {
        self.group_net = None;
        self.group_names.clear();
        self.selected_component = None;
        self.selected_components.clear();
        let repeated = self.selected_net.as_deref() == Some(name.as_str());
        self.selected_net = Some(name);
        self.blink_start = repeated.then_some(now);
        self.blink_after_transition = !repeated;
        if repeated {
            self.focus_target = None;
            self.transition = None;
        } else {
            self.focus_target = Some(FocusTarget::Net);
        }
        self.reveal_net = true;
    }
    fn selected_project_net<'a>(&'a self, bundle: &'a Bundle) -> Option<&'a ProjectNet> {
        self.group_net.as_ref().or_else(|| {
            self.selected_net
                .as_ref()
                .and_then(|name| bundle.nets.iter().find(|n| &n.name == name))
        })
    }
    fn replay_selection_blink(&mut self, now: f64) {
        self.blink_start = Some(now);
        self.blink_after_transition = false;
    }
    fn update_net_blink(&mut self, now: f64) {
        if self.blink_after_transition && self.focus_target.is_none() && self.transition.is_none() {
            self.blink_start = Some(now);
            self.blink_after_transition = false;
        }
        self.highlight_ink = if self.blink_after_transition {
            None
        } else {
            net_blink_ink(self.blink_start.map(|start| now - start))
        };
    }
    fn navigate(&mut self, instance: String) {
        if instance != self.instance {
            let focus = self.selected_components.iter().any(|(i, _)| i == &instance);
            self.instance = instance;
            if focus {
                self.focus_target = Some(FocusTarget::Components);
            }
            self.gpu_task = None;
            self.gpu_error = None;
            self.fit = true;
            self.message = None;
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui) {
        let bundle = self.bundle.clone();
        let nodes = bundle.nodes();
        let node = nodes
            .iter()
            .find(|n| n.instance_path == self.instance)
            .copied()
            .unwrap_or(&bundle.root);
        let Some(scene) = bundle.schematics.get(&node.file) else {
            ui.colored_label(
                Color32::LIGHT_RED,
                node.error.as_deref().unwrap_or("Schematic unavailable"),
            );
            return;
        };
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ui.available_height().max(120.)),
            egui::Sense::click_and_drag(),
        );
        if self.fit {
            let bounds = scene.bounds;
            self.center = Point::new(
                (bounds.min.x + bounds.max.x) / 2.,
                (bounds.min.y + bounds.max.y) / 2.,
            );
            self.scale = ((rect.width() as f64 - 40.) / bounds.width().max(1.))
                .min((rect.height() as f64 - 40.) / bounds.height().max(1.))
                .clamp(0.1, 200.);
            self.fit = false;
        }
        let now = ui.ctx().input(|i| i.time);
        if let Some(target) = self.focus_target.take() {
            let camera = match target {
                FocusTarget::Components => {
                    let mut bounds = egui::Rect::NOTHING;
                    for component in self.selected_in(scene) {
                        for &index in &component.primitives {
                            if let Some(primitive) = scene.primitives.get(index) {
                                bounds = bounds.union(primitive_bounds(primitive));
                            }
                        }
                    }
                    bounds.is_finite().then(|| {
                        net_camera_target(
                            Point::new(bounds.min.x as f64, bounds.min.y as f64),
                            Point::new(bounds.max.x as f64, bounds.max.y as f64),
                            rect.size(),
                        )
                    })
                }
                FocusTarget::Part(point) => Some((point, self.scale.max(8.))),
                FocusTarget::Net => self
                    .selected_project_net(&bundle)
                    .and_then(|net| net_bounds(net, &self.instance, scene))
                    .map(|(min, max)| net_camera_target(min, max, rect.size())),
            };
            if let Some((to, to_scale)) = camera {
                self.transition = Some(CameraTransition {
                    start: now,
                    from: self.center,
                    to,
                    from_scale: self.scale,
                    to_scale,
                });
            }
        }
        if let Some(animation) = &self.transition {
            let t = ((now - animation.start) / 0.5).clamp(0., 1.);
            let ease = t * t * (3. - 2. * t);
            self.center = Point::new(
                animation.from.x + (animation.to.x - animation.from.x) * ease,
                animation.from.y + (animation.to.y - animation.from.y) * ease,
            );
            self.scale = (animation.from_scale.ln()
                + (animation.to_scale.ln() - animation.from_scale.ln()) * ease)
                .exp();
            if t >= 1. {
                self.transition = None;
            } else {
                ui.ctx().request_repaint();
            }
        }
        self.update_net_blink(now);
        if self.blink_after_transition || self.blink_start.is_some_and(|start| now - start < 1.) {
            ui.ctx().request_repaint();
        }
        if response.dragged() {
            self.transition = None;
            let delta = ui.input(|i| i.pointer.delta());
            self.center.x -= delta.x as f64 / self.scale;
            self.center.y -= delta.y as f64 / self.scale;
        }
        if response.hovered() {
            if ui.input(|i| i.key_pressed(egui::Key::F)) {
                self.transition = None;
                self.focus_target = None;
                self.fit = true;
            }
            if let Some(mouse) = response.hover_pos() {
                let before = self.world(rect, mouse);
                let factor = ui.input(|i| {
                    (i.smooth_scroll_delta.y as f64 * 0.002).exp() * i.zoom_delta() as f64
                });
                if (factor - 1.).abs() > 0.00001 {
                    self.transition = None;
                    self.scale = (self.scale * factor).clamp(0.1, 200.);
                    self.center = Point::new(
                        before.x - (mouse.x - rect.center().x) as f64 / self.scale,
                        before.y - (mouse.y - rect.center().y) as f64 / self.scale,
                    );
                }
                if response.clicked() && !response.double_clicked() {
                    let point = self.world(rect, mouse);
                    let tolerance = 6. / self.scale;
                    let nearest = bundle
                        .nets
                        .iter()
                        .filter_map(|net| {
                            let distance = net
                                .members
                                .iter()
                                .filter(|m| m.instance == self.instance)
                                .flat_map(|m| &m.primitives)
                                .filter_map(|&i| scene.primitives.get(i))
                                .map(|p| net_primitive_distance(p, point))
                                .fold(f64::INFINITY, f64::min);
                            (distance <= tolerance).then_some((distance, &net.name))
                        })
                        .min_by(|a, b| a.0.total_cmp(&b.0));
                    if let Some((_, name)) = nearest {
                        self.select_net(name.clone(), now);
                        ui.ctx().request_repaint();
                    } else if let Some(component) = component_at(scene, point) {
                        self.select_component(
                            self.instance.clone(),
                            component.uuid.clone(),
                            component.at,
                            false,
                        );
                        ui.ctx().request_repaint();
                    }
                }
                if response.double_clicked() {
                    let point = self.world(rect, mouse);
                    if let Some(sheet) = scene.sheets.iter().find(|s| s.bounds.contains(point))
                        && let Some(child) =
                            node.children.iter().find(|n| n.sheet_uuid == sheet.uuid)
                    {
                        if bundle.schematics.contains_key(&child.file) {
                            self.navigate(child.instance_path.clone());
                        } else {
                            self.message = child.error.clone();
                        }
                    }
                }
            }
        }
        if self.instance != node.instance_path {
            ui.ctx().request_repaint();
            return;
        }
        self.update_net_blink(now);
        let mut highlighted: BTreeSet<usize> = self
            .selected_project_net(&self.bundle)
            .into_iter()
            .flat_map(|n| &n.members)
            .filter(|m| m.instance == self.instance)
            .flat_map(|m| m.primitives.iter().copied())
            .collect();
        highlighted.extend(
            self.selected_in(scene)
                .iter()
                .flat_map(|component| component.primitives.iter().copied()),
        );
        let painter = ui.painter_at(rect);
        let paper = Color32::from_rgb(255, 255, 235);
        if super::cad_gpu::available(ui.ctx()) {
            self.paint_retained(ui, rect, scene, &highlighted, paper);
        } else {
            painter.rect_filled(rect, 0., paper);
            let page = egui::Rect::from_two_pos(
                self.screen(rect, Point::default()),
                self.screen(rect, scene.paper),
            );
            painter.rect_stroke(
                page,
                0.,
                egui::Stroke::new(1., Color32::LIGHT_GRAY),
                egui::StrokeKind::Inside,
            );
            let bounds = self
                .primitive_bounds
                .entry(node.file.clone())
                .or_insert_with(|| scene.primitives.iter().map(primitive_bounds).collect());
            let visible = egui::Rect::from_center_size(
                egui::pos2(self.center.x as f32, self.center.y as f32),
                rect.size() / self.scale as f32,
            )
            .expand(2. / self.scale as f32);
            // Store only the visible indices so the immutable scene is not cloned and drawing
            // can borrow the viewer again after the cache lookup.
            let visible_indices: Vec<_> = bounds
                .iter()
                .enumerate()
                .filter_map(|(i, b)| visible.intersects(*b).then_some(i))
                .collect();
            for index in visible_indices {
                let primitive = &scene.primitives[index];
                if let Primitive::Bitmap {
                    center,
                    size,
                    width,
                    height,
                    rgba,
                } = primitive
                {
                    let image_rect = egui::Rect::from_center_size(
                        self.screen(rect, *center),
                        egui::vec2((size.x * self.scale) as f32, (size.y * self.scale) as f32),
                    );
                    if rect.intersects(image_rect) {
                        let texture = self.images.texture_with_grayscale(
                            ui.ctx(),
                            &node.file,
                            index,
                            [*width as usize, *height as usize],
                            rgba,
                            (self.selected_net.is_some()||!self.selected_components.is_empty())&&!highlighted.contains(&index),
                        );
                        painter.image(
                            texture.id(),
                            image_rect,
                            egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.)),
                            Color32::WHITE,
                        );
                    }
                } else {
                    self.paint(
                        ui,
                        &painter,
                        rect,
                        primitive,
                        paper,
                        highlighted.contains(&index),
                        (self.selected_net.is_some()||!self.selected_components.is_empty())&&!highlighted.contains(&index),
                    );
                }
            }
        }
        self.floating_controls(ui, rect);
    }
    fn floating_controls(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let bundle = self.bundle.clone();
        let nodes = bundle.nodes();
        let node = nodes
            .iter()
            .find(|n| n.instance_path == self.instance)
            .copied()
            .unwrap_or(&bundle.root);
        let Some(scene) = bundle.schematics.get(&node.file) else {
            return;
        };
        let right_width = (rect.width() * 0.336).clamp(240., 336.);
        let left_width = (rect.width() - right_width - 36.).clamp(180., 266.);
        let frame = || {
            egui::Frame::new()
                .fill(Color32::from_rgb(250, 249, 246))
                .stroke(egui::Stroke::new(1., Color32::from_rgb(225, 228, 217)))
                .corner_radius(12)
                .inner_margin(6)
                .shadow(egui::epaint::Shadow {
                    offset: [0, 2],
                    blur: 8,
                    spread: 0,
                    color: Color32::from_black_alpha(22),
                })
        };
        egui::Area::new(egui::Id::new(("schematic_net_overlay", &self.key)))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min + egui::vec2(12., 12.))
            .movable(false)
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                frame().show(ui, |ui| {
                    ui.set_width(left_width - 12.);
                    ui.visuals_mut().override_text_color = Some(Color32::from_rgb(30, 58, 45));
                    let name = self.selected_net.clone();
                    let refs: BTreeSet<_> = self
                        .selected_components
                        .iter()
                        .filter_map(|(instance, id)| component_reference(&bundle, instance, id))
                        .collect();
                    let symbol = if refs.is_empty() {
                        self.selected_component
                            .as_ref()
                            .and_then(|(instance, id)| component_reference(&bundle, instance, id))
                    } else {
                        Some(refs.into_iter().collect::<Vec<_>>().join(", "))
                    };
                    ui.horizontal(|ui| {
                        if theme::selection_lamp(ui, name.is_some() || symbol.is_some()).clicked() {
                            self.replay_selection_blink(ui.input(|i| i.time));
                            ui.ctx().request_repaint();
                        }
                        ui.add_sized(
                            [(left_width - 146.).max(40.), 30.],
                            egui::Label::new(
                                RichText::new(
                                    symbol
                                        .as_ref()
                                        .map(|reference| {
                                            format!(
                                                "{}: {reference}",
                                                if reference.contains(',') {
                                                    "Symbols"
                                                } else {
                                                    "Symbol"
                                                }
                                            )
                                        })
                                        .or_else(|| {
                                            name.as_ref().map(|n| {
                                                if n.starts_with("Net class: ")
                                                    || n.starts_with("Net group: ")
                                                {
                                                    n.clone()
                                                } else {
                                                    format!("Net: {n}")
                                                }
                                            })
                                        })
                                        .unwrap_or_else(|| "No net selected".into()),
                                )
                                .color(
                                    if name.is_some() || symbol.is_some() {
                                        Color32::from_rgb(180, 40, 205)
                                    } else {
                                        Color32::from_rgb(126, 139, 132)
                                    },
                                ),
                            )
                            .truncate()
                            .halign(egui::Align::Min),
                        );
                        if ui
                            .add_enabled(
                                name.is_some() || symbol.is_some(),
                                egui::Button::new("Clear").fill(Color32::WHITE),
                            )
                            .clicked()
                        {
                            self.clear_selection();
                        }
                        if ui
                            .add(egui::Button::new("Fit").fill(Color32::WHITE))
                            .clicked()
                        {
                            self.transition = None;
                            self.focus_target = None;
                            self.fit = true;
                        }
                    });
                    if let Some(details) = self
                        .details
                        .clone()
                        .filter(|d| symbol.as_deref() == Some(d.reference.as_str()))
                    {
                        ui.spacing_mut().item_spacing.y = 3.;
                        ui.separator();
                        egui::Grid::new(("symbol_details", &self.key))
                            .num_columns(2)
                            .spacing(egui::vec2(8., 3.))
                            .min_row_height(16.)
                            .show(ui, |ui| {
                                for (label, value) in [
                                    ("Part number", &details.number),
                                    ("Value", &details.value),
                                    ("Tolerance", &details.tolerance),
                                    ("Package", &details.package),
                                ] {
                                    if label == "Tolerance" && value.trim().is_empty() {
                                        continue;
                                    }
                                    ui.label(
                                        RichText::new(label)
                                            .size(11.)
                                            .color(Color32::from_rgb(100, 117, 112)),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(if value.trim().is_empty() {
                                                "—"
                                            } else {
                                                value.as_str()
                                            })
                                            .size(12.),
                                        )
                                        .wrap(),
                                    );
                                    ui.end_row();
                                }
                            });
                        for target in &details.targets {
                            let label = if details.targets.len() > 1 {
                                format!(
                                    "{}: {}",
                                    if target.purchase { "Purchase" } else { "Part" },
                                    target.number
                                )
                            } else if target.purchase {
                                "Go to Purchase".into()
                            } else {
                                "Go to Part".into()
                            };
                            if ui
                                .link(
                                    RichText::new(label)
                                        .size(12.)
                                        .color(Color32::from_rgb(180, 40, 205)),
                                )
                                .clicked()
                            {
                                self.part_navigation = Some(target.clone());
                            }
                        }
                    }
                    let net = self.selected_project_net(&bundle);
                    let sheets: Vec<_> = nodes
                        .iter()
                        .filter(|sheet| {
                            net.is_some_and(|n| {
                                n.members.iter().any(|m| m.instance == sheet.instance_path)
                            })
                        })
                        .collect();
                    if sheets.len() > 1 {
                        egui::ScrollArea::vertical()
                            .id_salt("net_overlay_sheets")
                            .max_height(192.)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for sheet in sheets {
                                    let selected = sheet.instance_path == self.instance;
                                    if ui
                                        .add_sized(
                                            [left_width - 12., 24.],
                                            egui::Button::new(
                                                RichText::new(&sheet.name)
                                                    .color(Color32::from_rgb(78, 98, 85)),
                                            )
                                            .fill(if selected {
                                                Color32::from_rgb(233, 237, 226)
                                            } else {
                                                Color32::TRANSPARENT
                                            })
                                            .stroke(egui::Stroke::NONE)
                                            .truncate(),
                                        )
                                        .clicked()
                                    {
                                        let changed = !selected;
                                        self.navigate(sheet.instance_path.clone());
                                        if let Some(name) = name.clone() {
                                            if self.group_net.is_some() {
                                                self.replay_selection_blink(
                                                    ui.ctx().input(|i| i.time),
                                                );
                                            } else {
                                                self.select_net(name, ui.ctx().input(|i| i.time));
                                            }
                                            if changed {
                                                self.focus_target = Some(FocusTarget::Net);
                                                self.blink_start = None;
                                                self.blink_after_transition = true;
                                            }
                                        }
                                    }
                                }
                            });
                    }
                    if let Some(message) = &self.message {
                        ui.label(RichText::new(message).color(Color32::DARK_RED));
                    }
                });
            });
        egui::Area::new(egui::Id::new(("schematic_symbol_count", &self.key)))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(rect.right() - 112., rect.top() + 12.))
            .movable(false)
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                frame().show(ui, |ui| {
                    ui.set_width(88.);
                    ui.add_sized(
                        [88., 24.],
                        egui::Label::new(
                            RichText::new(format!("{} symbols", scene.symbol_count))
                                .monospace()
                                .size(11.)
                                .color(Color32::from_rgb(100, 117, 112)),
                        )
                        .halign(egui::Align::Center),
                    );
                });
            });
    }
    fn build_document(
        &self,
        ui: &egui::Ui,
        scene: &inventory_kicad::Schematic,
        paper: Color32,
    ) -> Arc<super::cad_gpu::Scene> {
        let reference = egui::Rect::from_center_size(egui::Pos2::ZERO, egui::Vec2::splat(1e8));
        let mut builder = super::cad_gpu::Builder::default();
        builder.capture_schematic(ui, false, |p| {
            p.rect_stroke(
                egui::Rect::from_two_pos(
                    egui::Pos2::ZERO,
                    egui::pos2(
                        (scene.paper.x * self.scale) as f32,
                        (scene.paper.y * self.scale) as f32,
                    ),
                ),
                0.,
                egui::Stroke::new(1., Color32::LIGHT_GRAY),
                egui::StrokeKind::Inside,
            );
        });
        for primitive in &scene.primitives {
            if super::cad_gpu::cancelled() {
                break;
            }
            if let Primitive::Bitmap {
                center,
                size,
                width,
                height,
                rgba,
            } = primitive
            {
                builder.image(*center, *size, *width, *height, rgba);
            } else {
                builder.capture_schematic(ui, false, |p| {
                    self.paint(ui, p, reference, primitive, paper, false, false)
                });
            }
        }
        builder.finish()
    }
    fn poll_document(
        &mut self,
        ctx: &egui::Context,
        source: &inventory_kicad::Schematic,
    ) -> Option<Arc<super::cad_gpu::Scene>> {
        if let Some(scene) = self.gpu_base.get(&self.instance) {
            self.gpu_lru.retain(|k| k != &self.instance);
            self.gpu_lru.push_back(self.instance.clone());
            return Some(scene.clone());
        }
        if self
            .gpu_task
            .as_ref()
            .is_some_and(|(key, _)| key != &self.instance)
        {
            self.gpu_task = None;
        }
        if self.gpu_task.is_none() && self.gpu_error.is_none() {
            self.gpu_task = Some((
                self.instance.clone(),
                super::cad_jobs::spawn(
                    super::cad_jobs::Request::Schematic {
                        source: self
                            .bundle
                            .nodes()
                            .into_iter()
                            .find(|n| n.instance_path == self.instance)
                            .and_then(|n| self.bundle.schematics.get(&n.file))
                            .cloned()
                            .unwrap_or_else(|| Arc::new(source.clone())),
                        variables: self.bundle.project.text_variables.clone(),
                        instance: self.instance.clone(),
                    },
                    ctx,
                ),
            ));
        }
        if let Some(result) = self.gpu_task.as_ref().and_then(|(_, t)| t.poll()) {
            self.gpu_task = None;
            match result {
                Ok(scene) => {
                    self.gpu_base.insert(self.instance.clone(), scene.clone());
                    self.gpu_lru.retain(|k| k != &self.instance);
                    self.gpu_lru.push_back(self.instance.clone());
                    while self.gpu_base.len() > 1
                        && (self.gpu_base.len() > 4
                            || self
                                .gpu_base
                                .values()
                                .map(|s| s.resident_bytes())
                                .sum::<usize>()
                                > 96 * 1024 * 1024)
                    {
                        if let Some(old) = self.gpu_lru.pop_front() {
                            self.gpu_base.remove(&old);
                        } else {
                            break;
                        }
                    }
                    return Some(scene);
                }
                Err(e) => self.gpu_error = Some(e),
            }
        }
        None
    }
    fn paint_retained(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        scene: &inventory_kicad::Schematic,
        highlighted: &BTreeSet<usize>,
        paper: Color32,
    ) {
        let Some(base) = self.poll_document(ui.ctx(), scene) else {
            ui.painter_at(rect).text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                self.gpu_error
                    .as_deref()
                    .unwrap_or("Preparing schematic view…"),
                egui::FontId::proportional(16.),
                theme::MUTED,
            );
            return;
        };
        let (center, scale, ink) = (self.center, self.scale, self.highlight_ink);
        self.center = Point::default();
        self.scale = super::cad_gpu::REFERENCE as f64;
        let reference = egui::Rect::from_center_size(egui::Pos2::ZERO, egui::Vec2::splat(1e8));
        let highlight = if highlighted.is_empty() {
            None
        } else {
            if self.gpu_net.as_ref().is_none_or(|(instance, indices, _, _)| {
                instance != &self.instance || indices != highlighted
            }) {
                let mut builder = super::cad_gpu::Builder::default();
                let mut normal = super::cad_gpu::Builder::default();
                self.highlight_ink = Some(Color32::WHITE);
                for &index in highlighted {
                    if let Some(primitive) = scene.primitives.get(index) {
                        normal.capture_schematic(ui, false, |p| {
                            self.paint(ui, p, reference, primitive, paper, false, false)
                        });
                        builder.capture_schematic(ui, true, |p| {
                            self.paint(ui, p, reference, primitive, paper, true, false)
                        });
                    }
                }
                self.gpu_net = Some((self.instance.clone(), highlighted.clone(), normal.finish(), builder.finish()));
            }
            self.gpu_net.as_ref().map(|g| if ink.is_some(){g.3.clone()}else{g.2.clone()})
        };
        self.center = center;
        self.scale = scale;
        self.highlight_ink = ink;
        ui.painter_at(rect).add(super::cad_gpu::callback(
            ui.ctx(),
            rect,
            base,
            None,
            highlight,
            super::cad_gpu::View {
                glow_time: None,
                grayscale_base: self.selected_net.is_some() || !self.selected_components.is_empty(),
                center,
                scale: scale as f32,
                tint: ink.unwrap_or(Color32::WHITE),
                background: paper,
            },
        ));
    }
    fn screen(&self, rect: egui::Rect, p: Point) -> egui::Pos2 {
        rect.center()
            + egui::vec2(
                ((p.x - self.center.x) * self.scale) as f32,
                ((p.y - self.center.y) * self.scale) as f32,
            )
    }
    fn world(&self, rect: egui::Rect, p: egui::Pos2) -> Point {
        Point::new(
            self.center.x + (p.x - rect.center().x) as f64 / self.scale,
            self.center.y + (p.y - rect.center().y) as f64 / self.scale,
        )
    }
    fn paint(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        rect: egui::Rect,
        primitive: &Primitive,
        paper: Color32,
        highlighted: bool,
        grayscale: bool,
    ) {
        let appearance=|c|schematic_context_color(c,grayscale);
        let color = |ink| {
            if highlighted {
                self.highlight_ink.unwrap_or_else(|| ink_color(ink))
            } else {
                appearance(ink_color(ink))
            }
        };
        match primitive {
            Primitive::TextBox {
                at,
                size,
                angle,
                text,
                font_size,
                align,
                bold,
                italic,
                margins,
                stroke_width,
                stroke_color,
                fill_color,
            } => {
                let origin = self.screen(rect, *at);
                let (sin, cos) = (-angle.to_radians() as f32).sin_cos();
                let point =
                    |x: f32, y: f32| origin + egui::vec2(x * cos - y * sin, x * sin + y * cos);
                let width = (size.x * self.scale) as f32;
                let height = (size.y * self.scale) as f32;
                let corners = vec![
                    point(0., 0.),
                    point(width, 0.),
                    point(width, height),
                    point(0., height),
                ];
                if let Some(c) = fill_color {
                    fill_polygon(
                        painter,
                        &corners,
                        appearance(Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])),
                    );
                }
                if *stroke_width > 0. {
                    let mut border = corners.clone();
                    border.push(corners[0]);
                    painter.add(egui::Shape::line(
                        border,
                        egui::Stroke::new(
                            (*stroke_width * self.scale).max(0.5) as f32,
                            appearance(Color32::from_rgba_unmultiplied(
                                stroke_color[0], stroke_color[1], stroke_color[2], stroke_color[3],
                            )),
                        ),
                    ));
                }
                let margin = margins.map(|m| (m * self.scale) as f32);
                let inner_width = (width - margin[0] - margin[2]).max(1.);
                let inner_height = (height - margin[1] - margin[3]).max(1.);
                let mut shown = text.clone();
                for (key, value) in &self.bundle.project.text_variables {
                    shown = shown.replace(&format!("${{{key}}}"), value);
                }
                let text_scale =
                    (super::cad_text::limited_size(*font_size * self.scale * 1.3, 1., 512.) / 128.)
                        as f32;
                let font = egui::FontId::monospace(128.);
                let color = appearance(Color32::from_rgb(0, 0, 255));
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = inner_width / text_scale;
                job.halign = align_axis(align[0]);
                job.append(
                    &shown,
                    0.,
                    egui::TextFormat {
                        line_height: Some(font.size),
                        font_id: font,
                        color,
                        italics: *italic,
                        ..Default::default()
                    },
                );
                let mut galley = ui.fonts_mut(|f| f.layout_job(job));
                super::cad_text::prepare_galley(
                    &mut galley,
                    text_scale,
                    ui.ctx().pixels_per_point(),
                );
                let offset = text_box_offset(
                    egui::Rect::from_min_size(
                        egui::pos2(margin[0], margin[1]),
                        egui::vec2(inner_width, inner_height),
                    ),
                    galley.rect,
                    *align,
                );
                let (x, y) = (offset.x, offset.y);
                let clip = egui::Rect::from_points(&corners).intersect(rect);
                let clipped = painter.with_clip_rect(clip);
                clipped.add(
                    egui::epaint::TextShape::new(point(x, y), galley.clone(), color)
                        .with_angle(-angle.to_radians() as f32),
                );
                if *bold {
                    clipped.add(
                        egui::epaint::TextShape::new(point(x + 0.4, y), galley, color)
                            .with_angle(-angle.to_radians() as f32),
                    );
                }
            }
            Primitive::Bitmap { .. } => {}
            Primitive::Path {
                points,
                width,
                ink,
                fill,
                solid,
                fill_color,
                stroke_color,
            } => {
                let points: Vec<_> = points.iter().map(|p| self.screen(rect, *p)).collect();
                if *fill {
                    let fill = fill_color
                        .map(|c| appearance(Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])))
                        .unwrap_or_else(|| if *solid { color(*ink) } else { appearance(paper) });
                    fill_polygon(painter, &points, fill);
                }
                painter.add(egui::Shape::line(
                    points,
                    egui::Stroke::new(
                        (*width * self.scale) as f32,
                        if highlighted && self.highlight_ink.is_some() {
                            color(*ink)
                        } else {
                            stroke_color
                                .map(|c| appearance(Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])))
                                .unwrap_or_else(|| color(*ink))
                        },
                    ),
                ));
            }
            Primitive::Dot {
                center,
                radius,
                ink,
            } => {
                painter.circle_filled(
                    self.screen(rect, *center),
                    (*radius * self.scale) as f32,
                    color(*ink),
                );
            }
            Primitive::Text {
                at,
                text,
                size,
                angle,
                align,
                ink,
                bold,
                instances,
            } => {
                let mut text = instances.get(&self.instance).unwrap_or(text).clone();
                for (key, value) in &self.bundle.project.text_variables {
                    text = text.replace(&format!("${{{key}}}"), value);
                }
                let (text, bars) = overbar_text(&text);
                let font = egui::FontId::monospace(super::cad_text::limited_size(
                    *size * self.scale * 1.3,
                    1.,
                    512.,
                ) as f32);
                let color = color(*ink);
                let galley = super::cad_text::zoom_galley(painter, &text, font.size, 128., color);
                let anchor = egui::Align2([align_axis(align[0]), align_axis(align[1])]);
                let top_left = anchor
                    .anchor_size(self.screen(rect, *at), galley.size())
                    .min;
                for (start, end) in bars {
                    let prefix = super::cad_text::zoom_galley(
                        painter,
                        &text[..start],
                        font.size,
                        128.,
                        color,
                    )
                    .size()
                    .x;
                    let width = super::cad_text::zoom_galley(
                        painter,
                        &text[start..end],
                        font.size,
                        128.,
                        color,
                    )
                    .size()
                    .x;
                    let pivot = self.screen(rect, *at);
                    let rotate = |p: egui::Pos2| {
                        let d = p - pivot;
                        let a = -angle.to_radians() as f32;
                        pivot
                            + egui::vec2(
                                d.x * a.cos() - d.y * a.sin(),
                                d.x * a.sin() + d.y * a.cos(),
                            )
                    };
                    let y = top_left.y + font.size * 0.16;
                    painter.line_segment(
                        [
                            rotate(egui::pos2(top_left.x + prefix, y)),
                            rotate(egui::pos2(top_left.x + prefix + width, y)),
                        ],
                        egui::Stroke::new((font.size * 0.055).max(0.5), color),
                    );
                }
                painter.add(
                    egui::epaint::TextShape::new(top_left, galley.clone(), color)
                        .with_angle_and_anchor(-angle.to_radians() as f32, anchor),
                );
                if *bold {
                    painter.add(
                        egui::epaint::TextShape::new(top_left + egui::vec2(0.4, 0.), galley, color)
                            .with_angle_and_anchor(-angle.to_radians() as f32, anchor),
                    );
                }
            }
        }
    }
}
pub(super) fn prepare_document(
    source: Arc<inventory_kicad::Schematic>,
    variables: BTreeMap<String, String>,
    instance: String,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<Arc<super::cad_gpu::Scene>> {
    let bundle = Arc::new(Bundle {
        pcb: None,
        pcb_file: String::new(),
        pcb_available: false,
        pcb_error: None,
        nets: vec![],
        project: inventory_kicad::Project {
            net_classes: Default::default(),
            file: String::new(),
            root_schematic: String::new(),
            text_variables: variables,
            sheet_names: BTreeMap::new(),
        },
        root: SheetNode {
            excluded_from_bom: false,
            name: String::new(),
            file: String::new(),
            instance_path: instance.clone(),
            sheet_uuid: String::new(),
            children: vec![],
            error: None,
        },
        schematics: BTreeMap::new(),
    });
    let mut viewer = Viewer::new("prepare", "prepare", bundle, &instance);
    viewer.scale = super::cad_gpu::REFERENCE as f64;
    viewer.center = Point::default();
    let ctx = egui::Context::default();
    let mut result = None;
    super::cad_gpu::with_cancel(cancel.clone(), || {
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                result = Some(viewer.build_document(ui, &source, Color32::from_rgb(255, 255, 235)))
            });
        });
    });
    anyhow::ensure!(
        !cancel.load(std::sync::atomic::Ordering::Relaxed),
        "Preparation cancelled"
    );
    Ok(result.unwrap())
}

// Conservative world-space bounds, cached per document. Rotated text and boxes use
// an enclosing square so they cannot disappear at the viewport edges.
fn component_at(
    scene: &inventory_kicad::Schematic,
    point: Point,
) -> Option<&inventory_kicad::Component> {
    let point = egui::pos2(point.x as f32, point.y as f32);
    scene
        .components
        .iter()
        .filter_map(|component| {
            let mut bounds = egui::Rect::NOTHING;
            for &index in &component.primitives {
                if let Some(
                    primitive @ Primitive::Path {
                        ink: Ink::Symbol, ..
                    },
                ) = scene.primitives.get(index)
                {
                    bounds = bounds.union(primitive_bounds(primitive));
                }
            }
            bounds
                .expand(0.3)
                .contains(point)
                .then_some((bounds.area(), component))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, component)| component)
}
fn primitive_bounds(primitive: &Primitive) -> egui::Rect {
    let square = |p: Point, radius: f64| {
        egui::Rect::from_center_size(
            egui::pos2(p.x as f32, p.y as f32),
            egui::Vec2::splat((radius * 2.) as f32),
        )
    };
    match primitive {
        Primitive::Path { points, width, .. } => {
            let mut b = egui::Rect::NOTHING;
            for p in points {
                b.extend_with(egui::pos2(p.x as f32, p.y as f32));
            }
            b.expand(*width as f32)
        }
        Primitive::Dot { center, radius, .. } => square(*center, *radius),
        Primitive::Bitmap { center, size, .. } => square(*center, size.x.abs().max(size.y.abs())),
        Primitive::TextBox { at, size, .. } => square(*at, size.x.abs() + size.y.abs()),
        Primitive::Text {
            at,
            text,
            size,
            instances,
            ..
        } => {
            if text.contains("${") || instances.values().any(|s| s.contains("${")) {
                return egui::Rect::EVERYTHING;
            }
            let count = instances
                .values()
                .map(|s| s.chars().count())
                .chain(std::iter::once(text.chars().count()))
                .max()
                .unwrap_or(1);
            square(*at, *size * (count.max(2) as f64 + 2.) * 1.5)
        }
    }
}

fn text_box_offset(inner: egui::Rect, text: egui::Rect, align: [i8; 2]) -> egui::Vec2 {
    let factor = |a: i8| match a {
        -1 => 0.,
        1 => 1.,
        _ => 0.5,
    };
    inner.min.to_vec2()
        + egui::vec2(
            (inner.width() - text.width()) * factor(align[0]),
            (inner.height() - text.height()) * factor(align[1]),
        )
        - text.min.to_vec2()
}
fn align_axis(value: i8) -> egui::Align {
    if value < 0 {
        egui::Align::Min
    } else if value > 0 {
        egui::Align::Max
    } else {
        egui::Align::Center
    }
}
fn ink_color(ink: Ink) -> Color32 {
    match ink {
        Ink::Wire => Color32::from_rgb(0, 125, 0),
        Ink::Bus => Color32::from_rgb(0, 60, 170),
        Ink::Symbol | Ink::Pin => Color32::from_rgb(150, 25, 25),
        Ink::Text => Color32::from_rgb(20, 90, 90),
        Ink::Note => Color32::from_rgb(0, 0, 255),
        Ink::Label => Color32::from_rgb(25, 90, 25),
        Ink::Sheet => Color32::from_rgb(145, 40, 145),
        Ink::Paper => Color32::GRAY,
    }
}
/// Ear clipping handles concave filled symbol polygons without another geometry crate.
pub(super) fn fill_polygon(painter: &egui::Painter, points: &[egui::Pos2], color: Color32) {
    let mut points = points.to_vec();
    if points.first() == points.last() {
        points.pop();
    }
    if points.len() < 3 || points.len() > 4096 {
        return;
    }
    let cross = |a: egui::Pos2, b: egui::Pos2, c: egui::Pos2| {
        (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
    };
    let area: f32 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a.x * b.y - b.x * a.y)
        .sum();
    let direction = if area >= 0. { 1. } else { -1. };
    let mut indices: Vec<usize> = (0..points.len()).collect();
    let mut mesh = egui::Mesh::default();
    for point in &points {
        mesh.colored_vertex(*point, color);
    }
    let mut guard = 0;
    while indices.len() > 2 && guard < points.len() * points.len() {
        guard += 1;
        let mut clipped = false;
        for i in 0..indices.len() {
            let a = indices[(i + indices.len() - 1) % indices.len()];
            let b = indices[i];
            let c = indices[(i + 1) % indices.len()];
            if cross(points[a], points[b], points[c]) * direction <= 0.0001 {
                continue;
            }
            let inside = indices.iter().any(|j| {
                *j != a
                    && *j != b
                    && *j != c
                    && cross(points[a], points[b], points[*j]) * direction >= 0.
                    && cross(points[b], points[c], points[*j]) * direction >= 0.
                    && cross(points[c], points[a], points[*j]) * direction >= 0.
            });
            if inside {
                continue;
            }
            mesh.add_triangle(a as u32, b as u32, c as u32);
            indices.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// KiCad overbar markup is not literal text; retain byte ranges for the overline strokes.
fn overbar_text(source: &str) -> (String, Vec<(usize, usize)>) {
    let mut output = String::new();
    let mut bars = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find("~{") {
        output.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let Some(end) = tail.find('}') else {
            output.push_str(&rest[start..]);
            return (output, bars);
        };
        let offset = output.len();
        output.push_str(&tail[..end]);
        bars.push((offset, output.len()));
        rest = &tail[end + 1..];
    }
    output.push_str(rest);
    (output, bars)
}

#[cfg(test)]
mod monitoring_tests {
    use super::*;
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn slow_render_preparation_keeps_previous_document_and_gpu_scene_visible(){
        let old=Arc::new(inventory_kicad::load_project("demo.kicad_pro",|name|Ok(if name.ends_with(".kicad_pro"){"{}".into()}else{"(kicad_sch (version 20250114) (uuid root))".into()})).unwrap());
        let next=Arc::new((*old).clone());
        let instance=old.root.instance_path.clone();
        let mut viewer=Viewer::new("Demo","test",old.clone(),&instance);
        let old_scene=super::super::cad_gpu::Builder::default().finish();viewer.gpu_base.insert(instance.clone(),old_scene.clone());
        let prepared=super::super::cad_gpu::Builder::default().finish();let expected=prepared.clone();
        let (release,wait)=std::sync::mpsc::channel();
        let task=Task::spawn(move |_|{wait.recv_timeout(std::time::Duration::from_secs(5))?;Ok(prepared)},Box::new(||{}));
        viewer.pending_bundle=Some((next.clone(),instance.clone(),task));
        let ctx=egui::Context::default();viewer.poll_bundle_refresh(&ctx);
        assert!(Arc::ptr_eq(&viewer.bundle,&old));assert!(Arc::ptr_eq(&viewer.gpu_base[&instance],&old_scene));
        release.send(()).unwrap();
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);
        while viewer.pending_bundle.is_some(){assert!(std::time::Instant::now()<deadline);viewer.poll_bundle_refresh(&ctx);std::thread::sleep(std::time::Duration::from_millis(5));}
        assert!(Arc::ptr_eq(&viewer.bundle,&next));assert!(Arc::ptr_eq(&viewer.gpu_base[&instance],&expected));
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn received_revision_keeps_open_view_and_old_content_until_refresh_finishes() {
        let folder=tempfile::tempdir().unwrap();
        let project=folder.path().join("demo.kicad_pro");let schematic=folder.path().join("demo.kicad_sch");
        std::fs::write(&project,"{}").unwrap();
        std::fs::write(&schematic,"(kicad_sch (version 20250114) (uuid root))").unwrap();
        let mut data=Inventory::default();data.ensure_project("Demo").unwrap();
        data.projects[0].kicad=Some(KicadProject{path:project.to_string_lossy().into_owned(),revision:"git-snapshot:project:commit1".into(),..Default::default()});
        let ctx=egui::Context::default();let mut state=KicadState::default();
        let key=cache_key(data.projects[0].kicad.as_ref().unwrap());
        state.ensure(data.projects[0].kicad.as_ref().unwrap(),&ctx);
        fn finish(state:&mut KicadState,key:&str,ctx:&egui::Context){
            let deadline=std::time::Instant::now()+std::time::Duration::from_secs(10);
            while state.slots[key].task.is_some() || state.viewer.as_ref().is_some_and(|viewer|viewer.pending_bundle.is_some()){assert!(std::time::Instant::now()<deadline);state.poll(ctx);std::thread::sleep(std::time::Duration::from_millis(5));}
        }
        finish(&mut state,&key,&ctx);
        let old=state.slots[&key].result.as_ref().unwrap().as_ref().unwrap().clone();
        let mut viewer=Viewer::new("Demo",&key,old.clone(),"/root");
        viewer.center=Point{x:12.,y:34.};viewer.scale=2.5;viewer.fit=false;
        viewer.selected_component=Some(("/root".into(),"symbol-id".into()));
        state.viewer=Some(viewer);state.opened_project=Some("Demo".into());
        data.projects[0].kicad.as_mut().unwrap().revision="git-snapshot:project:commit2".into();
        std::fs::write(&schematic,"(kicad_sch (version 20250114) (uuid root) (wire (pts (xy 0 0) (xy 10 0)) (stroke (width 0) (type default)) (uuid w1)))").unwrap();
        assert_eq!(key,cache_key(data.projects[0].kicad.as_ref().unwrap()));
        state.refresh_managed_snapshots(&data,&ctx);state.retain_links(&data,None,Some("Demo"));
        assert!(state.slots[&key].task.is_some());assert!(Arc::ptr_eq(&state.viewer.as_ref().unwrap().bundle,&old));
        finish(&mut state,&key,&ctx);
        let viewer=state.viewer.as_ref().unwrap();
        assert!(!Arc::ptr_eq(&viewer.bundle,&old));assert_eq!(viewer.center.x,12.);assert_eq!(viewer.center.y,34.);assert_eq!(viewer.scale,2.5);assert!(!viewer.fit);
        assert_eq!(viewer.instance,"/root");assert_eq!(viewer.selected_component.as_ref().unwrap().1,"symbol-id");assert_eq!(state.opened_project.as_deref(),Some("Demo"));
        let good=viewer.bundle.clone();
        data.projects[0].kicad.as_mut().unwrap().revision="git-snapshot:project:commit3".into();std::fs::write(&schematic,"broken schematic").unwrap();
        state.refresh_managed_snapshots(&data,&ctx);finish(&mut state,&key,&ctx);
        assert!(Arc::ptr_eq(&state.viewer.as_ref().unwrap().bundle,&good));assert!(state.slots[&key].load_error.is_some());
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn board_is_loaded_on_demand_without_reloading_schematics() {
        let folder = tempfile::tempdir().unwrap();
        let project_path = folder.path().join("demo.kicad_pro");
        let sheet_path = folder.path().join("demo.kicad_sch");
        let board_path = folder.path().join("demo.kicad_pcb");
        std::fs::write(&project_path, "{}").unwrap();
        std::fs::write(&sheet_path, "(kicad_sch (version 20250114) (uuid root))").unwrap();
        std::fs::write(&board_path, "not a valid board yet").unwrap();
        let link = KicadProject {
            path: project_path.to_string_lossy().into(),
            ..Default::default()
        };
        let key = cache_key(&link);
        let ctx = egui::Context::default();
        let mut state = KicadState::default();
        state.ensure(&link, &ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.slots[&key].task.is_some() {
            assert!(std::time::Instant::now() < deadline);
            state.poll(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let bundle = state.slots[&key]
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        assert!(bundle.pcb_available);
        assert!(bundle.pcb.is_none() && bundle.pcb_error.is_none());
        assert!(state.slots[&key].pcb_task.is_none());
        assert!(state.pcb_viewer.is_none());
        let sheet = bundle.schematics["demo.kicad_sch"].clone();
        // If PCB selection rereads the schematic hierarchy, this now fails.
        std::fs::remove_file(&sheet_path).unwrap();
        std::fs::write(
            &board_path,
            r#"(kicad_pcb (version 20250114) (layers (0 "F.Cu" signal)))"#,
        )
        .unwrap();
        let slot = state.slots.get_mut(&key).unwrap();
        slot.pcb_requested = Some("Demo".into());
        slot.pcb_open_requested = true;
        assert!(
            state.has_pcb("Demo"),
            "PCB tab is available before parsing finishes"
        );
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                state.pcb_panel(ui, &Inventory::default(), 0, "Demo");
            });
        });
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Loading PCB…")));
        state.request_pcb(&key, &ctx);
        while state.slots[&key].pcb_task.is_some() {
            assert!(std::time::Instant::now() < deadline);
            state.poll(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let loaded = state.slots[&key].result.as_ref().unwrap().as_ref().unwrap();
        assert!(loaded.pcb.is_some(), "{:?}", loaded.pcb_error);
        assert!(Arc::ptr_eq(&sheet, &loaded.schematics["demo.kicad_sch"]));
        assert_eq!(state.slots[&key].generation, 1);
        assert!(state.has_pcb("Demo"));
        assert_eq!(state.take_opened_project().as_deref(), Some("Demo"));
        state.ensure(&link, &ctx);
        assert!(state.slots[&key].task.is_none());
        assert!(state.slots[&key].pcb_task.is_none());
    }
    #[test]
    fn request_navigation_selects_every_designator_across_sheets_and_pcb() {
        let files = BTreeMap::from([
            ("demo.kicad_pro", "{}"),
            (
                "demo.kicad_sch",
                r#"(kicad_sch (version 20250114) (uuid root)
                (symbol (lib_id "Missing") (at 10 10 0) (uuid r1) (property "Reference" "R1"))
                (symbol (lib_id "Missing") (at 30 10 0) (uuid r2) (property "Reference" "R2"))
                (sheet (at 0 30) (size 20 20) (uuid child) (property "Sheetname" "Child") (property "Sheetfile" "child.kicad_sch")))"#,
            ),
            (
                "child.kicad_sch",
                r#"(kicad_sch (version 20250114) (uuid childroot)
                (symbol (lib_id "Missing") (at 10 10 0) (uuid r3) (property "Reference" "R3")))"#,
            ),
            (
                "demo.kicad_pcb",
                r#"(kicad_pcb (version 20250114) (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
                (footprint "R" (layer "F.Cu") (at 1 1) (property "Reference" "R1") (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu")))
                (footprint "R" (layer "F.Cu") (at 10 10) (property "Reference" "R2") (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu")))
                (footprint "R" (layer "B.Cu") (at 20 20) (property "Reference" "R3") (pad "1" smd rect (at 0 0) (size 2 2) (layers "B.Cu"))))"#,
            ),
        ]);
        let bundle = Arc::new(
            inventory_kicad::load_project("demo.kicad_pro", |p| {
                files
                    .get(p)
                    .map(|s| s.to_string())
                    .ok_or_else(|| inventory_kicad::Error::new("Missing"))
            })
            .unwrap(),
        );
        let link = KicadProject {
            path: "demo.kicad_pro".into(),
            ..Default::default()
        };
        let key = cache_key(&link);
        let mut state = KicadState::default();
        state.slots.insert(
            key.clone(),
            Slot {
                link: link.clone(),
                generation: 1,
                dirty: false,
                load_error: None,
                #[cfg(not(target_arch = "wasm32"))]
                watcher: None,
                task: None,
                pcb_task: None,
                pcb_requested: None,
                pcb_open_requested: false,
                result: Some(Ok(bundle.clone())),
            },
        );
        let ctx = egui::Context::default();
        state
            .navigate_designators("Demo", &link, "R1-R3", false, &ctx)
            .unwrap();
        assert_eq!(state.take_opened_project().as_deref(), Some("Demo"));
        assert_eq!(state.opened_document_tab(), 3);
        let viewer = state.viewer.as_mut().unwrap();
        assert_eq!(viewer.selected_components.len(), 3);
        assert_eq!(
            viewer
                .selected_in(&bundle.schematics["demo.kicad_sch"])
                .len(),
            2
        );
        let prepared = prepare_document(
            bundle.schematics["demo.kicad_sch"].clone(),
            BTreeMap::new(),
            viewer.instance.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        viewer.gpu_base.insert(viewer.instance.clone(), prepared);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.paint_retained(
                    ui,
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800., 600.)),
                    &bundle.schematics["demo.kicad_sch"],
                    &BTreeSet::new(),
                    Color32::WHITE,
                );
            });
        });
        viewer.navigate("/root/child".into());
        assert_eq!(
            viewer
                .selected_in(&bundle.schematics["child.kicad_sch"])
                .len(),
            1
        );
        assert!(matches!(viewer.focus_target, Some(FocusTarget::Components)));
        state
            .navigate_designators("Demo", &link, "R1, R2, R3", true, &ctx)
            .unwrap();
        assert_eq!(state.opened_document_tab(), 4);
        assert_eq!(
            state
                .pcb_viewer
                .as_ref()
                .unwrap()
                .selected_footprints()
                .len(),
            3
        );
        state.sync_schematic_selection();
        assert_eq!(
            state
                .pcb_viewer
                .as_ref()
                .unwrap()
                .selected_footprints()
                .len(),
            3,
            "cross-selection preserves the full group"
        );
        state.viewer.as_mut().unwrap().select_component(
            "/root".into(),
            "r1".into(),
            Point::new(10., 10.),
            false,
        );
        state.sync_schematic_selection();
        assert_eq!(
            state.pcb_viewer.as_ref().unwrap().selected_footprints(),
            &BTreeSet::from(["R1".into()])
        );
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn request_navigation_waits_for_lazy_project_and_board_loading() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("demo.kicad_pro"), "{}").unwrap();
        std::fs::write(
            dir.path().join("demo.kicad_sch"),
            r#"(kicad_sch (version 20250114) (uuid root)
            (symbol (lib_id "Missing") (at 10 10 0) (uuid u1) (property "Reference" "U1")))"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("demo.kicad_pcb"),r#"(kicad_pcb (version 20250114) (layers (0 "F.Cu" signal))
            (footprint "U" (layer "F.Cu") (at 10 10) (property "Reference" "U1") (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu"))))"#).unwrap();
        let link = KicadProject {
            path: dir.path().join("demo.kicad_pro").to_string_lossy().into(),
            ..Default::default()
        };
        let mut state = KicadState::default();
        let ctx = egui::Context::default();
        state
            .navigate_designators("Demo", &link, "U1", true, &ctx)
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.designator_navigation.is_some() {
            assert!(std::time::Instant::now() < deadline);
            state.poll(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            state.navigation_error.is_none(),
            "{:?}",
            state.navigation_error
        );
        assert_eq!(state.opened_document_tab(), 4);
        assert_eq!(
            state.pcb_viewer.as_ref().unwrap().selected_footprints(),
            &BTreeSet::from(["U1".into()])
        );
    }
    #[test]
    fn footprint_and_symbol_selection_sync_by_designator_without_switching_tabs() {
        let bundle = Arc::new(inventory_kicad::load_schematics("demo.kicad_pro", |path| match path {
            "demo.kicad_pro" => Ok("{}".into()),
            "demo.kicad_sch" => Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (lib_symbols (symbol "Demo:IC" (symbol "IC_1_1"
                    (rectangle (start -2 -2) (end 2 2) (stroke (width 0.2)) (fill (type background))))))
                (symbol (lib_id "Demo:IC") (at 10 10 0) (uuid ic1) (property "Reference" "U1")))"#.into()),
            _ => Err(inventory_kicad::Error::new("Missing")),
        }).unwrap());
        let board = Arc::new(inventory_kicad::Pcb::parse(r#"(kicad_pcb (version 20250114)
            (layers (0 "F.Cu" signal)) (footprint "Demo:IC" (layer "F.Cu") (at 5 5)
                (property "Reference" "U1") (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu"))))"#).unwrap());
        let mut state = KicadState::default();
        state.slots.insert(
            "test".into(),
            Slot {
                link: KicadProject::default(),
                generation: 1,
                dirty: false,
                load_error: None,
                #[cfg(not(target_arch = "wasm32"))]
                watcher: None,
                task: None,
                pcb_task: None,
                pcb_requested: None,
                pcb_open_requested: false,
                result: Some(Ok(bundle.clone())),
            },
        );
        state.pcb_viewer = Some(super::super::pcb::PcbViewer::new("Demo", "test", board));
        state.select_pcb_footprint("test", "Demo", Some("U1"));
        let viewer = state.viewer.as_ref().unwrap();
        assert_eq!(
            viewer.selected_component,
            Some(("/root".into(), "ic1".into()))
        );
        assert!(matches!(viewer.focus_target, Some(FocusTarget::Part(_))));
        assert_eq!(
            component_reference(&bundle, "/root", "ic1").as_deref(),
            Some("U1")
        );
        let scene = &bundle.schematics["demo.kicad_sch"];
        assert_eq!(
            component_at(scene, Point::new(10., 10.)).unwrap().uuid,
            "ic1"
        );
        assert!(component_at(scene, Point::new(50., 50.)).is_none());
        // A body/search selection in schematic reaches PCB without producing a PCB click event.
        state.last_schematic_selection = None;
        state.sync_schematic_selection();
        let pcb = state.pcb_viewer.as_mut().unwrap();
        assert_eq!(pcb.selected_footprint(), Some("U1"));
        assert!(pcb.take_footprint_change().is_none());
        assert!(state.take_opened_project().is_none());
        state.viewer.as_mut().unwrap().clear_selection();
        state.sync_schematic_selection();
        assert!(
            state
                .pcb_viewer
                .as_ref()
                .unwrap()
                .selected_footprint()
                .is_none()
        );
        state.select_pcb_footprint("test", "Demo", None);
        assert!(state.viewer.as_ref().unwrap().selected_component.is_none());
    }
    #[test]
    fn pcb_generated_net_names_match_schematic_by_reference_and_pin_number() {
        let bundle = inventory_kicad::load_schematics("demo.kicad_pro", |path| match path {
            "demo.kicad_pro" => Ok("{}".into()),
            "demo.kicad_sch" => Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (lib_symbols (symbol "Demo:R" (symbol "R_1_1"
                    (pin passive line (at 0 0 0) (length 2.54) (name "~") (number "1")))))
                (symbol (lib_id "Demo:R") (at 10 10 0) (unit 1) (uuid r1)
                    (property "Reference" "R1") (property "Value" "1k")))"#
                .into()),
            _ => Err(inventory_kicad::Error::new("Missing")),
        })
        .unwrap();
        let board = inventory_kicad::Pcb::parse(
            r#"(kicad_pcb (version 20250114)
            (layers (0 "F.Cu" signal)) (net 1 "Net-(R1-Pad1)")
            (footprint "Demo:R" (layer "F.Cu") (at 0 0)
                (property "Reference" "R1")
                (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu") (net 1))))"#,
        )
        .unwrap();
        assert_eq!(board.pads[0].reference, "R1");
        assert!(board.items.iter().any(|item| item.net == "Net-(R1-Pad1)"));
        let matched = pcb_net_by_pins(&bundle, &board, "Net-(R1-Pad1)").unwrap();
        assert!(matched.name.starts_with("Net-"));
        assert_eq!(
            schematic_pcb_net(&bundle, &board, &matched.name).as_deref(),
            Some("Net-(R1-Pad1)")
        );
        assert!(schematic_pcb_net(&bundle, &board, "missing").is_none());
        assert!(pcb_net_by_pins(&bundle, &board, "not-present").is_none());
        let component = &bundle.schematics["demo.kicad_sch"].components[0];
        assert!(matched.members[0].primitives.contains(&component.pins["1"]));
    }
    #[test]
    fn cad_shortcuts_switch_open_views_and_escape_clears_both_selections() {
        let bundle = Arc::new(
            inventory_kicad::load_schematics("demo.kicad_pro", |path| match path {
                "demo.kicad_pro" => Ok("{}".into()),
                "demo.kicad_sch" => Ok("(kicad_sch (version 20250114) (uuid root))".into()),
                _ => Err(inventory_kicad::Error::new("Missing")),
            })
            .unwrap(),
        );
        let board = Arc::new(
            inventory_kicad::Pcb::parse("(kicad_pcb (layers (0 \"F.Cu\" signal)))").unwrap(),
        );
        let mut state = KicadState {
            viewer: Some(Viewer::new("Demo", "test", bundle, "/root")),
            ..Default::default()
        };
        state.viewer.as_mut().unwrap().select_net("SIG".into(), 0.);
        state.pcb_viewer = Some(super::super::pcb::PcbViewer::new("Demo", "test", board));
        state
            .pcb_viewer
            .as_mut()
            .unwrap()
            .select_schematic_net(Some("SIG".into()));
        let ctx = egui::Context::default();
        let mut tab = 3;
        for (key, expected) in [
            (egui::Key::Tab, 4),
            (egui::Key::Tab, 3),
            (egui::Key::Escape, 3),
        ] {
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                    ..Default::default()
                },
                |ctx| {
                    tab = state.cad_shortcuts(ctx, "Demo", tab);
                },
            );
            assert_eq!(tab, expected);
        }
        assert!(state.viewer.as_ref().unwrap().selected_net.is_none());
        assert!(state.pcb_viewer.as_ref().unwrap().selected_net().is_none());
        assert!(state.viewer.as_ref().unwrap().focus_target.is_none());
    }
    #[test]
    fn project_tree_opens_a_closable_3d_tab_before_nets_without_opening_pcb_tab() {
        let bundle=Arc::new(inventory_kicad::load_project("demo.kicad_pro",|file|match file {
            "demo.kicad_pro"=>Ok("{}".into()),
            "demo.kicad_sch"=>Ok("(kicad_sch (version 20250114) (uuid root))".into()),
            "demo.kicad_pcb"=>Ok(r#"(kicad_pcb (layers (0 "F.Cu" signal) (44 "Edge.Cuts" user))
                (gr_rect (start 0 0) (end 10 10) (stroke (width 0.05) (type default)) (fill none) (layer "Edge.Cuts")))"#.into()),
            _=>Err(inventory_kicad::Error::new("Missing")),
        }).unwrap());
        let link = KicadProject {
            path: "demo.kicad_pro".into(),
            ..Default::default()
        };
        let slot_key = cache_key(&link);
        let mut state = KicadState::default();
        state.slots.insert(
            slot_key.clone(),
            Slot {
                link: link.clone(),
                generation: 1,
                dirty: false,
                load_error: None,
                #[cfg(not(target_arch = "wasm32"))]
                watcher: None,
                task: None,
                pcb_task: None,
                pcb_requested: None,
                pcb_open_requested: false,
                result: Some(Ok(bundle)),
            },
        );
        let ctx = egui::Context::default();
        let draw = |state: &mut KicadState, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800., 600.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .show(ctx, |ui| state.project_tree(ui, "Demo", &link, 0));
                },
            )
        };
        draw(&mut state, vec![]);
        let output = draw(&mut state, vec![]);
        let text_pos = |name: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(t) if t.galley.text() == name => {
                        Some(t.pos + t.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap()
        };
        let pos = text_pos("3D board");
        assert!(pos.y < text_pos("Nets").y);
        assert!(text_pos("Nets").y < text_pos("Net Classes").y);
        draw(
            &mut state,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        draw(
            &mut state,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert!(state.has_3d("Demo"));
        assert_eq!(state.opened_document_tab(), 5);
        assert_eq!(state.take_opened_project().as_deref(), Some("Demo"));
        assert!(!state.has_pcb("Demo"));
        state.close_3d();
        assert!(!state.has_3d("Demo"));
        assert!(state.board3d.is_none());
        assert!(
            state.slots[&slot_key].result.is_some(),
            "closing 3D retains the parsed project"
        );
    }
    #[test]
    fn grouped_net_rows_expand_and_select_original_net_with_left_aligned_children() {
        let bundle = Arc::new(
            inventory_kicad::load_schematics("demo.kicad_pro", |path| match path {
                "demo.kicad_pro" => Ok("{}".into()),
                "demo.kicad_sch" => Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (wire (pts (xy 0 0) (xy 10 0))) (label "USB_N" (at 0 0 0))
                (wire (pts (xy 0 10) (xy 10 10))) (label "USB_P" (at 0 10 0)))"#
                    .into()),
                _ => Err(inventory_kicad::Error::new("Missing")),
            })
            .unwrap(),
        );
        let ctx = egui::Context::default();
        ctx.style_mut(|s| s.animation_time = 0.);
        let mut state = KicadState::default();
        let draw = |state: &mut KicadState, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(500., 400.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        state.net_rows(
                            ui,
                            ("Demo", "test", "all_nets"),
                            &bundle,
                            bundle.nets.iter().map(|n| n.name.clone()),
                            3,
                            None,
                        );
                    });
                },
            )
        };
        let find = |output: &egui::FullOutput, name: &str| {
            output.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == name => {
                    Some(t.pos + egui::vec2(1., t.galley.size().y * 0.5))
                }
                _ => None,
            })
        };
        let click = |pos| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ]
        };
        let output = draw(&mut state, vec![]);
        assert!(find(&output, "USB_N").is_none());
        let group = find(&output, "USB").unwrap();
        draw(&mut state, click(group));
        assert_eq!(
            state.viewer.as_ref().unwrap().group_names,
            BTreeSet::from(["USB_N".into(), "USB_P".into()])
        );
        assert_eq!(
            state.viewer.as_ref().unwrap().selected_net.as_deref(),
            Some("Net group: USB")
        );
        let output = draw(&mut state, vec![]);
        assert!(
            find(&output, "USB_N").is_none(),
            "clicking the label selects without expanding"
        );
        let arrow = group - egui::vec2(7. + ctx.style().spacing.indent * 0.5, 0.);
        draw(&mut state, click(arrow));
        let output = draw(&mut state, vec![]);
        let n = find(&output, "USB_N").unwrap();
        let p = find(&output, "USB_P").unwrap();
        assert!((n.x - p.x).abs() < 0.1);
        assert!(
            n.x < 150.,
            "children must be left-aligned rather than centered in the view"
        );
        draw(&mut state, click(n));
        assert_eq!(
            state.viewer.as_ref().unwrap().selected_net.as_deref(),
            Some("USB_N")
        );
    }
    #[test]
    fn net_class_colors_survive_reopening_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("net-class-colors.json");
        let mut state = KicadState::new(path.clone());
        state
            .class_styles
            .entry("project".into())
            .or_default()
            .insert(
                "Power".into(),
                super::super::net_class_style::ClassStyle {
                    color: [239, 83, 80],
                    enabled: true,
                },
            );
        state.save_class_styles();
        assert!(state.navigation_error.is_none());
        let reopened = KicadState::new(path);
        assert!(reopened.class_styles["project"]["Power"].enabled);
        assert_eq!(
            reopened.class_styles["project"]["Power"].color,
            [239, 83, 80]
        );
    }
    #[test]
    fn net_class_selection_combines_nets_and_single_selection_clears_group() {
        let bundle = Arc::new(
            inventory_kicad::load_schematics("demo.kicad_pro", |path| match path {
                "demo.kicad_pro" => Ok("{}".into()),
                "demo.kicad_sch" => Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (wire (pts (xy 0 0) (xy 10 0))) (label "SIG" (at 0 0 0))
                (wire (pts (xy 0 10) (xy 10 10))) (label "OTHER" (at 0 10 0)))"#
                    .into()),
                _ => Err(inventory_kicad::Error::new("Missing")),
            })
            .unwrap(),
        );
        let board = Arc::new(
            inventory_kicad::Pcb::parse(
                r#"(kicad_pcb
            (layers (0 "F.Cu" signal)) (net 1 "SIG") (net 2 "OTHER"))"#,
            )
            .unwrap(),
        );
        let mut state = KicadState {
            pcb_viewer: Some(super::super::pcb::PcbViewer::new("Demo", "test", board)),
            ..Default::default()
        };
        let names = BTreeSet::from(["SIG".into(), "OTHER".into()]);
        state.select_tree_class(
            "Demo",
            "test",
            bundle.clone(),
            ("Signals", names.clone()),
            4,
            10.,
        );
        assert_eq!(state.opened_document_tab(), 4);
        let viewer = state.viewer.as_ref().unwrap();
        assert_eq!(viewer.group_names, names);
        assert!(!viewer.reveal_net);
        let merged = viewer.selected_project_net(&bundle).unwrap();
        assert_eq!(merged.members.len(), 1);
        let expected: BTreeSet<_> = bundle
            .nets
            .iter()
            .flat_map(|n| n.members.iter())
            .flat_map(|m| m.primitives.iter().copied())
            .collect();
        assert_eq!(
            merged.members[0]
                .primitives
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert_eq!(state.pcb_viewer.as_ref().unwrap().selected_nets(), &names);
        state.open_3d = Some(("Demo".into(), "test".into()));
        state.select_tree_class("Demo", "test", bundle.clone(), ("Signals", names), 5, 11.);
        assert_eq!(state.opened_document_tab(), 5);
        state.select_tree_net("Demo", "test", bundle, "SIG", 4, 12.);
        assert!(state.viewer.as_ref().unwrap().group_net.is_none());
        assert_eq!(
            state.pcb_viewer.as_ref().unwrap().selected_nets(),
            &BTreeSet::from(["SIG".into()])
        );
    }
    #[test]
    fn tree_net_selection_keeps_active_view_and_defaults_to_schematic() {
        let bundle = Arc::new(
            inventory_kicad::load_schematics("demo.kicad_pro", |path| match path {
                "demo.kicad_pro" => Ok("{}".into()),
                "demo.kicad_sch" => Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (wire (pts (xy 0 0) (xy 10 0))) (label "SIG" (at 0 0 0)))"#
                    .into()),
                _ => Err(inventory_kicad::Error::new("Missing")),
            })
            .unwrap(),
        );
        let board = Arc::new(
            inventory_kicad::Pcb::parse(
                r#"(kicad_pcb
            (layers (0 "F.Cu" signal)) (net 1 "SIG")
            (segment (start 0 0) (end 10 0) (width 0.5) (layer "F.Cu") (net 1)))"#,
            )
            .unwrap(),
        );
        let mut state = KicadState::default();
        state.select_tree_net("Demo", "test", bundle.clone(), "SIG", 0, 10.);
        assert_eq!(state.opened_document_tab(), 3);
        assert_eq!(
            state.viewer.as_ref().unwrap().selected_net.as_deref(),
            Some("SIG")
        );
        state.pcb_viewer = Some(super::super::pcb::PcbViewer::new("Demo", "test", board));
        state.select_tree_net("Demo", "test", bundle.clone(), "SIG", 4, 11.);
        assert_eq!(state.opened_document_tab(), 4);
        assert_eq!(
            state.pcb_viewer.as_ref().unwrap().selected_net(),
            Some("SIG")
        );
        state.open_3d = Some(("Demo".into(), "test".into()));
        state.opened_3d = true;
        state.select_tree_net("Demo", "test", bundle.clone(), "SIG", 5, 11.5);
        assert_eq!(state.opened_document_tab(), 5);
        assert_eq!(
            state.pcb_viewer.as_ref().unwrap().selected_net(),
            Some("SIG")
        );
        state.select_tree_net("Demo", "test", bundle, "SIG", 3, 12.);
        assert_eq!(state.opened_document_tab(), 3);
    }
    #[test]
    fn pcb_selection_preselects_schematic_and_defers_blink_until_it_is_visible() {
        let bundle = inventory_kicad::load_schematics("demo.kicad_pro", |path| match path {
            "demo.kicad_pro" => Ok("{}".into()),
            "demo.kicad_sch" => Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (wire (pts (xy 0 0) (xy 10 0))) (label "SIG" (at 0 0 0)))"#
                .into()),
            _ => Err(inventory_kicad::Error::new("Missing")),
        })
        .unwrap();
        let mut state = KicadState::default();
        state.slots.insert(
            "test".into(),
            Slot {
                link: KicadProject::default(),
                generation: 1,
                dirty: false,
                load_error: None,
                #[cfg(not(target_arch = "wasm32"))]
                watcher: None,
                task: None,
                pcb_task: None,
                pcb_requested: None,
                pcb_open_requested: false,
                result: Some(Ok(Arc::new(bundle))),
            },
        );
        // PCB root-local names begin with '/', while schematic root names do not.
        state.select_pcb_net("test", "Demo", Some("/SIG"), 0.);
        let viewer = state.viewer.as_mut().unwrap();
        assert_eq!(viewer.selected_net.as_deref(), Some("SIG"));
        assert!(matches!(viewer.focus_target, Some(FocusTarget::Net)));
        assert!(viewer.blink_after_transition);
        assert!(viewer.blink_start.is_none());
        viewer.update_net_blink(30.);
        assert!(
            viewer.blink_start.is_none(),
            "hidden view must wait for camera transition"
        );
        viewer.focus_target = None;
        viewer.update_net_blink(30.5);
        assert_eq!(viewer.blink_start, Some(30.5));
        assert!(
            state.take_opened_project().is_none(),
            "PCB selection does not switch tabs"
        );
        let board = Arc::new(
            inventory_kicad::Pcb::parse(
                r#"(kicad_pcb (version 20250114)
            (layers (0 "F.Cu" signal)) (net 1 "/SIG") (net 2 "BOARD_ONLY")
            (segment (start 0 0) (end 10 0) (width 0.2) (layer "F.Cu") (net 1)))"#,
            )
            .unwrap(),
        );
        state.pcb_viewer = Some(super::super::pcb::PcbViewer::new(
            "Demo",
            "test",
            board.clone(),
        ));
        state.sync_schematic_selection();
        let pcb = state.pcb_viewer.as_mut().unwrap();
        assert_eq!(pcb.selected_net(), Some("/SIG"));
        assert!(
            pcb.take_net_change().is_none(),
            "cross-selection must not echo back into schematic animation"
        );
        assert!(
            state.take_opened_project().is_none(),
            "schematic selection does not change tabs"
        );
        state.sync_schematic_selection();
        assert_eq!(
            state.pcb_viewer.as_ref().unwrap().selected_net(),
            Some("/SIG")
        );
        state.viewer.as_mut().unwrap().selected_net = None;
        state.sync_schematic_selection();
        assert!(
            state.pcb_viewer.as_ref().unwrap().selected_net().is_none(),
            "Clear in schematic also clears PCB"
        );
        // A PCB-only net must not get cleared by the next frame's reverse sync.
        state
            .pcb_viewer
            .as_mut()
            .unwrap()
            .select_schematic_net(Some("BOARD_ONLY".into()));
        state.select_pcb_net("test", "Demo", Some("BOARD_ONLY"), 31.);
        state.sync_schematic_selection();
        assert_eq!(
            state.pcb_viewer.as_ref().unwrap().selected_net(),
            Some("BOARD_ONLY")
        );
        // If PCB is opened later, apply the current schematic selection then.
        state.close_pcb();
        state.viewer.as_mut().unwrap().select_net("SIG".into(), 32.);
        state.sync_schematic_selection();
        assert!(
            state.pcb_viewer.is_none(),
            "selection must not eagerly load a PCB"
        );
        state.pcb_viewer = Some(super::super::pcb::PcbViewer::new("Demo", "test", board));
        state.sync_schematic_selection();
        assert_eq!(
            state.pcb_viewer.as_ref().unwrap().selected_net(),
            Some("/SIG")
        );
        state.select_pcb_net("test", "Demo", None, 33.);
        assert!(state.viewer.as_ref().unwrap().selected_net.is_none());
    }
    #[test]
    fn only_selected_project_is_monitored_even_with_another_viewer_open() {
        let mut data = Inventory::default();
        data.ensure_project("A").unwrap();
        data.ensure_project("B").unwrap();
        for project in &mut data.projects {
            project.kicad = Some(KicadProject {
                path: format!("{}.kicad_pro", project.name),
                revision: "revision".into(),
                ..Default::default()
            });
        }
        let a = cache_key(data.projects[0].kicad.as_ref().unwrap());
        let b = cache_key(data.projects[1].kicad.as_ref().unwrap());
        let mut state = KicadState::default();
        state.retain_links(&data, None, None);
        assert!(state.monitored.is_empty());
        state.retain_links(&data, None, Some("A"));
        assert_eq!(state.monitored, BTreeSet::from([a.clone()]));
        let bundle = inventory_kicad::load_project("B.kicad_pro", |name| {
            Ok(if name.ends_with(".kicad_pro") {
                "{}".into()
            } else {
                "(kicad_sch (version 20250114) (uuid root))".into()
            })
        })
        .unwrap();
        state.viewer = Some(Viewer::new("B", &b, Arc::new(bundle), "/root"));
        for project in &data.projects {
            let link = project.kicad.as_ref().unwrap().clone();
            state.slots.insert(
                cache_key(&link),
                Slot {
                    link,
                    generation: 0,
                    dirty: true,
                    load_error: None,
                    #[cfg(not(target_arch = "wasm32"))]
                    watcher: None,
                    task: Some(Task::spawn(
                        |_| Err(anyhow::anyhow!("Test cancelled load")),
                        Box::new(|| {}),
                    )),
                    result: None,
                    pcb_task: None,
                    pcb_requested: None,
                    pcb_open_requested: false,
                },
            );
        }
        state.retain_links(&data, None, Some("A"));
        assert!(state.slots[&a].task.is_some());
        assert!(state.slots[&b].task.is_some()); // In-flight loads finish into the cache.
        assert!(state.slots[&b].dirty);
        assert_eq!(state.monitored, BTreeSet::from([a]));
        state.retain_links(&data, None, None);
        assert!(state.monitored.is_empty());
        assert!(state.slots.values().all(|slot| slot.task.is_some()));
        for slot in state.slots.values_mut() {
            slot.task = None;
            slot.dirty = false;
            slot.generation = 7;
        }
        state.retain_links(&data, None, Some("A"));
        state.retain_links(&data, None, None);
        state.retain_links(&data, None, Some("A"));
        assert!(
            state
                .slots
                .values()
                .all(|slot| slot.task.is_none() && !slot.dirty && slot.generation == 7)
        );
        state.viewer = None;
        state.retain_links(&data, None, None);
        assert!(state.monitored.is_empty());
    }
}

#[cfg(test)]
mod textbox_tests {
    use super::*;
    #[test]
    fn schematic_symbol_details_show_metadata_and_exact_part_links_without_loading_pcb() {
        let bundle=Arc::new(inventory_kicad::load_schematics("demo.kicad_pro",|path|match path {
            "demo.kicad_pro"=>Ok("{}".into()),
            "demo.kicad_sch"=>Ok(r#"(kicad_sch (version 20250114) (uuid root)
                (symbol (lib_id "Missing") (at 5 5 0) (uuid c29) (property "Reference" "C29")
                    (property "Value" "100nF") (property "Tolerance" "10%") (property "Footprint" "Capacitor:0402")))"#.into()),
            _=>Err(inventory_kicad::Error::new("Missing")),
        }).unwrap());
        assert!(bundle.pcb.is_none());
        for purchase in [false, true] {
            let mut data = Inventory::default();
            let part = Part {
                id: "exact-part".into(),
                number: "CAP-MPN".into(),
                ..Default::default()
            };
            if purchase {
                data.purchases.push(crate::model::Purchase {
                    part,
                    assignments: vec![crate::model::Assignment {
                        id: "assignment".into(),
                        project: "Demo".into(),
                        quantity: 1,
                        designators: "C29".into(),
                    }],
                });
            } else {
                data.parts.push(part);
                data.allocations.push(crate::model::Allocation {
                    id: "allocation".into(),
                    part_id: "exact-part".into(),
                    project: "Demo".into(),
                    quantity: 1,
                    designators: "C29".into(),
                });
            }
            let mut viewer = Viewer::new(
                "Demo",
                "details",
                bundle.clone(),
                &bundle.root.instance_path,
            );
            viewer.select_component(
                bundle.root.instance_path.clone(),
                "c29".into(),
                Point::new(5., 5.),
                false,
            );
            assert_eq!(viewer.single_symbol().as_deref(), Some("C29"));
            viewer.details = Some(
                super::super::footprint_details::FootprintDetails::build_for_symbol(
                    &data, &bundle, "Demo", "C29",
                ),
            );
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            let draw = |viewer: &mut Viewer, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1000., 700.),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            viewer.floating_controls(ui, ui.available_rect_before_wrap())
                        });
                    },
                )
            };
            draw(&mut viewer, vec![]);
            let output = draw(&mut viewer, vec![]);
            let label = if purchase {
                "Go to Purchase"
            } else {
                "Go to Part"
            };
            for value in ["CAP-MPN", "100nF", "10%", "0402", label] {
                assert!(
                    output
                        .shapes
                        .iter()
                        .any(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.text()==value)),
                    "missing {value}"
                );
            }
            let pos = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == label => {
                        Some(t.pos + t.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap();
            draw(
                &mut viewer,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            draw(
                &mut viewer,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            );
            let target = viewer.part_navigation.unwrap();
            assert_eq!(target.id, "exact-part");
            assert_eq!(target.purchase, purchase);
        }
    }
    #[test]
    fn symbol_selection_blinks_then_stays_purple_and_lamp_replays_without_moving_camera() {
        let bundle = Arc::new(
            inventory_kicad::load_project("demo.kicad_pro", |path| {
                Ok(if path.ends_with(".kicad_pro") {
                    "{}"
                } else {
                    "(kicad_sch (version 20250114) (uuid root))"
                }
                .into())
            })
            .unwrap(),
        );
        let mut viewer = Viewer::new("Demo", "demo", bundle, "/root");
        viewer.select_component("/root".into(), "u1".into(), Point::new(5., 5.), false);
        viewer.update_net_blink(10.);
        assert_eq!(viewer.highlight_ink, Some(Color32::YELLOW));
        viewer.update_net_blink(11.);
        assert_eq!(viewer.highlight_ink, Some(Color32::from_rgb(220, 0, 220)));
        let camera = (viewer.center, viewer.scale);
        viewer.replay_selection_blink(20.);
        viewer.update_net_blink(20.);
        assert_eq!(viewer.highlight_ink, Some(Color32::YELLOW));
        assert_eq!((viewer.center, viewer.scale), camera);
    }
    #[test]
    fn document_and_net_gpu_scenes_survive_camera_blink_and_sheet_switches() {
        let bundle=Arc::new(inventory_kicad::load_project("demo.kicad_pro",|path|Ok(if path.ends_with(".kicad_pro"){"{}"}else{"(kicad_sch (version 20250114) (uuid root) (wire (pts (xy 1 1) (xy 5 1))) (label \"GND\" (at 1 1 0)))"}.into())).unwrap());
        let scene = &bundle.schematics[&bundle.root.file];
        let mut viewer = Viewer::new("test", "test", bundle.clone(), &bundle.root.instance_path);
        let prepared = prepare_document(
            scene.clone(),
            BTreeMap::new(),
            viewer.instance.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        viewer
            .gpu_base
            .insert(viewer.instance.clone(), prepared.clone());
        viewer.gpu_base.insert("/child".into(), prepared);
        let root = viewer.instance.clone();
        let indices = [0usize].into_iter().collect();
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000., 700.));
                viewer.paint_retained(ui, rect, scene, &indices, Color32::WHITE);
                let base = viewer.gpu_base[&root].clone();
                let net = viewer.gpu_net.as_ref().unwrap().2.clone();
                let blink = viewer.gpu_net.as_ref().unwrap().3.clone();
                viewer.scale = 100.;
                viewer.center = Point::new(40., 40.);
                viewer.highlight_ink = Some(Color32::YELLOW);
                viewer.paint_retained(ui, rect, scene, &indices, Color32::WHITE);
                assert!(Arc::ptr_eq(&base, &viewer.gpu_base[&root]));
                assert!(Arc::ptr_eq(&net, &viewer.gpu_net.as_ref().unwrap().2));
                assert!(Arc::ptr_eq(&blink, &viewer.gpu_net.as_ref().unwrap().3));
                viewer.navigate("/child".into());
                viewer.paint_retained(ui, rect, scene, &indices, Color32::WHITE);
                viewer.navigate(root.clone());
                viewer.paint_retained(ui, rect, scene, &indices, Color32::WHITE);
                assert!(Arc::ptr_eq(&base, &viewer.gpu_base[&root]));
            });
        });
    }
    #[test]
    #[ignore = "Manual GPU schematic fixture preview"]
    fn retained_schematic_preview() {
        let path = std::env::var("KICAD_PROJECT_FIXTURE").unwrap();
        let bundle = Arc::new(
            inventory_kicad::load_project(&path, |p| {
                std::fs::read_to_string(p).map_err(|e| inventory_kicad::Error::new(e.to_string()))
            })
            .unwrap(),
        );
        let scene = &bundle.schematics[&bundle.root.file];
        let b = scene.bounds;
        let center = Point::new((b.min.x + b.max.x) / 2., (b.min.y + b.max.y) / 2.);
        let mut viewer = Viewer::new(
            "preview",
            "preview",
            bundle.clone(),
            &bundle.root.instance_path,
        );
        viewer.selected_net = bundle
            .nets
            .iter()
            .find(|n| n.name == "GND")
            .map(|n| n.name.clone());
        let indices = viewer
            .selected_net
            .as_ref()
            .and_then(|name| bundle.nets.iter().find(|n| &n.name == name))
            .into_iter()
            .flat_map(|n| &n.members)
            .filter(|m| m.instance == viewer.instance)
            .flat_map(|m| m.primitives.iter().copied())
            .collect();
        let prepared = prepare_document(
            scene.clone(),
            bundle.project.text_variables.clone(),
            viewer.instance.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        viewer.gpu_base.insert(viewer.instance.clone(), prepared);
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                viewer.paint_retained(
                    ui,
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400., 1000.)),
                    scene,
                    &indices,
                    Color32::from_rgb(255, 255, 235),
                )
            });
        });
        super::super::cad_gpu::render_preview(
            viewer.gpu_base[&viewer.instance].clone(),
            viewer.gpu_net.as_ref().map(|n| n.2.clone()),
            center,
            6.,
            [1400, 1000],
            Color32::from_rgb(255, 255, 235),
            &std::env::var("CAD_GPU_PREVIEW").unwrap(),
        );
    }
    #[test]
    fn wrapped_centered_and_right_justified_text_stays_inside_box() {
        let ctx = egui::Context::default();
        let _=ctx.run(egui::RawInput::default(),|ctx|{
            egui::CentralPanel::default().show(ctx,|ui|{
                for align in [-1,0,1] {
                    let mut job=egui::text::LayoutJob::default();job.wrap.max_width=250.;job.halign=align_axis(align);
                    job.append("All components have voltage margin. This paragraph must wrap and remain inside its text box.",0.,egui::TextFormat{font_id:egui::FontId::monospace(15.),..Default::default()});
                    let galley=ui.fonts_mut(|f|f.layout_job(job));
                    assert!(galley.rows.len()>1);
                    let inner=egui::Rect::from_min_size(egui::pos2(10.,10.),egui::vec2(250.,100.));
                    let rendered=galley.rect.translate(text_box_offset(inner,galley.rect,[align,0]));
                    assert!(rendered.min.x>=inner.min.x-0.01 && rendered.max.x<=inner.max.x+0.01);
                    assert!((rendered.center().y-inner.center().y).abs()<0.01);
                }
            });
        });
    }
}

/// Distance in schematic millimetres, so the hit tolerance stays constant in pixels.
fn net_primitive_distance(primitive: &Primitive, p: Point) -> f64 {
    match primitive {
        Primitive::Path { points, .. } => points
            .windows(2)
            .map(|pair| {
                let (a, b) = (pair[0], pair[1]);
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let length = dx * dx + dy * dy;
                let t = if length > 0. {
                    ((p.x - a.x) * dx + (p.y - a.y) * dy) / length
                } else {
                    0.
                }
                .clamp(0., 1.);
                (p.x - a.x - t * dx).hypot(p.y - a.y - t * dy)
            })
            .fold(f64::INFINITY, f64::min),
        Primitive::Dot { center, radius, .. } => {
            ((p.x - center.x).hypot(p.y - center.y) - radius).max(0.)
        }
        Primitive::Text {
            at,
            text,
            size,
            angle,
            align,
            ..
        } => {
            let a = angle.to_radians();
            let (dx, dy) = (p.x - at.x, p.y - at.y);
            let (x, y) = (dx * a.cos() - dy * a.sin(), dx * a.sin() + dy * a.cos());
            let (width, height) = (text.chars().count() as f64 * size * 0.78, size * 1.3);
            let offset = |align: i8, length: f64| match align {
                -1 => 0.,
                1 => -length,
                _ => -length / 2.,
            };
            let (left, top) = (offset(align[0], width), offset(align[1], height));
            (x - x.clamp(left, left + width)).hypot(y - y.clamp(top, top + height))
        }
        _ => f64::INFINITY,
    }
}

#[cfg(test)]
mod net_hit_tests {
    use super::*;
    #[test]
    fn hits_wire_and_pin_segments_without_selecting_distant_or_unrelated_graphics() {
        let path = Primitive::Path {
            points: vec![Point::new(10., 10.), Point::new(20., 10.)],
            width: 0.15,
            ink: Ink::Pin,
            fill: false,
            solid: false,
            fill_color: None,
            stroke_color: None,
        };
        assert_eq!(net_primitive_distance(&path, Point::new(15., 10.)), 0.);
        assert_eq!(net_primitive_distance(&path, Point::new(15., 12.)), 2.);
        assert_eq!(net_primitive_distance(&path, Point::new(23., 10.)), 3.);
        let label = Primitive::Text {
            at: Point::new(10., 10.),
            text: "VCC".into(),
            size: 1.,
            angle: 90.,
            align: [-1, 0],
            ink: Ink::Label,
            bold: false,
            instances: BTreeMap::new(),
        };
        assert_eq!(net_primitive_distance(&label, Point::new(10., 9.)), 0.);
        assert!(net_primitive_distance(&label, Point::new(30., 30.)) > 10.);
    }
}

fn schematic_context_color(color:Color32,grayscale:bool)->Color32{
    if !grayscale{return color;}
    let gray=(color.r() as f32*0.2126+color.g() as f32*0.7152+color.b() as f32*0.0722).round() as u8;
    Color32::from_rgba_premultiplied(gray,gray,gray,color.a())
}
fn net_blink_ink(elapsed: Option<f64>) -> Option<Color32> {
    match elapsed {
        Some(t) if (0.0..1.0).contains(&t) && ((t * 6.).floor() as u32).is_multiple_of(2) => Some(Color32::YELLOW),
        _ => Some(Color32::from_rgb(220, 0, 220)),
    }
}
fn net_bounds(
    net: &inventory_kicad::nets::ProjectNet,
    instance: &str,
    scene: &inventory_kicad::Schematic,
) -> Option<(Point, Point)> {
    let mut points = Vec::new();
    for member in net.members.iter().filter(|m| m.instance == instance) {
        for &index in &member.primitives {
            match scene.primitives.get(index) {
                Some(Primitive::Path {
                    points: vertices, ..
                }) => points.extend(vertices),
                Some(Primitive::Dot { center, .. }) => points.push(*center),
                Some(Primitive::Text { at, .. }) => points.push(*at),
                _ => {}
            }
        }
    }
    let first = *points.first()?;
    Some(points.into_iter().fold((first, first), |(min, max), p| {
        (
            Point::new(min.x.min(p.x), min.y.min(p.y)),
            Point::new(max.x.max(p.x), max.y.max(p.y)),
        )
    }))
}
fn schematic_matches(bundle: &Bundle, current: &str, query: &str) -> Vec<SchematicMatch> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return vec![];
    }
    let terms: Vec<_> = query.split_whitespace().collect();
    let property = |c: &inventory_kicad::Component, names: &[&str]| -> String {
        names
            .iter()
            .find_map(|name| {
                c.properties
                    .iter()
                    .find(|(k, v)| k.eq_ignore_ascii_case(name) && !v.trim().is_empty())
                    .map(|(_, v)| v.clone())
            })
            .unwrap_or_default()
    };
    let mut results = Vec::new();
    for node in bundle.nodes() {
        let Some(scene) = bundle.schematics.get(&node.file) else {
            continue;
        };
        for component in &scene.components {
            let reference = component
                .references
                .get(&node.instance_path)
                .unwrap_or(&component.reference)
                .clone();
            if reference.is_empty() || reference.starts_with('#') {
                continue;
            }
            let mpn = property(
                component,
                &[
                    "MPN",
                    "Manufacturer Part Number",
                    "Part Number",
                    "PartNumber",
                ],
            );
            let spn = property(
                component,
                &[
                    "Supplier Part Number",
                    "SPN",
                    "SNP",
                    "LCSC Part #",
                    "LCSC Part",
                    "LCSC",
                    "LCSC Part Number",
                ],
            );
            let value = property(component, &["Value"]);
            let package = property(component, &["Footprint", "Package"]);
            let fields = [
                reference.to_lowercase(),
                mpn.to_lowercase(),
                spn.to_lowercase(),
                value.to_lowercase(),
                package.to_lowercase(),
            ];
            if !terms
                .iter()
                .all(|term| fields.iter().any(|field| field.contains(term)))
            {
                continue;
            }
            let rank = fields
                .iter()
                .position(|field| field.contains(&query))
                .unwrap_or(5);
            let exact = fields.get(rank).is_some_and(|f| f == &query);
            results.push((
                rank,
                !exact,
                SchematicMatch {
                    component: Some(component.uuid.clone()),
                    title: reference,
                    value: if value.is_empty() { mpn } else { value },
                    package,
                    instance: node.instance_path.clone(),
                    point: Some(component.at),
                    net: None,
                },
            ));
        }
    }
    for net in &bundle.nets {
        if terms
            .iter()
            .all(|term| net.name.to_lowercase().contains(term))
            && let Some(member) = net
                .members
                .iter()
                .find(|m| m.instance == current)
                .or_else(|| net.members.first())
        {
            results.push((
                6,
                false,
                SchematicMatch {
                    component: None,
                    title: net.name.clone(),
                    value: "Net".into(),
                    package: String::new(),
                    instance: member.instance.clone(),
                    point: None,
                    net: Some(net.name.clone()),
                },
            ));
        }
    }
    results.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.title.cmp(&b.2.title))
            .then(a.2.instance.cmp(&b.2.instance))
    });
    results
        .into_iter()
        .take(20)
        .map(|(_, _, item)| item)
        .collect()
}

#[cfg(test)]
mod schematic_navigation_tests {
    use super::*;
    #[test]
    fn context_colors_preserve_alpha_and_restore_original_ink(){
        let ink=Color32::from_rgba_unmultiplied(170,40,20,180);
        assert_eq!(schematic_context_color(ink,false),ink);
        let gray=schematic_context_color(ink,true);
        assert_eq!(gray.r(),gray.g());assert_eq!(gray.g(),gray.b());assert_eq!(gray.a(),ink.a());
        assert_eq!(net_blink_ink(None),Some(Color32::from_rgb(220,0,220)));
    }
    #[test]
    fn blink_has_three_yellow_pulses_then_stays_purple() {
        for t in [0., 0.34, 0.67] {
            assert_eq!(net_blink_ink(Some(t)), Some(Color32::YELLOW));
        }
        for t in [0.2, 0.51, 0.9] {
            assert_eq!(net_blink_ink(Some(t)), Some(Color32::from_rgb(220,0,220)));
        }
        assert_eq!(
            net_blink_ink(Some(1.)),
            Some(Color32::from_rgb(220,0,220))
        );
    }
    #[test]
    fn autocomplete_prioritizes_designators_and_searches_properties_and_nets_with_twenty_result_cap()
     {
        let mut source = String::from("(kicad_sch (version 20250114) (uuid root)");
        for i in 0..30 {
            source.push_str(&format!(r#"(symbol (lib_id "Test:R") (at {} 20 0) (uuid r{}) (property "Reference" "R{}") (property "MPN" "CHIP-{}") (property "SPN" "C123") (property "Value" "1k") (property "Footprint" "0603"))"#,i*5,i,i,i));
        }
        source.push_str(r#"(label "R1_POWER" (at 10 10 0)))"#);
        let bundle = inventory_kicad::load_project("demo.kicad_pro", |path| {
            Ok(if path.ends_with(".kicad_pro") {
                "{}".into()
            } else {
                source.clone()
            })
        })
        .unwrap();
        let result = schematic_matches(&bundle, "/root", "R1");
        assert_eq!(result[0].title, "R1");
        assert!(result.last().unwrap().net.is_some());
        assert_eq!(schematic_matches(&bundle, "/root", "C123").len(), 20);
        assert_eq!(schematic_matches(&bundle, "/root", "CHIP-2")[0].title, "R2");
        assert_eq!(schematic_matches(&bundle, "/root", "1k 0603").len(), 20);
        assert_eq!(
            schematic_matches(&bundle, "/root", "R1_POWER")[0]
                .net
                .as_deref(),
            Some("R1_POWER")
        );
        assert_eq!(
            schematic_matches(&bundle, "/root", "R2")[0].point,
            Some(Point::new(10., 20.))
        );
    }
}

/// Limit automatic framing to a comfortable label size (~16 pt for 1.27 mm text).
/// Manual wheel/pinch zoom remains unrestricted by this automatic-framing cap.
fn net_camera_target(min: Point, max: Point, viewport: egui::Vec2) -> (Point, f64) {
    let center = Point::new((min.x + max.x) / 2., (min.y + max.y) / 2.);
    let scale = ((viewport.x as f64 - 80.) / (max.x - min.x).max(8.))
        .min((viewport.y as f64 - 80.) / (max.y - min.y).max(8.))
        .clamp(0.1, 10.);
    (center, scale)
}
#[cfg(test)]
mod net_camera_tests {
    use super::*;
    #[test]
    fn tiny_nets_are_centered_without_excessive_zoom_and_large_nets_fit() {
        let (center, scale) = net_camera_target(
            Point::new(10., 20.),
            Point::new(12., 21.),
            egui::vec2(1200., 800.),
        );
        assert_eq!(center, Point::new(11., 20.5));
        assert_eq!(scale, 10.);
        let (_, scale) = net_camera_target(
            Point::default(),
            Point::new(500., 300.),
            egui::vec2(1200., 800.),
        );
        assert!(scale * 500. <= 1120. && scale * 300. <= 720.);
    }
    #[test]
    fn selecting_same_net_only_restarts_blink_and_preserves_manual_camera() {
        let bundle = inventory_kicad::load_project("demo.kicad_pro", |path| {
            Ok(if path.ends_with(".kicad_pro") {
                "{}"
            } else {
                "(kicad_sch (version 20250114) (uuid root))"
            }
            .into())
        })
        .unwrap();
        let mut viewer = Viewer::new("Demo", "key", Arc::new(bundle), "/root");
        viewer.select_net("VCC".into(), 0.);
        assert!(matches!(viewer.focus_target, Some(FocusTarget::Net)));
        viewer.update_net_blink(0.1);
        assert_eq!(viewer.highlight_ink, None);
        assert_eq!(viewer.blink_start, None);
        viewer.focus_target = None;
        viewer.transition = Some(CameraTransition {
            start: 0.,
            from: Point::default(),
            to: Point::default(),
            from_scale: 1.,
            to_scale: 2.,
        });
        viewer.update_net_blink(0.5);
        assert_eq!(viewer.blink_start, None);
        viewer.transition = None;
        viewer.update_net_blink(1.);
        assert_eq!(viewer.blink_start, Some(1.));
        assert_eq!(viewer.highlight_ink, Some(Color32::YELLOW));
        viewer.center = Point::new(25., 30.);
        viewer.scale = 3.;
        viewer.select_net("VCC".into(), 2.);
        assert!(viewer.focus_target.is_none());
        assert!(viewer.transition.is_none());
        assert_eq!(viewer.center, Point::new(25., 30.));
        assert_eq!(viewer.scale, 3.);
        assert_eq!(viewer.blink_start, Some(2.));
        viewer.select_net("GND".into(), 3.);
        assert!(matches!(viewer.focus_target, Some(FocusTarget::Net)));
    }
}
