use super::cad_text::zoom_galley;
use super::*;
use inventory_kicad::{Pcb, PcbShape, Point};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct PcbMatch {
    pub(super) reference: Option<String>,
    pub(super) title: String,
    pub(super) value: String,
    pub(super) package: String,
}
pub(super) struct PcbViewer {
    pub project: String,
    pub key: String,
    pub board: Arc<Pcb>,
    details: Option<super::footprint_details::FootprintDetails>,
    part_navigation: Option<super::footprint_details::PartTarget>,
    active: String,
    hidden: BTreeSet<String>,
    center: Point,
    scale: f64,
    fit: bool,
    inactive_opacity: f32,
    show_nets: bool,
    show_clearance: bool,
    gpu_camera: bool,
    selected_net: Option<String>,
    selected_nets: BTreeSet<String>,
    class_colors: BTreeMap<String, [u8; 3]>,
    class_scene: Option<Arc<super::cad_gpu::Scene>>,
    class_task: Option<crate::tasks::Task<Arc<super::cad_gpu::Scene>>>,
    class_config: Option<(usize, super::cad_jobs::PcbSettings)>,
    selected_footprint: Option<String>,
    selected_footprints: BTreeSet<String>,
    focus_footprints: bool,
    footprint_change: Option<Option<String>>,
    net_change: Option<Option<String>>,
    blink_start: Option<f64>,
    net_scene: Option<Arc<super::cad_gpu::Scene>>,
    net_task: Option<crate::tasks::Task<Arc<super::cad_gpu::Scene>>>,
    net_config: Option<(String, super::cad_jobs::PcbSettings)>,
    net_error: Option<String>,
    gpu_task: Option<crate::tasks::Task<Arc<super::cad_gpu::Scene>>>,
    gpu_error: Option<String>,
    gpu_pending: bool,
    gpu_wait_until: f64,
    camera_scene: Option<super::pcb_layers::SceneCache>,
    item_bounds: Vec<egui::Rect>,
    gpu_scene: Option<Arc<super::cad_gpu::Scene>>,
    gpu_key: Option<(usize, String, BTreeSet<String>, u32, bool, bool)>,
}
impl PcbViewer {
    pub(super) fn details(&self) -> Option<super::footprint_details::FootprintDetails> {
        self.details.clone()
    }
    pub fn new(project: &str, key: &str, board: Arc<Pcb>) -> Self {
        let active = board
            .layers
            .iter()
            .find(|l| l.name == "F.Cu")
            .or_else(|| board.layers.first())
            .map(|l| l.name.clone())
            .unwrap_or_default();
        let hidden = board
            .layers
            .iter()
            .filter(|l| {
                l.name.ends_with(".Fab")
                    || l.name.ends_with(".CrtYd")
                    || l.name.ends_with(".Mask")
                    || l.name.ends_with(".Paste")
                    || l.name.ends_with(".Adhes")
            })
            .map(|l| l.name.clone())
            .collect();
        let item_bounds = Vec::new();
        Self {
            item_bounds,
            gpu_scene: None,
            gpu_key: None,
            project: project.into(),
            key: key.into(),
            board,
            details: None,
            part_navigation: None,
            active,
            hidden,
            center: Point::default(),
            scale: 1.,
            fit: true,
            inactive_opacity: 0.18,
            show_nets: true,
            show_clearance: true,
            gpu_camera: true,
            selected_net: None,
            selected_nets: BTreeSet::new(),
            class_colors: BTreeMap::new(),
            class_scene: None,
            class_task: None,
            class_config: None,
            selected_footprint: None,
            selected_footprints: BTreeSet::new(),
            focus_footprints: false,
            footprint_change: None,
            net_change: None,
            blink_start: None,
            net_scene: None,
            net_task: None,
            net_config: None,
            net_error: None,
            gpu_task: None,
            gpu_error: None,
            gpu_pending: true,
            gpu_wait_until: 0.,
            camera_scene: None,
        }
    }
    pub(super) fn single_footprint(&self) -> Option<&str> {
        (self.selected_footprints.len() == 1)
            .then(|| self.selected_footprints.first().unwrap().as_str())
    }
    pub(super) fn set_details(
        &mut self,
        details: Option<super::footprint_details::FootprintDetails>,
    ) {
        self.details = details;
    }
    pub(super) fn take_part_navigation(&mut self) -> Option<super::footprint_details::PartTarget> {
        self.part_navigation.take()
    }
    pub fn replace_board(&mut self, board: Arc<Pcb>) {
        self.class_scene = None;
        self.class_task = None;
        self.class_config = None;
        self.net_scene = None;
        self.net_task = None;
        self.net_config = None;
        self.item_bounds.clear();
        self.board = board;
        if self.selected_footprint.as_ref().is_some_and(|name| {
            !self
                .board
                .footprint_list
                .iter()
                .any(|f| &f.reference == name)
        }) {
            self.select_footprint(None);
        }
        self.camera_scene = None;
        self.gpu_scene = None;
        self.gpu_task = None;
        self.gpu_error = None;
        self.gpu_key = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(
            ui.available_size().max(egui::vec2(100., 100.)),
            egui::Sense::click_and_drag(),
        );
        let background = Color32::from_rgb(18, 21, 24);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0, background);
        if self.fit {
            let b = self.board.bounds;
            self.center = Point::new((b.min.x + b.max.x) / 2., (b.min.y + b.max.y) / 2.);
            self.scale = ((rect.width() as f64 - 60.) / b.width().max(1.))
                .min((rect.height() as f64 - 60.) / b.height().max(1.))
                .clamp(0.1, 300.);
            self.fit = false;
        }
        if std::mem::take(&mut self.focus_footprints) {
            let mut bounds = egui::Rect::NOTHING;
            for footprint in self
                .board
                .footprint_list
                .iter()
                .filter(|f| self.selected_footprints.contains(&f.reference))
            {
                bounds = bounds.union(egui::Rect::from_min_max(
                    egui::pos2(footprint.bounds.min.x as f32, footprint.bounds.min.y as f32),
                    egui::pos2(footprint.bounds.max.x as f32, footprint.bounds.max.y as f32),
                ));
            }
            if bounds.is_finite() && bounds.is_positive() {
                self.center = Point::new(bounds.center().x as f64, bounds.center().y as f64);
                self.scale = ((rect.width() - 100.) / bounds.width().max(1.))
                    .min((rect.height() - 100.) / bounds.height().max(1.))
                    .clamp(0.1, 45.) as f64;
                self.fit = false;
            }
        }
        if response.dragged() {
            let d = ui.input(|i| i.pointer.delta());
            self.center.x -= d.x as f64 / self.scale;
            self.center.y -= d.y as f64 / self.scale;
        }
        if response.hovered() {
            if ui.input(|i| i.key_pressed(egui::Key::F)) {
                self.fit = true;
            }
            if let Some(mouse) = response.hover_pos() {
                let delta = mouse - rect.center();
                let before = Point::new(
                    self.center.x + delta.x as f64 / self.scale,
                    self.center.y + delta.y as f64 / self.scale,
                );
                let factor = ui.input(|i| {
                    (i.smooth_scroll_delta.y as f64 * 0.002).exp() * i.zoom_delta() as f64
                });
                self.scale = (self.scale * factor).clamp(0.1, 500.);
                self.center = Point::new(
                    before.x - delta.x as f64 / self.scale,
                    before.y - delta.y as f64 / self.scale,
                );
            }
        }
        if response.clicked()
            && let Some(mouse) = response.interact_pointer_pos()
        {
            let world = Point::new(
                self.center.x + (mouse.x - rect.center().x) as f64 / self.scale,
                self.center.y + (mouse.y - rect.center().y) as f64 / self.scale,
            );
            self.select_at(world, 6. / self.scale);
        }
        if self.gpu_camera {
            if let Some(scene) = self.poll_scene(ui.ctx()) {
                let highlight = self.poll_net(ui.ctx());
                painter.add(super::cad_gpu::callback(
                    ui.ctx(),
                    rect,
                    scene,
                    None,
                    highlight,
                    super::cad_gpu::View {
                        glow_time: None,
                        center: self.center,
                        scale: self.scale as f32,
                        tint: Color32::WHITE,
                        background,
                        grayscale_base: self.selected_net.is_some()
                            || self.selected_footprint.is_some(),
                    },
                ));
            } else {
                if self.gpu_error.is_none() {
                    egui::Spinner::new().paint_at(
                        ui,
                        egui::Rect::from_center_size(
                            rect.center() - egui::vec2(0., 32.),
                            egui::vec2(28., 28.),
                        ),
                    );
                }
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    self.gpu_error.as_deref().unwrap_or("Preparing PCB view…"),
                    egui::FontId::proportional(16.),
                    theme::MUTED,
                );
            }
        } else {
            self.gpu_task = None;
            if self.net_task.take().is_some() {
                self.net_config = None;
            }
            self.paint_content(ui, rect, &painter);
        }
        for footprint in self
            .board
            .footprint_list
            .iter()
            .filter(|f| self.selected_footprints.contains(&f.reference))
        {
            let b = footprint.bounds;
            let screen = |p: Point| {
                rect.center()
                    + egui::vec2(
                        ((p.x - self.center.x) * self.scale) as f32,
                        ((p.y - self.center.y) * self.scale) as f32,
                    )
            };
            let area = egui::Rect::from_two_pos(screen(b.min), screen(b.max)).expand(3.);
            let pulse = (ui.input(|i| i.time) * 3.).sin() as f32 * 0.25 + 0.75;
            for (spread, alpha) in [(10., 7.), (7., 12.), (4., 23.), (1., 80.)] {
                painter.rect_stroke(
                    area.expand(spread),
                    5.,
                    egui::Stroke::new(
                        3.,
                        Color32::from_rgba_unmultiplied(165, 75, 245, (alpha * pulse) as u8),
                    ),
                    egui::StrokeKind::Outside,
                );
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
        if let Some(start) = self.blink_start {
            let elapsed = ui.input(|i| i.time) - start;
            if elapsed < 1. {
                ui.ctx().request_repaint();
                if ((elapsed * 6.).floor() as u32).is_multiple_of(2) {
                    let screen = |p: Point| {
                        rect.center()
                            + egui::vec2(
                                ((p.x - self.center.x) * self.scale) as f32,
                                ((p.y - self.center.y) * self.scale) as f32,
                            )
                    };
                    for item in &self.board.items {
                        let selected = self.selected_footprints.contains(&item.reference)
                            || self.is_net_selected(&item.net);
                        if !selected
                            || item.zone
                            || !item.layers.iter().any(|l| !self.hidden.contains(l))
                        {
                            continue;
                        }
                        if let PcbShape::Path { points, width, .. } = &item.shape {
                            painter.add(egui::Shape::line(
                                points.iter().copied().map(screen).collect(),
                                egui::Stroke::new(
                                    (*width * self.scale).max(1.) as f32 + 1.,
                                    Color32::YELLOW,
                                ),
                            ));
                        }
                    }
                    for via in &self.board.vias {
                        if self.is_net_selected(&via.net)
                            && via.layers.iter().any(|l| !self.hidden.contains(l))
                        {
                            painter.circle_stroke(
                                screen(via.at),
                                (via.size * self.scale * 0.5) as f32,
                                egui::Stroke::new(2., Color32::YELLOW),
                            );
                        }
                    }
                }
            }
        }
        let selection = self
            .selected_footprint
            .as_ref()
            .map(|_| {
                format!(
                    "{}: {}",
                    if self.selected_footprints.len() > 1 {
                        "Footprints"
                    } else {
                        "Footprint"
                    },
                    self.selected_footprints
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .or_else(|| {
                self.selected_net.as_ref().map(|name| {
                    if name.starts_with("Net class: ") || name.starts_with("Net group: ") {
                        name.clone()
                    } else {
                        format!("Net: {name}")
                    }
                })
            });
        let details = self
            .details
            .as_ref()
            .filter(|details| self.single_footprint() == Some(details.reference.as_str()))
            .cloned();
        if let Some(label) = selection {
            egui::Area::new(egui::Id::new(("pcb_net", &self.key)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.left_top() + egui::vec2(12., 12.))
                .movable(false)
                .constrain_to(rect)
                .show(ui.ctx(), |ui| {
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .stroke(egui::Stroke::new(1., Color32::from_gray(65)))
                        .corner_radius(10)
                        .inner_margin(8)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6., 3.);
                            ui.spacing_mut().interact_size.y = 20.;
                            ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.));
                            ui.set_max_width((rect.width() - 250.).clamp(120., 280.));
                            ui.horizontal(|ui| {
                                if theme::selection_lamp(ui, true).clicked() {
                                    self.blink_start = Some(ui.input(|i| i.time));
                                    ui.ctx().request_repaint();
                                }
                                ui.add(
                                    egui::Label::new(RichText::new(label).color(theme::YELLOW))
                                        .truncate(),
                                );
                                if ui.small_button("Clear").clicked() {
                                    self.select_net(None);
                                    self.footprint_change = Some(None);
                                }
                            });
                            if let Some(details) = &details
                                && let Some(target) = details.show(ui)
                            {
                                self.part_navigation = Some(target);
                            }
                            if self.net_task.is_some() {
                                ui.label(
                                    RichText::new(if self.selected_footprint.is_some() {
                                        "Preparing footprint…"
                                    } else {
                                        "Preparing net…"
                                    })
                                    .size(11.)
                                    .color(theme::MUTED),
                                );
                            }
                            if let Some(error) = &self.net_error {
                                ui.colored_label(Color32::LIGHT_RED, error);
                            }
                        });
                });
        }
        egui::Area::new(egui::Id::new(("pcb_layers", &self.key)))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(rect.right() - 196., rect.top() + 12.))
            .movable(false)
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .stroke(egui::Stroke::new(1., Color32::from_gray(65)))
                    .corner_radius(10)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_width(168.);
                        ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.));
                        let spacing = ui.spacing_mut();
                        spacing.item_spacing = egui::vec2(5., 3.);
                        spacing.interact_size = egui::vec2(16., 18.);
                        spacing.button_padding = egui::vec2(5., 2.);
                        spacing.icon_width = 13.;
                        spacing.icon_width_inner = 8.;
                        spacing.icon_spacing = 4.;
                        spacing.slider_width = 108.;
                        ui.horizontal(|ui| {
                            ui.strong("PCB Layers");
                            if ui.small_button("Fit").clicked() {
                                self.fit = true;
                            }
                        });
                        ui.label(
                            RichText::new(format!("{} footprints · Dimmed", self.board.footprints))
                                .size(11.)
                                .color(theme::MUTED),
                        );
                        ui.checkbox(&mut self.gpu_camera,RichText::new("GPU camera").size(12.))
                            .on_hover_text("Cache board geometry on the GPU. Zoom and pan update a camera transform; text, vias and pad labels stay on the GPU too. Unchanged views reuse the last frame.");
                        ui.label(RichText::new("Inactive layer opacity").size(12.));
                        ui.add(egui::Slider::new(&mut self.inactive_opacity, 0.0..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.)));
                        ui.checkbox(&mut self.show_nets, RichText::new("Net names / pad numbers").size(12.));
                        ui.checkbox(&mut self.show_clearance, RichText::new("Pad clearance").size(12.))
                            .on_hover_text("Pad override → footprint override → net class. Custom DRC rules are not evaluated.");
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_salt("pcb_layers_list")
                            .max_height((rect.height() - 246.).max(60.))
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for layer in &self.board.layers {
                                    ui.horizontal(|ui| {
                                        let mut visible = !self.hidden.contains(&layer.name);
                                        if ui.checkbox(&mut visible, "").changed() {
                                            if visible {
                                                self.hidden.remove(&layer.name);
                                            } else {
                                                self.hidden.insert(layer.name.clone());
                                            }
                                        }
                                        let (swatch, _) = ui.allocate_exact_size(
                                            egui::vec2(10., 10.),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().rect_filled(
                                            swatch,
                                            2,
                                            layer_color(&layer.name),
                                        );
                                        if ui
                                            .selectable_label(
                                                self.active == layer.name,
                                                &layer.name,
                                            )
                                            .clicked()
                                        {
                                            self.active = layer.name.clone();
                                            self.hidden.remove(&layer.name);
                                        }
                                    });
                                }
                            });
                        if !self.board.warnings.is_empty() {
                            ui.label(
                                RichText::new(format!(
                                    "{} unsupported items",
                                    self.board.warnings.len()
                                ))
                                .size(11.)
                                .color(theme::MUTED),
                            )
                            .on_hover_text(self.board.warnings.join("\n"));
                        }
                    });
            });
    }
    pub(super) fn apply_search_match(
        &mut self,
        found: PcbMatch,
        rect: egui::Rect,
        ctx: &egui::Context,
    ) {
        if let Some(reference) = found.reference {
            if let Some(footprint) = self
                .board
                .footprint_list
                .iter()
                .find(|f| f.reference == reference)
            {
                self.active = footprint.layer.clone();
                self.hidden.remove(&footprint.layer);
            }
            self.select_footprint(Some(reference));
            self.focus_footprints = true;
        } else {
            self.select_net(Some(found.title));
            let mut bounds = egui::Rect::NOTHING;
            for item in &self.board.items {
                if self.is_net_selected(&item.net)
                    && let PcbShape::Path { points, .. } = &item.shape
                {
                    for point in points {
                        bounds.extend_with(egui::pos2(point.x as f32, point.y as f32));
                    }
                }
            }
            for via in &self.board.vias {
                if self.is_net_selected(&via.net) {
                    bounds.extend_with(egui::pos2(via.at.x as f32, via.at.y as f32));
                }
            }
            if bounds.is_finite() {
                self.center = Point::new(bounds.center().x as f64, bounds.center().y as f64);
                self.scale = ((rect.width() - 240.) / bounds.width().max(5.))
                    .min((rect.height() - 100.) / bounds.height().max(5.))
                    .clamp(0.1, 45.) as f64;
                self.fit = false;
            }
        }
        self.blink_selection(ctx.input(|i| i.time));
        ctx.request_repaint();
    }
    fn select_net(&mut self, net: Option<String>) {
        self.blink_start = None;
        self.net_change = Some(net.clone());
        self.selected_footprints.clear();
        if self.selected_footprint.take().is_some() {
            self.net_config = None;
            self.net_scene = None;
            self.net_task = None;
        }
        if self.selected_net == net {
            return;
        }
        self.selected_nets = net.iter().cloned().collect();
        self.selected_net = net;
        self.net_config = None;
        self.net_scene = None;
        self.net_task = None;
        self.net_error = None;
    }
    pub(super) fn select_net_group(&mut self, label: String, nets: BTreeSet<String>) {
        self.select_schematic_net(Some(label));
        if self.selected_nets != nets {
            self.net_config = None;
            self.net_scene = None;
            self.net_task = None;
        }
        self.selected_nets = nets;
    }
    pub(super) fn selected_nets(&self) -> &BTreeSet<String> {
        &self.selected_nets
    }
    fn is_net_selected(&self, net: &str) -> bool {
        self.selected_nets.contains(net)
    }
    pub(super) fn select_schematic_net(&mut self, net: Option<String>) {
        self.select_net(net);
        // Cross-selection is not a new PCB click; do not echo it back as a blink.
        self.net_change = None;
        self.footprint_change = None;
    }
    #[cfg(test)]
    pub(super) fn select_schematic_footprint(&mut self, reference: Option<String>) {
        self.select_footprint(reference);
        self.footprint_change = None;
        self.net_change = None;
    }
    pub(super) fn select_schematic_footprints(
        &mut self,
        references: BTreeSet<String>,
        focus: bool,
    ) {
        let changed = self.selected_footprints != references;
        self.select_footprint(references.iter().next().cloned());
        self.selected_footprints = references;
        for footprint in self
            .board
            .footprint_list
            .iter()
            .filter(|f| self.selected_footprints.contains(&f.reference))
        {
            self.hidden.remove(&footprint.layer);
        }
        if changed {
            self.net_config = None;
            self.net_scene = None;
            self.net_task = None;
        }
        self.focus_footprints = focus;
        self.footprint_change = None;
        self.net_change = None;
    }
    fn select_footprint(&mut self, reference: Option<String>) {
        self.blink_start = None;
        let was_multiple = self.selected_footprints.len() > 1;
        self.selected_footprints = reference.iter().cloned().collect();
        self.footprint_change = Some(reference.clone());
        self.net_change = None;
        let had_net = self.selected_net.take().is_some();
        self.selected_nets.clear();
        if self.selected_footprint == reference && !had_net && !was_multiple {
            return;
        }
        self.selected_footprint = reference;
        self.net_config = None;
        self.net_scene = None;
        self.net_task = None;
        self.net_error = None;
        if let Some(footprint) = self.selected_footprint.as_ref().and_then(|name| {
            self.board
                .footprint_list
                .iter()
                .find(|f| &f.reference == name)
        }) {
            self.active = footprint.layer.clone();
            self.hidden.remove(&self.active);
        }
    }
    pub(super) fn take_footprint_change(&mut self) -> Option<Option<String>> {
        self.footprint_change.take()
    }
    fn select_at(&mut self, point: Point, tolerance: f64) {
        if let Some(net) = self.net_at_kind(point, tolerance, true) {
            self.select_net(Some(net));
        } else if let Some(reference) = self.footprint_at(point) {
            self.select_footprint(Some(reference));
        } else if let Some(net) = self.net_at(point, tolerance) {
            self.select_net(Some(net));
        }
    }
    fn footprint_at(&self, point: Point) -> Option<String> {
        let side = if self.active.starts_with("F.") {
            "F.Cu"
        } else if self.active.starts_with("B.") {
            "B.Cu"
        } else {
            return None;
        };
        if self.hidden.contains(&self.active) {
            return None;
        }
        self.board
            .footprint_list
            .iter()
            .filter(|f| !f.reference.is_empty() && f.layer == side && f.bounds.contains(point))
            .min_by(|a, b| {
                (a.bounds.width() * a.bounds.height())
                    .total_cmp(&(b.bounds.width() * b.bounds.height()))
            })
            .map(|f| f.reference.clone())
    }
    pub(super) fn selected_footprints(&self) -> &BTreeSet<String> {
        &self.selected_footprints
    }
    #[cfg(test)]
    pub(super) fn selected_footprint(&self) -> Option<&str> {
        self.selected_footprint.as_deref()
    }
    pub(super) fn selected_net(&self) -> Option<&str> {
        self.selected_net.as_deref()
    }
    pub(super) fn blink_selection(&mut self, now: f64) {
        self.blink_start = Some(now);
    }
    pub(super) fn take_net_change(&mut self) -> Option<Option<String>> {
        self.net_change.take()
    }
    pub(super) fn set_class_colors(&mut self, colors: BTreeMap<String, [u8; 3]>) {
        if self.class_colors != colors {
            self.class_colors = colors;
            self.class_config = None;
            self.class_task = None;
            self.class_scene = None;
        }
    }
    fn class_copper_color(&self, item: &inventory_kicad::PcbItem) -> Option<Color32> {
        if self.selected_net.is_none() && self.selected_footprint.is_none() && is_net_copper(item) {
            self.class_colors
                .get(&item.net)
                .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
        } else {
            None
        }
    }
    fn clearance_color(&self, net: &str) -> Color32 {
        let color = if self.selected_net.is_none() && self.selected_footprint.is_none() {
            self.class_colors
                .get(net)
                .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
        } else {
            None
        }
        .unwrap_or_else(|| layer_color(&self.active));
        self.net_color(net, color.gamma_multiply(0.65))
    }
    fn net_color(&self, net: &str, color: Color32) -> Color32 {
        if self.selected_footprint.is_some()
            || self
                .selected_net
                .as_ref()
                .is_some_and(|_| !self.is_net_selected(net))
        {
            let gray =
                (0.2126 * color.r() as f32 + 0.7152 * color.g() as f32 + 0.0722 * color.b() as f32)
                    .round() as u8;
            Color32::from_rgba_premultiplied(gray, gray, gray, color.a())
        } else {
            color
        }
    }
    fn item_color(&self, reference: &str, net: &str, color: Color32) -> Color32 {
        if self.selected_footprints.contains(reference) {
            color
        } else {
            self.net_color(net, color)
        }
    }
    fn net_at(&self, point: Point, tolerance: f64) -> Option<String> {
        self.net_at_kind(point, tolerance, false)
    }
    fn net_at_kind(&self, point: Point, tolerance: f64, routed_only: bool) -> Option<String> {
        let mut best: Option<((u8, bool, u64), &str)> = None;
        for item in &self.board.items {
            if item.net.is_empty()
                || !item
                    .layers
                    .iter()
                    .any(|l| l.ends_with(".Cu") && !self.hidden.contains(l))
            {
                continue;
            }
            let PcbShape::Path {
                points,
                width,
                filled,
                ..
            } = &item.shape
            else {
                continue;
            };
            if routed_only
                && (item.zone
                    || !item.reference.is_empty()
                    || *filled
                    || !item.layers.contains(&self.active))
            {
                continue;
            }
            let distance = if *filled && polygon_contains(points, point) {
                0.
            } else {
                points
                    .windows(2)
                    .map(|p| segment_distance(point, p[0], p[1]))
                    .fold(f64::INFINITY, f64::min)
            };
            if distance > tolerance + if *filled { 0. } else { width / 2. } {
                continue;
            }
            let rank = (
                if item.zone {
                    3
                } else if item.reference.is_empty() {
                    1
                } else {
                    2
                },
                !item.layers.contains(&self.active),
                distance.to_bits(),
            );
            if best.as_ref().is_none_or(|(old, _)| rank < *old) {
                best = Some((rank, &item.net));
            }
        }
        for via in &self.board.vias {
            if via.net.is_empty() || !via.layers.iter().any(|l| !self.hidden.contains(l)) {
                continue;
            }
            let distance = (via.at.x - point.x).hypot(via.at.y - point.y);
            if distance <= via.size * 0.5 + tolerance {
                let rank = (
                    0,
                    !via.layers.contains(&self.active),
                    (distance - via.size * 0.5).max(0.).to_bits(),
                );
                if best.as_ref().is_none_or(|(old, _)| rank < *old) {
                    best = Some((rank, &via.net));
                }
            }
        }
        best.map(|(_, net)| net.to_owned())
    }
    fn poll_net(&mut self, ctx: &egui::Context) -> Option<Arc<super::cad_gpu::Scene>> {
        let footprint = self.selected_footprint.clone();
        let name = if footprint.is_some() {
            self.selected_footprints
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\0")
        } else {
            self.selected_net.clone()?
        };
        let settings = self.settings();
        let config = (
            format!(
                "{}:{name}",
                if footprint.is_some() {
                    "footprint"
                } else {
                    "net"
                }
            ),
            settings.clone(),
        );
        if self.net_config.as_ref() != Some(&config) {
            self.net_scene = None;
            self.net_error = None;
            let request = if footprint.is_some() {
                super::cad_jobs::Request::PcbFootprints {
                    board: self.board.clone(),
                    settings,
                    references: self.selected_footprints.clone(),
                }
            } else {
                super::cad_jobs::Request::PcbNet {
                    board: self.board.clone(),
                    settings,
                    net: name,
                    nets: self.selected_nets.clone(),
                }
            };
            self.net_task = Some(super::cad_jobs::spawn(request, ctx));
            self.net_config = Some(config);
        }
        if let Some(result) = self.net_task.as_ref().and_then(|task| task.poll()) {
            self.net_task = None;
            match result {
                Ok(scene) => self.net_scene = Some(scene),
                Err(error) => self.net_error = Some(error),
            }
        }
        self.net_scene.clone()
    }
    pub(super) fn prepare_net(
        board: Arc<Pcb>,
        mut settings: super::cad_jobs::PcbSettings,
        nets: &BTreeSet<String>,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<Arc<super::cad_gpu::Scene>> {
        // Filter on the worker. Keep the large base scene and its GPU buffers intact.
        let selected = Pcb {
            copper_depths: board.copper_depths.clone(),
            thickness: board.thickness,
            embedded_files: BTreeMap::new(),
            items: board
                .items
                .iter()
                .filter(|item| nets.contains(&item.net))
                .cloned()
                .collect(),
            pads: board
                .pads
                .iter()
                .filter(|pad| nets.contains(&pad.net))
                .cloned()
                .collect(),
            vias: board
                .vias
                .iter()
                .filter(|via| nets.contains(&via.net))
                .cloned()
                .collect(),
            holes: board
                .holes
                .iter()
                .filter(|hole| {
                    hole.first().is_some_and(|p| {
                        board.pads.iter().any(|pad| {
                            nets.contains(&pad.net)
                                && (pad.at.x - p.x).hypot(pad.at.y - p.y)
                                    <= pad.size.x.hypot(pad.size.y)
                                && polygon_contains(&pad.contour, *p)
                        })
                    })
                })
                .cloned()
                .collect(),
            layers: board.layers.clone(),
            net_names: BTreeMap::new(),
            bounds: board.bounds,
            footprints: 0,
            footprint_list: vec![],
            warnings: vec![],
        };
        settings.opacity = 1.;
        Self::prepare(Arc::new(selected), settings, cancel)
    }
    #[cfg(test)]
    pub(super) fn prepare_footprint(
        board: Arc<Pcb>,
        settings: super::cad_jobs::PcbSettings,
        reference: &str,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<Arc<super::cad_gpu::Scene>> {
        Self::prepare_footprints(
            board,
            settings,
            BTreeSet::from([reference.to_owned()]),
            cancel,
        )
    }
    pub(super) fn prepare_footprints(
        board: Arc<Pcb>,
        mut settings: super::cad_jobs::PcbSettings,
        references: BTreeSet<String>,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<Arc<super::cad_gpu::Scene>> {
        let selected = Pcb {
            copper_depths: board.copper_depths.clone(),
            thickness: board.thickness,
            embedded_files: BTreeMap::new(),
            items: board
                .items
                .iter()
                .filter(|item| references.contains(&item.reference))
                .cloned()
                .collect(),
            pads: board
                .pads
                .iter()
                .filter(|pad| references.contains(&pad.reference))
                .cloned()
                .collect(),
            vias: vec![],
            holes: board
                .holes
                .iter()
                .filter(|hole| {
                    hole.first().is_some_and(|p| {
                        board.pads.iter().any(|pad| {
                            references.contains(&pad.reference)
                                && (pad.at.x - p.x).hypot(pad.at.y - p.y)
                                    <= pad.size.x.hypot(pad.size.y)
                                && polygon_contains(&pad.contour, *p)
                        })
                    })
                })
                .cloned()
                .collect(),
            layers: board.layers.clone(),
            net_names: BTreeMap::new(),
            bounds: board.bounds,
            footprints: 1,
            footprint_list: vec![],
            warnings: vec![],
        };
        settings.opacity = 1.;
        Self::prepare(Arc::new(selected), settings, cancel)
    }
    fn settings(&self) -> super::cad_jobs::PcbSettings {
        super::cad_jobs::PcbSettings {
            active: self.active.clone(),
            hidden: self.hidden.clone(),
            opacity: self.inactive_opacity,
            labels: self.show_nets,
            clearance: self.show_clearance,
            net_colors: Default::default(),
        }
    }
    fn poll_scene(&mut self, ctx: &egui::Context) -> Option<Arc<super::cad_gpu::Scene>> {
        let key = (
            Arc::as_ptr(&self.board) as usize,
            self.active.clone(),
            self.hidden.clone(),
            self.inactive_opacity.to_bits(),
            self.show_nets,
            self.show_clearance,
        );
        let now = ctx.input(|i| i.time);
        if self.gpu_key.as_ref() != Some(&key) {
            self.gpu_task = None;
            self.camera_scene = None;
            self.gpu_error = None;
            self.gpu_wait_until = now + if self.gpu_key.is_some() { 0.12 } else { 0. };
            self.gpu_pending = true;
            self.gpu_key = Some(key);
        }
        if self.gpu_pending && self.gpu_task.is_none() && self.gpu_error.is_none() {
            if now >= self.gpu_wait_until {
                self.gpu_task = Some(super::cad_jobs::spawn(
                    super::cad_jobs::Request::Pcb {
                        board: self.board.clone(),
                        settings: self.settings(),
                    },
                    ctx,
                ));
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
        if let Some(result) = self.gpu_task.as_ref().and_then(|t| t.poll()) {
            self.gpu_task = None;
            self.gpu_pending = false;
            match result {
                Ok(scene) => self.gpu_scene = Some(scene),
                Err(e) => self.gpu_error = Some(e),
            }
        }
        if self.selected_net.is_none()
            && self.selected_footprint.is_none()
            && !self.class_colors.is_empty()
        {
            let mut settings = self.settings();
            settings.net_colors = self.class_colors.clone();
            let config = (Arc::as_ptr(&self.board) as usize, settings.clone());
            if self.class_config.as_ref() != Some(&config) {
                self.class_scene = None;
                self.class_task = Some(super::cad_jobs::spawn(
                    super::cad_jobs::Request::Pcb {
                        board: self.board.clone(),
                        settings,
                    },
                    ctx,
                ));
                self.class_config = Some(config);
            }
            if let Some(result) = self.class_task.as_ref().and_then(|t| t.poll()) {
                self.class_task = None;
                match result {
                    Ok(scene) => self.class_scene = Some(scene),
                    Err(error) => self.gpu_error = Some(error),
                }
            }
            return self.class_scene.clone().or_else(|| self.gpu_scene.clone());
        }
        self.gpu_scene.clone()
    }
    pub(super) fn prepare(
        board: Arc<Pcb>,
        settings: super::cad_jobs::PcbSettings,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<Arc<super::cad_gpu::Scene>> {
        let mut viewer = Self::new("prepare", "prepare", board);
        viewer.active = settings.active;
        viewer.hidden = settings.hidden;
        viewer.inactive_opacity = settings.opacity;
        viewer.show_nets = settings.labels;
        viewer.show_clearance = settings.clearance;
        viewer.class_colors = settings.net_colors;
        let ctx = egui::Context::default();
        let mut result = None;
        super::cad_gpu::with_cancel(cancel.clone(), || {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    result = Some(viewer.retained_scene(ui));
                });
            });
        });
        anyhow::ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "Preparation cancelled"
        );
        Ok(result.unwrap())
    }
    fn retained_scene(&mut self, ui: &egui::Ui) -> Arc<super::cad_gpu::Scene> {
        let key = (
            Arc::as_ptr(&self.board) as usize,
            self.active.clone(),
            self.hidden.clone(),
            self.inactive_opacity.to_bits(),
            self.show_nets,
            self.show_clearance,
        );
        if self.gpu_key.as_ref() == Some(&key) {
            return self.gpu_scene.as_ref().unwrap().clone();
        }
        let paths = super::pcb_layers::colored_scene(
            ui,
            &mut self.camera_scene,
            &self.board,
            &self.active,
            &self.hidden,
            self.inactive_opacity,
            &self.class_colors,
        );
        let mut builder = super::cad_gpu::Builder::default();
        for layer in &paths.meshes {
            builder.mesh_layer(layer);
        }
        let (center, scale) = (self.center, self.scale);
        self.center = Point::default();
        self.scale = super::cad_gpu::REFERENCE as f64;
        let rect = egui::Rect::from_center_size(egui::Pos2::ZERO, egui::Vec2::splat(1e8));
        builder.capture(ui, false, |painter| self.paint_content(ui, rect, painter));
        self.center = center;
        self.scale = scale;
        if self.show_clearance
            && !self.hidden.contains(&self.active)
            && self.active.ends_with(".Cu")
        {
            for pad in &self.board.pads {
                if super::cad_gpu::cancelled() {
                    break;
                }
                if pad.layers.contains(&self.active) && pad.clearance > 0. {
                    let mut points = pad.clearance_contour.clone();
                    if let Some(first) = points.first().copied() {
                        points.push(first);
                    }
                    builder.line(
                        &points,
                        0.,
                        0.8,
                        self.clearance_color(&pad.net),
                        1. / pad.clearance as f32,
                    );
                }
            }
        }
        let scene = builder.finish();
        self.camera_scene = None;
        self.gpu_scene = Some(scene.clone());
        self.gpu_pending = false;
        self.gpu_key = Some(key);
        scene
    }
    fn paint_content(&mut self, ui: &egui::Ui, rect: egui::Rect, painter: &egui::Painter) {
        // Cull in board coordinates before generating any screen geometry or glyphs.
        let background = Color32::from_rgb(18, 21, 24);
        let capturing = super::cad_gpu::capturing();
        if !capturing && self.item_bounds.len() != self.board.items.len() {
            self.item_bounds = board_bounds(&self.board);
        }
        let visible_world = if capturing {
            egui::Rect::EVERYTHING
        } else {
            egui::Rect::from_center_size(
                egui::pos2(self.center.x as f32, self.center.y as f32),
                rect.size() / self.scale as f32,
            )
            .expand(2. / self.scale as f32)
        };
        let screen = |p: Point| {
            rect.center()
                + egui::vec2(
                    ((p.x - self.center.x) * self.scale) as f32,
                    ((p.y - self.center.y) * self.scale) as f32,
                )
        };
        let inactive_slot = painter.add(egui::Shape::Noop);
        let mut inactive_layers: BTreeMap<String, Vec<egui::Shape>> = BTreeMap::new();
        // Active layer is painted last, with other visible layers dimmed against the canvas.
        for zones in [true, false] {
            for active in [false, true] {
                for (index, item) in self.board.items.iter().enumerate() {
                    if super::cad_gpu::cancelled() {
                        return;
                    }
                    if !capturing && !visible_world.intersects(self.item_bounds[index]) {
                        continue;
                    }
                    if self.gpu_camera && matches!(item.shape, PcbShape::Path { .. }) {
                        continue;
                    }
                    if item.zone != zones {
                        continue;
                    }
                    let visible: Vec<_> = item
                        .layers
                        .iter()
                        .filter(|l| !self.hidden.contains(*l))
                        .collect();
                    if visible.is_empty() {
                        continue;
                    }
                    let is_active = visible.iter().any(|l| **l == self.active);
                    if is_active != active {
                        continue;
                    }
                    let layer = if is_active { &self.active } else { visible[0] };
                    let mut color = self.item_color(&item.reference, &item.net, layer_color(layer));
                    let class_color = layer
                        .ends_with(".Cu")
                        .then(|| self.class_copper_color(item))
                        .flatten();
                    if let Some(class_color) = class_color {
                        color = class_color;
                    }
                    if item.zone {
                        color = color.gamma_multiply(0.4);
                    }
                    match &item.shape {
                        PcbShape::Path {
                            points,
                            width,
                            filled,
                            triangles,
                        } => {
                            let mut points: Vec<_> = points.iter().map(|p| screen(*p)).collect();
                            if *filled {
                                let mut mesh = egui::Mesh::default();
                                for point in &points {
                                    mesh.colored_vertex(*point, color);
                                }
                                for triangle in triangles {
                                    mesh.add_triangle(triangle[0], triangle[1], triangle[2]);
                                }
                                if is_active
                                    || self.is_net_selected(&item.net)
                                    || self.selected_footprints.contains(&item.reference)
                                {
                                    painter.add(mesh);
                                } else {
                                    inactive_layers
                                        .entry(layer.to_owned())
                                        .or_default()
                                        .push(mesh.into());
                                }
                            } else {
                                if points.len() > 1 {
                                    let shapes = rounded_path(
                                        std::mem::take(&mut points),
                                        egui::Stroke::new(
                                            (*width * self.scale).max(0.7) as f32,
                                            color,
                                        ),
                                    );
                                    if is_active
                                        || self.is_net_selected(&item.net)
                                        || self.selected_footprints.contains(&item.reference)
                                    {
                                        painter.extend(shapes);
                                    } else {
                                        inactive_layers
                                            .entry(layer.to_owned())
                                            .or_default()
                                            .extend(shapes);
                                    }
                                }
                            }
                        }
                        PcbShape::Text {
                            at,
                            text,
                            size,
                            angle,
                            mirrored,
                        } => {
                            if !is_active
                                && !self.is_net_selected(&item.net)
                                && !self.selected_footprints.contains(&item.reference)
                            {
                                color = color.gamma_multiply(self.inactive_opacity);
                            }
                            let at_screen = screen(*at);
                            let extent = (*size * self.scale) as f32
                                * (text.chars().count().max(1) as f32 + 2.);
                            if !rect.intersects(egui::Rect::from_center_size(
                                at_screen,
                                egui::Vec2::splat(extent * 2.),
                            )) {
                                continue;
                            }
                            if *size * self.scale < 3. && !capturing {
                                continue;
                            }
                            let galley = zoom_galley(
                                painter,
                                text,
                                (*size * self.scale) as f32,
                                128.,
                                color,
                            );
                            let mut shape = egui::epaint::TextShape::new(
                                screen(*at) - galley.size() * 0.5,
                                galley.clone(),
                                color,
                            )
                            .with_angle_and_anchor(
                                -(*angle as f32).to_radians(),
                                egui::Align2::CENTER_CENTER,
                            );
                            if *mirrored {
                                for row in &mut Arc::make_mut(&mut shape.galley).rows {
                                    let visuals = &mut Arc::make_mut(&mut row.row).visuals;
                                    for vertex in &mut visuals.mesh.vertices {
                                        vertex.pos.x = galley.size().x - vertex.pos.x;
                                        if capturing {
                                            vertex.uv.x = -vertex.uv.x;
                                        }
                                    }
                                }
                            }
                            super::cad_gpu::tag_text(
                                &shape.galley,
                                (*size * self.scale) as f32,
                                3.,
                                f32::INFINITY,
                            );
                            if *mirrored && capturing {
                                super::cad_gpu::tag_mirror(&shape.galley);
                            }
                            painter.add(shape);
                        }
                    }
                }
            }
        }
        let geometry = if capturing {
            egui::Shape::Noop
        } else if self.gpu_camera {
            let scene = super::pcb_layers::cached_scene(
                ui,
                &mut self.camera_scene,
                &self.board,
                &self.active,
                &self.hidden,
                self.inactive_opacity,
            );
            super::pcb_layers::world_callback(rect, scene, self.center, self.scale as f32)
        } else {
            super::pcb_layers::callback(ui, rect, inactive_layers, self.inactive_opacity)
        };
        painter.set(inactive_slot, geometry);
        for hole in &self.board.holes {
            if !visible_world.intersects(world_bounds(hole)) {
                continue;
            }
            painter.add(egui::Shape::convex_polygon(
                hole.iter().map(|p| screen(*p)).collect(),
                background,
                egui::Stroke::NONE,
            ));
        }
        // Vias are a distinct plated object, rather than a copper disk with a black cutout.
        for via in &self.board.vias {
            if super::cad_gpu::cancelled() {
                return;
            }
            if !visible_world.intersects(egui::Rect::from_center_size(
                egui::pos2(via.at.x as f32, via.at.y as f32),
                egui::Vec2::splat(via.size as f32),
            )) {
                continue;
            }
            if !via.layers.iter().any(|l| !self.hidden.contains(l)) {
                continue;
            }
            let center = screen(via.at);
            let radius = (via.size * self.scale * 0.5) as f32;
            let active = via.layers.contains(&self.active) && !self.hidden.contains(&self.active);
            let mut ring = if active {
                Color32::from_rgb(225, 65, 75)
            } else {
                Color32::from_rgb(105, 85, 95)
            };
            if self.selected_net.is_none()
                && self.selected_footprint.is_none()
                && let Some(c) = self.class_colors.get(&via.net)
            {
                ring = Color32::from_rgb(c[0], c[1], c[2]);
            }
            let mut drill = if active {
                Color32::from_rgb(235, 200, 35)
            } else {
                Color32::from_rgb(115, 105, 60)
            };
            if !active {
                ring = ring.gamma_multiply(self.inactive_opacity);
                drill = drill.gamma_multiply(self.inactive_opacity);
            }
            ring = self.net_color(&via.net, ring);
            drill = self.net_color(&via.net, drill);
            painter.circle_filled(center, radius, ring);
            painter.circle_stroke(
                center,
                radius * 0.79,
                egui::Stroke::new((radius * 0.13).max(0.5), Color32::from_rgb(210, 215, 210)),
            );
            painter.circle_filled(center, (via.drill * self.scale * 0.5) as f32, drill);
            if self.show_nets && active && (radius >= 9. || capturing) && !via.net.is_empty() {
                let size = (radius * 1.5 / (via.net.chars().count() as f32 * 0.6).max(1.))
                    .min(radius * 0.9);
                if size >= 6. || capturing {
                    paint_net_label(
                        painter,
                        center,
                        egui::vec2(radius * 1.5, size * 1.2),
                        &via.net,
                        0.,
                        Color32::BLACK,
                    );
                }
            }
        }
        if !self.hidden.contains(&self.active) {
            for pad in &self.board.pads {
                if super::cad_gpu::cancelled() {
                    return;
                }
                if !visible_world.intersects(
                    world_bounds(&pad.clearance_contour).union(world_bounds(&pad.contour)),
                ) {
                    continue;
                }
                if !pad.layers.contains(&self.active) || !self.active.ends_with(".Cu") {
                    continue;
                }
                if !capturing
                    && self.show_clearance
                    && pad.clearance > 0.
                    && pad.clearance * self.scale >= 1.
                {
                    let mut contour: Vec<_> =
                        pad.clearance_contour.iter().map(|p| screen(*p)).collect();
                    if let Some(first) = contour.first().copied() {
                        contour.push(first);
                    }
                    painter.add(egui::Shape::line(
                        contour,
                        egui::Stroke::new(0.8, self.clearance_color(&pad.net)),
                    ));
                }
                if self.show_nets {
                    let (size, angle) = pad_label_layout(pad, self.scale);
                    let text = if pad.number.is_empty() {
                        pad.net.clone()
                    } else if pad.net.is_empty() {
                        pad.number.clone()
                    } else {
                        format!("{}\n{}", pad.number, pad.net)
                    };
                    paint_net_label(
                        painter,
                        screen(pad.at),
                        size,
                        &text,
                        angle,
                        self.item_color(&pad.reference, &pad.net, Color32::from_rgb(255, 215, 215)),
                    );
                }
            }
            if self.show_nets {
                let mut occupied: Vec<_> = self
                    .board
                    .pads
                    .iter()
                    .filter(|p| {
                        p.layers.contains(&self.active)
                            && visible_world.intersects(world_bounds(&p.contour))
                    })
                    .map(|p| {
                        egui::Rect::from_points(
                            &p.contour.iter().map(|p| screen(*p)).collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                occupied.extend(
                    self.board
                        .vias
                        .iter()
                        .filter(|v| {
                            v.layers.contains(&self.active)
                                && visible_world
                                    .expand(v.size as f32)
                                    .contains(egui::pos2(v.at.x as f32, v.at.y as f32))
                        })
                        .map(|v| {
                            egui::Rect::from_center_size(
                                screen(v.at),
                                egui::Vec2::splat((v.size * self.scale) as f32),
                            )
                        }),
                );
                let occupied = OccupiedGrid::new(occupied, (self.scale as f32 * 2.).max(1.));
                for (index, item) in self.board.items.iter().enumerate() {
                    if super::cad_gpu::cancelled() {
                        return;
                    }
                    if !capturing && !visible_world.intersects(self.item_bounds[index]) {
                        continue;
                    }
                    if item.net.is_empty() || !item.layers.contains(&self.active) {
                        continue;
                    }
                    if let PcbShape::Path {
                        points,
                        width,
                        filled: false,
                        ..
                    } = &item.shape
                    {
                        let segments: Vec<_> = points.windows(2).collect();
                        if segments.is_empty() {
                            continue;
                        }
                        let mid = segments[segments.len() / 2];
                        let a = screen(mid[0]);
                        let b = screen(mid[1]);
                        let length: f32 = points
                            .windows(2)
                            .map(|p| screen(p[0]).distance(screen(p[1])))
                            .sum();
                        let center = (a + b.to_vec2()) * 0.5;
                        let height = (*width * self.scale) as f32 * 0.75;
                        let font = height
                            .min(length * 0.8 / (item.net.chars().count().max(1) as f32 * 0.65))
                            .min(if capturing { f32::INFINITY } else { 48. });
                        let label_width = font * item.net.chars().count() as f32 * 0.65;
                        let angle = (b.y - a.y).atan2(b.x - a.x);
                        let label_rect = egui::Rect::from_center_size(
                            center,
                            egui::vec2(
                                label_width * angle.cos().abs() + font * angle.sin().abs(),
                                label_width * angle.sin().abs() + font * angle.cos().abs(),
                            ),
                        );
                        if !rect.intersects(label_rect) || occupied.intersects(label_rect) {
                            continue;
                        }
                        paint_net_label(
                            painter,
                            center,
                            egui::vec2(length * 0.8, (*width * self.scale) as f32 * 0.75),
                            &item.net,
                            (b.y - a.y).atan2(b.x - a.x),
                            self.item_color(
                                &item.reference,
                                &item.net,
                                Color32::from_rgb(255, 215, 215),
                            ),
                        );
                    }
                }
            }
        }
    }
}
pub(super) fn pcb_matches(board: &Pcb, query: &str) -> Vec<PcbMatch> {
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut results = Vec::new();
    for footprint in &board.footprint_list {
        let property = |names: &[&str]| {
            footprint
                .properties
                .iter()
                .find(|(key, value)| {
                    !value.trim().is_empty()
                        && names.iter().any(|name| {
                            key.chars()
                                .filter(|c| c.is_alphanumeric())
                                .flat_map(char::to_lowercase)
                                .eq(name
                                    .chars()
                                    .filter(|c| c.is_alphanumeric())
                                    .flat_map(char::to_lowercase))
                        })
                })
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let number = property(&["MPN", "Part Number", "Manufacturer Part Number"]);
        let spn = property(&["SPN", "Supplier Part Number", "LCSC", "LCSC Part"]);
        let value = property(&["Value"]);
        let package = property(&["Package", "Footprint"]);
        let package = if package.is_empty() {
            footprint.library.clone()
        } else {
            package
        };
        let fields = [
            footprint.reference.to_lowercase(),
            number.to_lowercase(),
            spn.to_lowercase(),
            value.to_lowercase(),
            package.to_lowercase(),
        ];
        if !terms
            .iter()
            .all(|term| fields.iter().any(|field| field.contains(term)))
        {
            continue;
        }
        let rank = terms
            .iter()
            .filter_map(|term| fields.iter().position(|field| field.contains(term)))
            .min()
            .unwrap_or(0);
        results.push((
            rank,
            PcbMatch {
                reference: Some(footprint.reference.clone()),
                title: footprint.reference.clone(),
                value: if value.is_empty() { number } else { value },
                package,
            },
        ));
    }
    let nets: BTreeSet<_> = if board.net_names.is_empty() {
        board
            .items
            .iter()
            .map(|item| &item.net)
            .chain(board.vias.iter().map(|via| &via.net))
            .filter(|name| !name.is_empty())
            .collect()
    } else {
        board
            .net_names
            .values()
            .filter(|name| !name.is_empty())
            .collect()
    };
    for name in nets {
        if terms.iter().all(|term| name.to_lowercase().contains(term)) {
            results.push((
                5,
                PcbMatch {
                    reference: None,
                    title: name.clone(),
                    value: String::new(),
                    package: String::new(),
                },
            ));
        }
    }
    results
        .sort_by(|(ar, a), (br, b)| ar.cmp(br).then_with(|| part_number_cmp(&a.title, &b.title)));
    results
        .into_iter()
        .take(20)
        .map(|(_, found)| found)
        .collect()
}

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / (dx * dx + dy * dy).max(1e-20)).clamp(0., 1.);
    (p.x - a.x - t * dx).hypot(p.y - a.y - t * dy)
}
fn polygon_contains(points: &[Point], p: Point) -> bool {
    let mut inside = false;
    for (a, b) in points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
    {
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}
pub(super) fn is_net_copper(item: &inventory_kicad::PcbItem) -> bool {
    !item.net.is_empty()
        && item.layers.iter().any(|l| l.ends_with(".Cu"))
        && matches!(&item.shape, PcbShape::Path { .. })
}
pub(super) fn layer_color(name: &str) -> Color32 {
    match name {
        "F.Cu" => Color32::from_rgb(220, 65, 65),
        "B.Cu" => Color32::from_rgb(75, 100, 235),
        "F.SilkS" => Color32::from_rgb(245, 245, 220),
        "B.SilkS" => Color32::from_rgb(215, 185, 235),
        "Edge.Cuts" => Color32::from_rgb(235, 220, 60),
        "F.Mask" | "B.Mask" => Color32::from_rgb(150, 65, 180),
        "F.Paste" | "B.Paste" => Color32::from_rgb(170, 180, 185),
        _ if name.ends_with(".Cu") => Color32::from_rgb(180, 165, 60),
        _ if name.ends_with(".CrtYd") => Color32::from_rgb(210, 70, 210),
        _ => Color32::from_rgb(100, 190, 180),
    }
}

// KiCad tracks are swept disks (round caps). egui's polyline has flat caps,
// which leave wedges when independently stored segments meet at an angle.
pub(super) fn rounded_path(points: Vec<egui::Pos2>, stroke: egui::Stroke) -> Vec<egui::Shape> {
    let mut shapes = Vec::with_capacity(3);
    if let (Some(first), Some(last)) = (points.first(), points.last())
        && first != last
    {
        let radius = stroke.width * 0.5;
        shapes.push(egui::Shape::circle_filled(*first, radius, stroke.color));
        shapes.push(egui::Shape::circle_filled(*last, radius, stroke.color));
    }
    shapes.push(egui::Shape::line(points, stroke));
    shapes
}

// Spatial lookup avoids comparing every track label with every pad/via on a large board.
struct OccupiedGrid {
    rects: Vec<egui::Rect>,
    cells: std::collections::HashMap<(i32, i32), Vec<usize>>,
    large: Vec<usize>,
    cell: f32,
}
impl OccupiedGrid {
    fn keys(&self, r: egui::Rect) -> (i32, i32, i32, i32) {
        (
            (r.min.x / self.cell).floor() as i32,
            (r.min.y / self.cell).floor() as i32,
            (r.max.x / self.cell).floor() as i32,
            (r.max.y / self.cell).floor() as i32,
        )
    }
    fn new(rects: Vec<egui::Rect>, cell: f32) -> Self {
        let mut grid = Self {
            rects,
            cells: Default::default(),
            large: vec![],
            cell,
        };
        for (i, r) in grid.rects.iter().enumerate() {
            let (x0, y0, x1, y1) = grid.keys(*r);
            if (x1 as i64 - x0 as i64 + 1) * (y1 as i64 - y0 as i64 + 1) > 256 {
                grid.large.push(i);
                continue;
            }
            for x in x0..=x1 {
                for y in y0..=y1 {
                    grid.cells.entry((x, y)).or_default().push(i);
                }
            }
        }
        grid
    }
    fn intersects(&self, r: egui::Rect) -> bool {
        let (x0, y0, x1, y1) = self.keys(r);
        if (x1 as i64 - x0 as i64 + 1) * (y1 as i64 - y0 as i64 + 1) > 256 {
            return self.rects.iter().any(|p| p.intersects(r));
        }
        self.large.iter().any(|&i| self.rects[i].intersects(r))
            || (x0..=x1).any(|x| {
                (y0..=y1).any(|y| {
                    self.cells
                        .get(&(x, y))
                        .is_some_and(|items| items.iter().any(|&i| self.rects[i].intersects(r)))
                })
            })
    }
}

fn board_bounds(board: &Pcb) -> Vec<egui::Rect> {
    board
        .items
        .iter()
        .map(|item| match &item.shape {
            PcbShape::Path { points, width, .. } => world_bounds(points).expand(*width as f32),
            PcbShape::Text { at, size, text, .. } => egui::Rect::from_center_size(
                egui::pos2(at.x as f32, at.y as f32),
                egui::Vec2::splat((*size * (text.chars().count().max(1) as f64 + 2.) * 2.) as f32),
            ),
        })
        .collect()
}

fn world_bounds(points: &[Point]) -> egui::Rect {
    let mut bounds = egui::Rect::NOTHING;
    for p in points {
        bounds.extend_with(egui::pos2(p.x as f32, p.y as f32));
    }
    bounds
}

fn pad_label_layout(pad: &inventory_kicad::PcbPad, scale: f64) -> (egui::Vec2, f32) {
    let mut size = egui::vec2((pad.size.x * scale) as f32, (pad.size.y * scale) as f32) * 0.85;
    let mut angle = -(pad.angle as f32).to_radians();
    if size.y > size.x {
        size = egui::vec2(size.y, size.x);
        angle -= std::f32::consts::FRAC_PI_2;
    }
    // Square pads have no longer axis; keep their number/net labels horizontal.
    if (pad.size.x - pad.size.y).abs() <= 1e-6 {
        angle = 0.;
    }
    (size, angle)
}
fn paint_net_label(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: egui::Vec2,
    text: &str,
    mut angle: f32,
    color: Color32,
) {
    if text.is_empty() {
        return;
    }
    let lines = text.lines().count().max(1) as f32;
    let columns = text
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let capturing = super::cad_gpu::capturing();
    let font = (size.y / (lines * 1.2))
        .min(size.x / (columns * 0.65))
        .min(if capturing { f32::INFINITY } else { 48. });
    if font < 7. && !capturing {
        return;
    }
    angle = (angle + std::f32::consts::FRAC_PI_2).rem_euclid(std::f32::consts::PI)
        - std::f32::consts::FRAC_PI_2;
    let galley = zoom_galley(painter, text, font, 48., color);
    super::cad_gpu::tag_text(&galley, font, 7., 48.);
    painter.add(
        egui::epaint::TextShape::new(center - galley.size() * 0.5, galley, color)
            .with_angle_and_anchor(angle, egui::Align2::CENTER_CENTER),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcb_autocomplete_matches_fields_ranks_designators_and_limits_results() {
        let mut board = Pcb::parse(
            r#"(kicad_pcb (layers (0 "F.Cu" signal))
            (net 1 "U1_NET")
            (footprint "Capacitor:0402" (layer "F.Cu") (at 5 5)
                (property "Reference" "U1") (property "Value" "100nF")
                (property "MPN" "ABC123") (property "SPN" "C12345")
                (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu"))))"#,
        )
        .unwrap();
        assert_eq!(
            pcb_matches(&board, "u1")[0].reference.as_deref(),
            Some("U1")
        );
        assert_eq!(pcb_matches(&board, "u1")[1].title, "U1_NET");
        for query in ["abc123 0402", "C12345", "100nf", "0402"] {
            assert_eq!(
                pcb_matches(&board, query)[0].reference.as_deref(),
                Some("U1")
            );
        }
        assert!(pcb_matches(&board, "missing").is_empty());
        board.net_names = (0..30)
            .map(|i| (i.to_string(), format!("NET_{i}")))
            .collect();
        assert_eq!(pcb_matches(&board, "net").len(), 20);
    }
    #[test]
    fn single_footprint_panel_shows_details_and_returns_the_correct_link_destination() {
        use super::super::footprint_details::{FootprintDetails, PartTarget};
        let board = Arc::new(
            Pcb::parse(
                r#"(kicad_pcb (layers (0 "F.Cu" signal))
            (footprint "LED" (layer "F.Cu") (at 5 5) (property "Reference" "D6")
                (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu"))))"#,
            )
            .unwrap(),
        );
        let mut viewer = PcbViewer::new("Demo", "test", board);
        viewer.gpu_camera = false;
        viewer.select_schematic_footprint(Some("D6".into()));
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let draw = |viewer: &mut PcbViewer, time, events| {
            ctx.run(
                egui::RawInput {
                    time: Some(time),
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800., 600.),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| viewer.ui(ui));
                },
            )
        };
        for (index, purchase) in [false, true].into_iter().enumerate() {
            viewer.set_details(Some(FootprintDetails {
                reference: "D6".into(),
                number: "LED-123".into(),
                value: "Red LED".into(),
                package: "0603".into(),
                tolerance: String::new(),
                targets: vec![PartTarget {
                    id: "specific-part".into(),
                    number: "LED-123".into(),
                    purchase,
                }],
            }));
            let time = index as f64;
            draw(&mut viewer, time, vec![]);
            let output = draw(&mut viewer, time + 0.1, vec![]);
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some(t),
                    _ => None,
                })
                .collect();
            assert!(text.iter().any(|t| t.galley.text() == "LED-123"));
            assert!(text.iter().any(|t| t.galley.text() == "Red LED"));
            assert!(text.iter().any(|t| t.galley.text() == "0603"));
            assert!(
                !text.iter().any(|t| t.galley.text() == "Tolerance"),
                "empty tolerance is omitted"
            );
            let label = if purchase {
                "Go to Purchase"
            } else {
                "Go to Part"
            };
            let link = text.iter().find(|t| t.galley.text() == label).unwrap();
            let title = text
                .iter()
                .find(|t| t.galley.text() == "Footprint: D6")
                .unwrap();
            assert!(
                link.pos.y - title.pos.y < 130.,
                "footprint metadata stays compact under the application theme"
            );
            assert!(
                link.pos.x < title.pos.x + 20.,
                "navigation remains left aligned"
            );
            let pos = link.pos + link.galley.size() * 0.5;
            draw(
                &mut viewer,
                time + 0.2,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            draw(
                &mut viewer,
                time + 0.3,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            );
            let target = viewer
                .take_part_navigation()
                .expect("metadata link produces navigation");
            assert_eq!(target.id, "specific-part");
            assert_eq!(target.purchase, purchase);
        }
        viewer.select_schematic_footprints(BTreeSet::from(["D6".into(), "D7".into()]), false);
        let output = draw(&mut viewer, 2., vec![]);
        assert!(
            !output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.text()=="LED-123")),
            "multi-selection does not show one footprint's metadata"
        );
    }
    #[test]
    fn square_exposed_pad_labels_are_horizontal_while_rectangular_labels_follow_orientation() {
        let board = Pcb::parse(
            r#"(kicad_pcb (layers (0 "F.Cu" signal))
            (footprint "QFN" (layer "F.Cu") (at 10 10 -90) (property "Reference" "IC1")
                (pad "49" smd roundrect (at 0 0 270) (size 5.6 5.6) (layers "F.Cu"))
                (pad "13" smd roundrect (at -2.75 3.375) (size 0.55 0.3) (layers "F.Cu"))
                (pad "1" smd roundrect (at -3.375 -2.75 90) (size 0.55 0.3) (layers "F.Cu"))))"#,
        )
        .unwrap();
        assert_eq!(pad_label_layout(&board.pads[0], 100.).1, 0.);
        assert_eq!(pad_label_layout(&board.pads[1], 100.).1, 0.);
        assert!(
            (pad_label_layout(&board.pads[2], 100.).1 + std::f32::consts::FRAC_PI_2).abs() < 1e-6
        );
    }
    #[test]
    fn footprint_selection_respects_side_and_clears_net_without_echo() {
        let board = Arc::new(
            Pcb::parse(
                r#"(kicad_pcb (version 20250114)
            (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
            (footprint "Front" (layer "F.Cu") (at 5 5) (property "Reference" "U1")
                (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu") (net "SIG")))
            (footprint "Back" (layer "B.Cu") (at 5 5) (property "Reference" "U2")
                (pad "1" smd rect (at 0 0) (size 2 2) (layers "B.Cu") (net "OTHER"))))"#,
            )
            .unwrap(),
        );
        let mut viewer = PcbViewer::new("Demo", "key", board);
        assert_eq!(
            viewer.footprint_at(Point::new(5., 5.)).as_deref(),
            Some("U1")
        );
        viewer.active = "B.Cu".into();
        assert_eq!(
            viewer.footprint_at(Point::new(5., 5.)).as_deref(),
            Some("U2")
        );
        viewer.hidden.insert("B.Cu".into());
        assert!(viewer.footprint_at(Point::new(5., 5.)).is_none());
        viewer.select_schematic_footprint(Some("U1".into()));
        assert_eq!(viewer.active, "F.Cu");
        assert_eq!(viewer.selected_footprint(), Some("U1"));
        assert!(viewer.take_footprint_change().is_none());
        viewer.gpu_camera = false;
        let ctx = egui::Context::default();
        let mut pulse_frames = Vec::new();
        for time in [0.1, 0.6] {
            let output = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800., 600.),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| viewer.ui(ui));
                },
            );
            let alphas: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.stroke.width == 3. && rect.stroke.color.a() < 100 =>
                    {
                        Some(rect.stroke.color.a())
                    }
                    _ => None,
                })
                .collect();
            assert!(!alphas.is_empty(), "footprint selection draws its glow");
            pulse_frames.push(alphas);
        }
        assert_ne!(
            pulse_frames[0], pulse_frames[1],
            "glow intensity animates without changing selection"
        );
        assert_eq!(viewer.item_color("U1", "SIG", Color32::RED), Color32::RED);
        let gray = viewer.item_color("U2", "OTHER", Color32::BLUE);
        assert_eq!(gray.r(), gray.g());
        assert_eq!(gray.g(), gray.b());
        viewer.select_net(Some("SIG".into()));
        assert!(viewer.selected_footprint().is_none());
        viewer.select_schematic_footprint(None);
        assert!(viewer.selected_net().is_none());
    }
    #[test]
    fn overlapping_selection_prioritizes_vias_then_tracks_then_footprints() {
        let board = Arc::new(
            Pcb::parse(
                r#"(kicad_pcb
            (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
            (net 1 "TRACK") (net 2 "VIA")
            (footprint "Demo" (layer "F.Cu") (at 5 5) (property "Reference" "U1")
                (pad "1" smd rect (at 0 0) (size 8 8) (layers "F.Cu")))
            (segment (start 2 5) (end 8 5) (width 0.5) (layer "F.Cu") (net 1))
            (segment (start 2 7) (end 12 7) (width 0.5) (layer "B.Cu") (net 1))
            (via (at 5 5) (size 1) (drill 0.4) (layers "F.Cu" "B.Cu") (net 2)))"#,
            )
            .unwrap(),
        );
        let mut viewer = PcbViewer::new("Demo", "overlap", board);
        viewer.select_at(Point::new(5., 5.), 0.1);
        assert_eq!(viewer.selected_net(), Some("VIA"));
        assert!(viewer.selected_footprint().is_none());
        viewer.select_at(Point::new(7., 5.), 0.1);
        assert_eq!(viewer.selected_net(), Some("TRACK"));
        viewer.select_at(Point::new(11., 7.), 0.1);
        assert_eq!(
            viewer.selected_net(),
            Some("TRACK"),
            "inactive traces remain selectable outside footprints"
        );
        viewer.select_at(Point::new(7., 7.), 0.1);
        assert_eq!(
            viewer.selected_footprint(),
            Some("U1"),
            "footprint wins over an inactive-layer trace"
        );
        assert!(viewer.selected_net().is_none());
        viewer.hidden.insert("F.Cu".into());
        viewer.hidden.insert("B.Cu".into());
        viewer.select_net(None);
        viewer.select_at(Point::new(5., 5.), 0.1);
        assert!(viewer.selected_net().is_none());
        assert!(viewer.selected_footprint().is_none());
    }
    fn selection_board() -> Arc<Pcb> {
        Arc::new(
            Pcb::parse(
                r#"(kicad_pcb (version 20250114)
            (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
            (net 1 "SIG") (net 2 "GND")
            (segment (start 0 0) (end 10 0) (width 0.5) (layer "F.Cu") (net 1))
            (segment (start 0 0) (end 0 10) (width 0.5) (layer "B.Cu") (net 2))
            (via (at 10 0) (size 1) (drill 0.4) (layers "F.Cu" "B.Cu") (net 1)))"#,
            )
            .unwrap(),
        )
    }
    #[test]
    fn net_click_prefers_active_copper_and_ignores_hidden_layers() {
        let mut viewer = PcbViewer::new("Demo", "key", selection_board());
        assert_eq!(
            viewer.net_at(Point::new(5., 0.2), 0.1).as_deref(),
            Some("SIG")
        );
        assert_eq!(
            viewer.net_at(Point::new(0., 0.), 0.1).as_deref(),
            Some("SIG")
        );
        assert_eq!(
            viewer.net_at(Point::new(0., 5.), 0.1).as_deref(),
            Some("GND")
        );
        assert_eq!(
            viewer.net_at(Point::new(10.4, 0.), 0.1).as_deref(),
            Some("SIG")
        );
        assert!(viewer.net_at(Point::new(5., 5.), 0.1).is_none());
        viewer.hidden.insert("F.Cu".into());
        assert!(viewer.net_at(Point::new(5., 0.), 0.1).is_none());
        viewer.select_net(Some("SIG".into()));
        assert_eq!(viewer.net_color("SIG", Color32::RED), Color32::RED);
        let gray = viewer.net_color("GND", Color32::RED);
        assert_eq!(gray.r(), gray.g());
        assert_eq!(gray.g(), gray.b());
        assert_eq!(viewer.take_net_change(), Some(Some("SIG".into())));
        viewer.select_net(None);
        assert_eq!(viewer.take_net_change(), Some(None));
        assert_eq!(viewer.net_color("GND", Color32::RED), Color32::RED);
    }
    #[test]
    fn class_colors_apply_to_idle_copper_and_selection_uses_layer_colors() {
        let mut viewer = PcbViewer::new("Demo", "key", selection_board());
        viewer.set_class_colors(BTreeMap::from([("SIG".into(), [38, 198, 218])]));
        let expected = Some(Color32::from_rgb(38, 198, 218));
        assert_eq!(
            viewer.clearance_color("SIG"),
            Color32::from_rgb(38, 198, 218).gamma_multiply(0.65)
        );
        assert_eq!(viewer.class_copper_color(&viewer.board.items[0]), expected);
        let mut pad = viewer.board.items[0].clone();
        pad.reference = "U1".into();
        assert_eq!(viewer.class_copper_color(&pad), expected);
        pad.zone = true;
        assert_eq!(viewer.class_copper_color(&pad), expected);
        pad.layers = vec!["F.SilkS".into()];
        assert_eq!(viewer.class_copper_color(&pad), None);
        viewer.select_net_group("Net class: Signals".into(), BTreeSet::from(["SIG".into()]));
        assert_eq!(viewer.class_copper_color(&viewer.board.items[0]), None);
        assert!(
            viewer.settings().net_colors.is_empty(),
            "selection retains the original layer-color scene"
        );
        viewer.select_footprint(Some("U1".into()));
        assert_eq!(viewer.class_copper_color(&viewer.board.items[0]), None);
        viewer.select_net(None);
        assert_eq!(viewer.class_copper_color(&viewer.board.items[0]), expected);
        viewer.select_net(Some("SIG".into()));
        assert_eq!(viewer.class_copper_color(&viewer.board.items[0]), None);
        assert_eq!(viewer.net_color("SIG", Color32::RED), Color32::RED);
        assert_eq!(
            viewer.clearance_color("SIG"),
            layer_color(&viewer.active).gamma_multiply(0.65)
        );
        viewer.select_net(None);
        viewer.set_class_colors(BTreeMap::new());
        assert_eq!(viewer.class_copper_color(&viewer.board.items[0]), None);
    }
    #[test]
    fn idle_class_scene_and_selected_layer_scene_are_reused() {
        let mut viewer = PcbViewer::new("Demo", "key", selection_board());
        viewer.show_nets = false;
        viewer.set_class_colors(BTreeMap::from([("SIG".into(), [38, 198, 218])]));
        let ctx = egui::Context::default();
        for _ in 0..200 {
            viewer.poll_scene(&ctx);
            if viewer.class_scene.is_some() && viewer.gpu_scene.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let idle = viewer.poll_scene(&ctx).unwrap();
        let normal = viewer.gpu_scene.as_ref().unwrap().clone();
        assert!(!Arc::ptr_eq(&idle, &normal));
        viewer.select_net(Some("SIG".into()));
        assert!(Arc::ptr_eq(&normal, &viewer.poll_scene(&ctx).unwrap()));
        viewer.select_net(None);
        assert!(Arc::ptr_eq(&idle, &viewer.poll_scene(&ctx).unwrap()));
        viewer.set_class_colors(BTreeMap::new());
        assert!(Arc::ptr_eq(&normal, &viewer.poll_scene(&ctx).unwrap()));
    }
    #[test]
    fn zoom_reuses_font_atlas_and_scales_bounds_without_changing_uvs() {
        let ctx = egui::Context::default();
        let mut first: Option<(f32, Vec<egui::Pos2>)> = None;
        for frame in 0..5 {
            let factor = (frame + 1) as f32;
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let galley =
                        zoom_galley(ui.painter(), "R12\nGND", 48. * factor, 48., Color32::WHITE);
                    if let Some((size, uv)) = &first {
                        assert!((galley.size().x - size * factor).abs() < 0.01);
                        assert_eq!(
                            &galley.rows[0]
                                .visuals
                                .mesh
                                .vertices
                                .iter()
                                .map(|v| v.uv)
                                .collect::<Vec<_>>(),
                            uv
                        );
                    } else {
                        first = Some((
                            galley.size().x,
                            galley.rows[0]
                                .visuals
                                .mesh
                                .vertices
                                .iter()
                                .map(|v| v.uv)
                                .collect::<Vec<_>>(),
                        ));
                    }
                    ui.painter().add(egui::epaint::TextShape::new(
                        egui::Pos2::ZERO,
                        galley,
                        Color32::WHITE,
                    ));
                });
            });
            if frame > 0 {
                assert!(
                    output.textures_delta.set.is_empty(),
                    "zoom must not rasterize more glyphs"
                );
            }
            std::hint::black_box(ctx.tessellate(output.shapes, output.pixels_per_point));
        }
    }

    #[test]
    fn complete_scene_cache_reuses_camera_changes_and_invalidates_settings_and_board_updates() {
        let board=Arc::new(Pcb::parse(r#"(kicad_pcb (layers (0 "F.Cu" signal)) (segment (start 1 1) (end 5 1) (width 0.2) (layer "F.Cu")))"#).unwrap());
        let mut viewer = PcbViewer::new("test", "test", board.clone());
        viewer.gpu_camera = true;
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let a = viewer.retained_scene(ui);
                viewer.scale = 200.;
                viewer.center = Point::new(100., 50.);
                assert!(Arc::ptr_eq(&a, &viewer.retained_scene(ui)));
                viewer.select_net(Some("SIG".into()));
                assert!(
                    Arc::ptr_eq(&a, &viewer.retained_scene(ui)),
                    "net selection reuses the base scene"
                );
                viewer.select_net(None);
                viewer.show_nets = false;
                let b = viewer.retained_scene(ui);
                assert!(!Arc::ptr_eq(&a, &b));
                viewer.show_clearance = false;
                let c = viewer.retained_scene(ui);
                assert!(!Arc::ptr_eq(&b, &c));
                viewer.replace_board(Arc::new((*board).clone()));
                assert!(!Arc::ptr_eq(&c, &viewer.retained_scene(ui)));
            });
        });
    }
    #[test]
    #[ignore = "Manual CPU preparation profile with KICAD_PCB_FIXTURE"]
    fn profile_large_board_preparation() {
        let path = std::env::var("KICAD_PCB_FIXTURE").unwrap();
        let parsed = std::time::Instant::now();
        let board = Arc::new(Pcb::parse(&std::fs::read_to_string(path).unwrap()).unwrap());
        eprintln!(
            "Parsed {} items, {} pads, {} vias in {:?}",
            board.items.len(),
            board.pads.len(),
            board.vias.len(),
            parsed.elapsed()
        );
        let viewer = PcbViewer::new("profile", "profile", board.clone());
        let ctx = egui::Context::default();
        let submitted = std::time::Instant::now();
        let task = super::super::cad_jobs::spawn(
            super::super::cad_jobs::Request::Pcb {
                board,
                settings: viewer.settings(),
            },
            &ctx,
        );
        let submit_time = submitted.elapsed();
        let mut frames = 0;
        let mut worst = std::time::Duration::ZERO;
        let scene = loop {
            if let Some(result) = task.poll() {
                break result.unwrap();
            }
            let start = std::time::Instant::now();
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label("UI remains interactive");
                });
            });
            worst = worst.max(start.elapsed());
            frames += 1;
            assert!(submitted.elapsed().as_secs() < 300, "Preparation timeout");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let (bytes, letters, expanded) = scene.profile();
        eprintln!(
            "Scene prepared in {:?}; submitted in {:?}; UI frames {}; worst UI frame {:?}; retained {:.1} MiB; {} glyph instances; equivalent expanded text {:.1} MiB",
            submitted.elapsed(),
            submit_time,
            frames,
            worst,
            bytes as f64 / 1048576.,
            letters,
            expanded as f64 / 1048576.
        );
    }
    #[test]
    #[ignore = "Manual GPU board fixture preview"]
    fn retained_pcb_preview() {
        let path = std::env::var("KICAD_PCB_FIXTURE").unwrap();
        let board = Arc::new(Pcb::parse(&std::fs::read_to_string(path).unwrap()).unwrap());
        let footprint = std::env::var("KICAD_PCB_FOOTPRINT").ok();
        let b = footprint
            .as_ref()
            .and_then(|reference| {
                board
                    .footprint_list
                    .iter()
                    .find(|f| &f.reference == reference)
            })
            .map_or(board.bounds, |f| f.bounds);
        let scale = if footprint.is_some() {
            (1200. / b.width().max(1.))
                .min(850. / b.height().max(1.))
                .clamp(1., 150.)
        } else {
            35.
        };
        let center = Point::new((b.min.x + b.max.x) / 2., (b.min.y + b.max.y) / 2.);
        let mut viewer = PcbViewer::new("preview", "preview", board);
        if let Ok(layer) = std::env::var("KICAD_PCB_LAYER") {
            viewer.active = layer;
        }
        viewer.gpu_camera = true;
        let ctx = egui::Context::default();
        let mut scene = None;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                scene = Some(viewer.retained_scene(ui));
            });
        });
        let highlight = if let Some(reference) = footprint {
            Some(
                PcbViewer::prepare_footprint(
                    viewer.board.clone(),
                    viewer.settings(),
                    &reference,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
                .unwrap(),
            )
        } else {
            std::env::var("KICAD_PCB_NET").ok().map(|net| {
                PcbViewer::prepare_net(
                    viewer.board.clone(),
                    viewer.settings(),
                    &BTreeSet::from([net]),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
                .unwrap()
            })
        };
        super::super::cad_gpu::render_preview(
            if std::env::var_os("CAD_GPU_NET_ONLY").is_some() {
                highlight.clone().unwrap()
            } else {
                scene.unwrap()
            },
            if std::env::var_os("CAD_GPU_NET_ONLY").is_some() {
                None
            } else {
                highlight
            },
            center,
            scale as f32,
            [1400, 1000],
            Color32::from_rgb(18, 21, 24),
            &std::env::var("CAD_GPU_PREVIEW").unwrap(),
        );
    }
    #[test]
    #[ignore = "Manual zoom profiling with KICAD_PCB_FIXTURE"]
    fn profile_zoom_cpu() {
        let path = std::env::var("KICAD_PCB_FIXTURE").expect("Set KICAD_PCB_FIXTURE");
        let board = Arc::new(Pcb::parse(&std::fs::read_to_string(path).unwrap()).unwrap());
        for (camera, text, clearance) in [
            (false, true, true),
            (true, true, true),
            (true, false, true),
            (true, false, false),
        ] {
            let ctx = egui::Context::default();
            let mut viewer = PcbViewer::new("bench", "bench", board.clone());
            viewer.gpu_camera = camera;
            viewer.show_nets = text;
            viewer.show_clearance = clearance;
            if camera {
                let _ = ctx.run(Default::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        viewer.retained_scene(ui);
                    });
                });
            }
            let mut total = 0.;
            let mut uploads = 0usize;
            for i in 0..24 {
                viewer.fit = false;
                viewer.center = Point::new(
                    (board.bounds.min.x + board.bounds.max.x) / 2.,
                    (board.bounds.min.y + board.bounds.max.y) / 2.,
                );
                viewer.scale = 12. * (i as f64 * 0.15).exp();
                let start = std::time::Instant::now();
                let out = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1380., 860.),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| viewer.ui(ui));
                    },
                );
                for (_, delta) in &out.textures_delta.set {
                    uploads += delta.image.width() * delta.image.height();
                }
                std::hint::black_box(ctx.tessellate(out.shapes, 1.));
                if i > 0 {
                    total += start.elapsed().as_secs_f64() * 1000.;
                }
            }
            eprintln!(
                "PCB zoom: camera={camera}, labels={text}, clearance={clearance}: {:.2} ms CPU/frame; font texture delta={} pixels",
                total / 23.,
                uploads
            );
        }
    }
    #[test]
    fn open_tracks_have_half_width_caps_at_both_endpoints() {
        let points = vec![egui::pos2(10., 20.), egui::pos2(30., 40.)];
        let shapes = rounded_path(points.clone(), egui::Stroke::new(6., Color32::RED));
        assert_eq!(shapes.len(), 3);
        for (shape, endpoint) in shapes.iter().zip(points) {
            let egui::Shape::Circle(circle) = shape else {
                panic!("round endpoint expected")
            };
            assert_eq!(circle.center, endpoint);
            assert_eq!(circle.radius, 3.);
        }
    }
    #[test]
    fn closed_outlines_do_not_add_endcaps() {
        let points = vec![
            egui::pos2(0., 0.),
            egui::pos2(10., 0.),
            egui::pos2(0., 10.),
            egui::pos2(0., 0.),
        ];
        assert_eq!(
            rounded_path(points, egui::Stroke::new(2., Color32::RED)).len(),
            1
        );
    }
}
