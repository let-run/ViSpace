#[cfg(target_arch = "wasm32")]
mod browser_git;
pub(crate) mod board3d;
#[cfg(target_arch = "wasm32")]
mod browser;
mod bulk;
mod cad_bitmap;
mod cad_gpu;
pub mod cad_jobs;
mod cad_text;
mod catalog;
mod dialogs;
mod editor;
mod footprint_details;
mod history_graph;
mod history_ui;
mod git_history;
mod inspector;
mod kicad;
mod net_class_style;
mod network_status;
mod notes_editor;
mod parts_import;
mod pcb;
mod pcb_layers;
mod popovers;
mod preview;
mod printer;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod project_git;
mod projects;
mod requests;
mod scanner;
mod shell;
mod surfaces;
#[cfg(not(target_arch = "wasm32"))]
mod sync_ui;
#[cfg(target_os = "macos")]
mod sync_hdr;
#[cfg(target_os = "linux")]
#[path = "ui/sync_hdr_wayland.rs"]
mod sync_hdr;
mod table;
mod theme;
use crate::{
    csv_io::{self, ReviewRow},
    model::*,
    store::SharedStore,
};
use anyhow::{Result, ensure};
use dialogs::StockEditor;
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use notes_editor::NotesEditor;
use std::collections::BTreeSet;
use theme::{Icon, YELLOW as GOLD};

fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).color(Color32::BLACK)).fill(GOLD)
}
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum View {
    Parts,
    Storage,
    Projects,
    Purchase,
}
impl View {
    fn index(self) -> usize {
        match self {
            Self::Parts => 0,
            Self::Storage => 1,
            Self::Projects => 2,
            Self::Purchase => 3,
        }
    }
}
struct TabState {
    query: String,
    selected: Option<String>,
    catalog: Option<String>,
    page: usize,
    page_size: usize,
    detail_tab: usize,
    project_tab: usize,
    bulk: Option<bulk::BulkAdd>,
    preview: Option<preview::PartPreview>,
}
impl Default for TabState {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected: None,
            catalog: None,
            page: 0,
            page_size: 25,
            detail_tab: 0,
            project_tab: 0,
            bulk: None,
            preview: None,
        }
    }
}
#[derive(Default)]
struct TabMemory {
    states: [TabState; 4],
}
impl TabMemory {
    fn exchange(&mut self, previous: View, next: View, state: TabState) -> TabState {
        self.states[previous.index()] = state;
        std::mem::take(&mut self.states[next.index()])
    }
}
fn queued_for_approval(error: &anyhow::Error) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        error.is::<crate::component_sync::Queued>()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = error;
        false
    }
}

