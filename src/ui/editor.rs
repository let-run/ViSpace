use super::*;
use anyhow::Context;
use surfaces as form;

impl InventoryApp {
    pub(super) fn editor_window(&mut self, ctx: &egui::Context) {
        let Some(mut e) = self.editor.take() else {
            return;
        };
        let data = self.store.lock().unwrap().data.clone();
        let response = form::sheet("part_editor").show(ctx, |ui| {
            form::configure(ui, 538.);
            form::heading(
                ui,
                if e.purchase {
                    "Purchase details"
                } else {
                    "Part details"
                },
                Icon::Cpu,
            );
            // Keep the scrollbar in a separate gutter instead of overlaying fields.
            ui.spacing_mut().scroll.floating = false;
            ui.spacing_mut().scroll.bar_inner_margin = 10.;
            ui.spacing_mut().scroll.bar_width = 6.;
            egui::ScrollArea::vertical()
                .id_salt("part_form")
                .max_height((ctx.content_rect().height() - 190.).clamp(150., 540.))
                .show(ui, |ui| {
                    ui.set_width(520.);
                    form::row(ui, "Image", |ui| {
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(70., 70.), egui::Sense::hover());
                            theme::image_tile(
                                ui,
                                rect,
                                e.part
                                    .image
                                    .as_ref()
                                    .and_then(|v| v.get("thumbnailPath"))
                                    .and_then(|v| v.as_str()),
                                Icon::Photo,
                            );
                            if e.part.image.is_some() {
                                let r = egui::Rect::from_center_size(
                                    rect.right_top() + egui::vec2(-4., 4.),
                                    egui::vec2(18., 18.),
                                );
                                if ui
                                    .put(
                                        r,
                                        egui::Button::new("×")
                                            .fill(theme::PANEL)
                                            .corner_radius(9)
                                            .min_size(egui::vec2(18., 18.)),
                                    )
                                    .on_hover_text("Remove part image")
                                    .clicked()
                                {
                                    e.part.image = None;
                                }
                            }
                            if theme::tool(ui, Icon::Photo, "Import image from file").clicked()
                                && let Some(path) = crate::platform::dialogs::FileDialog::new()
                                    .add_filter("Images", &["png", "jpg", "jpeg", "webp", "tiff"])
                                    .pick_file()
                            {
                                match e.draft.import_image(&path) {
                                    Ok(image) => {
                                        e.part.image = Some(image);
                                        e.error = None;
                                    }
                                    Err(error) => e.error = Some(error.to_string()),
                                }
                            }
                            if theme::tool(ui, Icon::Clipboard, "Paste image from clipboard")
                                .clicked()
                            {
                                match e.draft.clipboard_image() {
                                    Ok(image) => {
                                        e.part.image = Some(image);
                                        e.error = None;
                                    }
                                    Err(error) => e.error = Some(error.to_string()),
                                }
                            }
                        });
                    });
                    form::row(ui, "Part number", |ui| {
                        form::field(ui, &mut e.part.number);
                    });
                    form::row(ui, "Supplier part number", |ui| {
                        form::field(ui, &mut e.part.spn);
                    });
                    form::row(ui, "Supplier", |ui| {
                        form::suggestion(
                            ui,
                            "supplier_suggestions",
                            &mut e.part.supplier,
                            ["LCSC", "DigiKey", "Mouser", "Newark"].map(str::to_owned),
                        );
                    });
                    form::row(ui, "Manufacturer", |ui| {
                        form::suggestion(
                            ui,
                            "manufacturer_suggestions",
                            &mut e.part.manufacturer,
                            data.parts
                                .iter()
                                .map(|p| p.manufacturer.clone())
                                .filter(|s| !s.is_empty()),
                        );
                    });
                    form::row(ui, "Labels", |ui| {
                        if !e.part.labels.is_empty() {
                            form::chips(ui, &mut e.part.labels);
                        }
                        let labels = data
                            .parts
                            .iter()
                            .flat_map(|p| p.labels.iter().cloned())
                            .chain(
                                [
                                    "Resistor",
                                    "Capacitor",
                                    "Inductor",
                                    "Crystal",
                                    "Oscillator",
                                    "IC",
                                ]
                                .map(str::to_owned),
                            )
                            .filter(|s| !e.part.labels.iter().any(|l| key(l) == key(s)))
                            .collect::<Vec<_>>();
                        let (r, chosen) =
                            form::suggestion_choice(ui, "label_suggestions", &mut e.labels, labels);
                        if let Some(label) = chosen {
                            if !e
                                .part
                                .labels
                                .iter()
                                .any(|existing| key(existing) == key(&label))
                            {
                                e.part.labels.push(label);
                            }
                            e.labels.clear();
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            commit_labels(&mut e.part.labels, &mut e.labels);
                            r.request_focus();
                        }
                        form::hint(
                            ui,
                            "Return adds a label • Recent labels appear when focused",
                        );
                    });
                    let mut analyze_description = false;
                    form::row_with_label(ui, |ui| {
                        ui.label(RichText::new("Description").color(theme::TEXT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            analyze_description = theme::tool_sized(
                                ui, Icon::Analyze, "Analyze description", egui::vec2(26.,24.), false,
                            ).on_hover_text("Fill missing value and tolerance and add a component label.")
                                .on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
                        });
                    }, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut e.part.description)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .margin(egui::vec2(7., 5.)),
                        );
                    });
                    if analyze_description {
                        csv_io::analyze(&mut e.part);
                    }
                    form::row(ui, "Package", |ui| {
                        form::suggestion(
                            ui,
                            "package_suggestions",
                            &mut e.part.package,
                            data.parts
                                .iter()
                                .map(|p| p.package.clone())
                                .filter(|s| !s.is_empty()),
                        );
                    });
                    form::row(ui, "Value", |ui| {
                        form::field(ui, &mut e.part.value);
                    });
                    form::row(ui, "Tolerance", |ui| {
                        form::suggestion(
                            ui,
                            "tolerance_suggestions",
                            &mut e.part.tolerance,
                            ["1%", "5%", "10%", "20%"].map(str::to_owned),
                        );
                        form::hint(ui, "Choose a preset or enter a custom tolerance.");
                    });
                    form::row(ui, "Storage", |ui| {
                        form::suggestion(
                            ui,
                            "storage_suggestions",
                            &mut e.part.storage,
                            data.storage.keys().cloned(),
                        );
                    });
                    form::row(
                        ui,
                        if e.purchase {
                            "Quantity to purchase"
                        } else {
                            "Stock"
                        },
                        |ui| {
                            ui.add(
                                egui::DragValue::new(&mut e.part.stock)
                                    .range(0..=1_000_000_000)
                                    .speed(1.),
                            );
                        },
                    );
                    form::row(ui, "Datasheet", |ui| {
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(!self.busy, |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut e.part.datasheet)
                                        .desired_width((ui.available_width() - 100.).max(60.))
                                        .margin(egui::vec2(7., 5.)),
                                );
                            });
                            if theme::tool_sized(
                                ui,
                                Icon::Folder,
                                "Import PDF from file",
                                egui::vec2(28., 28.),
                                false,
                            )
                            .clicked()
                                && let Some(path) = crate::platform::dialogs::FileDialog::new()
                                    .add_filter("PDF", &["pdf"])
                                    .pick_file()
                            {
                                match e.draft.import_pdf(&path) {
                                    Ok(path) => {
                                        e.part.datasheet = path.to_string_lossy().into();
                                        e.error = None;
                                    }
                                    Err(error) => e.error = Some(error.to_string()),
                                }
                            }
                            ui.add_enabled_ui(
                                !self.busy && e.part.datasheet.starts_with("http"),
                                |ui| {
                                    if theme::tool_sized(
                                        ui,
                                        Icon::Import,
                                        "Download URL as local PDF",
                                        egui::vec2(28., 28.),
                                        false,
                                    )
                                    .clicked()
                                    {
                                        self.download_pdf(&e);
                                    }
                                },
                            );
                            if self.busy {
                                ui.spinner();
                            }
                            if !e.part.datasheet.is_empty()
                                && theme::tool_sized(
                                    ui,
                                    Icon::Close,
                                    "Remove datasheet",
                                    egui::vec2(18., 24.),
                                    false,
                                )
                                .clicked()
                            {
                                e.part.datasheet.clear();
                            }
                        });
                        form::hint(ui, "Keep a web link, download it, or import a PDF.");
                    });
                    ui.horizontal(|ui| {
                        ui.label("Custom fields");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::tool_sized(
                                ui,
                                Icon::Add,
                                "Add custom field",
                                egui::vec2(24., 24.),
                                false,
                            )
                            .clicked()
                            {
                                e.custom.push((String::new(), String::new()));
                            }
                        });
                    });
                    let mut remove = None;
                    for (i, (name, value)) in e.custom.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(name)
                                    .hint_text("Field name")
                                    .desired_width(150.),
                            );
                            ui.add(
                                egui::TextEdit::singleline(value)
                                    .hint_text("Value")
                                    .desired_width(305.),
                            );
                            if theme::tool_sized(
                                ui,
                                Icon::Close,
                                "Remove custom field",
                                egui::vec2(24., 24.),
                                false,
                            )
                            .clicked()
                            {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        e.custom.remove(i);
                    }
                    form::row(ui, "Barcode", |ui| {
                        ui.label(
                            RichText::new(&e.part.barcode)
                                .monospace()
                                .size(10.)
                                .color(theme::MUTED),
                        );
                    });
                });
            form::error(ui, &e.error);
            form::actions(
                ui,
                if e.purchase {
                    "Save purchase"
                } else {
                    "Save part"
                },
                !self.busy,
            )
        });
        let (save, cancel) = response.inner;
        if save {
            commit_labels(&mut e.part.labels, &mut e.labels);
            let result = self.save_editor(&e);
            match result {
                Ok(()) => {
                    self.selected = Some(e.part.id.clone());
                    self.status = if e.new {
                        "Part created"
                    } else {
                        "Changes saved"
                    }
                    .into();
                    return;
                }
                Err(error) if queued_for_approval(&error) => {
                    self.status = "Edit queued for leader approval".into();
                    return;
                }
                Err(error) => e.error = Some(error.to_string()),
            }
        }
        if !cancel && !form::escape(&response) {
            self.editor = Some(e);
        }
    }
    fn save_editor(&mut self, e: &Editor) -> Result<()> {
        let mut part = e.part.clone();
        let mut assignments = e.assignments.clone();
        part.custom_fields = custom_fields(&e.custom)?;
        part.validate()?;
        let mut created = vec![];
        let result = (|| -> Result<()> {
            let mut store = self.store.lock().unwrap();
            if store.uses_write_approval() && !e.new {
                let current = if e.purchase {
                    let purchase = store
                        .data
                        .purchases
                        .iter()
                        .find(|item| item.part.id == part.id)
                        .context("This purchase was removed while you were editing")?;
                    let original = serde_json::to_value(&e.original_assignments)?;
                    let proposed = serde_json::to_value(&assignments)?;
                    let latest = serde_json::to_value(&purchase.assignments)?;
                    ensure!(
                        proposed == original || latest == original || proposed == latest,
                        "Purchase assignments changed while you were editing"
                    );
                    if proposed == original {
                        assignments.clone_from(&purchase.assignments);
                    }
                    &purchase.part
                } else {
                    store
                        .data
                        .parts
                        .iter()
                        .find(|item| item.id == part.id)
                        .context("This part was removed while you were editing")?
                };
                part = rebase_part_edit(&e.original_part, &part, current)?;
            }
            if let Some(image) = part.image.as_mut().and_then(|v| v.as_object_mut()) {
                for name in ["originalPath", "thumbnailPath", "previewPath"] {
                    if let Some(path) = image
                        .get(name)
                        .and_then(|v| v.as_str())
                        .map(std::path::PathBuf::from)
                        && path.starts_with(e.draft.root())
                    {
                        let saved = store.attach(&path, &part.id)?;
                        image.insert(name.into(), serde_json::json!(saved));
                        created.push(saved);
                    }
                }
            }
            if std::path::Path::new(&part.datasheet).starts_with(e.draft.root()) {
                let saved = store.attach(std::path::Path::new(&part.datasheet), &part.id)?;
                part.datasheet = saved.to_string_lossy().into();
                created.push(saved);
            }
            store.change(|d| {
                if e.purchase {
                    ensure!(part.stock > 0, "Purchase quantity must be positive");
                    let item = Purchase { part, assignments };
                    if let Some(old) = d.purchases.iter_mut().find(|p| p.part.id == item.part.id) {
                        *old = item;
                    } else {
                        d.purchases.push(item);
                    }
                } else {
                    d.save_part(part)?;
                }
                Ok(())
            })?;
            Ok(())
        })();
        let keep_attachments = {
            let store = self.store.lock().unwrap();
            store.recovery_required() || store.uses_write_approval()
        };
        if result.is_err() && !keep_attachments {
            for path in created {
                let _ = crate::platform::fs::remove_file(path);
            }
        }
        result
    }
    fn download_pdf(&mut self, e: &Editor) {
        let source = e.part.datasheet.clone();
        let destination = e.draft.root().join(format!("{}.pdf", id()));
        let sender = self.job_sender.clone();
        let owner = e.draft.root().to_string_lossy().to_string();
        self.busy = true;
        let result = crate::async_io::spawn(async move {
            let result = crate::async_io::download_pdf(&source, &destination).await;
            let _ = sender.send((owner, result));
        });
        if let Err(error) = result {
            self.busy = false;
            self.error = Some(error.to_string());
        }
    }
}

