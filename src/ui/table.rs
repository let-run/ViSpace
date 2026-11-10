use super::*;

impl InventoryApp {
    pub(super) fn center(&mut self, ctx: &egui::Context, data: &Inventory) {
        egui::CentralPanel::default()
            .frame(theme::frame(theme::ROW))
            .show(ctx, |ui| {
                if self.view == View::Projects {
                    self.project_panel(ui, data);
                    return;
                }
                if self.view == View::Storage && self.catalog.is_none() {
                    theme::empty(
                        ui,
                        "Select a storage location",
                        "Add a storage location or select (no storage).",
                    );
                    return;
                }
                let purchase = self.view == View::Purchase;
                let parts: Vec<Part> = if purchase {
                    data.purchases.iter().map(|p| p.part.clone()).collect()
                } else {
                    data.parts
                        .iter()
                        .filter(|p| {
                            self.view != View::Storage
                                || self
                                    .catalog
                                    .as_ref()
                                    .is_none_or(|n| key(&p.storage) == key(n))
                        })
                        .cloned()
                        .collect()
                };
                if purchase {
                    theme::frame(theme::RAISED)
                        .inner_margin(egui::Margin::symmetric(14, 6))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("PURCHASE")
                                        .size(12.)
                                        .strong()
                                        .color(theme::MUTED),
                                );
                                if ui
                                    .add_enabled(
                                        !self.receive.is_empty(),
                                        primary(format!(
                                            "Receive selected ({})",
                                            self.receive.len()
                                        )),
                                    )
                                    .clicked()
                                {
                                    let ids = self.receive.iter().cloned().collect::<Vec<_>>();
                                    self.change("Purchases received into inventory", |d| {
                                        d.receive(&ids)?;
                                        Ok(())
                                    });
                                    self.receive.clear();
                                }
                                ui.label(
                                    RichText::new("Check delivered items to receive stock.")
                                        .size(12.)
                                        .color(theme::MUTED),
                                );
                            });
                        });
                } else if self.view == View::Storage {
                    ui.spacing_mut().item_spacing.y = 0.;
                    egui::TopBottomPanel::top("storage_toolbar")
                        .exact_height(40.)
                        .frame(
                            theme::frame(theme::RAISED)
                                .inner_margin(egui::Margin::symmetric(14, 0)),
                        )
                        .show_inside(ui, |ui| {
                            ui.horizontal_centered(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "INVENTORY IN {}",
                                            self.catalog
                                                .as_deref()
                                                .filter(|s| !s.is_empty())
                                                .unwrap_or("(NO STORAGE)")
                                                .to_uppercase()
                                        ))
                                        .size(12.)
                                        .strong()
                                        .color(theme::MUTED),
                                    )
                                    .truncate(),
                                );
                                self.bulk_controls(ui);
                            });
                        });
                    if self.bulk.is_some() {
                        self.bulk_table(ui, data);
                        return;
                    }
                }
                self.parts_table(ui, data, &parts, purchase);
            });
    }
    fn parts_table(&mut self, ui: &mut egui::Ui, data: &Inventory, parts: &[Part], purchase: bool) {
        let active = if purchase {
            &self.purchase_labels
        } else {
            &self.labels
        };
        let mut filtered: Vec<_> = parts
            .iter()
            .filter(|p| {
                (if purchase {
                    data.purchases
                        .iter()
                        .find(|item| item.part.id == p.id)
                        .is_some_and(|item| item.matches(&self.query))
                } else {
                    p.matches(&self.query)
                }) && (!matches!(self.view, View::Parts | View::Purchase)
                    || active.is_empty()
                    || p.labels
                        .iter()
                        .any(|label| active.iter().any(|s| key(s) == key(label))))
            })
            .collect();
        filtered.sort_by(|a, b| part_number_cmp(&a.number, &b.number));
        let pages = filtered.len().div_ceil(self.page_size).max(1);
        self.page = self.page.min(pages - 1);
        let full = ui.available_rect_before_wrap();
        let pagebar = egui::Rect::from_min_max(
            egui::pos2(
                full.left(),
                (full.bottom() - theme::PAGEBAR_HEIGHT).max(full.top()),
            ),
            full.right_bottom(),
        );
        let table_rect =
            egui::Rect::from_min_max(full.left_top(), egui::pos2(full.right(), pagebar.top()));
        let visible = &filtered[(self.page * self.page_size).min(filtered.len())
            ..((self.page + 1) * self.page_size).min(filtered.len())];
        ui.scope_builder(egui::UiBuilder::new().max_rect(table_rect), |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(1., 0.);
            let body_height = (table_rect.height() - 50.).max(0.);
            egui::ScrollArea::horizontal()
                .id_salt(("parts_horizontal", self.view))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let content_width = 1235. + if purchase { 45. } else { 0. };
                    ui.set_min_width(content_width);
                    let header = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(ui.available_width(), 32.),
                    );
                    ui.painter().rect_filled(header, 0., theme::RAISED);
                    // Text selection must not consume the table's row clicks.
                    ui.style_mut().interaction.selectable_labels = false;
                    let table_ctx = ui.ctx().clone();
                    theme::table_style(ui);
                    let mut table = TableBuilder::new(ui)
                        .id_salt(("parts_table", self.view, purchase))
                        .striped(false)
                        .resizable(true)
                        .sense(egui::Sense::click())
                        .min_scrolled_height(body_height)
                        .max_scroll_height(body_height)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                    if purchase {
                        table = table.column(Column::exact(45.));
                    }
                    for (_, width) in PART_COLUMNS {
                        table = table.column(Column::initial(width).at_least(55.).clip(true));
                    }
                    table
                        .header(32., |mut row| {
                            if purchase {
                                row.col(|ui| {
                                    ui.label(RichText::new("Recv").size(12.).color(theme::MUTED));
                                });
                            }
                            for (index, (title, _)) in PART_COLUMNS.into_iter().enumerate() {
                                let title = if purchase && index == 6 {
                                    "To purchase"
                                } else {
                                    title
                                };
                                row.col(|ui| {
                                    ui.add_space(7.);
                                    ui.label(RichText::new(title).size(12.).color(theme::MUTED));
                                });
                            }
                        })
                        .body(|mut body| {
                            for p in visible {
                                body.row(theme::ROW_HEIGHT, |mut row| {
                                    row.set_overline(true);
                                    row.set_selected(self.selected.as_ref() == Some(&p.id));
                                    if purchase {
                                        row.col(|ui| {
                                            let mut selected = self.receive.contains(&p.id);
                                            if ui.checkbox(&mut selected, "").changed() {
                                                if selected {
                                                    self.receive.insert(p.id.clone());
                                                } else {
                                                    self.receive.remove(&p.id);
                                                }
                                            }
                                        });
                                    }
                                    row.col(|ui| {
                                        theme::part_number_cell(ui, p);
                                    });
                                    for value in [
                                        &p.manufacturer,
                                        &p.description,
                                        &p.package,
                                        &p.value,
                                        &p.tolerance,
                                    ] {
                                        row.col(|ui| {
                                            ui.add_space(7.);
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(value).color(theme::TEXT),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(value);
                                        });
                                    }
                                    row.col(|ui| {
                                        ui.add_space(7.);
                                        ui.label(RichText::new(p.stock.to_string()).color(
                                            if p.stock > 0 {
                                                theme::GREEN
                                            } else {
                                                theme::MUTED
                                            },
                                        ));
                                    });
                                    for value in [&p.supplier, &p.spn, &p.storage] {
                                        row.col(|ui| {
                                            ui.add_space(7.);
                                            ui.add(
                                                egui::Label::new(RichText::new(value).color(
                                                    if value == &p.storage {
                                                        theme::MUTED
                                                    } else {
                                                        theme::TEXT
                                                    },
                                                ))
                                                .truncate(),
                                            )
                                            .on_hover_text(value);
                                        });
                                    }
                                    let response = row.response();
                                    if response.clicked() {
                                        if self.view == View::Storage {
                                            self.open_part_preview(&p.id, false, &response);
                                        } else {
                                            self.selected = Some(p.id.clone());
                                        }
                                        table_ctx.request_repaint();
                                    }
                                    if response.double_clicked() && self.view != View::Storage {
                                        let assignments = data
                                            .purchases
                                            .iter()
                                            .find(|x| x.part.id == p.id)
                                            .map(|x| x.assignments.clone())
                                            .unwrap_or_default();
                                        self.edit((*p).clone(), purchase, false, assignments);
                                    }
                                });
                            }
                        });
                });
            if filtered.is_empty() {
                let empty_rect = egui::Rect::from_min_max(
                    table_rect.left_top() + egui::vec2(0., 32.),
                    table_rect.right_bottom(),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(empty_rect), |ui| {
                    theme::empty(
                        ui,
                        "No matching parts",
                        if parts.is_empty() {
                            "Add a component or import a CSV to begin."
                        } else {
                            "Try another search or clear the label filters."
                        },
                    );
                });
            }
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(pagebar), |ui| {
            theme::frame(theme::RAISED)
                .inner_margin(egui::Margin::symmetric(12, 5))
                .show(ui, |ui| {
                    ui.set_min_width((pagebar.width() - 24.).max(0.));
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} part{}",
                                filtered.len(),
                                if filtered.len() == 1 { "" } else { "s" }
                            ))
                            .color(GOLD),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.scope(|ui| {
                                ui.spacing_mut().combo_width = 54.;
                                egui::ComboBox::from_id_salt("page_size")
                                    .width(54.)
                                    .selected_text(self.page_size.to_string())
                                    .show_ui(ui, |ui| {
                                        for n in [25, 50, 100] {
                                            if ui
                                                .selectable_value(
                                                    &mut self.page_size,
                                                    n,
                                                    n.to_string(),
                                                )
                                                .changed()
                                            {
                                                self.page = 0;
                                            }
                                        }
                                    });
                            });
                            ui.label(
                                RichText::new(if filtered.is_empty() {
                                    "0 of 0".into()
                                } else {
                                    format!(
                                        "{}–{} of {}",
                                        self.page * self.page_size + 1,
                                        ((self.page + 1) * self.page_size).min(filtered.len()),
                                        filtered.len()
                                    )
                                })
                                .size(12.)
                                .color(theme::MUTED),
                            );
                            ui.add_enabled_ui(self.page + 1 < pages, |ui| {
                                if theme::tool_sized(
                                    ui,
                                    Icon::Last,
                                    "Last page",
                                    egui::vec2(18., 24.),
                                    false,
                                )
                                .clicked()
                                {
                                    self.page = pages - 1;
                                }
                                if theme::tool_sized(
                                    ui,
                                    Icon::Right,
                                    "Next page",
                                    egui::vec2(18., 24.),
                                    false,
                                )
                                .clicked()
                                {
                                    self.page += 1;
                                }
                            });
                            ui.label(
                                RichText::new(format!("Page {} of {}", self.page + 1, pages))
                                    .size(12.),
                            );
                            ui.add_enabled_ui(self.page > 0, |ui| {
                                if theme::tool_sized(
                                    ui,
                                    Icon::Left,
                                    "Previous page",
                                    egui::vec2(18., 24.),
                                    false,
                                )
                                .clicked()
                                {
                                    self.page -= 1;
                                }
                                if theme::tool_sized(
                                    ui,
                                    Icon::First,
                                    "First page",
                                    egui::vec2(18., 24.),
                                    false,
                                )
                                .clicked()
                                {
                                    self.page = 0;
                                }
                            });
                        });
                    });
                });
        });
    }
}