struct Editor {
    part: Part,
    original_part: Part,
    purchase: bool,
    assignments: Vec<Assignment>,
    original_assignments: Vec<Assignment>,
    labels: String,
    custom: Vec<(String, String)>,
    draft: crate::attachments::AttachmentDraft,
    error: Option<String>,
    new: bool,
}
struct CatalogEditor {
    #[cfg(not(target_arch="wasm32"))]
    relocation:Option<crate::tasks::Task<KicadProject>>,
    #[cfg(not(target_arch = "wasm32"))]
    git_origin: Option<(Option<std::path::PathBuf>, GitRemote, GitRemote)>,
    #[cfg(target_arch = "wasm32")]
    token: String,
    #[cfg(target_arch = "wasm32")]
    kicad_choices: Vec<String>,
    git_destination:Option<(String,String)>,
    kicad: Option<KicadProject>,
    error: Option<String>,
    project: bool,
    old: Option<String>,
    name: String,
    description: String,
    original_description: String,
}
struct AllocationEditor {
    error: Option<String>,
    existing: Option<String>,
    part_id: String,
    purchase: bool,
    project: String,
    quantity: i64,
    designators: String,
}
struct ImportReview {
    document: crate::csv_import::CsvImport,
    error: Option<String>,
    applying: Option<crate::tasks::Task<()>>,
}
struct BomReview {
    project: String,
    rows: Vec<ReviewRow>,
}
enum Delete {
    Part(String, bool),
    Project(String),
    Storage(String),
    Allocation(String),
    PurchaseAssignment {
        part_id: String,
        assignment_id: String,
    },
    Request(String),
}
pub struct InventoryApp {
    cad_render_state: Option<egui_wgpu::RenderState>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    sync_hdr: sync_hdr::Overlay,
    store: SharedStore,
    data_snapshot: std::sync::Arc<Inventory>,
    store_revision: u64,
    local_head_hash: String,
    request_quantities: std::collections::BTreeMap<String, i64>,
    request_items: std::collections::BTreeMap<String, BomItem>,
    request_apply: Option<requests::ApplyTask>,
    view: View,
    tabs: TabMemory,
    query: String,
    scanner: Option<scanner::Scanner>,
    kicad: kicad::KicadState,
    printer: printer::PrinterUi,
    camera_preference: String,
    camera_settings_path: std::path::PathBuf,
    selected: Option<String>,
    catalog: Option<String>,
    page: usize,
    page_size: usize,
    detail_tab: usize,
    detail_width: Option<f32>,
    layout_dirty: bool,
    summary_height: Option<f32>,
    project_tab: usize,
    requests: requests::RequestState,
    labels: BTreeSet<String>,
    purchase_labels: BTreeSet<String>,
    editor: Option<Editor>,
    catalog_editor: Option<CatalogEditor>,
    allocation: Option<AllocationEditor>,
    import: Option<ImportReview>,
    csv_loading: Option<crate::tasks::Task<crate::csv_import::CsvImport>>,
    bom: Option<BomReview>,
    delete: Option<Delete>,
    status: String,
    error: Option<String>,
    popover_error: Option<String>,
    settings: bool,
    preferences: dialogs::Preferences,
    label_query: String,
    stock_editor: Option<StockEditor>,
    notes_editor: Option<NotesEditor>,
    #[cfg(not(target_arch = "wasm32"))]
    project_git: project_git::ProjectGit,
    #[cfg(target_arch = "wasm32")]
    browser_git: browser_git::State,
    part_preview: Option<preview::PartPreview>,
    project_part_reveal: Option<String>,
    clipboard: Option<crate::platform::clipboard::Clipboard>,
    bulk: Option<bulk::BulkAdd>,
    receive: BTreeSet<String>,
    jobs: std::sync::mpsc::Receiver<(String, Result<String>)>,
    job_sender: std::sync::mpsc::Sender<(String, Result<String>)>,
    busy: bool,
    network_node_count: usize,
    network_zoom: f32,
    network_pan: egui::Vec2,
    #[cfg(not(target_arch = "wasm32"))]
    sync: sync_ui::SyncUi,
    action_log_cache: history_ui::HistoryUi,
    git_history:git_history::State,
}
impl InventoryApp {
    fn switch_view(&mut self, next: View) {
        if next == self.view {
            return;
        }
        let current = TabState {
            query: std::mem::take(&mut self.query),
            selected: self.selected.take(),
            catalog: self.catalog.take(),
            page: self.page,
            page_size: self.page_size,
            detail_tab: self.detail_tab,
            project_tab: self.project_tab,
            bulk: self.bulk.take(),
            preview: self.part_preview.take(),
        };
        let restored = self.tabs.exchange(self.view, next, current);
        self.view = next;
        self.query = restored.query;
        self.selected = restored.selected;
        self.catalog = restored.catalog;
        self.page = restored.page;
        self.page_size = restored.page_size;
        self.detail_tab = restored.detail_tab;
        self.project_tab = restored.project_tab;
        self.bulk = restored.bulk;
        self.part_preview = restored.preview;
        if let Some(preview) = &mut self.part_preview {
            preview.resume();
        }
    }
    pub fn new(cc: &eframe::CreationContext<'_>, store: SharedStore) -> Self {
        if let Some(state) = &cc.wgpu_render_state {
            pcb_layers::register(state);
            cad_gpu::register(state, &cc.egui_ctx);
            inventory_kicad_renderer::register(state, &cc.egui_ctx);
        }
        theme::apply(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let (job_sender, jobs) = std::sync::mpsc::channel();
        let settings_path = store.lock().unwrap().path.with_file_name("layout.json");
        let layout: [Option<f32>; 2] =
            crate::platform::fs::read(settings_path.with_file_name("layout.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
        let (store_revision, data_snapshot) = {
            let locked = store.lock().unwrap();
            (locked.revision(), std::sync::Arc::new(locked.data.clone()))
        };
        let camera_settings_path = settings_path.with_file_name("camera.json");
        let camera_preference = crate::platform::fs::read(&camera_settings_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let request_items = data_snapshot.requested_items();
        let app = Self {
            cad_render_state: cc.wgpu_render_state.clone(),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            sync_hdr: sync_hdr::Overlay::default(),
            printer: printer::PrinterUi::new(settings_path.with_file_name("printer.json")),
            scanner: None,
            kicad: kicad::KicadState::new(settings_path.with_file_name("net-class-colors.json")),
            camera_preference,
            camera_settings_path,
            store,
            request_quantities: request_items
                .iter()
                .map(|(id, item)| (id.clone(), item.quantity))
                .collect(),
            request_items,
            data_snapshot,
            store_revision,
            local_head_hash: String::new(),
            request_apply: None,
            view: View::Parts,
            tabs: TabMemory::default(),
            query: String::new(),
            selected: None,
            catalog: None,
            page: 0,
            page_size: 25,
            detail_tab: 0,
            layout_dirty: false,
            detail_width: layout[0]
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(330., 680.)),
            summary_height: layout[1].filter(|v| v.is_finite()).map(|v| v.max(230.)),
            project_tab: 0,
            requests: Default::default(),
            labels: BTreeSet::new(),
            purchase_labels: BTreeSet::new(),
            editor: None,
            catalog_editor: None,
            allocation: None,
            import: None,
            csv_loading: None,
            bom: None,
            delete: None,
            status: "Ready".into(),
            error: None,

            popover_error: None,
            settings: false,
            preferences: dialogs::Preferences::default(),
            label_query: String::new(),
            stock_editor: None,
            notes_editor: None,
            #[cfg(not(target_arch = "wasm32"))]
            project_git: Default::default(),
            #[cfg(target_arch = "wasm32")]
            browser_git: Default::default(),
            part_preview: None,
            project_part_reveal: None,
            clipboard: None,
            bulk: None,
            receive: BTreeSet::new(),
            jobs,
            job_sender,
            busy: false,
            network_node_count: 1,
            network_zoom: 1.,
            network_pan: egui::Vec2::ZERO,
            #[cfg(not(target_arch = "wasm32"))]
            sync: sync_ui::SyncUi::default(),
            action_log_cache: history_ui::HistoryUi::default(),
            git_history:Default::default(),
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut app = app;
            app.sync.restore_host(&app.store);
            app
        }
        #[cfg(target_arch = "wasm32")]
        {
            app
        }
    }
    fn save_detail_layout(&self) {
        let path = self
            .store
            .lock()
            .unwrap()
            .path
            .with_file_name("layout.json");
        if let Ok(bytes) = serde_json::to_vec(&[self.detail_width, self.summary_height]) {
            let temporary = path.with_extension("json.tmp");
            if crate::platform::fs::write(&temporary, bytes).is_ok() {
                let _ = crate::platform::fs::rename(temporary, path);
            }
        }
    }
    fn change(&mut self, message: &str, op: impl FnOnce(&mut Inventory) -> Result<()>) {
        let result = self.store.lock().unwrap().change(op);
        match result {
            Ok(()) => self.status = message.into(),
            Err(e) if queued_for_approval(&e) => {
                self.status = "Edit queued for leader approval".into()
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    fn report<T>(&mut self, result: Result<T>) -> Option<T> {
        match result {
            Ok(v) => Some(v),
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }
    fn edit(&mut self, p: Part, purchase: bool, new: bool, assignments: Vec<Assignment>) {
        let Some(draft) = self.report(crate::attachments::AttachmentDraft::new()) else {
            return;
        };
        self.editor = Some(Editor {
            labels: String::new(),
            custom: p
                .custom_fields
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            draft,
            error: None,
            original_part: p.clone(),
            part: p,
            purchase,
            original_assignments: assignments.clone(),
            assignments,
            new,
        });
    }
    fn add(&mut self) {
        let mut part = Part::default();
        if self.view == View::Storage {
            part.storage = self.catalog.clone().unwrap_or_default();
        }
        if self.view == View::Purchase {
            part.stock = 1;
        }
        self.edit(part, self.view == View::Purchase, true, vec![]);
    }
    fn edit_selected(&mut self, data: &Inventory) {
        if let Some(id) = &self.selected {
            if self.view == View::Purchase {
                if let Some(p) = data.purchases.iter().find(|p| &p.part.id == id) {
                    self.edit(p.part.clone(), true, false, p.assignments.clone());
                }
            } else if let Some(p) = data.parts.iter().find(|p| &p.id == id) {
                self.edit(p.clone(), false, false, vec![]);
            }
        }
    }
    fn export_csv(&mut self, parts: &[Part]) {
        if let Some(path) = crate::platform::dialogs::FileDialog::new()
            .set_file_name("inventory.csv")
            .save_file()
        {
            let result =
                (|| csv_io::write_parts(crate::platform::fs::File::create(path)?, parts))();
            if self.report(result).is_some() {
                self.status = "CSV exported".into();
            }
        }
    }
    fn bom_window(&mut self, ctx: &egui::Context, data: &Inventory) {
        let Some(mut review) = self.bom.take() else {
            return;
        };
        let mut apply = false;
        let mut cancel = false;
        let response=surfaces::sheet("bom_review").show(ctx,|ui|{
            surfaces::configure(ui,(ctx.content_rect().width()-104.).clamp(600.,900.));
            surfaces::heading(ui,&format!("Import BOM — {}",review.project),Icon::Import);
            ui.label("Confirm one candidate per row. Unselected and short-stock quantities remain Requested. Re-import replaces overlapping designators.");
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height()-250.).clamp(150.,480.)).show(ui,|ui|{for (i,row)in review.rows.iter_mut().enumerate(){
                ui.separator();ui.strong(format!("{} · {} × {} · {}",row.item.designators,row.item.quantity,row.item.value,row.item.package));
                egui::ComboBox::from_id_salt(("bom_candidate",i)).selected_text(row.selected.as_ref().and_then(|id|data.parts.iter().find(|p|&p.id==id)).map(|p|p.number.as_str()).unwrap_or("Unmatched / request only")).width(500.).show_ui(ui,|ui|{
                    ui.selectable_value(&mut row.selected,None,"Request only");
                    for id in &row.candidates{if let Some(p)=data.parts.iter().find(|p|&p.id==id){ui.selectable_value(&mut row.selected,Some(id.clone()),format!("{} · {} · available {}",p.number,p.value,data.available(p)));}}
                });
            }});
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{apply=ui.add(primary("Apply BOM")).clicked();cancel=ui.add(primary("Cancel")).clicked();});
        });
        if apply {
            let result = self
                .store
                .lock()
                .unwrap()
                .change(|d| csv_io::apply_bom(d, &review.project, &review.rows));
            if self.report(result).is_some() {
                self.status = "BOM imported and stock reserved".into();
                return;
            }
        }
        if !cancel && !surfaces::escape(&response) {
            self.bom = Some(review);
        }
    }
}
impl eframe::App for InventoryApp {
    #[cfg(target_os = "linux")]
    fn on_exit(&mut self) {
        self.sync_hdr = sync_hdr::Overlay::default();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(state) = &self.cad_render_state {
            cad_gpu::maintain(state);
        }
        #[cfg(target_arch = "wasm32")]
        self.browser_events(ctx);
        while let Ok((draft, result)) = self.jobs.try_recv() {
            self.busy = false;
            if let Some(editor) = self
                .editor
                .as_mut()
                .filter(|e| e.draft.root().to_string_lossy() == draft)
            {
                match result {
                    Ok(path) => {
                        editor.part.datasheet = path;
                        self.status = "PDF downloaded — save the part to keep the reference".into();
                    }
                    Err(error) => editor.error = Some(error.to_string()),
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.sync.poll_discovery(ctx,&self.store);
        #[cfg(not(target_arch = "wasm32"))]
        if self.sync.active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // Poll active background jobs/API changes; let wgpu sleep while the app is idle.
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // Keep rendering the last committed snapshot while a worker saves to SQLite.
        // Never wait for a background writer or clone the inventory on every redraw.
        if let Ok(store) = self.store.try_lock()
            && store.revision() != self.store_revision
        {
            self.store_revision = store.revision();
            self.data_snapshot = std::sync::Arc::new(store.data.clone());
            self.request_items = self.data_snapshot.requested_items();
            self.request_quantities = self
                .request_items
                .iter()
                .map(|(id, item)| (id.clone(), item.quantity))
                .collect();
        }
        self.kicad.refresh_managed_snapshots(&self.data_snapshot, ctx);
        self.kicad.retain_links(
            &self.data_snapshot,
            self.catalog_editor.as_ref(),
            if self.view == View::Projects {
                self.catalog.as_deref()
            } else {
                None
            },
        );
        self.kicad.poll(ctx);
        for error in self
            .kicad
            .sync_requests(&self.data_snapshot, &self.store, ctx)
        {
            self.error = Some(error);
        }
        self.poll_request_jobs(ctx);
        self.poll_csv_import(ctx);
        let data = self.data_snapshot.clone();
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
            self.settings = true;
        }
        let modal_open = self.scanner.is_some()
            || self.editor.is_some()
            || self.requests.editor_open()
            || self.request_apply.is_some()
            || self.catalog_editor.is_some()
            || self.allocation.is_some()
            || self.stock_editor.is_some()
            || self.notes_editor.is_some()
            || self.import.is_some()
            || self.bom.is_some()
            || self.delete.is_some()
            || self.settings
            || self.error.is_some();
        if !modal_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::N)) {
            self.add();
        }
        if !modal_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::E)) {
            self.edit_selected(&data);
        }
        if !modal_open
            && self.view == View::Projects
            && matches!(self.project_tab, 3 | 4)
            && let Some(project) = self.catalog.as_deref()
        {
            self.project_tab = self.kicad.cad_shortcuts(ctx, project, self.project_tab);
        }
        self.header(ctx, &data);
        self.status_bar(ctx, &data);
        self.sidebar(ctx, &data);
        if matches!(self.view, View::Parts | View::Purchase) {
            self.detail(ctx, &data);
        }
        self.center(ctx, &data);
        self.part_preview_popup(ctx, &data);
        self.editor_window(ctx);
        self.request_window(ctx);
        self.catalog_window(ctx, &data);
        #[cfg(not(target_arch = "wasm32"))]
        self.project_git.window(ctx, &self.store);
        #[cfg(target_arch = "wasm32")]
        self.browser_git_window(ctx);
        self.git_history_window(ctx);
        self.allocation_window(ctx, &data);
        self.import_window(ctx);
        self.bom_window(ctx, &data);
        self.delete_window(ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.sync_window(ctx);
        self.settings_window(ctx);
        self.stock_window(ctx);
        self.notes_window(ctx);
        self.error_window(ctx);
        self.scanner_window(ctx, &data);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let icon = ctx.data_mut(|data| {
                let id = egui::Id::new(sync_hdr::ICON_ID);
                let icon = data.get_temp::<sync_hdr::Icon>(id);
                data.remove::<sync_hdr::Icon>(id);
                icon
            });
            let icon = icon.filter(|(rect, _)| {
                !modal_open && !self.sync.open
                    && ctx.layer_id_at(rect.center())
                        .is_none_or(|layer| layer.order == egui::Order::Background)
            });
            self.sync_hdr.update(_frame, ctx, icon);
        }
    }
}

#[cfg(test)]
mod tab_memory_tests {
    use super::*;
    #[test]
    fn switching_tabs_preserves_independent_selection_search_page_and_project_schematic_tab() {
        let mut memory = TabMemory::default();
        let parts = TabState {
            selected: Some("part-a".into()),
            query: "resistor".into(),
            page: 4,
            page_size: 50,
            detail_tab: 1,
            ..Default::default()
        };
        let mut projects = memory.exchange(View::Parts, View::Projects, parts);
        assert!(projects.selected.is_none());
        projects.catalog = Some("Motor".into());
        projects.project_tab = 3;
        projects.query = "CAN".into();
        let mut purchase = memory.exchange(View::Projects, View::Purchase, projects);
        purchase.selected = Some("purchase-b".into());
        purchase.page = 2;
        let projects = memory.exchange(View::Purchase, View::Projects, purchase);
        assert_eq!(projects.catalog.as_deref(), Some("Motor"));
        assert_eq!(projects.project_tab, 3);
        assert_eq!(projects.query, "CAN");
        let parts = memory.exchange(View::Projects, View::Parts, projects);
        assert_eq!(parts.selected.as_deref(), Some("part-a"));
        assert_eq!(parts.query, "resistor");
        assert_eq!((parts.page, parts.page_size, parts.detail_tab), (4, 50, 1));
        let storage = memory.exchange(View::Parts, View::Storage, parts);
        assert!(storage.catalog.is_none());
        assert!(storage.query.is_empty());
        let purchase = memory.exchange(
            View::Storage,
            View::Purchase,
            TabState {
                catalog: Some("Drawer".into()),
                ..storage
            },
        );
        assert_eq!(purchase.selected.as_deref(), Some("purchase-b"));
        assert_eq!(purchase.page, 2);
        let storage = memory.exchange(View::Purchase, View::Storage, purchase);
        assert_eq!(storage.catalog.as_deref(), Some("Drawer"));
    }
}
