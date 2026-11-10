use super::*;

impl InventoryApp {
    pub(super) fn sidebar(&mut self, ctx: &egui::Context, data: &Inventory) {
        if !matches!(self.view, View::Storage | View::Projects) {
            return;
        }
        let projects = self.view == View::Projects;
        let entries: Vec<_> = if projects {
            data.projects
                .iter()
                .map(|p| (p.name.clone(), p.description.clone(), p.notes.clone()))
                .collect()
        } else {
            std::iter::once((
                String::new(),
                "Unassigned inventory".into(),
                Notes::default(),
            ))
            .chain(
                data.storage
                    .iter()
                    .map(|(n, d)| (n.clone(), d.clone(), Notes::default())),
            )
            .collect()
        };
        let panel = egui::SidePanel::left("catalog")
            .resizable(true)
            .show_separator_line(!projects)
            .default_width(340.)
            .width_range(250. ..=500.)
            .frame(theme::frame(theme::ROW))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.;
                egui::TopBottomPanel::top("catalog_toolbar")
                    .exact_height(40.)
                    .frame(theme::frame(theme::RAISED).inner_margin(egui::Margin::symmetric(14, 0)))
                    .show_inside(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.label(
                                RichText::new(if projects { "PROJECTS" } else { "LOCATIONS" })
                                    .size(12.)
                                    .strong()
                                    .color(theme::MUTED),
                            );
                            // Keep Add/Edit/Delete together against the trailing edge.
                            let controls_width = 3. * 26. + 2. * ui.spacing().item_spacing.x;
                            ui.add_space((ui.available_width() - controls_width).max(0.));
                            if theme::tool_sized(ui, Icon::Add, "Add", egui::vec2(26., 26.), false)
                                .clicked()
                            {
                                self.catalog_editor = Some(CatalogEditor {
                                    #[cfg(not(target_arch="wasm32"))]
                                    relocation:None,
                                    #[cfg(not(target_arch = "wasm32"))]
                                    git_origin: None,
                                    #[cfg(target_arch = "wasm32")]
                                    token: id(),
                                    #[cfg(target_arch = "wasm32")]
                                    kicad_choices: vec![],
                                    git_destination:None,
                                    kicad: None,
                                    error: None,
                                    project: projects,
                                    old: None,
                                    name: String::new(),
                                    description: String::new(),
                                    original_description: String::new(),
                                });
                            }
                            let selected = self
                                .catalog
                                .as_ref()
                                .and_then(|n| entries.iter().find(|(name, _, _)| name == n))
                                .filter(|(n, _, _)| !n.is_empty());
                            ui.add_enabled_ui(selected.is_some(), |ui| {
                                if theme::tool_sized(
                                    ui,
                                    Icon::Edit,
                                    "Edit",
                                    egui::vec2(26., 26.),
                                    false,
                                )
                                .clicked()
                                    && let Some((name, description, _)) = selected
                                {
                                    self.catalog_editor = Some(CatalogEditor {
                                    #[cfg(not(target_arch="wasm32"))]
                                    relocation:None,
                                        #[cfg(not(target_arch = "wasm32"))]
                                        git_origin: None,
                                        #[cfg(target_arch = "wasm32")]
                                        token: id(),
                                        #[cfg(target_arch = "wasm32")]
                                        kicad_choices: vec![],
                                        git_destination:data.projects.iter().find(|p|key(&p.name)==key(name)).and_then(|p|{let tips=p.unresolved_git_tips();(tips.len()==1).then(||(p.id.clone(),tips.iter().next().unwrap().to_string()))}),
                                        kicad: data
                                            .projects
                                            .iter()
                                            .find(|p| key(&p.name) == key(name))
                                            .and_then(|p| p.kicad.clone()),
                                        error: None,
                                        project: projects,
                                        old: Some(name.clone()),
                                        name: name.clone(),
                                        description: description.clone(),
                                        original_description: description.clone(),
                                    });
                                }
                                if theme::tool_sized(
                                    ui,
                                    Icon::Trash,
                                    "Delete",
                                    egui::vec2(26., 26.),
                                    false,
                                )
                                .clicked()
                                    && let Some((name, _, _)) = selected
                                {
                                    self.delete = Some(if projects {
                                        Delete::Project(name.clone())
                                    } else {
                                        Delete::Storage(name.clone())
                                    });
                                }
                            });
                        });
                    });
                ui.spacing_mut().item_spacing.y = 0.;
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 32.),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(rect, 0., theme::RAISED);
                catalog_text(ui, rect, "Name", "Description", 12.);
                egui::ScrollArea::vertical()
                    .id_salt(("catalog_scroll", self.view))
                    .show(ui, |ui| {
                        for (name, description, notes) in &entries {
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 36.),
                                egui::Sense::click(),
                            );
                            let selected = self.catalog.as_ref() == Some(name);
                            ui.painter().rect_filled(
                                rect,
                                0.,
                                if selected {
                                    theme::SELECTION
                                } else if response.hovered() {
                                    theme::PANEL
                                } else {
                                    theme::ROW
                                },
                            );
                            #[cfg(not(target_arch = "wasm32"))]
                            let git_folder = (projects
                                && selected
                                && self.project_git.is_open_project(name))
                                .then(|| {
                                    let project = data.projects.iter().find(|p| key(&p.name) == key(name))?;
                                    let path = project.kicad.as_ref().and_then(project_git::folder)?;
                                    Some((project, path))
                                })
                                .flatten();
                            #[cfg(not(target_arch = "wasm32"))]
                            let git_width = if git_folder.is_some() { 30. } else { 0. };
                            #[cfg(target_arch = "wasm32")]
                            let git_width = if projects && selected && data.projects.iter().find(|p|key(&p.name)==key(name)).and_then(|p|p.kicad.as_ref()).is_some_and(|link|link.browser_folder.is_some()){30.}else{0.};
                            catalog_text(
                                ui,
                                if projects {
                                    egui::Rect::from_min_max(
                                        rect.min,
                                        rect.max - egui::vec2(66. + git_width, 0.),
                                    )
                                } else {
                                    rect
                                },
                                if name.is_empty() {
                                    "(no storage)"
                                } else {
                                    name
                                },
                                description,
                                13.,
                            );
                            #[cfg(not(target_arch = "wasm32"))]
                            if let Some((project, path)) = git_folder {
                                let git_rect = egui::Rect::from_center_size(
                                    egui::pos2(rect.right() - 84., rect.center().y),
                                    egui::vec2(26., 26.),
                                );
                                self.project_git.control(ui, path, project, git_rect);
                            }
                            #[cfg(target_arch = "wasm32")]
                            if projects && selected {
                                if let Some(project)=data.projects.iter().find(|p| key(&p.name)==key(name))
                                    && project.kicad.as_ref().is_some_and(|link|link.browser_folder.is_some())
                                {
                                    let git_rect=egui::Rect::from_center_size(egui::pos2(rect.right()-84.,rect.center().y),egui::vec2(26.,26.));
                                    let git=ui.interact(git_rect,ui.id().with(("browser_git",&project.id)),egui::Sense::click());
                                    theme::paint_icon(ui.painter(),Icon::Commit,git_rect.shrink(5.),GOLD);
                                    if git.on_hover_text("Local version control").clicked(){self.open_browser_git(project);}
                                }
                            }
                            if projects {
                                let log_rect=egui::Rect::from_center_size(egui::pos2(rect.right()-54.,rect.center().y),egui::vec2(26.,26.));
                                let log=ui.interact(log_rect,ui.id().with(("git_log",name)),egui::Sense::click());
                                let blocked=data.projects.iter().find(|p|key(&p.name)==key(name)).is_some_and(|project|self.project_git_blocked(project));
                                theme::paint_icon(ui.painter(),Icon::GitLog,log_rect.shrink(5.),if blocked{Color32::LIGHT_RED}else{GOLD});
                                if log.on_hover_text(if blocked{"Git update blocked — open diagnostics to resolve"}else{"Git history and sync status"}).clicked() && let Some(project)=data.projects.iter().find(|p|key(&p.name)==key(name)){self.open_git_history(project,ui.ctx());}
                                let note_rect = egui::Rect::from_center_size(
                                    egui::pos2(rect.right() - 24., rect.center().y),
                                    egui::vec2(26., 26.),
                                );
                                let mut note_ui =
                                    ui.new_child(egui::UiBuilder::new().max_rect(note_rect));
                                let response = note_ui
                                    .allocate_response(note_rect.size(), egui::Sense::click());
                                theme::paint_icon(
                                    note_ui.painter(),
                                    Icon::Notes,
                                    response.rect.shrink(5.),
                                    if notes.is_empty() { theme::MUTED } else { GOLD },
                                );
                                if response.on_hover_text("Project notes").clicked()
                                    && let Some(project) =
                                        data.projects.iter().find(|p| key(&p.name) == key(name))
                                {
                                    self.open_project_notes(project);
                                }
                            }
                            if response.on_hover_text(description).clicked() {
                                #[cfg(not(target_arch = "wasm32"))]
                                if projects {
                                    self.project_git.open_project(name);
                                }
                                self.part_preview = None;
                                if projects && self.catalog.as_ref() != Some(name) {
                                    self.project_tab = 0;
                                    self.kicad.close_viewer();
                                }
                                self.bulk = None;
                                if projects
                                    && let Some(link) = data
                                        .projects
                                        .iter()
                                        .find(|p| key(&p.name) == key(name))
                                        .and_then(|p| p.kicad.as_ref())
                                {
                                    self.kicad.ensure(link, ui.ctx());
                                }
                                self.catalog = Some(name.clone());
                                self.selected = None;
                                self.page = 0;
                            }
                            if projects
                                && self.catalog.as_ref() == Some(name)
                                && let Some(project) =
                                    data.projects.iter().find(|p| key(&p.name) == key(name))
                                && let Some(link) = &project.kicad
                            {
                                self.kicad.project_tree(ui, name, link, self.project_tab);
                                if let Some(project) = self.kicad.take_opened_project() {
                                    self.catalog = Some(project);
                                    self.project_tab = self.kicad.opened_document_tab();
                                    self.bulk = None;
                                    self.part_preview = None;
                                    self.selected = None;
                                }
                            }
                        }
                    });
            });
        if projects {
            let rect = panel.response.rect;
            ctx.layer_painter(panel.response.layer_id).vline(
                rect.right() - 0.5,
                rect.y_range(),
                egui::Stroke::new(1., Color32::BLACK),
            );
        }
    }
}

fn catalog_text(ui: &egui::Ui, rect: egui::Rect, name: &str, description: &str, size: f32) {
    let split = rect.left() + 150.;
    for (text, left, right, color) in [
        (
            name,
            rect.left() + 14.,
            split - 10.,
            if size == 12. {
                theme::MUTED
            } else {
                theme::TEXT
            },
        ),
        (description, split, rect.right() - 14., theme::MUTED),
    ] {
        let clip = egui::Rect::from_min_max(
            egui::pos2(left, rect.top()),
            egui::pos2(right, rect.bottom()),
        );
        ui.painter().with_clip_rect(clip).text(
            egui::pos2(left, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(size),
            color,
        );
    }
}
