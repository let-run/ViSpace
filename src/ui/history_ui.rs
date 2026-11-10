use super::history_graph::Graph;
use super::*;
use crate::action_log::{HistoryCursor, HistoryEntry, HistoryPage};
use std::sync::mpsc;

#[derive(serde::Deserialize)]
struct HistoryBatch {
    page: HistoryPage,
    author: String,
    conflicts: Vec<String>,
}

type PageResult = std::result::Result<HistoryBatch, String>;
const PAGE_SIZE: usize = 30;
#[derive(Default)]
pub(super) struct HistoryUi {
    entries: Vec<HistoryEntry>,
    graph: Graph,
    author: String,
    next: Option<HistoryCursor>,
    loaded: bool,
    scroll_to_top: bool,
    error: Option<String>,
    head: String,
    pending: Option<mpsc::Receiver<PageResult>>,
}
impl InventoryApp {
    pub(super) fn history_popup(&mut self, anchor: &egui::Response) {
        let ctx = &anchor.ctx;
        let width = 760_f32.min((ctx.content_rect().width() - 32.).max(1.));
        let height = 480_f32.min((anchor.rect.top() - ctx.content_rect().top() - 32.).max(1.));
        let popup = egui::Popup::from_toggle_button_response(anchor)
            .align(egui::RectAlign::TOP_START)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .frame(super::surfaces::popup_frame(theme::PANEL, 12))
            .gap(8.)
            .width(width);
        if !popup.is_open() && !anchor.clicked() {
            self.action_log_cache = HistoryUi::default();
            return;
        }
        let state = &mut self.action_log_cache;
        if let Some(pending) = &state.pending {
            match pending.try_recv() {
                Ok(result) => {
                    match result {
                        Ok(batch) => {
                            state.entries.extend(batch.page.entries);
                            state.next = batch.page.next;
                            state.author = batch.author;
                            state.graph = Graph::build(&state.entries, &batch.conflicts);
                        }
                        Err(error) => state.error = Some(error),
                    }
                    state.scroll_to_top = !state.loaded;
                    state.loaded = true;
                    state.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.error = Some("History request ended unexpectedly".into());
                    state.loaded = true;
                    state.pending = None;
                }
            }
        }
        let new_actions = state.loaded && self.local_head_hash != state.head;
        let mut load_more = false;
        let mut refresh = false;
        let shown = popup.show(|ui| {
            ui.set_min_size(egui::vec2(width, height));
            ui.set_max_size(egui::vec2(width, height));
            ui.heading("Inventory history");
            ui.horizontal(|ui| {
                refresh = ui.button("Refresh").clicked();
                ui.label(format!(
                    "{} operations loaded · causal order · local time",
                    state.entries.len()
                ));
                if new_actions {
                    ui.small("New actions available");
                }
            });
            if !state.loaded {
                ui.label("Loading history…");
                return;
            }
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt("inventory_history_pages")
                .auto_shrink([false, false])
                .max_height(ui.available_height());
            if std::mem::take(&mut state.scroll_to_top) {
                scroll = scroll.vertical_scroll_offset(0.);
            }
            scroll.show(ui, |ui| {
                let left = ui.cursor().min.x;
                let spacing =
                    (ui.available_width() * 0.22 / state.graph.lanes.max(1) as f32).clamp(1., 15.);
                let gutter = 22. + spacing * state.graph.lanes as f32;
                let mut centers = Vec::with_capacity(state.graph.rows.len());
                for row in &state.graph.rows {
                    let item = &state.entries[row.entry];
                    let entry = &item.entry;
                    ui.push_id((&entry.chunk, entry.offset), |ui| {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.add_space(gutter);
                            ui.vertical(|ui| {
                                let header = ui.horizontal_wrapped(|ui| {
                                    ui.label(format!(
                                        "{}{}",
                                        format_history_time(entry.record.timestamp_ms),
                                        if entry.record.initial {
                                            " · Initial inventory"
                                        } else {
                                            ""
                                        }
                                    ));
                                    ui.monospace(format!("#{}", &item.hash[..10]))
                                        .on_hover_text(format!(
                                            "Action hash (SHA-1)\n{}\n\nEvent ID: {}\nChunk: {}",
                                            item.hash, entry.record.id, entry.chunk
                                        ));
                                    if let Some(meta) = &entry.record.mesh {
                                        let author = if meta.author() == state.author {
                                            "Me".to_owned()
                                        } else {
                                            meta.author().chars().take(8).collect()
                                        };
                                        ui.small(RichText::new(author).color(theme::MUTED))
                                            .on_hover_text(format!(
                                                "Author node\n{}",
                                                meta.author()
                                            ));
                                    } else {
                                        ui.small("Legacy");
                                    }
                                    if row.resolution {
                                        ui.small(RichText::new("Resolution").color(theme::GREEN));
                                    } else if row.conflict {
                                        ui.small(
                                            RichText::new("Conflicting edit").color(theme::YELLOW),
                                        );
                                    }
                                });
                                centers.push(header.response.rect.center().y);
                                for action in &entry.record.actions {
                                    ui.label(format!(
                                        "{} · {}",
                                        action.kind.replace('_', " "),
                                        action.label
                                    ))
                                    .on_hover_text(&action.key);
                                    if !action.changes.is_empty() {
                                        ui.small(action.changes.join(", ").replace('_', " "));
                                    }
                                }
                            });
                        });
                    });
                }
                draw_graph(ui, &state.graph, &centers, left + 9., spacing);
                if let Some(error) = &state.error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                } else if state.next.is_some() {
                    let sentinel = ui.label("Loading older actions…");
                    load_more = ui.is_rect_visible(sentinel.rect);
                } else if state.entries.is_empty() {
                    ui.label("No inventory actions yet.");
                } else {
                    ui.small("Beginning of history");
                }
            });
        });
        if shown.is_none() {
            *state = HistoryUi::default();
        } else if refresh {
            *state = HistoryUi::default();
            ctx.request_repaint();
        } else if state.pending.is_none() && (!state.loaded || load_more) {
            if !state.loaded {
                state.head = self.local_head_hash.clone();
            }
            state.pending = request_page(&self.store, state.next.as_ref(), ctx);
        }
    }
}

