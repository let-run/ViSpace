use super::*;
use surfaces as form;

impl InventoryApp {
    pub(super) fn barcode_popover(&mut self, response: &egui::Response, part: &Part) {
        egui::Popup::from_toggle_button_response(response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .frame(form::popup_frame(theme::PURPLE, 14))
            .gap(8.)
            .layout(egui::Layout::top_down(egui::Align::Min))
            .show(|ui| {
                ui.set_width(400.);
                let label_key = egui::Id::new((
                    "print-label",
                    serde_json::to_string(part).unwrap_or_default(),
                ));
                let cached = ui.ctx().data_mut(|d| d.get_temp::<Vec<u8>>(label_key));
                let png = cached
                    .map(Ok)
                    .unwrap_or_else(|| crate::label::render(part, false));
                if let Ok(png) = &png {
                    ui.ctx().data_mut(|d| d.insert_temp(label_key, png.clone()));
                }
                ui.spacing_mut().item_spacing.x = 12.;
                let visuals = ui.visuals_mut();
                for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered] {
                    widget.bg_fill = Color32::from_rgb(135, 119, 179);
                    widget.weak_bg_fill = Color32::from_rgb(135, 119, 179);
                    widget.corner_radius = 5.into();
                }
                match png {
                    Ok(png) => {
                        ui.add(
                            egui::Image::from_bytes(
                                format!(
                                    "bytes://label-{}-{}.png",
                                    part.id,
                                    egui::util::hash(
                                        serde_json::to_string(part).unwrap_or_default()
                                    )
                                ),
                                png,
                            )
                            .fit_to_exact_size(egui::vec2(400., 240.))
                            .texture_options(egui::TextureOptions::NEAREST),
                        );
                        ui.add_space(8.);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("50 × 30 mm • 300 dpi")
                                    .size(13.)
                                    .color(Color32::WHITE),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let copied_id = egui::Id::new(("label_copied", &part.id));
                                    let copied = ui
                                        .ctx()
                                        .data_mut(|d| d.get_temp::<bool>(copied_id))
                                        .unwrap_or(false);
                                    if theme::label_tool(
                                        ui,
                                        if copied { Icon::Check } else { Icon::Clipboard },
                                        "Copy label image",
                                    )
                                    .clicked()
                                    {
                                        match (|| -> Result<()> {
                                            if self.clipboard.is_none() {
                                                self.clipboard = Some(
                                                    crate::platform::clipboard::Clipboard::new()?,
                                                );
                                            }
                                            crate::label::copy(
                                                part,
                                                self.clipboard.as_mut().unwrap(),
                                            )
                                        })() {
                                            Ok(()) => {
                                                self.popover_error = None;
                                                ui.ctx()
                                                    .data_mut(|d| d.insert_temp(copied_id, true));
                                            }
                                            Err(error) => {
                                                self.popover_error = Some(error.to_string())
                                            }
                                        }
                                    }
                                    for (title, pdf) in [("PDF", true), ("PNG", false)] {
                                        if !pdf
                                            && theme::label_tool(
                                                ui,
                                                Icon::Printer,
                                                "Print on Niimbot B1",
                                            )
                                            .clicked()
                                        {
                                            let id = egui::Id::new(("niimbot_options", &part.id));
                                            ui.ctx().data_mut(|d| {
                                                let open = d.get_temp::<bool>(id).unwrap_or(false);
                                                d.insert_temp(id, !open);
                                            });
                                        }
                                        if ui
                                            .button(RichText::new(title).color(Color32::WHITE))
                                            .clicked()
                                            && let Some(path) =
                                                crate::platform::dialogs::FileDialog::new()
                                                    .add_filter(
                                                        title,
                                                        &[if pdf { "pdf" } else { "png" }],
                                                    )
                                                    .set_file_name(format!(
                                                        "{}-label.{}",
                                                        part.number.replace('/', "-"),
                                                        if pdf { "pdf" } else { "png" }
                                                    ))
                                                    .save_file()
                                        {
                                            let result =
                                                crate::label::render(part, pdf).and_then(|data| {
                                                    Ok(crate::platform::fs::write(path, data)?)
                                                });
                                            if let Err(error) = result {
                                                self.popover_error = Some(error.to_string());
                                            }
                                        }
                                    }
                                },
                            );
                        });
                    }
                    Err(error) => {
                        ui.label(error.to_string());
                    }
                }
                if ui
                    .ctx()
                    .data_mut(|d| d.get_temp::<bool>(egui::Id::new(("niimbot_options", &part.id))))
                    .unwrap_or(false)
                {
                    ui.add_space(8.);
                    self.printer.controls(ui, part);
                }
                form::error(ui, &self.popover_error);
            });
    }
}

pub(super) fn label_filter(
    ui: &mut egui::Ui,
    active: &mut BTreeSet<String>,
    query: &mut String,
    labels: BTreeSet<String>,
) -> bool {
    let original = active.clone();
    form::configure(ui, 288.);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Filter by labels").size(15.).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    !active.is_empty(),
                    egui::Button::new(RichText::new("Clear all").color(GOLD)).frame(false),
                )
                .clicked()
            {
                active.clear();
            }
        });
    });
    ui.label(
        RichText::new("Show parts matching any selected label.")
            .size(11.)
            .color(theme::MUTED),
    );
    if !active.is_empty() {
        let mut chips = active.iter().cloned().collect();
        form::chips(ui, &mut chips);
        *active = chips.into_iter().collect();
    }
    ui.add(
        egui::TextEdit::singleline(query)
            .hint_text("Search labels…")
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(7., 5.)),
    );
    egui::ScrollArea::vertical()
        .id_salt("label_filter_scroll")
        .min_scrolled_height(220.)
        .max_height(220.)
        .show(ui, |ui| {
            let mut count = 0;
            for label in labels.into_iter().filter(|s| key(s).contains(&key(query))) {
                count += 1;
                let mut checked = active.contains(&label);
                if ui
                    .checkbox(&mut checked, RichText::new(&label).color(theme::TEXT))
                    .changed()
                {
                    if checked {
                        active.insert(label);
                    } else {
                        active.remove(&label);
                    }
                }
                ui.add_space(6.);
            }
            if count == 0 {
                ui.label(RichText::new("No labels found").color(theme::MUTED));
            }
        });
    original != *active
}
