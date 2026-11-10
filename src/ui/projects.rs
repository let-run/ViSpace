use super::*;

impl InventoryApp {
    pub(super) fn project_panel(&mut self, ui: &mut egui::Ui, data: &Inventory) {
        let Some(name) = self.catalog.clone() else {
            theme::empty(
                ui,
                "Select a project",
                "Add a project or allocate stock from PARTS.",
            );
            return;
        };
        ui.spacing_mut().item_spacing.y = 0.;
        if self.project_tab == 3 && !self.kicad.has_viewer(&name) {
            self.project_tab = 0;
        }
        if self.project_tab == 4 && !self.kicad.has_pcb(&name) {
            self.project_tab = 0;
        }
        if self.project_tab == 5 && !self.kicad.has_3d(&name) {
            self.project_tab = 0;
        }
        self.project_toolbar(ui, data, &name);
        if self.project_tab == 5 {
            if self
                .kicad
                .board3d_panel(ui, data, &name, self.store_revision)
            {
                self.choose_model_library(&name);
            }
            if let Some(target) = self.kicad.take_3d_part_navigation() {
                self.open_project_part(target, data, ui.ctx());
            }
            return;
        }
        if self.project_tab == 4 {
            if let Some(target) = self.kicad.pcb_panel(ui, data, self.store_revision, &name) {
                self.open_project_part(target, data, ui.ctx());
            }
            return;
        }
        if self.project_tab == 3 {
            if let Some(target) = self.kicad.viewer_panel(ui, data, self.store_revision) {
                self.open_project_part(target, data, ui.ctx());
            }
            return;
        }
        if self.bulk.is_some() {
            self.bulk_table(ui, data);
            return;
        }
        ui.spacing_mut().item_spacing.y = 0.;
        ui.style_mut().interaction.selectable_labels = false;
        if self.project_tab == 1 {
            self.requests_panel(ui, data, &name);
        } else {
            self.project_parts_table(ui, data, &name, self.project_tab == 2);
        }
    }
    fn choose_model_library(&mut self, project: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Choose KiCad 3D model library (contains .3dshapes folders)")
            .pick_folder()
        {
            let project = project.to_owned();
            let path = path.to_string_lossy().into_owned();
            self.change("3D model library updated", move |data| {
                if let Some(link) = data
                    .projects
                    .iter_mut()
                    .find(|p| p.name == project)
                    .and_then(|p| p.kicad.as_mut())
                {
                    link.models_directory = Some(path);
                }
                Ok(())
            });
        }
        #[cfg(target_arch = "wasm32")]
        crate::web::pick_models_folder(project.to_owned());
    }
    fn open_project_part(
        &mut self,
        target: footprint_details::PartTarget,
        data: &Inventory,
        ctx: &egui::Context,
    ) {
        let part = if target.purchase {
            data.purchases
                .iter()
                .find(|p| p.part.id == target.id)
                .map(|p| &p.part)
        } else {
            data.parts.iter().find(|p| p.id == target.id)
        };
        let Some(_) = part else {
            return;
        };
        self.project_tab = if target.purchase { 2 } else { 0 };
        self.selected = Some(target.id.clone());
        self.project_part_reveal = Some(target.id);
        self.query.clear();
        self.part_preview = None;
        self.bulk = None;
        ctx.request_repaint();
    }
    fn project_parts_table(
        &mut self,
        ui: &mut egui::Ui,
        data: &Inventory,
        project: &str,
        purchase: bool,
    ) {
        let query = key(&self.query);
        let mut entries: Vec<ProjectPart<'_>> = if purchase {
            data.purchases
                .iter()
                .flat_map(|item| {
                    item.assignments
                        .iter()
                        .filter(|a| key(&a.project) == key(project))
                        .map(move |a| ProjectPart {
                            part: &item.part,
                            id: &a.id,
                            designators: &a.designators,
                            quantity: a.quantity,
                        })
                })
                .collect()
        } else {
            data.allocations
                .iter()
                .filter(|a| key(&a.project) == key(project))
                .filter_map(|a| {
                    data.parts
                        .iter()
                        .find(|p| p.id == a.part_id)
                        .map(|part| ProjectPart {
                            part,
                            id: &a.id,
                            designators: &a.designators,
                            quantity: a.quantity,
                        })
                })
                .collect()
        };
        entries.retain(|row| {
            row.part.matches(&self.query)
                || (!query.is_empty() && key(row.designators).contains(&query))
        });
        entries.sort_by(|a, b| part_number_cmp(&a.part.number, &b.part.number));
        let height = (ui.available_height() - 20.).max(80.);
        let link = data
            .projects
            .iter()
            .find(|p| key(&p.name) == key(project))
            .and_then(|p| p.kicad.as_ref());
        let schematic_enabled =
            link.is_some_and(|link| self.kicad.navigation_available(link, false));
        let pcb_enabled = link.is_some_and(|link| self.kicad.navigation_available(link, true));
        let table_viewport = ui.available_rect_before_wrap().intersect(ui.clip_rect());
        let mut navigate = None;
        let reveal = self
            .project_part_reveal
            .take()
            .and_then(|id| entries.iter().position(|row| row.part.id == id));
        egui::ScrollArea::horizontal()
            .id_salt(("project_parts_horizontal", purchase))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(
                    table::PART_COLUMNS
                        .iter()
                        .map(|(_, width)| width)
                        .sum::<f32>()
                        + 40.
                        + 180.
                        + 65.
                        + 64.
                        + 14.,
                );
                ui.spacing_mut().item_spacing = egui::vec2(1., 0.);
                theme::table_style(ui);
                theme::table_header(ui, 32.);
                let mut builder = TableBuilder::new(ui)
                    .id_salt(("project_parts", purchase))
                    .striped(false)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .min_scrolled_height(height - 32.)
                    .max_scroll_height(height - 32.)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(40.).clip(true))
                    .column(Column::initial(180.).at_least(55.).clip(true))
                    .column(Column::initial(65.).at_least(45.).clip(true))
                    .column(Column::initial(64.).at_least(58.).clip(true));
                for (_, width) in table::PART_COLUMNS {
                    builder = builder.column(Column::initial(width).at_least(55.).clip(true));
                }
                if let Some(index) = reveal {
                    builder = builder.scroll_to_row(index, Some(egui::Align::Center));
                }
                builder
                    .header(32., |mut row| {
                        for title in ["#", "Designators", "Qty", ""] {
                            row.col(|ui| {
                                if title == "#" {
                                    right_cell(ui, title, theme::MUTED);
                                } else {
                                    ui.add_space(7.);
                                    ui.label(RichText::new(title).size(12.).color(theme::MUTED));
                                }
                            });
                        }
                        for (index, (title, _)) in table::PART_COLUMNS.into_iter().enumerate() {
                            row.col(|ui| {
                                ui.add_space(7.);
                                ui.label(
                                    RichText::new(if purchase && index == 6 {
                                        "To purchase"
                                    } else {
                                        title
                                    })
                                    .size(12.)
                                    .color(theme::MUTED),
                                );
                            });
                        }
                    })
                    .body(|mut body| {
                        for (index, entry) in entries.iter().enumerate() {
                            let part = entry.part;
                            let mut navigated = false;
                            body.row(theme::ROW_HEIGHT, |mut row| {
                                row.set_overline(true);
                                row.set_selected(
                                    self.selected.as_deref() == Some(part.id.as_str())
                                        || self.is_previewing(&part.id, purchase),
                                );
                                row.col(|ui| {
                                    right_cell(ui, &(index + 1).to_string(), theme::MUTED)
                                });
                                row.col(|ui| {
                                    ui.add_space(7.);
                                    if !purchase || !entry.designators.trim().is_empty() {
                                        if let Some(pcb) = requests::designators_navigation_cell(
                                            ui,
                                            table_viewport,
                                            entry.designators,
                                            schematic_enabled,
                                            pcb_enabled,
                                        ) {
                                            navigate = Some((entry.designators.to_owned(), pcb));
                                            navigated = true;
                                        }
                                    } else {
                                        ui.add(egui::Label::new(entry.designators).truncate())
                                            .on_hover_text(entry.designators);
                                    }
                                });
                                row.col(|ui| {
                                    right_cell(ui, &entry.quantity.to_string(), theme::TEXT)
                                });
                                row.col(|ui| {
                                    ui.horizontal(|ui| {
                                        if theme::tool_sized(
                                            ui,
                                            Icon::Edit,
                                            "Edit allocation",
                                            egui::vec2(24., 24.),
                                            false,
                                        )
                                        .clicked()
                                        {
                                            navigated = true;
                                            self.allocation = Some(AllocationEditor {
                                                error: None,
                                                existing: Some(entry.id.into()),
                                                part_id: part.id.clone(),
                                                purchase,
                                                project: project.into(),
                                                quantity: entry.quantity,
                                                designators: entry.designators.into(),
                                            });
                                        }
                                        if theme::tool_sized(
                                            ui,
                                            Icon::Release,
                                            "Release from project",
                                            egui::vec2(24., 24.),
                                            false,
                                        )
                                        .clicked()
                                        {
                                            navigated = true;
                                            self.delete = Some(if purchase {
                                                Delete::PurchaseAssignment {
                                                    part_id: part.id.clone(),
                                                    assignment_id: entry.id.into(),
                                                }
                                            } else {
                                                Delete::Allocation(entry.id.into())
                                            });
                                        }
                                    });
                                });
                                table::part_cells(&mut row, part);
                                let response = row.response();
                                if response.clicked() && !navigated {
                                    self.selected = Some(part.id.clone());
                                    self.open_part_preview(&part.id, purchase, &response);
                                }
                            });
                        }
                    });
            });
        if let Some((designators, pcb)) = navigate
            && let Some(link) = link
        {
            self.part_preview = None;
            if let Err(error) =
                self.kicad
                    .navigate_designators(project, link, &designators, pcb, ui.ctx())
            {
                self.error = Some(error.to_string());
            }
        }
    }
    fn project_toolbar(&mut self, ui: &mut egui::Ui, data: &Inventory, name: &str) {
        let units: i64 = self.bulk_units().unwrap_or_else(|| match self.project_tab {
            0 => data
                .allocations
                .iter()
                .filter(|a| key(&a.project) == key(name))
                .map(|a| a.quantity)
                .sum(),
            1 => data
                .bom
                .iter()
                .filter(|e| key(&e.project) == key(name))
                .map(|e| self.request_quantities.get(&e.id).copied().unwrap_or(0))
                .sum(),
            2 => data
                .purchases
                .iter()
                .flat_map(|p| p.assignments.iter())
                .filter(|a| key(&a.project) == key(name))
                .map(|a| a.quantity)
                .sum(),
            _ => 0,
        });
        egui::TopBottomPanel::top("project_toolbar")
            .exact_height(40.)
            .frame(theme::frame(theme::RAISED).inner_margin(egui::Margin::symmetric(14, 0)))
            .show_inside(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let title = format!("ALLOCATIONS IN {}", name.to_uppercase());
                    let title_width = (ui.available_width()
                        - 400.
                        - if self.kicad.has_viewer(name) {
                            140.
                        } else {
                            0.
                        }
                        - if self.kicad.has_pcb(name) { 95. } else { 0. }
                        - if self.kicad.has_3d(name) { 110. } else { 0. })
                    .max(70.)
                    .min(theme::measure_text(ui, &title, 12., 1).x);
                    ui.add_sized(
                        egui::vec2(title_width, 26.),
                        egui::Label::new(
                            RichText::new(&title).size(12.).strong().color(theme::MUTED),
                        )
                        .truncate(),
                    )
                    .on_hover_text(title);
                    self.bulk_controls(ui);
                    if ui
                        .add_enabled_ui(self.bulk.is_none(), |ui| {
                            theme::tool_sized(
                                ui,
                                Icon::Import,
                                "Import project BOM",
                                egui::vec2(26., 26.),
                                false,
                            )
                        })
                        .inner
                        .clicked()
                        && let Some(path) = crate::platform::dialogs::FileDialog::new()
                            .add_filter("KiCad CSV", &["csv"])
                            .pick_file()
                    {
                        let result = (|| {
                            csv_io::read_bom(crate::platform::fs::File::open(path)?, data, name)
                        })();
                        if let Some(rows) = self.report(result) {
                            self.bom = Some(BomReview {
                                project: name.into(),
                                rows,
                            });
                        }
                    }
                    if ui
                        .add_enabled_ui(
                            self.project_tab == 1
                                && self.bulk.is_none()
                                && data.bom.iter().any(|e| {
                                    key(&e.project) == key(name)
                                        && self.request_quantities.get(&e.id).copied().unwrap_or(0)
                                            > 0
                                }),
                            |ui| {
                                theme::tool_sized(
                                    ui,
                                    Icon::Export,
                                    "Export requested BOM as CSV",
                                    egui::vec2(26., 26.),
                                    false,
                                )
                            },
                        )
                        .inner
                        .clicked()
                        && let Some(path) = crate::platform::dialogs::FileDialog::new()
                            .set_file_name("requested.csv")
                            .save_file()
                    {
                        self.report((|| {
                            csv_io::write_requested(
                                crate::platform::fs::File::create(path)?,
                                data,
                                name,
                            )
                        })());
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.;
                        for (index, title) in [(0, "Allocated"), (1, "Requested"), (2, "Purchase")]
                        {
                            let selected = self.project_tab == index;
                            if ui
                                .add_enabled(
                                    self.bulk.is_none(),
                                    egui::Button::new(
                                        RichText::new(title).size(12.).color(if selected {
                                            theme::TEXT
                                        } else {
                                            GOLD
                                        }),
                                    )
                                    .fill(if selected {
                                        theme::PURPLE
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .min_size(egui::vec2(78., 28.))
                                    .corner_radius(0),
                                )
                                .clicked()
                            {
                                self.project_tab = index;
                                self.part_preview = None;
                            }
                        }
                        if self.kicad.has_viewer(name) {
                            ui.add_space(6.);
                            egui::Frame::new()
                                .fill(if self.project_tab == 3 {
                                    theme::PURPLE
                                } else {
                                    theme::PANEL
                                })
                                .corner_radius(12)
                                .inner_margin(egui::Margin::symmetric(6, 0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("Schematic")
                                                        .size(12.)
                                                        .color(theme::TEXT),
                                                )
                                                .frame(false)
                                                .min_size(egui::vec2(78., 28.)),
                                            )
                                            .clicked()
                                        {
                                            self.project_tab = 3;
                                            self.part_preview = None;
                                        }
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("×")
                                                        .size(17.)
                                                        .color(theme::MUTED),
                                                )
                                                .frame(false)
                                                .min_size(egui::vec2(18., 24.)),
                                            )
                                            .on_hover_text("Close schematic")
                                            .clicked()
                                        {
                                            self.kicad.close_viewer();
                                            self.project_tab = 0;
                                        }
                                    });
                                });
                        }
                        if self.kicad.has_pcb(name) {
                            ui.add_space(6.);
                            egui::Frame::new()
                                .fill(if self.project_tab == 4 {
                                    theme::PURPLE
                                } else {
                                    theme::PANEL
                                })
                                .corner_radius(12)
                                .inner_margin(egui::Margin::symmetric(6, 0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("PCB")
                                                        .size(12.)
                                                        .color(theme::TEXT),
                                                )
                                                .frame(false)
                                                .min_size(egui::vec2(78., 28.)),
                                            )
                                            .clicked()
                                        {
                                            self.project_tab = 4;
                                            self.part_preview = None;
                                        }
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("×")
                                                        .size(17.)
                                                        .color(theme::MUTED),
                                                )
                                                .frame(false)
                                                .min_size(egui::vec2(18., 24.)),
                                            )
                                            .on_hover_text("Close PCB")
                                            .clicked()
                                        {
                                            self.kicad.close_pcb();
                                            self.project_tab = 0;
                                        }
                                    });
                                });
                        }
                        if self.kicad.has_3d(name) {
                            ui.add_space(6.);
                            egui::Frame::new()
                                .fill(if self.project_tab == 5 {
                                    theme::PURPLE
                                } else {
                                    theme::PANEL
                                })
                                .corner_radius(12)
                                .inner_margin(egui::Margin::symmetric(6, 0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("3D board")
                                                        .size(12.)
                                                        .color(theme::TEXT),
                                                )
                                                .frame(false)
                                                .min_size(egui::vec2(78., 28.)),
                                            )
                                            .clicked()
                                        {
                                            self.project_tab = 5;
                                            self.part_preview = None;
                                        }
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("×")
                                                        .size(17.)
                                                        .color(theme::MUTED),
                                                )
                                                .frame(false)
                                                .min_size(egui::vec2(18., 24.)),
                                            )
                                            .on_hover_text("Close 3D board")
                                            .clicked()
                                        {
                                            self.kicad.close_3d();
                                            self.project_tab = 0;
                                        }
                                    });
                                });
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{units} units"))
                                .size(12.)
                                .color(theme::MUTED),
                        );
                    });
                });
            });
    }
}

struct ProjectPart<'a> {
    part: &'a Part,
    id: &'a str,
    designators: &'a str,
    quantity: i64,
}
fn right_cell(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(7.);
        ui.label(RichText::new(text).color(color));
    });
}