fn rebase_part_edit(base: &Part, draft: &Part, current: &Part) -> Result<Part> {
    let base = serde_json::to_value(base)?;
    let draft = serde_json::to_value(draft)?;
    let mut current = serde_json::to_value(current)?;
    for (field, value) in draft.as_object().context("Invalid part draft")? {
        if value == &base[field] {
            continue;
        }
        ensure!(
            current[field] == base[field] || &current[field] == value,
            "{} changed while you were editing. Current value: {}. Your draft: {}.",
            field,
            crate::merge_review::display(&current[field]),
            crate::merge_review::display(value)
        );
        current[field] = value.clone();
    }
    Ok(serde_json::from_value(current)?)
}
fn commit_labels(labels: &mut Vec<String>, pending: &mut String) {
    for label in pending
        .split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !labels.iter().any(|s| key(s) == key(label)) {
            labels.push(label.into());
        }
    }
    pending.clear();
}
fn custom_fields(rows: &[(String, String)]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut names = BTreeSet::new();
    let mut fields = std::collections::BTreeMap::new();
    for (name, value) in rows {
        let name = name.trim();
        ensure!(
            !name.is_empty(),
            "Enter a name for every custom field or remove it."
        );
        ensure!(
            names.insert(key(name)),
            "Custom field names must be unique."
        );
        fields.insert(name.into(), value.clone());
    }
    Ok(fields)
}
