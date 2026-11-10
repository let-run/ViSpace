use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct Draft {
    quantity: String,
    designators: String,
}
pub(super) struct BulkAdd {
    target: String,
    storage: bool,
    purchase: bool,
    drafts: BTreeMap<String, Draft>,
    checked: BTreeSet<String>,
    error: Option<String>,
}
impl InventoryApp {
    pub(super) fn bulk_units(&self) -> Option<i64> {
        self.bulk.as_ref().filter(|b| !b.storage).map(|b| {
            b.drafts
                .values()
                .filter_map(|d| d.quantity.parse::<i64>().ok())
                .fold(0_i64, i64::saturating_add)
        })
    }
    pub(super) fn bulk_controls(&mut self, ui: &mut egui::Ui) {
        let allowed = self.view == View::Storage || matches!(self.project_tab, 0 | 2);
        let active = self.bulk.is_some();
        let title = if active {
            "Apply bulk adding"
        } else {
            "Bulk add parts"
        };
        if ui
            .add_enabled_ui(allowed, |ui| {
                theme::tool_sized(
                    ui,
                    if active { Icon::Check } else { Icon::BulkAdd },
                    title,
                    egui::vec2(26., 26.),
                    false,
                )
            })
            .inner
            .clicked()
        {
            if active {
                let result = self.apply_bulk();
                match result {
                    Ok(()) => {
                        self.bulk = None;
                        self.page = 0;
                        self.status = "Bulk additions saved".into();
                    }
                    Err(error) => {
                        if let Some(bulk) = &mut self.bulk {
                            bulk.error = Some(error.to_string());
                        }
                    }
                }
            } else if let Some(target) = self.catalog.clone() {
                self.bulk = Some(BulkAdd {
                    target,
                    storage: self.view == View::Storage,
                    purchase: self.project_tab == 2 && self.view == View::Projects,
                    drafts: BTreeMap::new(),
                    checked: BTreeSet::new(),
                    error: None,
                });
                self.page = 0;
                self.selected = None;
                self.part_preview = None;
            }
        }
        if self.bulk.is_some()
            && theme::tool_sized(
                ui,
                Icon::Close,
                "Cancel bulk adding",
                egui::vec2(26., 26.),
                false,
            )
            .clicked()
        {
            self.bulk = None;
            self.page = 0;
        }
    }
    fn apply_bulk(&mut self) -> Result<()> {
        let bulk = self.bulk.as_ref().unwrap();
        let drafts =
            bulk.drafts
                .iter()
                .map(|(id, d)| {
                    let quantity = if d.quantity.trim().is_empty() {
                        0
                    } else {
                        d.quantity.trim().parse::<i64>().map_err(|_| {
                            anyhow::anyhow!("Enter a positive whole-number quantity")
                        })?
                    };
                    Ok((id.clone(), quantity, d.designators.clone()))
                })
                .collect::<Result<Vec<_>>>()?;
        self.store.lock().unwrap().change(|data| {
            if bulk.storage {
                data.bulk_storage(&bulk.target, &bulk.checked)
            } else {
                data.bulk_allocate(&bulk.target, &drafts, bulk.purchase)
            }
        })
    }
    pub(super) fn bulk_table(&mut self, ui: &mut egui::Ui, data: &Inventory) {
        let Some(bulk) = self.bulk.as_mut() else {
            return;
        };
        let parts: Vec<_> = if bulk.purchase {
            data.purchases
                .iter()
                .filter(|p| {
                    !p.assignments
                        .iter()
                        .any(|a| key(&a.project) == key(&bulk.target))
                })
                .map(|p| &p.part)
                .collect()
        } else {
            data.parts
                .iter()
                .filter(|p| {
                    if bulk.storage {
                        key(&p.storage) != key(&bulk.target)
                    } else {
                        !data
                            .allocations
                            .iter()
                            .any(|a| a.part_id == p.id && key(&a.project) == key(&bulk.target))
                    }
                })
                .collect()
        };
        let mut parts: Vec<_> = parts
            .into_iter()
            .filter(|p| {
                (if bulk.purchase {
                    data.purchases
                        .iter()
                        .find(|item| item.part.id == p.id)
                        .is_some_and(|item| item.matches(&self.query))
                } else {
                    p.matches(&self.query)
                }) || (!self.query.is_empty()
                    && bulk
                        .drafts
                        .get(&p.id)
                        .is_some_and(|d| key(&d.designators).contains(&key(&self.query))))
            })
            .collect();
        parts.sort_by(|a, b| part_number_cmp(&a.number, &b.number));
        let count = parts.len();
        let pages = count.div_ceil(self.page_size).max(1);
        self.page = self.page.min(pages - 1);
        egui::TopBottomPanel::bottom("bulk_pagination")
            .exact_height(38.)
            .frame(theme::frame(theme::RAISED).inner_margin(8))
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(error) = &bulk.error {
                        ui.add(
                            egui::Label::new(RichText::new(error).color(Color32::LIGHT_RED))
                                .truncate(),
                        )
                        .on_hover_text(error);
                    }
                    let pending = if bulk.storage {
                        bulk.checked.len()
                    } else {
                        bulk.drafts
                            .values()
                            .filter(|d| !d.quantity.is_empty() || !d.designators.is_empty())
                            .count()
                    };
                    ui.label(format!("{count} parts · {pending} selected"));
                    if ui
                        .add_enabled(self.page > 0, egui::Button::new("Previous"))
                        .clicked()
                    {
                        self.page -= 1;
                    }
                    ui.label(format!("{} / {pages}", self.page + 1));
                    if ui
                        .add_enabled(self.page + 1 < pages, egui::Button::new("Next"))
                        .clicked()
                    {
                        self.page += 1;
                    }
                });
            });
        theme::table_header(ui, 32.);
        let mut table = TableBuilder::new(ui)
            .id_salt(("bulk_parts", bulk.storage, bulk.purchase))
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(36.))
            .column(Column::initial(170.).clip(true));
        if bulk.storage {
            table = table
                .column(Column::exact(55.))
                .column(Column::initial(120.).clip(true));
        } else {
            table = table
                .column(Column::initial(150.))
                .column(Column::initial(70.))
                .column(Column::exact(75.));
        }
        table
            .column(Column::remainder().at_least(100.))
            .header(32., |mut row| {
                let headers = if bulk.storage {
                    vec!["#", "Part Number", "Add", "Storage", "Description"]
                } else {
                    vec![
                        "#",
                        "Part Number",
                        "Designators",
                        "Qty",
                        if bulk.purchase {
                            "To purchase"
                        } else {
                            "Available"
                        },
                        "Description",
                    ]
                };
                for h in headers {
                    row.col(|ui| {
                        if h == "#" {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(RichText::new(h).size(12.).color(theme::MUTED));
                                },
                            );
                        } else {
                            ui.label(RichText::new(h).size(12.).color(theme::MUTED));
                        }
                    });
                }
            })
            .body(|mut body| {
                for (index, p) in parts
                    .iter()
                    .enumerate()
                    .skip(self.page * self.page_size)
                    .take(self.page_size)
                {
                    body.row(36., |mut row| {
                        row.set_overline(true);
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label((index + 1).to_string());
                                },
                            );
                        });
                        row.col(|ui| {
                            theme::part_number_cell(ui, p);
                        });
                        if bulk.storage {
                            row.col(|ui| {
                                let mut checked = bulk.checked.contains(&p.id);
                                if ui.checkbox(&mut checked, "").changed() {
                                    if checked {
                                        bulk.checked.insert(p.id.clone());
                                    } else {
                                        bulk.checked.remove(&p.id);
                                    }
                                }
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(&p.storage).truncate());
                            });
                        } else {
                            let draft = bulk.drafts.entry(p.id.clone()).or_default();
                            row.col(|ui| {
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut draft.designators)
                                            .id_salt(("bulk_designators", &p.id))
                                            .desired_width(f32::INFINITY),
                                    )
                                    .changed()
                                    && !draft.designators.trim().is_empty()
                                {
                                    match references(&draft.designators) {
                                        Ok(refs) => {
                                            draft.quantity = refs.len().to_string();
                                            bulk.error = None;
                                        }
                                        Err(e) => bulk.error = Some(e.to_string()),
                                    }
                                }
                            });
                            row.col(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.quantity)
                                        .id_salt(("bulk_quantity", &p.id))
                                        .desired_width(f32::INFINITY)
                                        .hint_text("0"),
                                );
                            });
                            row.col(|ui| {
                                let available = if bulk.purchase {
                                    data.purchases
                                        .iter()
                                        .find(|x| x.part.id == p.id)
                                        .map(|x| {
                                            x.part.stock
                                                - x.assignments
                                                    .iter()
                                                    .map(|a| a.quantity)
                                                    .sum::<i64>()
                                        })
                                        .unwrap_or(0)
                                } else {
                                    data.available(p)
                                };
                                ui.label(available.to_string());
                            });
                        }
                        row.col(|ui| {
                            ui.add(egui::Label::new(&p.description).truncate());
                        });
                    });
                }
            });
    }
}
