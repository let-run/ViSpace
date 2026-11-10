use super::*;

pub(super) fn sheet(id: &str) -> egui::Modal {
    sheet_with_margin(id, 26.into())
}

pub(super) fn sheet_with_margin(id: &str, margin: egui::Margin) -> egui::Modal {
    egui::Modal::new(egui::Id::new(id))
        .backdrop_color(Color32::from_black_alpha(105))
        .frame(
            theme::frame(theme::PANEL)
                .inner_margin(margin)
                .corner_radius(10)
                .stroke(egui::Stroke::new(1., theme::LINE))
                .shadow(egui::Shadow {
                    offset: [0, 10],
                    blur: 32,
                    spread: 1,
                    color: Color32::from_black_alpha(110),
                }),
        )
}
pub(super) fn configure(ui: &mut egui::Ui, width: f32) {
    ui.set_width(width);
    ui.spacing_mut().item_spacing = egui::vec2(10., 12.);
    let visuals = ui.visuals_mut();
    visuals.override_text_color = Some(theme::TEXT);
    visuals.text_edit_bg_color = Some(theme::RAISED);
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1., Color32::from_rgb(65, 65, 72));
    visuals.widgets.inactive.corner_radius = 4.into();
    visuals.widgets.hovered.corner_radius = 4.into();
    visuals.widgets.active.corner_radius = 4.into();
}
pub(super) fn heading(ui: &mut egui::Ui, title: &str, icon: Icon) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(22.).strong().color(theme::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            theme::icon(ui, icon, 20., GOLD);
        });
    });
    ui.add_space(8.);
}
pub(super) fn field(ui: &mut egui::Ui, value: &mut String) -> egui::Response {
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .text_color(theme::TEXT)
            .margin(egui::vec2(7., 5.)),
    )
}
pub(super) fn row<R>(ui: &mut egui::Ui, label: &str, draw: impl FnOnce(&mut egui::Ui) -> R) -> R {
    row_with_label(
        ui,
        |ui| {
            ui.label(RichText::new(label).color(theme::TEXT));
        },
        draw,
    )
}
pub(super) fn row_with_label<R>(
    ui: &mut egui::Ui,
    label: impl FnOnce(&mut egui::Ui),
    draw: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let width = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(138., 24.),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(138.);
                label(ui);
            },
        );
        ui.vertical(|ui| {
            ui.set_width((width - 148.).max(80.));
            draw(ui)
        })
        .inner
    })
    .inner
}
pub(super) fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(10.).color(theme::MUTED));
}
pub(super) fn error(ui: &mut egui::Ui, error: &Option<String>) {
    if let Some(error) = error {
        ui.label(RichText::new(error).color(Color32::from_rgb(245, 121, 125)));
    }
}
pub(super) fn actions(ui: &mut egui::Ui, title: &str, enabled: bool) -> (bool, bool) {
    ui.add_space(8.);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let save = ui
            .add_enabled(
                enabled,
                egui::Button::new(RichText::new(title).color(Color32::WHITE))
                    .fill(theme::PURPLE)
                    .corner_radius(5)
                    .min_size(egui::vec2(90., 28.)),
            )
            .clicked();
        let cancel = ui
            .add(
                egui::Button::new(RichText::new("Cancel").color(theme::TEXT))
                    .fill(theme::RAISED)
                    .corner_radius(5)
                    .min_size(egui::vec2(75., 28.)),
            )
            .clicked();
        (save, cancel)
    })
    .inner
}
pub(super) fn escape<R>(response: &egui::ModalResponse<R>) -> bool {
    // A Swift sheet ignores outside clicks. Escape first dismisses a child popover.
    response.is_top_modal
        && !response.any_popup_open
        && response
            .response
            .ctx
            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
}
pub(super) fn popup_frame(fill: Color32, padding: i8) -> egui::Frame {
    theme::frame(fill)
        .inner_margin(padding)
        .corner_radius(8)
        .stroke(egui::Stroke::new(1., theme::LINE))
        .shadow(egui::Shadow {
            offset: [0, 5],
            blur: 20,
            spread: 0,
            color: Color32::from_black_alpha(90),
        })
}
pub(super) fn suggestion(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    value: &mut String,
    suggestions: impl IntoIterator<Item = String>,
) -> egui::Response {
    let (response, choice) = suggestion_choice(ui, id, value, suggestions);
    if let Some(choice) = choice {
        *value = choice;
    }
    response
}
/// Selection is separate from the text draft so chip inputs can commit it immediately.
pub(super) fn suggestion_choice(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    value: &mut String,
    suggestions: impl IntoIterator<Item = String>,
) -> (egui::Response, Option<String>) {
    let response = field(ui, value);
    let values = suggestions
        .into_iter()
        .filter(|s| value.is_empty() || key(s).contains(&key(value)))
        .collect::<BTreeSet<_>>();
    let mut chosen = None;
    egui::Popup::from_response(&response)
        .id(egui::Id::new(id))
        .layout(egui::Layout::top_down(egui::Align::Min))
        .open_memory(if values.is_empty() {
            Some(egui::SetOpenCommand::Bool(false))
        } else {
            (response.clicked() || response.changed()).then_some(egui::SetOpenCommand::Bool(true))
        })
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .frame(popup_frame(theme::RAISED, 5))
        .width(response.rect.width())
        .show(|ui| {
            ui.set_width((response.rect.width() - 10.).max(100.));
            egui::ScrollArea::vertical()
                .max_height(260.)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.;
                    for choice in values.into_iter().take(10) {
                        if ui
                            .add_sized(
                                egui::vec2(ui.available_width(), 27.),
                                egui::Button::new((
                                    RichText::new(&choice).color(theme::TEXT),
                                    egui::Atom::grow(),
                                ))
                                .frame(false)
                                .wrap_mode(egui::TextWrapMode::Truncate),
                            )
                            .clicked()
                        {
                            chosen = Some(choice);
                            ui.close();
                        }
                    }
                });
        });
    (response, chosen)
}

pub(super) fn chips(ui: &mut egui::Ui, labels: &mut Vec<String>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6., 6.);
        let mut remove = None;
        for (i, label) in labels.iter().enumerate() {
            theme::frame(theme::PURPLE)
                .corner_radius(3)
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).size(12.).color(theme::TEXT));
                        if ui
                            .add(
                                egui::Button::new(RichText::new("×").size(12.).color(theme::TEXT))
                                    .frame(false)
                                    .min_size(egui::vec2(12., 16.)),
                            )
                            .on_hover_text(format!("Remove {label}"))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
        }
        if let Some(i) = remove {
            labels.remove(i);
        }
    });
}
