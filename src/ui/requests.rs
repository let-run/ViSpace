use super::*;
use crate::requests::RequestReview;
use crate::tasks::Task;
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::Ordering;
use surfaces as form;

#[derive(Default)]
pub(super) struct RequestState {
    project: String,
    selected: Option<String>,
    reviewing: bool,
    matches: BTreeMap<String, String>,
    message: Option<String>,
    editor: Option<RequestEditor>,
    review: Option<RequestReview>,
    review_revision: Option<u64>,
    review_task: Option<Task<RequestReview>>,
    previews: Vec<Vec<i64>>,
    display: Vec<ReviewItem>,
    display_query: Option<String>,
    display_height: f32,
}
enum ReviewItemKind {
    Header,
    Candidate(usize),
    Empty,
}
struct ReviewItem {
    row: usize,
    kind: ReviewItemKind,
    top: f32,
    height: f32,
}
impl RequestState {
    pub(super) fn editor_open(&self) -> bool {
        self.editor.is_some()
    }
}
pub(super) struct ApplyTask {
    project: String,
    task: Task<i64>,
}
struct RequestEditor {
    project: String,
    existing: Option<String>,
    item: BomItem,
    quantity: String,
    error: Option<String>,
}
impl InventoryApp {
    pub(super) fn requests_panel(&mut self, ui: &mut egui::Ui, data: &Inventory, project: &str) {
        if key(&self.requests.project) != key(project) {
            self.requests = RequestState {
                project: project.into(),
                ..Default::default()
            };
        }
        let query = key(&self.query);
        let selected = data.bom.iter().find(|e| {
            Some(&e.id) == self.requests.selected.as_ref()
                && key(&e.project) == key(project)
                && self.request_quantities.get(&e.id).copied().unwrap_or(0) > 0
                && (query.is_empty()
                    || key(&format!(
                        "{} {} {} {} {}",
                        e.item.designators,
                        e.item.value,
                        e.item.package,
                        e.item.supplier,
                        e.item.spn
                    ))
                    .contains(&query))
        });
        egui::TopBottomPanel::top("request_controls")
            .exact_height(32.)
            .frame(theme::frame(theme::RAISED).inner_margin(egui::Margin::symmetric(14, 0)))
            .show_inside(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_enabled_ui(
                        !self.requests.reviewing && self.request_apply.is_none(),
                        |ui| {
                            if theme::tool_sized(
                                ui,
                                Icon::Add,
                                "Add requested part",
                                egui::vec2(26., 26.),
                                false,
                            )
                            .clicked()
                            {
                                self.requests.editor = Some(RequestEditor {
                                    project: project.into(),
                                    existing: None,
                                    item: BomItem::default(),
                                    quantity: "1".into(),
                                    error: None,
                                });
                            }
                            ui.add_enabled_ui(
                                selected.is_some_and(|e| e.kicad_source.is_none()),
                                |ui| {
                                    if theme::tool_sized(
                                        ui,
                                        Icon::Edit,
                                        "Edit requested part",
                                        egui::vec2(26., 26.),
                                        false,
                                    )
                                    .clicked()
                                        && let Some(e) = selected
                                    {
                                        self.requests.editor = Some(RequestEditor {
                                            project: project.into(),
                                            existing: Some(e.id.clone()),
                                            item: e.item.clone(),
                                            quantity: self
                                                .request_quantities
                                                .get(&e.id)
                                                .copied()
                                                .unwrap_or(0)
                                                .to_string(),
                                            error: None,
                                        });
                                    }
                                    if theme::tool_sized(
                                        ui,
                                        Icon::Trash,
                                        "Delete requested part",
                                        egui::vec2(26., 26.),
                                        false,
                                    )
                                    .clicked()
                                        && let Some(e) = selected
                                    {
                                        self.delete = Some(Delete::Request(e.id.clone()));
                                    }
                                },
                            );
                        },
                    );
                    if ui
                        .add_enabled_ui(
                            self.request_apply.is_none()
                                && (!self.requests.reviewing || self.requests.review.is_some()),
                            |ui| {
                                theme::tool_sized(
                                    ui,
                                    if self.requests.reviewing {
                                        Icon::Check
                                    } else {
                                        Icon::Sync
                                    },
                                    if self.requests.reviewing {
                                        "Apply checked matches"
                                    } else {
                                        "Review matches with available inventory"
                                    },
                                    egui::vec2(26., 26.),
                                    false,
                                )
                            },
                        )
                        .inner
                        .clicked()
                    {
                        if self.requests.reviewing {
                            self.apply_request_sync(ui.ctx(), project);
                        } else {
                            self.requests.matches.clear();
                            self.requests.message = None;
                            self.requests.reviewing = true;
                            self.begin_request_review(ui.ctx(), project);
                        }
                    }
                    if self.requests.reviewing
                        && self.request_apply.is_none()
                        && theme::tool_sized(
                            ui,
                            Icon::Close,
                            "Cancel sync",
                            egui::vec2(26., 26.),
                            false,
                        )
                        .clicked()
                    {
                        self.requests.reviewing = false;
                        self.requests.matches.clear();
                        self.requests.review_task = None;
                        self.requests.review = None;
                    }
                    if self.requests.review_task.is_some() || self.request_apply.is_some() {
                        ui.spinner();
                        ui.label(if self.request_apply.is_some() {
                            "Applying matches…"
                        } else {
                            "Matching inventory…"
                        });
                    }
                    if selected.is_some_and(|e| e.kicad_source.is_some()) {
                        ui.label(
                            RichText::new("Edit this part in KiCad")
                                .size(11.)
                                .color(theme::MUTED),
                        );
                    }
                    if let Some(message) = &self.requests.message {
                        ui.label(RichText::new(message).small().color(theme::MUTED));
                    }
                });
            });
        if self.requests.reviewing {
            if self.requests.review_revision != Some(self.store_revision)
                && self.request_apply.is_none()
            {
                self.begin_request_review(ui.ctx(), project);
            }
            self.request_review(ui);
            return;
        }
        let query = key(&self.query);
        let pending = &self.request_items;
        let entries: Vec<_> = data
            .bom
            .iter()
            .filter(|e| {
                key(&e.project) == key(project)
                    && self.request_quantities.get(&e.id).copied().unwrap_or(0) > 0
            })
            .map(|e| BomEntry {
                item: pending
                    .get(&e.id)
                    .cloned()
                    .unwrap_or_else(|| e.item.clone()),
                ..e.clone()
            })
            .filter(|e| {
                query.is_empty()
                    || key(&format!(
                        "{} {} {} {} {}",
                        e.item.designators,
                        e.item.value,
                        e.item.package,
                        e.item.supplier,
                        e.item.spn
                    ))
                    .contains(&query)
            })
            .collect();
        if entries.is_empty() {
            theme::empty(
                ui,
                "No requested parts",
                "Add a request or import a project BOM.",
            );
            return;
        }
        let link = data
            .projects
            .iter()
            .find(|p| key(&p.name) == key(project))
            .and_then(|p| p.kicad.as_ref());
        let schematic_enabled =
            link.is_some_and(|link| self.kicad.navigation_available(link, false));
        let pcb_enabled = link.is_some_and(|link| self.kicad.navigation_available(link, true));
        let table_rect = ui.available_rect_before_wrap();
        let mut navigate = None;
        theme::table_header(ui, 32.);
        TableBuilder::new(ui)
            .id_salt("project_requests")
            .striped(false)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .sense(egui::Sense::click())
            .column(Column::exact(36.).clip(true))
            .column(Column::initial(150.).clip(true))
            .column(Column::initial(90.).clip(true))
            .column(Column::initial(220.).clip(true))
            .column(Column::initial(170.).clip(true))
            .column(Column::remainder().at_least(100.))
            .header(32., |mut row| {
                for title in [
                    "#",
                    "Designators",
                    "Requested",
                    "Value / Part Number",
                    "Package",
                    "Supplier / SPN",
                ] {
                    row.col(|ui| {
                        if title == "#" {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add_space(4.);
                                    ui.label(RichText::new(title).color(theme::MUTED));
                                },
                            );
                        } else {
                            ui.label(RichText::new(title).color(theme::MUTED));
                        }
                    });
                }
            })
            .body(|mut body| {
                for (index, e) in entries.iter().enumerate() {
                    body.row(36., |mut row| {
                        row.set_overline(true);
                        row.set_selected(self.requests.selected.as_ref() == Some(&e.id));
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add_space(4.);
                                    ui.label(
                                        RichText::new((index + 1).to_string())
                                            .color(theme::MUTED)
                                            .monospace(),
                                    );
                                },
                            );
                        });
                        row.col(|ui| {
                            if let Some(pcb) = designators_navigation_cell(
                                ui,
                                table_rect,
                                &e.item.designators,
                                schematic_enabled,
                                pcb_enabled,
                            ) {
                                navigate = Some((e.item.designators.clone(), pcb));
                            }
                        });
                        for value in [
                            self.request_quantities
                                .get(&e.id)
                                .copied()
                                .unwrap_or(0)
                                .to_string(),
                            e.item.value.clone(),
                            e.item.package.clone(),
                            format!("{} {}", e.item.supplier, e.item.spn),
                        ] {
                            row.col(|ui| {
                                ui.add(egui::Label::new(value).truncate());
                            });
                        }
                        if row.response().clicked() {
                            self.requests.selected = Some(e.id.clone());
                        }
                    });
                }
            });
        if let Some((designators, pcb)) = navigate
            && let Some(link) = link
            && let Err(error) =
                self.kicad
                    .navigate_designators(project, link, &designators, pcb, ui.ctx())
        {
            self.error = Some(error.to_string());
        }
    }
    fn begin_request_review(&mut self, ctx: &egui::Context, project: &str) {
        self.requests.review = None;
        self.requests.previews.clear();
        self.requests.display_query = None;
        self.requests.review_revision = Some(self.store_revision);
        self.requests.review_task = None;
        let data = self.data_snapshot.clone();
        let project = project.to_owned();
        let wake = ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.requests.review_task = Some(Task::spawn(
                move |cancel| {
                    RequestReview::build(&data, &project, || cancel.load(Ordering::Relaxed))
                },
                Box::new(move || wake.request_repaint()),
            ));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = wake;
            self.requests.review_task = Some(Task::browser(
                crate::web::jobs::Input::Review {
                    data: (*data).clone(),
                    project,
                },
                |value| Ok(serde_json::from_value(value)?),
                ctx.clone(),
            ));
        }
    }
    fn apply_request_sync(&mut self, ctx: &egui::Context, project: &str) {
        if self.request_apply.is_some() {
            return;
        }
        let snapshot = self.data_snapshot.clone();
        let revision = self.store_revision;
        let matches = self.requests.matches.clone();
        let store = self.store.clone();
        let project = project.to_owned();
        let target = project.clone();
        let wake = ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        let task = Task::spawn(
            move |cancel| {
                let mut next = (*snapshot).clone();
                let count = next.sync_requests_cancellable(&project, &matches, || {
                    cancel.load(Ordering::Relaxed)
                })?;
                ensure!(!cancel.load(Ordering::Relaxed), "Sync cancelled");
                if count > 0 {
                    store
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Inventory is unavailable"))?
                        .commit_if_revision(revision, next)?;
                }
                Ok(count)
            },
            Box::new(move || wake.request_repaint()),
        );
        #[cfg(target_arch = "wasm32")]
        let task = {
            let _ = wake;
            Task::browser(
                crate::web::jobs::Input::Apply {
                    data: (*snapshot).clone(),
                    project,
                    matches,
                },
                move |value| {
                    let count = value["count"]
                        .as_i64()
                        .ok_or_else(|| anyhow::anyhow!("Invalid sync result"))?;
                    if count > 0 {
                        store
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?
                            .commit_if_revision(
                                revision,
                                serde_json::from_value(value["data"].clone())?,
                            )?;
                    }
                    Ok(count)
                },
                ctx.clone(),
            )
        };
        self.request_apply = Some(ApplyTask {
            project: target,
            task,
        });
    }
    pub(super) fn poll_request_jobs(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.requests.review_task.as_ref().and_then(Task::poll) {
            self.requests.review_task = None;
            if self.requests.review_revision == Some(self.store_revision) {
                match result {
                    Ok(review) => {
                        self.requests.matches.retain(|id, part| {
                            review
                                .rows
                                .iter()
                                .any(|row| &row.entry.id == id && row.candidates.contains(part))
                        });
                        self.requests.previews = review.previews(&self.requests.matches);
                        self.requests.review = Some(review);
                        self.requests.display_query = None;
                    }
                    Err(error) => {
                        self.requests.reviewing = false;
                        self.error = Some(error);
                    }
                }
            }
        }
        if let Some(result) = self
            .request_apply
            .as_ref()
            .and_then(|apply| apply.task.poll())
        {
            let apply = self.request_apply.take().unwrap();
            match result {
                Ok(count) => {
                    let message = format!(
                        "Allocated {count} unit{}",
                        if count == 1 { "" } else { "s" }
                    );
                    self.status = message.clone();
                    if key(&self.requests.project) == key(&apply.project) {
                        self.requests.message = Some(message);
                        self.requests.reviewing = false;
                        self.requests.matches.clear();
                        self.requests.review = None;
                        self.requests.review_task = None;
                    }
                }
                Err(error) => {
                    self.error = Some(error);
                    self.requests.review_revision = None;
                }
            }
            ctx.request_repaint();
        }
        if self.requests.review_task.is_some() || self.request_apply.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    fn request_review(&mut self, ui: &mut egui::Ui) {
        let Some(review) = &self.requests.review else {
            return;
        };
        let query = key(&self.query);
        if self.requests.display_query.as_ref() != Some(&query) {
            self.requests.display.clear();
            let mut top = 0.;
            for (row_index, row) in review.rows.iter().enumerate() {
                let entry = &row.entry;
                if !query.is_empty()
                    && !key(&format!(
                        "{} {} {} {} {}",
                        entry.item.designators,
                        entry.item.value,
                        entry.item.package,
                        entry.item.supplier,
                        entry.item.spn
                    ))
                    .contains(&query)
                {
                    continue;
                }
                self.requests.display.push(ReviewItem {
                    row: row_index,
                    kind: ReviewItemKind::Header,
                    top,
                    height: 62.,
                });
                top += 62.;
                if row.candidates.is_empty() {
                    self.requests.display.push(ReviewItem {
                        row: row_index,
                        kind: ReviewItemKind::Empty,
                        top,
                        height: 40.,
                    });
                    top += 40.;
                }
                for index in 0..row.candidates.len() {
                    self.requests.display.push(ReviewItem {
                        row: row_index,
                        kind: ReviewItemKind::Candidate(index),
                        top,
                        height: 90.,
                    });
                    top += 90.;
                }
                top += 12.;
            }
            self.requests.display_height = top;
            self.requests.display_query = Some(query);
        }
        let mut toggle = None;
        egui::Frame::NONE.inner_margin(12).show(ui, |ui| {
            ui.label(RichText::new("Check one match per request, then Apply to allocate available stock.").small().color(theme::MUTED));
            ui.add_space(12.);
            egui::ScrollArea::vertical().id_salt("request_sync_review").max_height(ui.available_height()).auto_shrink([false, false]).show_viewport(ui, |ui,viewport| {
                let origin = ui.min_rect().min;
                let width = ui.available_width();
                ui.set_min_size(egui::vec2(width, self.requests.display_height));
                let start = self.requests.display.partition_point(|item| item.top + item.height < viewport.top());
                for item in self.requests.display[start..].iter().take_while(|item| item.top <= viewport.bottom()) {
                    let row = &review.rows[item.row];
                    let entry = &row.entry;
                    let rect = egui::Rect::from_min_size(origin+egui::vec2(0.,item.top),egui::vec2(width,item.height));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        ui.set_clip_rect(ui.clip_rect().intersect(rect));
                        ui.spacing_mut().item_spacing.y = 4.;
                        match item.kind {
                            ReviewItemKind::Header => {
                                ui.painter().rect_filled(rect,0.,theme::RAISED);
                                egui::Frame::NONE.inner_margin(10).show(ui, |ui| {
                                    ui.add(egui::Label::new(RichText::new(format!("REQUEST • {}",entry.item.designators)).strong().color(theme::TEXT)).truncate());
                                    ui.add(egui::Label::new(format!("Qty {} • {} • {} • {}",row.requested,entry.item.value,entry.item.package,entry.item.spn)).truncate());
                                });
                            }
                            ReviewItemKind::Empty => {
                                ui.painter().rect_filled(rect,0.,theme::PANEL);
                                egui::Frame::NONE.inner_margin(10).show(ui, |ui| { ui.label(RichText::new("No matching inventory parts").color(theme::MUTED)); });
                            }
                            ReviewItemKind::Candidate(index) => {
                                // Text belongs to the card's click target, rather than text selection.
                                ui.style_mut().interaction.selectable_labels = false;
                                let id = &row.candidates[index];
                                let part = &review.parts[id];
                                let checked = self.requests.matches.get(&entry.id) == Some(id);
                                let count = self.requests.previews[item.row][index];
                                let card = ui.interact(
                                    rect,
                                    ui.id().with(("request_candidate", &entry.id, id)),
                                    if self.request_apply.is_none() { egui::Sense::click() } else { egui::Sense::hover() },
                                );
                                if self.request_apply.is_none() {
                                    let card = card.on_hover_cursor(egui::CursorIcon::PointingHand);
                                    if card.clicked() {
                                        toggle = Some((entry.id.clone(), id.clone(), !checked));
                                    }
                                }
                                ui.painter().rect_filled(rect,0.,if checked {theme::SELECTION} else {theme::PANEL});
                                egui::Frame::NONE.inner_margin(10).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let mut selected = checked;
                                        if ui.add_enabled(self.request_apply.is_none(),egui::Checkbox::new(&mut selected, "")).changed() { toggle = Some((entry.id.clone(),id.clone(),selected)); }
                                        ui.vertical(|ui| {
                                            ui.add(egui::Label::new(RichText::new(format!("{} • {}",part.number,part.manufacturer)).strong().color(theme::TEXT)).truncate());
                                            ui.add(egui::Label::new(RichText::new(format!("{} • {} • Available {} • {} to allocate",part.value,part.package,review.available[id],count)).color(theme::MUTED)).truncate());
                                            ui.add(egui::Label::new(RichText::new(&part.description).color(theme::MUTED)).truncate());
                                        });
                                    });
                                });
                            }
                        }
                    });
                }
            });
        });
        if let Some((entry, part, selected)) = toggle {
            if selected {
                self.requests.matches.insert(entry, part);
            } else {
                self.requests.matches.remove(&entry);
            }
            self.requests.previews = review.previews(&self.requests.matches);
        }
    }
    pub(super) fn request_window(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.requests.editor.take() else {
            return;
        };
        let response = form::sheet("request_editor").show(ctx, |ui| {
            form::configure(ui, 480.);
            form::heading(
                ui,
                if editor.existing.is_some() {
                    "Edit requested part"
                } else {
                    "Add requested part"
                },
                Icon::Edit,
            );
            for (label, value) in [
                ("Value / Part Number", &mut editor.item.value),
                ("Designators", &mut editor.item.designators),
                ("Requested quantity", &mut editor.quantity),
                ("Package", &mut editor.item.package),
                ("Supplier", &mut editor.item.supplier),
                ("Supplier part number", &mut editor.item.spn),
            ] {
                form::row(ui, label, |ui| {
                    form::field(ui, value);
                });
            }
            form::error(ui, &editor.error);
            form::actions(ui, "Save", true)
        });
        let (save, cancel) = response.inner;
        if save {
            let result = (|| {
                editor.item.quantity = editor
                    .quantity
                    .trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("Enter a positive whole-number quantity."))?;
                self.store.lock().unwrap().change(|d| {
                    d.save_request(
                        &editor.project,
                        editor.existing.as_deref(),
                        editor.item.clone(),
                    )
                })
            })();
            match result {
                Ok(()) => {
                    self.status = "Request saved".into();
                    return;
                }
                Err(error) => editor.error = Some(error.to_string()),
            }
        }
        if !cancel && !form::escape(&response) {
            self.requests.editor = Some(editor);
        }
    }
}

