use super::*;

pub(super) struct PartPreview {
    part_id: String,
    purchase: bool,
    anchor: egui::Rect,
    just_opened: bool,
    layer: egui::LayerId,
}
impl PartPreview {
    pub(super) fn resume(&mut self) {
        self.just_opened = true;
    }
}
impl InventoryApp {
    pub(super) fn open_part_preview(
        &mut self,
        part_id: &str,
        purchase: bool,
        response: &egui::Response,
    ) {
        self.part_preview = Some(PartPreview {
            part_id: part_id.into(),
            purchase,
            anchor: response.rect,
            just_opened: true,
            layer: response.layer_id,
        });
        response.ctx.request_repaint();
    }
    pub(super) fn is_previewing(&self, id: &str, purchase: bool) -> bool {
        self.part_preview
            .as_ref()
            .is_some_and(|p| p.part_id == id && p.purchase == purchase)
    }
    pub(super) fn open_project_notes(&mut self, project: &Project) {
        self.notes_editor = Some(NotesEditor::new(
            NotesTarget::Project(project.name.clone()),
            format!("Notes — {}", project.name),
            project.notes.clone(),
        ));
    }
    pub(super) fn part_preview_popup(&mut self, ctx: &egui::Context, data: &Inventory) {
        let Some(mut preview) = self.part_preview.take() else {
            return;
        };
        let part = if preview.purchase {
            data.purchases
                .iter()
                .find(|p| p.part.id == preview.part_id)
                .map(|p| &p.part)
        } else {
            data.parts.iter().find(|p| p.id == preview.part_id)
        };
        let mut open = true;
        let previous_view = self.view;
        let previous_catalog = self.catalog.clone();
        let width = 560_f32.min((ctx.content_rect().width() - 40.).max(280.));
        let height = 420_f32.min((ctx.content_rect().height() - 60.).max(240.));
        let response = egui::Popup::new(
            egui::Id::new("part_preview"),
            ctx.clone(),
            preview.anchor,
            preview.layer,
        )
        .open_bool(&mut open)
        .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
        .align(egui::RectAlign::BOTTOM_START)
        .gap(5.)
        .width(width)
        .frame(
            theme::frame(theme::PANEL)
                .corner_radius(6)
                .stroke(egui::Stroke::new(1., theme::LINE)),
        )
        .layout(egui::Layout::top_down(egui::Align::Min))
        .show(|ui| {
            ui.set_width(width);
            ui.set_height(height);
            ui.spacing_mut().item_spacing.y = 0.;
            egui::TopBottomPanel::top("preview_heading")
                .exact_height(34.)
                .frame(theme::frame(theme::RAISED).inner_margin(egui::Margin::symmetric(16, 0)))
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(if preview.purchase {
                                "PART PREVIEW • PURCHASE"
                            } else {
                                "PART PREVIEW"
                            })
                            .size(11.)
                            .strong()
                            .color(theme::MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::tool_sized(
                                ui,
                                Icon::Close,
                                "Close Part Preview",
                                egui::vec2(24., 28.),
                                false,
                            )
                            .clicked()
                            {
                                ui.close();
                            }
                        });
                    });
                });
            if let Some(p) = part {
                if !preview.purchase {
                    egui::TopBottomPanel::bottom("preview_navigation")
                        .exact_height(38.)
                        .frame(
                            theme::frame(theme::RAISED)
                                .inner_margin(egui::Margin::symmetric(20, 0)),
                        )
                        .show_inside(ui, |ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if theme::plain_action(ui, Icon::Right, "Go to Part").clicked()
                                    {
                                        self.switch_view(View::Parts);
                                        self.selected = Some(p.id.clone());
                                        self.catalog = None;
                                        self.query.clear();
                                        self.page = 0;
                                        ctx.request_repaint();
                                        ui.close();
                                    }
                                },
                            );
                        });
                }
                egui::CentralPanel::default()
                    .frame(theme::frame(theme::PANEL))
                    .show_inside(ui, |ui| {
                        let allocated = if preview.purchase {
                            data.purchases
                                .iter()
                                .find(|item| item.part.id == p.id)
                                .map(|item| item.assignments.iter().map(|a| a.quantity).sum())
                                .unwrap_or(0)
                        } else {
                            data.allocated(&p.id)
                        };
                        let projects = if preview.purchase {
                            data.purchases
                                .iter()
                                .find(|item| item.part.id == p.id)
                                .map(|item| item.assignments.len())
                                .unwrap_or(0)
                        } else {
                            data.allocations
                                .iter()
                                .filter(|a| a.part_id == p.id)
                                .map(|a| key(&a.project))
                                .collect::<BTreeSet<_>>()
                                .len()
                        };
                        egui::ScrollArea::vertical()
                            .id_salt(("preview_summary", &p.id))
                            .show(ui, |ui| {
                                self.part_summary(
                                    ui,
                                    data,
                                    p,
                                    preview.purchase,
                                    allocated,
                                    projects,
                                )
                            });
                    });
            } else {
                theme::empty(ui, "Part unavailable", "This item is no longer available.");
            }
        });
        let clicked_outside = response.as_ref().is_some_and(|response| {
            ctx.input(|i| {
                i.pointer.any_click()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|pos| !response.response.rect.contains(pos))
            })
        });
        if !preview.just_opened && clicked_outside && !egui::Popup::is_any_open(ctx) {
            open = false;
        }
        preview.just_opened = false;
        if open
            && self.editor.is_none()
            && self.notes_editor.is_none()
            && self.view == previous_view
            && self.catalog == previous_catalog
        {
            self.part_preview = Some(preview);
        }
    }
}
