use super::*;

impl InventoryApp {
    pub(super) fn detail(&mut self, ctx: &egui::Context, data: &Inventory) {
        let purchase = self.view == View::Purchase;
        let part = self
            .selected
            .as_ref()
            .and_then(|id| {
                if purchase {
                    data.purchases
                        .iter()
                        .find(|p| &p.part.id == id)
                        .map(|p| &p.part)
                } else {
                    data.parts.iter().find(|p| &p.id == id)
                }
            })
            .cloned();
        let default_width = if ctx.content_rect().width() < 1150. {
            380.
        } else {
            510.
        };
        let old_layout = (self.detail_width, self.summary_height);
        let panel = egui::SidePanel::right("details").resizable(true).default_width(self.detail_width.unwrap_or(default_width)).min_width(330.).max_width(680.)
            .frame(theme::frame(theme::PANEL))
            .show(ctx, |ui| {
                // Content must wrap/clip inside the chosen pane, never grow its frame.
                let bounds = ui.max_rect();
                ui.set_min_size(bounds.size());
                let mut content = ui.new_child(egui::UiBuilder::new().max_rect(bounds));
                content.set_clip_rect(bounds);
                let ui = &mut content;
                let Some(p)=part else {
                    theme::empty(ui, if data.parts.is_empty() {"Your inventory starts here"} else {"Select a part"},
                        if data.parts.is_empty() {"Add your first component to track stock, storage, and project allocations."} else {"Select a row to view its details and manage inventory."});
                    return;
                };
                let allocations = if purchase {data.purchases.iter().find(|x|x.part.id==p.id).map(|p|p.assignments.iter().map(|a|a.quantity).sum::<i64>()).unwrap_or(0)} else {data.allocated(&p.id)};
                let available = p.stock-allocations;
                let projects = if purchase {data.purchases.iter().filter(|x|x.part.id==p.id).flat_map(|x|x.assignments.iter().map(|a|key(&a.project))).collect::<BTreeSet<_>>().len()}
                    else {data.allocations.iter().filter(|a|a.part_id==p.id).map(|a|key(&a.project)).collect::<BTreeSet<_>>().len()};
                let max_height = (ui.available_height() - 240.).max(0.);
                let summary = egui::TopBottomPanel::top("part_summary").resizable(true).default_height(self.summary_height.unwrap_or(390.)).height_range(230_f32.min(max_height)..=max_height)
                    .frame(theme::frame(theme::PANEL)).show_inside(ui, |ui| {
                        let bounds = ui.max_rect();
                        ui.set_min_size(bounds.size());
                        let mut summary_content = ui.new_child(egui::UiBuilder::new().max_rect(bounds));
                        summary_content.set_clip_rect(bounds.intersect(ui.clip_rect()));
                        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("part_summary_scroll", purchase)).show(&mut summary_content, |ui| {
                            self.part_summary(ui,data,&p,purchase,allocations,projects);

                        });
                    });
                self.summary_height = Some(summary.response.rect.height());
                egui::TopBottomPanel::top("inventory_tabs").exact_height(46.).frame(theme::frame(theme::RAISED)).show_inside(ui, |ui| {
                    ui.spacing_mut().item_spacing.x=0.;
                    ui.horizontal_centered(|ui| {
                        for (index, title) in [(0, "Inventory"), (1, "Project allocations")] {
                            let selected = self.detail_tab == index;
                            let width = theme::measure_text(ui, title, 13., 1).x + 32.;
                            let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 46.), egui::Sense::click());
                            ui.painter().rect_filled(rect, 0., if selected { theme::PURPLE } else { Color32::TRANSPARENT });
                            theme::paint_text(ui, title, 13., 1, if selected { Color32::WHITE } else { GOLD }, egui::pos2(rect.left() + 16., rect.center().y));
                            if response.clicked() {
                                self.detail_tab = index;
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(12.);
                            let inventory = self.detail_tab == 0;
                            let tooltip = if inventory { "Manage physical stock" } else { "Reserve stock for a project" };
                            let icon = if inventory { Icon::Edit } else { Icon::Add };
                            if theme::tool(ui, icon, tooltip).clicked() {
                                if inventory {
                                    self.stock_editor = Some(StockEditor {
                                        part_id: p.id.clone(),
                                        purchase,
                                        quantity: p.stock,
                                        original_quantity: p.stock,
                                        error: None,
                                    });
                                } else {
                                    self.open_assignment(&p, purchase);
                                }
                            }
                        });
                    });
                });
                egui::TopBottomPanel::bottom("inventory_hint").exact_height(38.).frame(theme::frame(theme::RAISED).inner_margin(12)).show_inside(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(if self.detail_tab==0 {"Stock is shared across all project allocations."} else {"Releasing an allocation returns it to available stock."}).size(11.).color(theme::MUTED)).truncate());
                });
                egui::CentralPanel::default().frame(theme::frame(theme::ROW)).show_inside(ui, |ui| {
                    let bounds = ui.max_rect().intersect(ui.clip_rect());
                    let mut table = ui.new_child(egui::UiBuilder::new().max_rect(bounds));
                    table.set_clip_rect(bounds);
                    let ui = &mut table;
                    if self.detail_tab==0 {
                        egui::ScrollArea::vertical()
                            .id_salt(("stock_rows", purchase))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y=1.;
                                stock_row(ui,"STATUS","QTY","STORAGE",theme::MUTED,true);
                                stock_row(ui,"Available",&available.to_string(),&p.storage,theme::GREEN,false);
                                stock_row(ui,"Allocated",&allocations.to_string(),&format!("{} reservations",if purchase {data.purchases.iter().filter(|x|x.part.id==p.id).map(|x|x.assignments.len()).sum::<usize>()}else{data.allocations.iter().filter(|a|a.part_id==p.id).count()}),theme::TEXT,false);
                                stock_row(ui,if purchase{"To purchase"}else{"Stock"},&p.stock.to_string(),&p.storage,theme::TEXT,false);
                            });
                    }else{
                        stock_row(ui,"PROJECT","QTY","",theme::MUTED,true);
                        egui::ScrollArea::vertical().id_salt(("allocation_rows", purchase)).show(ui, |ui| {
                            let entries:Vec<_>=if purchase {data.purchases.iter().filter(|x|x.part.id==p.id).flat_map(|x|x.assignments.iter().map(|a|(a.id.clone(),a.project.clone(),a.quantity,a.designators.clone()))).collect()}
                                else {data.allocations.iter().filter(|a|a.part_id==p.id).map(|a|(a.id.clone(),a.project.clone(),a.quantity,a.designators.clone())).collect()};
                            if entries.is_empty(){ui.add_space(24.);ui.centered_and_justified(|ui|{ui.label(RichText::new("No project allocations").color(theme::MUTED));});}
                            for (id,project,quantity,refs) in entries {
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 60.), egui::Sense::hover());
                                ui.painter().rect_filled(rect, 0., theme::PANEL);
                                let project_rect = egui::Rect::from_min_max(rect.min + egui::vec2(14., 12.), egui::pos2(rect.right()-160., rect.bottom()-8.));
                                let mut name_ui = ui.new_child(egui::UiBuilder::new().max_rect(project_rect));
                                name_ui.set_clip_rect(project_rect.intersect(ui.clip_rect()));
                                name_ui.spacing_mut().item_spacing.y = 3.;
                                if name_ui.add(egui::Label::new(RichText::new(&project).size(13.).color(theme::TEXT)).truncate().sense(egui::Sense::click())).clicked() {
                                    self.view=View::Projects;self.catalog=Some(project.clone());self.page=0;
                                }
                                if !refs.is_empty() { name_ui.add(egui::Label::new(RichText::new(&refs).size(11.).color(theme::MUTED)).truncate()); }
                                let controls = egui::Rect::from_min_max(egui::pos2(rect.right()-150.,rect.top()),rect.max-egui::vec2(14.,0.));
                                let mut actions = ui.new_child(egui::UiBuilder::new().max_rect(controls).layout(egui::Layout::right_to_left(egui::Align::Center)));
                                if actions.add(egui::Button::new(if purchase {"Remove"} else {"Release"}).frame(false)).clicked() {
                                    if purchase {let pid=p.id.clone();self.change("Assignment removed",|d|{if let Some(p)=d.purchases.iter_mut().find(|p|p.part.id==pid){p.assignments.retain(|a|a.id!=id);}Ok(())});}
                                    else {self.delete=Some(Delete::Allocation(id.clone()));}
                                }
                                actions.add_space(10.);
                                if theme::tool_sized(&mut actions,Icon::Edit,"Edit quantity and designators",egui::vec2(24.,24.),false).clicked() {
                                    self.allocation=Some(AllocationEditor{error:None,existing:Some(id),part_id:p.id.clone(),purchase,project,quantity,designators:refs});
                                }
                                actions.add_space(10.);
                                actions.label(RichText::new(quantity.to_string()).monospace().size(13.));
                                ui.painter().hline(rect.x_range(),rect.bottom(),egui::Stroke::new(1.,theme::LINE));
                            }
                        });
                    }
                });
            });
        self.detail_width = Some(panel.response.rect.width());
        self.layout_dirty |= old_layout != (self.detail_width, self.summary_height);
        if self.layout_dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.save_detail_layout();
            self.layout_dirty = false;
        }
    }
    pub(super) fn part_summary(
        &mut self,
        ui: &mut egui::Ui,
        data: &Inventory,
        p: &Part,
        purchase: bool,
        allocations: i64,
        projects: usize,
    ) {
        let available = p.stock - allocations;
        theme::frame(theme::PANEL).inner_margin(24).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 18.;
            self.summary_heading(ui, p);
            theme::text(
                ui,
                if p.description.is_empty() {
                    "No description"
                } else {
                    &p.description
                },
                15.,
                0,
                theme::TEXT,
            );
            ui.horizontal(|ui| {
                let text = [
                    p.value.clone(),
                    if p.tolerance.is_empty() {
                        String::new()
                    } else {
                        format!("Tolerance {}", p.tolerance)
                    },
                    p.package.clone(),
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("  •  ");
                let reserved = 38.
                    + theme::measure_text(ui, "Datasheet", 13., 0).x
                    + 2. * ui.spacing().button_padding.x
                    + 2. * ui.spacing().item_spacing.x
                    + 4.;
                let width = (ui.available_width() - reserved).max(20.);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 32.),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(width);
                        theme::text(ui, &text, 13., 0, theme::MUTED);
                    },
                );
                let barcode_response = theme::tool(ui, Icon::Qr, "Show barcode label");
                self.barcode_popover(&barcode_response, p);
                ui.add_enabled_ui(!p.datasheet.is_empty(), |ui| {
                    if ui
                        .add(egui::Button::new("Datasheet").wrap_mode(egui::TextWrapMode::Extend))
                        .clicked()
                    {
                        self.report(crate::platform::open_link(&p.datasheet).map_err(Into::into));
                    }
                });
            });
            if !p.spn.is_empty() {
                let supplier = p
                    .supplier
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase();
                let reference = match supplier.as_str() {
                    "digikey" => Some((
                        "DigiKey",
                        "https://www.digikey.com/en/products/result?keywords=",
                    )),
                    "mouser" => Some(("Mouser", "https://www.mouser.com/c/?q=")),
                    "lcsc" => Some(("LCSC", "https://www.lcsc.com/search?q=")),
                    "newark" => Some(("Newark", "https://www.newark.com/search?st=")),
                    _ => None,
                };
                if let Some((name, base)) = reference {
                    if theme::plain_action(ui, Icon::External, &format!("{name}  {}", p.spn))
                        .clicked()
                    {
                        let query = url::form_urlencoded::byte_serialize(p.spn.trim().as_bytes())
                            .collect::<String>();
                        self.report(
                            crate::platform::open_link(format!("{base}{query}"))
                                .map_err(Into::into),
                        );
                    }
                } else {
                    theme::text(
                        ui,
                        &format!("{}  {}", p.supplier, p.spn),
                        12.,
                        0,
                        theme::TEXT,
                    );
                }
            }
            if !p.custom_fields.is_empty() {
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 5.;
                    for (name, value) in &p.custom_fields {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(140., 16.),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    theme::text(ui, name, 12., 0, theme::MUTED);
                                },
                            );
                            theme::text(ui, value, 12., 0, theme::TEXT);
                        });
                    }
                });
            }
            ui.horizontal(|ui| {
                let width = ((ui.available_width() - 24.) / 4.).max(50.);
                for (title, value, color) in [
                    (
                        if purchase { "To purchase" } else { "Stock" },
                        p.stock,
                        theme::TEXT,
                    ),
                    ("Available", available, theme::GREEN),
                    ("Allocated", allocations, theme::TEXT),
                    ("Projects", projects as i64, theme::TEXT),
                ] {
                    theme::frame(theme::SELECTION.gamma_multiply(0.65))
                        .corner_radius(3)
                        .inner_margin(11)
                        .show(ui, |ui| {
                            ui.set_width((width - 22.).max(28.));
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 6.;
                                theme::text(ui, title, 12., 0, theme::MUTED);
                                theme::text(ui, &value.to_string(), 20., 1, color);
                            });
                        });
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.;
                theme::icon(ui, Icon::Box, 12., theme::MUTED);
                let reserved = 210.;
                ui.allocate_ui_with_layout(
                    egui::vec2(
                        (ui.available_width() - reserved - 2. * ui.spacing().item_spacing.x)
                            .max(30.),
                        22.,
                    ),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        if p.storage.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new("No storage assigned")
                                        .size(12.)
                                        .color(theme::MUTED),
                                )
                                .truncate(),
                            );
                        } else if ui
                            .add(
                                egui::Label::new(RichText::new(&p.storage).size(12.).color(GOLD))
                                    .truncate()
                                    .sense(egui::Sense::click()),
                            )
                            .clicked()
                        {
                            ui.close_kind(egui::UiKind::Popup);
                            self.switch_view(View::Storage);
                            self.catalog = Some(p.storage.clone());
                            self.page = 0;
                        }
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::plain_action(ui, Icon::Edit, "Edit details").clicked() {
                        let assignments = if purchase {
                            data.purchases
                                .iter()
                                .find(|item| item.part.id == p.id)
                                .map(|item| item.assignments.clone())
                                .unwrap_or_default()
                        } else {
                            vec![]
                        };
                        self.edit(p.clone(), purchase, false, assignments);
                    }
                    if theme::plain_action(
                        ui,
                        Icon::Notes,
                        if p.notes.text.is_empty() && p.notes.rtf.is_empty() {
                            "Add notes"
                        } else {
                            "Notes"
                        },
                    )
                    .clicked()
                    {
                        self.notes_editor = Some(NotesEditor::new(
                            if purchase {
                                NotesTarget::Purchase(p.id.clone())
                            } else {
                                NotesTarget::Part(p.id.clone())
                            },
                            format!("Notes — {}", p.number),
                            p.notes.clone(),
                        ));
                    }
                });
            });
        });
    }
    fn summary_heading(&mut self, ui: &mut egui::Ui, part: &Part) {
        let width = ui.available_width();
        let top = ui.cursor().min;
        let image_rect =
            egui::Rect::from_min_size(egui::pos2(top.x + width - 70., top.y), egui::vec2(70., 70.));
        let text_rect = egui::Rect::from_min_size(top, egui::vec2((width - 88.).max(90.), 70.));
        let mut text = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        text.set_width(text_rect.width());
        text.spacing_mut().item_spacing.y = 7.;
        theme::text(
            &mut text,
            if part.manufacturer.is_empty() {
                "Manufacturer unspecified"
            } else {
                &part.manufacturer
            },
            16.,
            1,
            theme::MUTED,
        );
        theme::text(&mut text, &part.number, 26., 2, theme::TEXT);
        text.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6., 6.);
            for label in &part.labels {
                theme::frame(theme::PURPLE)
                    .corner_radius(3)
                    .inner_margin(egui::Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        theme::text(ui, label, 12., 1, theme::TEXT);
                    });
            }
        });
        let height = text.min_rect().height().max(70.);
        let image_path = part
            .image
            .as_ref()
            .and_then(|v| v.get("thumbnailPath").or_else(|| v.get("originalPath")))
            .and_then(|v| v.as_str());
        let mut tile = ui.new_child(egui::UiBuilder::new().max_rect(image_rect));
        let response = theme::image_tile(&mut tile, image_rect, image_path, Icon::Cpu);
        if let Some(path) = image_path {
            if response.clicked() {
                let original = part
                    .image
                    .as_ref()
                    .and_then(|v| v.get("originalPath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(path);
                self.report(crate::platform::open_link(original).map_err(Into::into));
            }
            let preview = part
                .image
                .as_ref()
                .and_then(|v| v.get("previewPath").filter(|v| v.is_string()))
                .and_then(|v| v.as_str())
                .unwrap_or(path);
            egui::Popup::from_response(&response)
                .open(response.hovered())
                .align(egui::RectAlign::LEFT_START)
                .gap(8.)
                .frame(surfaces::popup_frame(theme::PANEL, 12))
                .layout(egui::Layout::top_down(egui::Align::Center))
                .show(|ui| {
                    ui.set_width(360.);
                    let (slot, _) =
                        ui.allocate_exact_size(egui::vec2(360., 300.), egui::Sense::hover());
                    ui.put(
                        slot,
                        crate::platform::image_widget(preview)
                            .fit_to_exact_size(slot.size())
                            .maintain_aspect_ratio(true),
                    );
                    ui.label(RichText::new(&part.number).size(13.).color(theme::TEXT));
                });
        }
        ui.allocate_space(egui::vec2(width, height));
    }
    fn open_assignment(&mut self, p: &Part, purchase: bool) {
        self.allocation = Some(AllocationEditor {
            error: None,
            existing: None,
            part_id: p.id.clone(),
            purchase,
            project: self
                .catalog
                .clone()
                .filter(|_| self.view == View::Projects)
                .unwrap_or_default(),
            quantity: 1,
            designators: String::new(),
        });
    }
}
fn stock_row(
    ui: &mut egui::Ui,
    title: &str,
    quantity: &str,
    storage: &str,
    color: Color32,
    header: bool,
) {
    let height = if header { 34. } else { 42. };
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(
        rect,
        0.,
        if header {
            theme::RAISED
        } else {
            theme::PANEL.gamma_multiply(0.65)
        },
    );
    let column_width = (rect.width() - 32. - 60. - 20.) / 2.;
    let label_right = rect.left() + 16. + column_width;
    let quantity_right = label_right + 60.;
    let storage_left = quantity_right + 28.;
    let columns = [
        (
            title,
            rect.left() + 16.,
            label_right,
            color,
            egui::Align2::LEFT_CENTER,
        ),
        (
            quantity,
            label_right,
            quantity_right,
            theme::TEXT,
            egui::Align2::RIGHT_CENTER,
        ),
        (
            storage,
            storage_left,
            rect.right() - 16.,
            theme::MUTED,
            egui::Align2::LEFT_CENTER,
        ),
    ];
    for (text, left, right, color, align) in columns {
        let clip = egui::Rect::from_min_max(
            egui::pos2(left, rect.top()),
            egui::pos2(right, rect.bottom()),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(clip)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child.set_clip_rect(clip);
        if align == egui::Align2::RIGHT_CENTER {
            child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                theme::text(
                    ui,
                    text,
                    if header { 11. } else { 13. },
                    if header { 1 } else { 0 },
                    color,
                );
            });
        } else {
            theme::text(
                &mut child,
                text,
                if header { 11. } else { 13. },
                if header { 1 } else { 0 },
                color,
            );
        }
    }
}