fn format_history_time(timestamp_ms: u64) -> String {
    i64::try_from(timestamp_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M %:z")
                .to_string()
        })
        .unwrap_or_else(|| "Unknown date".into())
}

fn draw_graph(ui: &egui::Ui, graph: &Graph, centers: &[f32], left: f32, spacing: f32) {
    let painter = ui.painter();
    let x = |lane: usize| left + spacing * lane as f32;
    let line = egui::Stroke::new(1.7, theme::PURPLE.gamma_multiply(1.6));
    for (index, row) in graph.rows.iter().enumerate() {
        let y = centers[index];
        let top = index
            .checked_sub(1)
            .map_or(y, |above| (centers[above] + y) * 0.5);
        let bottom = centers.get(index + 1).map_or(y, |below| (below + y) * 0.5);
        if bottom < ui.clip_rect().top() || top > ui.clip_rect().bottom() {
            continue;
        }
        let dot = egui::pos2(x(row.lane), y);
        for &lane in &row.through {
            painter.line_segment(
                [egui::pos2(x(lane), top), egui::pos2(x(lane), bottom)],
                line,
            );
        }
        if row.incoming {
            painter.line_segment([egui::pos2(dot.x, top), dot], line);
        }
        for &parent in &row.parents {
            let middle = (y + bottom) * 0.5;
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [
                    dot,
                    egui::pos2(dot.x, middle),
                    egui::pos2(x(parent), middle),
                    egui::pos2(x(parent), bottom),
                ],
                false,
                Color32::TRANSPARENT,
                line,
            ));
        }
        let color = if row.resolution {
            theme::GREEN
        } else if row.conflict {
            theme::YELLOW
        } else {
            theme::MUTED
        };
        painter.circle_filled(dot, 5., theme::PANEL);
        painter.circle_filled(dot, 3.5, color);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn request_page(
    store: &SharedStore,
    before: Option<&HistoryCursor>,
    ctx: &egui::Context,
) -> Option<mpsc::Receiver<PageResult>> {
    let Ok(store) = store.try_lock() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        return None;
    };
    let (send, receive) = mpsc::channel();
    let _ = send.send(
        store
            .action_log_page(before, PAGE_SIZE)
            .map(|page| HistoryBatch {
                page,
                author: store.application_id().to_owned(),
                conflicts: store
                    .merge_reviews()
                    .into_iter()
                    .flat_map(|review| review.events)
                    .collect(),
            })
            .map_err(|error| error.to_string()),
    );
    ctx.request_repaint();
    Some(receive)
}

#[cfg(target_arch = "wasm32")]
fn request_page(
    _store: &SharedStore,
    before: Option<&HistoryCursor>,
    ctx: &egui::Context,
) -> Option<mpsc::Receiver<PageResult>> {
    let (send, receive) = mpsc::channel();
    let before = serde_json::to_string(&before).expect("serializable history cursor");
    let request = crate::web::host_history_page(&before, PAGE_SIZE);
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let value = wasm_bindgen_futures::JsFuture::from(request)
                .await
                .map_err(|error| format!("Could not load history: {error:?}"))?;
            let json = value.as_string().ok_or("Invalid history response")?;
            serde_json::from_str(&json).map_err(|error| error.to_string())
        }
        .await;
        let _ = send.send(result);
        ctx.request_repaint();
    });
    Some(receive)
}