// The entire row is the hover target; reserve the action space so text never jumps.
pub(super) fn designators_navigation_cell(
    ui: &mut egui::Ui,
    table: egui::Rect,
    designators: &str,
    schematic: bool,
    pcb: bool,
) -> Option<bool> {
    let cell = ui.max_rect();
    let hovered = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| {
        table.x_range().contains(p.x)
            && cell.y_range().contains(p.y)
            && ui.clip_rect().y_range().contains(p.y)
            && ui
                .ctx()
                .layer_id_at(p)
                .is_none_or(|layer| layer == ui.layer_id())
    });
    let mut chosen = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 1.;
        if hovered {
            let image = egui::Image::new(egui::include_image!("../../assets/icons/pcb.png"))
                .fit_to_exact_size(egui::vec2(24., 24.));
            if ui
                .add_enabled(
                    pcb && !designators.trim().is_empty(),
                    egui::Button::image(image)
                        .frame(false)
                        .min_size(egui::vec2(26., 28.)),
                )
                .on_hover_text("Show designators on PCB")
                .clicked()
            {
                chosen = Some(true);
            }
            let image = egui::Image::new(egui::include_image!("../../assets/icons/schematics.png"))
                .fit_to_exact_size(egui::vec2(24., 24.));
            if ui
                .add_enabled(
                    schematic && !designators.trim().is_empty(),
                    egui::Button::image(image)
                        .frame(false)
                        .min_size(egui::vec2(26., 28.)),
                )
                .on_hover_text("Show designators in schematics")
                .clicked()
            {
                chosen = Some(false);
            }
        } else {
            ui.add_space(53.);
        }
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width().max(0.), 30.),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(
                    egui::Label::new(designators)
                        .truncate()
                        .halign(egui::Align::Min),
                );
            },
        );
    });
    chosen
}

