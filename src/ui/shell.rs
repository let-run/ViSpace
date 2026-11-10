use super::*;

impl InventoryApp {
    pub(super) fn header(&mut self, ctx: &egui::Context, data: &Inventory) {
        egui::TopBottomPanel::top("navigation")
            .exact_height(theme::TOOLBAR_HEIGHT)
            .frame(theme::frame(theme::BACKGROUND).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.x = 5.;
                ui.spacing_mut().interact_size.y = 26.;
                ui.spacing_mut().button_padding.y = 4.;
                ui.horizontal(|ui| {
                    // Register the title-bar background before the controls so buttons
                    // and text fields retain their own pointer interactions.
                    let drag = ui.interact(
                        ui.max_rect(),
                        ui.id().with("title_drag"),
                        egui::Sense::click_and_drag(),
                    );
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                    if cfg!(target_os = "macos") {
                        ui.add_space(72.); // Native close, minimize, and zoom buttons.
                    }
                    ui.add(
                        egui::Image::from_bytes(
                            "bytes://vinventory-title-logo.png",
                            include_bytes!("../../assets/vinventory-title-logo.png"),
                        )
                        .fit_to_exact_size(egui::vec2(28., 28.)),
                    )
                    .on_hover_text("Vinventory");
                    ui.add_space(6.);
                    if theme::tool_sized(
                        ui,
                        Icon::Add,
                        if self.view == View::Purchase {
                            "Add purchase (⌘N)"
                        } else {
                            "Add part (⌘N)"
                        },
                        egui::vec2(32., 26.),
                        false,
                    )
                    .clicked()
                    {
                        self.add();
                    }
                    ui.add_enabled_ui(self.selected.is_some(), |ui| {
                        if theme::tool_sized(
                            ui,
                            Icon::Edit,
                            "Edit selected part (⌘E)",
                            egui::vec2(32., 26.),
                            false,
                        )
                        .clicked()
                        {
                            self.edit_selected(data);
                        }
                        if theme::tool_sized(
                            ui,
                            Icon::Trash,
                            "Delete selected part",
                            egui::vec2(32., 26.),
                            false,
                        )
                        .clicked()
                        {
                            self.delete = self
                                .selected
                                .clone()
                                .map(|id| Delete::Part(id, self.view == View::Purchase));
                        }
                    });
                    let filter_available = matches!(self.view, View::Parts | View::Purchase);
                    ui.add_enabled_ui(filter_available, |ui| {
                        let active = if self.view == View::Purchase {
                            &mut self.purchase_labels
                        } else {
                            &mut self.labels
                        };
                        let response = theme::tool_sized(
                            ui,
                            Icon::Filter,
                            "Filter by labels",
                            egui::vec2(32., 26.),
                            filter_available && !active.is_empty(),
                        );
                        if filter_available && !active.is_empty() {
                            ui.painter().circle_filled(
                                response.rect.right_top() + egui::vec2(-4., 4.),
                                3.,
                                GOLD,
                            );
                        }
                        egui::Popup::from_toggle_button_response(&response)
                            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                            .frame(surfaces::popup_frame(theme::PANEL, 16))
                            .gap(8.)
                            .layout(egui::Layout::top_down(egui::Align::Min))
                            .show(|ui| {
                                let labels = if self.view == View::Purchase {
                                    data.purchases
                                        .iter()
                                        .flat_map(|p| p.part.labels.iter().cloned())
                                        .collect()
                                } else {
                                    data.parts
                                        .iter()
                                        .flat_map(|p| p.labels.iter().cloned())
                                        .collect()
                                };
                                if popovers::label_filter(ui, active, &mut self.label_query, labels)
                                {
                                    self.page = 0;
                                }
                            });
                    });
                    ui.add_space(6.);
                    ui.separator();
                    ui.add_space(6.);
                    for view in [View::Parts, View::Storage, View::Projects, View::Purchase] {
                        let selected = self.view == view;
                        let response = ui.add_sized(
                            [82., 26.],
                            egui::Button::new(
                                RichText::new(view_title(view)).strong().color(if selected {
                                    theme::TEXT
                                } else {
                                    theme::MUTED
                                }),
                            )
                            .fill(if selected {
                                theme::SELECTION
                            } else {
                                theme::BACKGROUND
                            })
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(7),
                        );
                        if response.clicked() {
                            self.switch_view(view);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(8.);
                        if theme::tool_sized(
                            ui,
                            Icon::Settings,
                            "Preferences",
                            egui::vec2(32., 26.),
                            false,
                        )
                        .clicked()
                        {
                            self.settings = true;
                        }
                        if theme::tool_sized(
                            ui,
                            Icon::Export,
                            "Export parts to CSV",
                            egui::vec2(32., 26.),
                            false,
                        )
                        .clicked()
                        {
                            let parts = if self.view == View::Purchase {
                                data.purchases.iter().map(|p| p.part.clone()).collect()
                            } else {
                                data.parts.clone()
                            };
                            self.export_csv(&parts);
                        }
                        if theme::tool_sized(
                            ui,
                            Icon::Import,
                            "Import parts from CSV",
                            egui::vec2(32., 26.),
                            false,
                        )
                        .clicked()
                        {
                            self.import_csv(ctx);
                        }
                        if theme::tool_sized(
                            ui,
                            Icon::Scan,
                            "Scan barcode with camera",
                            egui::vec2(32., 26.),
                            false,
                        )
                        .clicked()
                        {
                            self.start_scanner(ctx);
                        }
                        let cad_search =
                            self.view == View::Projects && (3..=5).contains(&self.project_tab);
                        let width = (ui.available_width() - 28.).clamp(100., 280.);
                        theme::frame(theme::RAISED)
                            .inner_margin(egui::Margin::symmetric(10, 3))
                            .show(ui, |ui| {
                                ui.spacing_mut().interact_size.y = 20.;
                                ui.set_width(width);
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        theme::icon(ui, Icon::Search, 16., theme::MUTED);
                                        let clear_width =
                                            if self.query.is_empty() { 0. } else { 22. };
                                        if cad_search {
                                            let search_width = ui.available_width();
                                            self.kicad.global_search(
                                                ui,
                                                search_width,
                                                self.catalog.as_deref().unwrap_or(""),
                                                self.project_tab,
                                            );
                                        } else if ui
                                            .add(
                                                egui::TextEdit::singleline(&mut self.query)
                                                    .frame(false)
                                                    .desired_width(width - 42. - clear_width)
                                                    .hint_text(match self.view {
                                                        View::Storage => "Search parts in storage…",
                                                        View::Projects => {
                                                            "Search parts or designators…"
                                                        }
                                                        _ => "Search parts, values, packages…",
                                                    }),
                                            )
                                            .changed()
                                        {
                                            self.page = 0;
                                        }
                                        if !cad_search
                                            && !self.query.is_empty()
                                            && theme::tool_sized(
                                                ui,
                                                Icon::Close,
                                                "Clear search",
                                                egui::vec2(16., 16.),
                                                false,
                                            )
                                            .clicked()
                                        {
                                            self.query.clear();
                                            self.page = 0;
                                        }
                                    },
                                );
                            });
                    });
                });
            });
    }

    pub(super) fn status_bar(&mut self, ctx: &egui::Context, data: &Inventory) {
        if let Ok(mut store) = self.store.try_lock() {
            self.local_head_hash = store.local_head_hash();
            self.network_node_count = store
                .network_graph()
                .nodes
                .iter()
                .filter(|node| node.reachable)
                .count()
                .max(1);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let pairing = self.store.try_lock().is_ok_and(|store| !store.network_pairing_ids().is_empty());
        #[cfg(target_arch = "wasm32")]
        let pairing = crate::web::inventory_pairing_pending();
        let receiving=crate::git_progress::observe(ctx);
        egui::TopBottomPanel::bottom("status")
            .exact_height(theme::FOOTER_HEIGHT)
            .frame(theme::frame(theme::BACKGROUND).inner_margin(egui::Margin::symmetric(14, 4)))
            .show(ctx, |ui| {
                let footer=ui.available_rect_before_wrap();
                let progress=receiving.filter(|_|footer.width()>=640.).map(|value|{
                    (egui::Rect::from_center_size(footer.center(),egui::vec2((footer.width()*0.18).clamp(140.,220.),18.)),value)
                });
                ui.allocate_ui_with_layout(
                    ui.available_size(),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        let count = self.network_node_count;
                        let status = ui
                            .horizontal(|ui| {
                                let (rect, _) = ui
                                    .allocate_exact_size(egui::vec2(6., 6.), egui::Sense::hover());
                                let color = if count > 1 {
                                    theme::GREEN
                                } else {
                                    Color32::GRAY
                                };
                                ui.painter().circle_filled(rect.center(), 3., color);
                                let label = if count == 1 {
                                    "1 NODE".into()
                                } else {
                                    format!("{count} NODES")
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(label).size(10.).strong().color(theme::MUTED),
                                    )
                                    .sense(egui::Sense::click()),
                                )
                            })
                            .inner
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("Show network status");
                        self.network_status_popup(&status);
                        let short: String = self.local_head_hash.chars().take(10).collect();
                        let (label, hint) = if short.is_empty() {
                            (
                                "HEAD —".into(),
                                "No applied operations\nOpen inventory history".into(),
                            )
                        } else {
                            (
                                format!("HEAD {short}"),
                                format!(
                                    "Applied HEAD\n{}\n\nOpen inventory history",
                                    self.local_head_hash
                                ),
                            )
                        };
                        let head = ui
                            .add(
                                egui::Label::new(
                                    RichText::new(label)
                                        .monospace()
                                        .size(10.)
                                        .color(theme::MUTED),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text(hint);
                        self.history_popup(&head);
                        if self.status != "Ready" {
                            let width=progress.as_ref().map_or(ui.available_width(),|(rect,_)|(rect.left()-ui.next_widget_position().x-8.).max(0.));
                            ui.add_sized(egui::vec2(width,18.),egui::Label::new(RichText::new(&self.status).size(11.).color(theme::MUTED)).truncate());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (rect, sync) = ui.allocate_exact_size(egui::vec2(58., 24.), egui::Sense::click());
                            let color = if pairing {
                                ctx.request_repaint_after(std::time::Duration::from_millis(33));
                                let pulse = ui.input(|input| (1. - (input.time * 3.).cos()) * 0.5) as f32;
                                GOLD.lerp_to_gamma(Color32::from_rgb(169, 124, 244), pulse)
                            } else if sync.hovered() {
                                theme::TEXT
                            } else {
                                GOLD
                            };
                            let icon_rect = egui::Rect::from_center_size(
                                egui::pos2(rect.left() + 9., rect.center().y),
                                egui::vec2(14., 14.),
                            );
                            theme::paint_icon(ui.painter(), Icon::Mesh, icon_rect, color);
                            #[cfg(any(target_os = "macos", target_os = "linux"))]
                            if pairing {
                                ui.data_mut(|data| data.insert_temp(egui::Id::new(super::sync_hdr::ICON_ID), (icon_rect, color)));
                            }
                            ui.painter().text(
                                egui::pos2(rect.left() + 22., rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                "Sync",
                                egui::FontId::proportional(12.),
                                color,
                            );
                            sync.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Sync"));
                            if sync.on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("Connect to inventory peers").clicked()
                            {
                                #[cfg(not(target_arch = "wasm32"))]
                                { self.sync.open = true; }
                                #[cfg(target_arch = "wasm32")]
                                crate::web::open_sync_dialog();
                            }
                            ui.label(
                                RichText::new(format!(
                                    "{}  •  {} parts",
                                    if cfg!(target_arch = "wasm32") {
                                        "Browser storage"
                                    } else {
                                        "SQLite"
                                    },
                                    data.parts.len()
                                ))
                                .size(11.)
                                .color(theme::MUTED),
                            );
                        });
                    },
                );
                if let Some((rect,value))=progress {
                    let fraction=if value.total>0{(value.received as f64/value.total as f64).clamp(0.,1.)as f32}else{0.};
                    let painter=ui.painter();painter.rect_filled(rect,4.,theme::RAISED);
                    let label=if value.applying>0 || value.creating>0 {
                        let time=ui.input(|input|input.time)as f32;
                        let width=rect.width()*0.32;let x=rect.left()-width+(time*0.8).fract()*(rect.width()+width);
                        painter.with_clip_rect(rect).rect_filled(egui::Rect::from_min_size(egui::pos2(x,rect.top()),egui::vec2(width,rect.height())),4.,theme::PURPLE);
                        ctx.request_repaint_after(std::time::Duration::from_millis(33));
                        if value.applying>0{"Applying Git…"}else{"Creating Git bundle…"}.to_owned()
                    }else{
                        if fraction>0.{painter.rect_filled(egui::Rect::from_min_size(rect.min,egui::vec2(rect.width()*fraction,rect.height())),4.,theme::PURPLE);}
                        format!("Receiving Git{} · {:.0}%",if value.receiving>1{format!(" ({})",value.receiving)}else{String::new()},fraction*100.)
                    };
                    painter.text(rect.center(),egui::Align2::CENTER_CENTER,label,egui::FontId::proportional(10.),theme::YELLOW);
                    let help=if value.applying>0{"Processing and applying received Git bundles".to_owned()}else if value.creating>0{"Creating Git bundles for transfer".to_owned()}else{format!("Receiving Git bundles: {:.1} / {:.1} MiB",value.received as f64/1048576.,value.total as f64/1048576.)};
                    ui.interact(rect,ui.id().with("git_receiving_progress"),egui::Sense::hover()).on_hover_text(help);
                }
            });
    }
}
fn view_title(view: View) -> &'static str {
    match view {
        View::Parts => "PARTS",
        View::Storage => "STORAGE",
        View::Projects => "PROJECTS",
        View::Purchase => "PURCHASE",
    }
}
