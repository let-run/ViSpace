use super::*;
use surfaces as form;

#[derive(Default)]
pub(super) struct Preferences {
    tab: usize,
    #[cfg(not(target_arch = "wasm32"))]
    git: Option<project_git::GitPreferences>,
    #[cfg(not(target_arch = "wasm32"))]
    git_status: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    folder: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    folder_status: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    models_folder: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    models_status: Option<String>,
}

pub(super) struct StockEditor {
    pub part_id: String,
    pub purchase: bool,
    pub quantity: i64,
    pub original_quantity: i64,
    pub error: Option<String>,
}
impl InventoryApp {
    pub(super) fn catalog_window(&mut self, ctx: &egui::Context, _data: &Inventory) {
        let Some(mut e) = self.catalog_editor.take() else {
            return;
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(result)=e.relocation.as_ref().and_then(crate::tasks::Task::poll){
            e.relocation=None;match result{Ok(link)=>{e.kicad=Some(link);e.error=None;},Err(error)=>e.error=Some(error)}
        }
        #[cfg(not(target_arch="wasm32"))]
        let relocating=e.relocation.is_some();
        #[cfg(target_arch="wasm32")]
        let relocating=false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut fetch_opened = false;
        let response = form::sheet("catalog_editor").show(ctx, |ui| {
            form::configure(ui, 412.);
            let kind = if e.project {
                "project"
            } else {
                "storage location"
            };
            form::heading(
                ui,
                &format!("{} {kind}", if e.old.is_some() { "Edit" } else { "Add" }),
                if e.project { Icon::Folder } else { Icon::Box },
            );
            form::row(
                ui,
                if e.project {
                    "Project name"
                } else {
                    "Location name"
                },
                |ui| {
                    form::field(ui, &mut e.name);
                },
            );
            form::row(ui, "Description", |ui| {
                form::field(ui, &mut e.description);
            });
            if e.project {
                #[cfg(not(target_arch = "wasm32"))]
                self.kicad.link_controls(ui, &mut e);
                #[cfg(target_arch = "wasm32")]
                {
                    self.kicad.link_controls(ui, &mut e);
                    if let Some(project)=e.old.as_ref().and_then(|name|_data.projects.iter().find(|p| key(&p.name)==key(name)))
                        && project.kicad.as_ref().is_some_and(|link|link.browser_folder.is_some())
                        && ui.button("Local version control…").clicked()
                    { self.open_browser_git(project); }
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let folder = e.kicad.as_ref().and_then(project_git::folder);
                    let project = e.old.as_ref().and_then(|name| {
                        _data
                            .projects
                            .iter()
                            .find(|project| key(&project.name) == key(name))
                    });
                    if e.git_origin
                        .as_ref()
                        .is_none_or(|(path, _, _)| path != &folder)
                    {
                        let remote = project
                            .as_ref()
                            .and_then(|p| p.git_remote.clone())
                            .unwrap_or_else(|| {
                                match folder
                                    .as_deref()
                                    .map(project_git::configured_remote)
                                    .transpose()
                                {
                                    Ok(remote) => remote.unwrap_or_default(),
                                    Err(error) => {
                                        e.error = Some(error.to_string());
                                        GitRemote::default()
                                    }
                                }
                            });
                        e.git_origin = Some((folder, remote.clone(), remote));
                    }
                    if let Some((folder, remote, original)) = &mut e.git_origin {
                        project_git::remote_fields(ui, remote);
                        if let Some(project) = project
                            && let Some(saved) = project.git_remote.clone()
                            && !saved.url.is_empty()
                            && ui
                                .add_enabled(
                                    remote == original
                                        && project.name == e.name
                                        && project.description == e.description
                                        && project.kicad == e.kicad,
                                    egui::Button::new("Fetch repository…"),
                                )
                                .clicked()
                        {
                            fetch_opened =
                                self.project_git
                                    .fetch_dialog(project, folder.clone(), saved);
                        }
                    }
                }
            }
            form::error(ui, &e.error);
            // A KiCad link is saved before its schematics are loaded on selection.
            if relocating{ui.horizontal(|ui|{ui.spinner();ui.label("Preparing repository folder…");});}
            form::actions(ui, "Save", !relocating)
        });
        #[cfg(not(target_arch = "wasm32"))]
        if fetch_opened {
            return;
        }
        let (save, cancel) = response.inner;
        if save {
            let result = self.store.lock().unwrap().change(|d| {
                let name = e.name.trim();
                ensure!(!name.is_empty(), "Name is required");
                if e.project {
                    if let Some(old) = &e.old {
                        let current = d
                            .projects
                            .iter()
                            .find(|p| key(&p.name) == key(old))
                            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
                        let description = if e.description == e.original_description {
                            current.description.clone()
                        } else {
                            e.description.clone()
                        };
                        d.rename_project(old, name, &description)?;
                    } else {
                        ensure!(
                            !d.projects.iter().any(|p| key(&p.name) == key(name)),
                            "Project already exists"
                        );
                        d.ensure_project(name)?;
                    }
                    if e.kicad.is_none() {
                        d.detach_kicad_requests(name);
                    }
                    let p = d
                        .projects
                        .iter_mut()
                        .find(|p| key(&p.name) == key(name))
                        .unwrap();
                    if e.old.is_none() {
                        p.description = e.description.clone();
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some((path, remote, original)) = &e.git_origin
                        && (remote != original
                            || (p.git_remote.is_none() && !remote.url.is_empty()))
                    {
                        let mut remote = remote.clone();
                        project_git::validate_remote(&mut remote)?;
                        if remote != *original
                            && let Some(path) = path
                        {
                            project_git::configure(path, &remote)?;
                            self.project_git.invalidate(path);
                        }
                        p.git_remote = Some(remote);
                    }
                    p.kicad = e.kicad.clone();
                } else {
                    ensure!(
                        !d.storage.keys().any(|s| key(s) == key(name)
                            && e.old.as_ref().is_none_or(|old| key(s) != key(old))),
                        "Location already exists"
                    );
                    if let Some(old) = &e.old {
                        d.storage.retain(|s, _| key(s) != key(old));
                        for p in &mut d.parts {
                            if key(&p.storage) == key(old) {
                                p.storage = name.into();
                            }
                        }
                    }
                    d.storage.insert(name.into(), e.description.clone());
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    self.catalog = Some(e.name.trim().into());
                    #[cfg(target_arch = "wasm32")]
                    if e.project {
                        let project=self.store.lock().unwrap().data.projects.iter().find(|p|key(&p.name)==key(e.name.trim())).cloned();
                        if let Some(project)=project && let Some(link)=&project.kicad
                            && !link.revision.starts_with("git-snapshot:") && let Some(folder)=&link.browser_folder {
                            crate::web::local_git(&project.id,folder,"share","","","");
                        }
                    }

                    return;
                }
                Err(error) => e.error = Some(error.to_string()),
            }
        }
        if !cancel && !form::escape(&response) {
            self.catalog_editor = Some(e);
        }
    }
    pub(super) fn allocation_window(&mut self, ctx: &egui::Context, data: &Inventory) {
        let Some(mut e) = self.allocation.take() else {
            return;
        };
        let response = form::sheet("allocation_editor").show(ctx, |ui| {
            form::configure(ui, 412.);
            form::heading(
                ui,
                &if e.existing.is_some() {
                    format!("Edit allocation — {}", e.project)
                } else {
                    "Allocate to project".into()
                },
                Icon::Box,
            );
            if e.existing.is_none() {
                form::row(ui, "Project", |ui| {
                    form::suggestion(
                        ui,
                        "allocation_projects",
                        &mut e.project,
                        data.projects.iter().map(|p| p.name.clone()),
                    );
                });
            }
            form::row(ui, "Quantity", |ui| {
                ui.add(
                    egui::DragValue::new(&mut e.quantity)
                        .range(1..=1_000_000_000)
                        .speed(1.),
                );
            });
            form::row(ui, "Designators", |ui| {
                if form::field(ui, &mut e.designators).changed()
                    && !e.designators.trim().is_empty()
                    && let Ok(refs) = references(&e.designators)
                {
                    e.quantity = refs.len() as i64;
                }
                form::hint(ui, "For example: R1, R2 or C16-C20.");
            });
            form::error(ui, &e.error);
            form::actions(
                ui,
                if e.existing.is_some() {
                    "Save"
                } else {
                    "Allocate"
                },
                true,
            )
        });
        let (save, cancel) = response.inner;
        if save {
            let result = self.store.lock().unwrap().change(|d| {
                let quantity = if e.designators.trim().is_empty() {
                    e.quantity
                } else {
                    references(&e.designators)?.len() as i64
                };
                let project = d.ensure_project(&e.project)?;
                if e.purchase {
                    let p = d
                        .purchases
                        .iter_mut()
                        .find(|p| p.part.id == e.part_id)
                        .ok_or_else(|| anyhow::anyhow!("Purchase no longer exists"))?;
                    if let Some(existing) = &e.existing {
                        let a = p
                            .assignments
                            .iter_mut()
                            .find(|a| &a.id == existing)
                            .ok_or_else(|| anyhow::anyhow!("Assignment no longer exists"))?;
                        a.project = project;
                        a.quantity = quantity;
                        a.designators = e.designators.clone();
                    } else {
                        p.assignments.push(Assignment {
                            id: id(),
                            project,
                            quantity,
                            designators: e.designators.clone(),
                        });
                    }
                } else if let Some(existing) = &e.existing {
                    let a = d
                        .allocations
                        .iter_mut()
                        .find(|a| &a.id == existing)
                        .ok_or_else(|| anyhow::anyhow!("Allocation no longer exists"))?;
                    a.project = project;
                    a.quantity = quantity;
                    a.designators = e.designators.clone();
                } else {
                    d.allocate(&e.part_id, &project, quantity, &e.designators)?;
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    self.status = "Project assignment saved".into();
                    return;
                }
                Err(error) => e.error = Some(error.to_string()),
            }
        }
        if !cancel && !form::escape(&response) {
            self.allocation = Some(e);
        }
    }
    pub(super) fn stock_window(&mut self, ctx: &egui::Context) {
        let Some(mut e) = self.stock_editor.take() else {
            return;
        };
        let response = form::sheet("stock_editor").show(ctx, |ui| {
            form::configure(ui, 342.);
            form::heading(
                ui,
                if e.purchase {
                    "Update purchase quantity"
                } else {
                    "Update on-hand stock"
                },
                Icon::Box,
            );
            let reserved = {
                let store = self.store.lock().unwrap();
                store.data.allocated(&e.part_id)
            };
            ui.label(
                RichText::new(format!("{reserved} units are allocated to projects."))
                    .color(theme::MUTED),
            );
            form::row(ui, "Total quantity", |ui| {
                ui.add(
                    egui::DragValue::new(&mut e.quantity)
                        .range(0..=1_000_000_000)
                        .speed(1.),
                );
            });
            form::error(ui, &e.error);
            form::actions(ui, "Save stock", true)
        });
        let (save, cancel) = response.inner;
        if save {
            let mut store = self.store.lock().unwrap();
            let managed = store.uses_write_approval();
            let result = store.change(|d| {
                if e.purchase {
                    let p = d
                        .purchases
                        .iter_mut()
                        .find(|p| p.part.id == e.part_id)
                        .ok_or_else(|| anyhow::anyhow!("Purchase not found"))?;
                    ensure!(
                        !managed
                            || p.part.stock == e.original_quantity
                            || p.part.stock == e.quantity,
                        "Purchase quantity changed to {} while you were editing",
                        p.part.stock
                    );
                    p.part.stock = e.quantity;
                } else {
                    let mut p = d
                        .parts
                        .iter()
                        .find(|p| p.id == e.part_id)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Part not found"))?;
                    ensure!(
                        !managed || p.stock == e.original_quantity || p.stock == e.quantity,
                        "Stock changed to {} while you were editing",
                        p.stock
                    );
                    p.stock = e.quantity;
                    d.save_part(p)?;
                }
                Ok(())
            });
            drop(store);
            match result {
                Ok(()) => {
                    self.status = "Stock updated".into();
                    return;
                }
                Err(error) => e.error = Some(error.to_string()),
            }
        }
        if !cancel && !form::escape(&response) {
            self.stock_editor = Some(e);
        }
    }
    pub(super) fn delete_window(&mut self, ctx: &egui::Context) {
        let Some(action) = self.delete.take() else {
            return;
        };
        let releasing = matches!(
            action,
            Delete::Allocation(_) | Delete::PurchaseAssignment { .. }
        );
        let response = form::sheet("delete_confirmation").show(ctx, |ui| {
            form::configure(ui, 350.);
            form::heading(
                ui,
                match &action {
                    Delete::Allocation(_) => "Release allocation?",
                    Delete::PurchaseAssignment { .. } => "Release purchase assignment?",
                    _ => "Confirm deletion",
                },
                if releasing { Icon::Release } else { Icon::Trash },
            );
            ui.label(match &action {
                Delete::Part(_, true) => "Remove this planned purchase and its assignments?",
                Delete::Part(_, false) => {
                    "The selected part and its project allocations will be permanently removed."
                }
                Delete::Project(_) => {
                    "All allocations in this project will be released back to available stock."
                }
                Delete::Storage(_) => "Parts in this storage location will move to (no storage).",
                Delete::Request(_) => {
                    "Remove this request. Any stock already allocated will be kept."
                }
                Delete::Allocation(_) => {
                    "Release this reservation? BOM requirements will return to Requested."
                }
                Delete::PurchaseAssignment { .. } => "Release this purchase assignment from the project? Its BOM requirements will return to Requested. The part will remain in Purchase.",
            });
            form::actions(
                ui,
                if releasing {
                    "Release"
                } else {
                    "Delete"
                },
                true,
            )
        });
        let (confirmed, cancel) = response.inner;
        if confirmed {
            self.change(if releasing { "Released" } else { "Removed" }, |d| {
                match &action {
                    Delete::Part(id, true) => d.purchases.retain(|p| &p.part.id != id),
                    Delete::Part(id, false) => d.delete_part(id),
                    Delete::Project(name) => d.delete_project(name),
                    Delete::Storage(name) => {
                        d.storage.retain(|n, _| key(n) != key(name));
                        for p in &mut d.parts {
                            if key(&p.storage) == key(name) {
                                p.storage.clear();
                            }
                        }
                    }
                    Delete::Allocation(id) => d.release(id),
                    Delete::PurchaseAssignment {
                        part_id,
                        assignment_id,
                    } => {
                        if let Some(item) = d.purchases.iter_mut().find(|p| &p.part.id == part_id) {
                            item.assignments.retain(|a| &a.id != assignment_id);
                        }
                    }
                    Delete::Request(id) => d.delete_request(id)?,
                }
                Ok(())
            });
            self.selected = None;
        } else if !cancel && !form::escape(&response) {
            self.delete = Some(action);
        }
    }
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings {
            return;
        }
        let size = ctx.content_rect().size();
        let width = (size.x - 70.).clamp(300., 740.);
        let height = (size.y - 190.).clamp(120., 440.);
        let response = form::sheet("settings")
            .frame(
                theme::frame(theme::PANEL)
                    .inner_margin(10)
                    .corner_radius(10)
                    .stroke(egui::Stroke::new(1., theme::LINE)),
            )
            .show(ctx, |ui| {
                form::configure(ui, width);
                ui.spacing_mut().item_spacing.y = 5.;
                let close = ui
                    .horizontal(|ui| {
                        theme::icon(ui, Icon::Settings, 16., GOLD);
                        ui.label(RichText::new("Preferences").size(15.).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            theme::tool_sized(
                                ui,
                                Icon::Close,
                                "Close preferences",
                                egui::vec2(26., 26.),
                                false,
                            )
                            .clicked()
                        })
                        .inner
                    })
                    .inner;
                ui.separator();
                ui.horizontal_top(|ui| {
                    let sidebar_width = (width * 0.28).clamp(132., 178.);
                    theme::frame(theme::RAISED)
                        .inner_margin(10)
                        .corner_radius(6)
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_min_size(egui::vec2(sidebar_width - 20., height - 20.));
                                for (tab, title, icon) in [
                                    (0, "General", Icon::Settings),
                                    (1, "Sync", Icon::Mesh),
                                    #[cfg(not(target_arch = "wasm32"))]
                                    (2, "Version control", Icon::Git),
                                    #[cfg(target_arch = "wasm32")]
                                    (2, "Version control", Icon::Settings),
                                ] {
                                    if tab == 1 {
                                        ui.add_space(8.);
                                        ui.label(
                                            RichText::new("NETWORK")
                                                .small()
                                                .strong()
                                                .color(theme::MUTED),
                                        );
                                    }
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(sidebar_width - 20., 32.),
                                        egui::Sense::click(),
                                    );
                                    if response.clicked() {
                                        self.preferences.tab = tab;
                                    }
                                    let selected = self.preferences.tab == tab;
                                    if selected || response.hovered() {
                                        ui.painter().rect_filled(
                                            rect,
                                            5,
                                            if selected {
                                                theme::SELECTION
                                            } else {
                                                theme::PANEL
                                            },
                                        );
                                    }
                                    theme::paint_icon(
                                        ui.painter(),
                                        icon,
                                        egui::Rect::from_center_size(
                                            rect.left_center() + egui::vec2(14., 0.),
                                            egui::vec2(16., 16.),
                                        ),
                                        GOLD,
                                    );
                                    ui.painter().text(
                                        rect.left_center() + egui::vec2(29., 0.),
                                        egui::Align2::LEFT_CENTER,
                                        title,
                                        egui::FontId::proportional(13.),
                                        theme::TEXT,
                                    );
                                }
                            });
                        });
                    ui.vertical(|ui| {
                        ui.set_width((width - sidebar_width - 10.).max(80.));
                        egui::ScrollArea::vertical()
                            .id_salt(("preferences", self.preferences.tab))
                            .max_height(height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if self.preferences.tab == 1 {
                                    ui.heading("Sync");
                                    ui.label("One WebSocket listener for peer sync (/sync), merge review (/merge), and local Chrome extension imports (/extension).");
                                    #[cfg(not(target_arch = "wasm32"))]
                                    self.mesh_host_controls(ui);
                                    #[cfg(target_arch = "wasm32")]
                                    ui.label("Mesh hosting is available in the desktop app.");
                                } else if self.preferences.tab == 2 {
                                    #[cfg(not(target_arch = "wasm32"))]
                                    self.version_control_preferences(ui);
                                    #[cfg(target_arch = "wasm32")]
                                    {
                                        ui.heading("Version control");
                                        let mut author=super::browser_git::author();
                                        let name=form::row(ui,"Author name",|ui|form::field(ui,&mut author.name)).changed();
                                        let email=form::row(ui,"Author email",|ui|form::field(ui,&mut author.email)).changed();
                                        if name||email {
                                            let saved=crate::web::storage().and_then(|storage|storage.set_item("vinventory.git.author",&serde_json::to_string(&author)?).map_err(crate::web::js_error));
                                            if let Err(error)=saved{ui.colored_label(Color32::LIGHT_RED,error.to_string());}
                                        }
                                        ui.separator();
                                        let mut keep = crate::web::keep_git_bundles();
                                        if ui.checkbox(&mut keep, "Preserve Git bundles").changed() {
                                            crate::web::set_keep_git_bundles(keep);
                                        }
                                        ui.label("Keep the original bundle bytes in browser storage so they can be reused as bases.");
                                    }
                                } else {
                                    self.general_preferences(ui);
                                }
                            });
                    });
                });
                ui.separator();
                let done = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Button::new(RichText::new("Done").color(Color32::WHITE))
                                .fill(theme::PURPLE)
                                .min_size(egui::vec2(70., 24.)),
                        )
                    })
                    .inner
                    .clicked();
                close || done
            });
        if response.inner || form::escape(&response) {
            self.settings = false;
            self.preferences = Preferences::default();
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn version_control_preferences(&mut self, ui: &mut egui::Ui) {
        ui.heading("Version control");
        if self.preferences.git.is_none() {
            match project_git::load_preferences() {
                Ok(settings) => self.preferences.git = Some(settings),
                Err(error) => {
                    ui.label(format!(
                        "Could not load version control preferences: {error}"
                    ));
                    return;
                }
            }
        }
        let settings = self.preferences.git.as_mut().unwrap();
        ui.checkbox(&mut settings.preserve_bundles, "Preserve Git bundles");
        ui.separator();
        ui.strong("SSH");
        form::row(ui, "SSH agent socket", |ui| {
            form::field(ui, &mut settings.ssh.socket)
        });
        ui.separator();
        for (label, path) in [
            ("Private key file", &mut settings.ssh.private_key),
            ("Public key file", &mut settings.ssh.public_key),
        ] {
            form::row(ui, label, |ui| {
                form::field(ui, path);
                if ui.button("Browse…").clicked()
                    && let Some(file) = rfd::FileDialog::new().pick_file()
                {
                    *path = file.to_string_lossy().into_owned();
                }
            });
        }
        form::hint(
            ui,
            "SSH key files stay where they are. A public key file can select its matching private key from the agent.",
        );

        ui.separator();
        ui.strong("HTTPS personal access tokens");
        form::hint(
            ui,
            "Use one token per HTTPS host. Tokens are saved in the app’s local preferences file without encryption.",
        );
        let mut remove = None;
        for (index, entry) in settings.tokens.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                form::row(ui, "Host", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut entry.host)
                            .hint_text("github.com")
                            .desired_width(f32::INFINITY),
                    );
                });
                form::row(ui, "Username", |ui| form::field(ui, &mut entry.username));
                form::row(ui, "Token", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut entry.token)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                });
                if ui.button("Remove token").clicked() {
                    remove = Some(index);
                }
                ui.separator();
            });
        }
        if let Some(index) = remove {
            settings.tokens.remove(index);
        }
        if ui.button("Add token").clicked() {
            settings.tokens.push(project_git::HttpsToken::default());
        }
        if ui.button("Save").clicked() {
            self.preferences.git_status = Some(match project_git::save_preferences(settings) {
                Ok(()) => "Version control preferences saved.".into(),
                Err(error) => format!("Could not save version control preferences: {error}"),
            });
        }
        if let Some(status) = &self.preferences.git_status {
            ui.label(status);
        }
    }

    fn general_preferences(&mut self, ui: &mut egui::Ui) {
        ui.heading("General");
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = self.store.lock().unwrap().path.clone();
            ui.strong("Data folder");
            let folder = self.preferences.folder.get_or_insert_with(|| {
                crate::data_location::configured_folder()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| path.parent().unwrap_or(std::path::Path::new("")).to_owned())
                    .to_string_lossy()
                    .into_owned()
            });
            form::field(ui, folder);
            ui.horizontal(|ui| {
                if ui.button("Browse…").clicked()
                    && let Some(path) = crate::platform::dialogs::FileDialog::new().pick_folder()
                {
                    *folder = path.to_string_lossy().into_owned();
                    self.preferences.folder_status = None;
                }
                if ui.button("Save folder").clicked() {
                    self.preferences.folder_status = Some(match crate::data_location::save_folder(
                        std::path::Path::new(folder.trim()),
                    ) {
                        Ok(()) => "Folder saved. Restart the app to open it.".into(),
                        Err(error) => format!("Could not save folder: {error}"),
                    });
                }
            });
            form::hint(
                ui,
                "The folder holds SQLite, attachments and sync chunks. Restart to open the inventory in that folder. An empty folder starts a new inventory. Existing data stays in its current folder.",
            );
            if let Some(status) = &self.preferences.folder_status {
                ui.label(status);
            }
            ui.label(
                RichText::new(format!("Active database: {}", path.display()))
                    .small()
                    .color(theme::MUTED),
            );
        }
        #[cfg(target_arch = "wasm32")]
        {
            ui.strong("Inventory workspace");
            if ui.button("Choose folder…").clicked() {
                crate::web::choose_inventory_workspace();
            }
            form::hint(ui, "Choose a folder for attachments and action history. Choose the same folder again if the browser asks you to restore access.");
            let status = crate::web::inventory_workspace_status();
            if !status.is_empty() {
                form::hint(ui, &status);
            }
        }
        ui.separator();
        ui.strong("KiCad 3D model library");
        #[cfg(not(target_arch = "wasm32"))]
        {
            let folder = self.preferences.models_folder.get_or_insert_with(|| {
                crate::model_library::configured_folder().unwrap_or_default()
            });
            form::field(ui, folder);
            ui.horizontal(|ui| {
                if ui.button("Browse…").clicked()
                    && let Some(path) = crate::platform::dialogs::FileDialog::new()
                        .set_title("Choose KiCad 3D model library (contains .3dshapes folders)")
                        .pick_folder()
                {
                    *folder = path.to_string_lossy().into_owned();
                    self.preferences.models_status = None;
                }
                if ui.button("Save model library").clicked() {
                    self.preferences.models_status = Some(match crate::model_library::save_folder(folder) {
                        Ok(()) => "Model library saved.".into(),
                        Err(error) => format!("Could not save model library: {error}"),
                    });
                }
            });
            form::hint(ui, "Choose the library containing .3dshapes folders, rather than an individual STEP file. Used by all projects unless a project has its own model folder. Leave empty for automatic detection.");
            if let Some(status) = &self.preferences.models_status {
                ui.label(status);
            }
        }
        #[cfg(target_arch = "wasm32")]
        form::hint(ui, "Choose a model library using Models folder… in the 3D board view to grant browser access to its files.");
        #[cfg(target_arch = "wasm32")]
        {
            ui.separator();
            let demo = crate::web::is_browser_demo();
            ui.strong(if demo { "Demo inventory" } else { "Testing" });
            let label = if demo { "Reset demo…" } else { "Reset browser inventory…" };
            if ui
                .button(RichText::new(label).color(Color32::LIGHT_RED))
                .clicked()
            {
                crate::web::reset_browser_inventory();
            }
            form::hint(
                ui,
                if demo {
                    "Clears your demo changes and reloads the sample inventory."
                } else {
                    "Deletes the browser database, attachments, and action history."
                },
            );
        }
    }

    pub(super) fn error_window(&mut self, ctx: &egui::Context) {
        let Some(error) = self.error.clone() else {
            return;
        };
        let response = form::sheet("inventory_error").show(ctx, |ui| {
            form::configure(ui, 370.);
            form::heading(ui, "Inventory error", Icon::Box);
            ui.label(error);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::Button::new(RichText::new("OK").color(Color32::WHITE))
                        .fill(theme::PURPLE)
                        .min_size(egui::vec2(80., 28.)),
                )
            })
            .inner
            .clicked()
        });
        if response.inner || form::escape(&response) {
            self.error = None;
        }
    }
}