/// Shared with Projects so every part table keeps the same fields and order.
pub(super) const PART_COLUMNS: [(&str, f32); 10] = [
    ("Part Number", 160.),
    ("Manufacturer", 150.),
    ("Description", 220.),
    ("Package", 90.),
    ("Value", 80.),
    ("Tolerance", 85.),
    ("Stock Balance", 110.),
    ("Supplier", 100.),
    ("SPN", 120.),
    ("Storage", 120.),
];
pub(super) fn part_cells(row: &mut egui_extras::TableRow<'_, '_>, part: &Part) {
    row.col(|ui| {
        theme::part_number_cell(ui, part);
    });
    for value in [
        &part.manufacturer,
        &part.description,
        &part.package,
        &part.value,
        &part.tolerance,
    ] {
        row.col(|ui| {
            ui.add_space(7.);
            ui.add(egui::Label::new(RichText::new(value).color(theme::TEXT)).truncate())
                .on_hover_text(value);
        });
    }
    row.col(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(7.);
            ui.label(
                RichText::new(part.stock.to_string()).color(if part.stock > 0 {
                    theme::GREEN
                } else {
                    theme::MUTED
                }),
            );
        });
    });
    for (value, muted) in [
        (&part.supplier, false),
        (&part.spn, false),
        (&part.storage, true),
    ] {
        row.col(|ui| {
            ui.add_space(7.);
            ui.add(
                egui::Label::new(RichText::new(value).color(if muted {
                    theme::MUTED
                } else {
                    theme::TEXT
                }))
                .truncate(),
            )
            .on_hover_text(value);
        });
    }
}