#[cfg(test)]
mod navigation_cell_tests {
    use super::*;
    #[test]
    fn row_hover_shows_right_aligned_icons_and_clicking_them_navigates() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let draw = |events: Vec<egui::Event>| {
            let mut chosen = None;
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600., 100.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(160., 36.),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    chosen = designators_navigation_cell(
                                        ui,
                                        egui::Rect::from_min_max(
                                            egui::Pos2::ZERO,
                                            egui::pos2(600., 36.),
                                        ),
                                        "R1, R2",
                                        true,
                                        true,
                                    );
                                },
                            );
                        });
                },
            );
            (out, chosen)
        };
        let pointer = |x, y| egui::Event::PointerMoved(egui::pos2(x, y));
        draw(vec![pointer(400., 18.)]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let hover = loop {
            let (output, _) = draw(vec![pointer(400., 18.)]);
            let ready = output
                .shapes
                .iter()
                .filter(|s| matches!(&s.shape,egui::Shape::Rect(rect) if rect.brush.is_some()))
                .count()
                == 2;
            if ready || std::time::Instant::now() >= deadline {
                break output;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let icons = hover
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape,egui::Shape::Rect(rect) if rect.brush.is_some()))
            .count();
        assert_eq!(
            icons, 2,
            "hovering another cell of the row shows both navigation icons"
        );
        let text_x = hover
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == "R1, R2" => Some(t.pos.x),
                _ => None,
            })
            .unwrap();
        assert!(text_x < 10., "designators remain left aligned");
        let (hidden, _) = draw(vec![pointer(400., 70.)]);
        assert!(
            !hidden
                .shapes
                .iter()
                .any(|s| matches!(&s.shape,egui::Shape::Rect(rect) if rect.brush.is_some())),
            "icons are hidden outside this row"
        );
        for (x, pcb) in [(144., true), (114., false)] {
            draw(vec![pointer(x, 18.)]);
            draw(vec![egui::Event::PointerButton {
                pos: egui::pos2(x, 18.),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }]);
            let (_, chosen) = draw(vec![egui::Event::PointerButton {
                pos: egui::pos2(x, 18.),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }]);
            assert_eq!(chosen, Some(pcb));
        }
    }
}
