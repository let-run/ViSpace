use super::*;
use std::collections::{BTreeMap, BTreeSet};
impl InventoryApp {
    pub(super) fn network_status_popup(&mut self, anchor: &egui::Response) {
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
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        let graph = match self.store.try_lock() {
            Ok(mut store) => store.network_graph(),
            Err(_) => return,
        };
        popup.show(|ui| {
            ui.set_min_size(egui::vec2(width, height));
            ui.set_max_size(egui::vec2(width, height));
            ui.heading("Network status");
            ui.horizontal(|ui| {
                ui.heading(format!(
                    "{} nodes · {} connections",
                    graph.nodes.len(),
                    graph.edges.len()
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Fit").clicked() {
                        self.network_zoom = 1.;
                        self.network_pan = egui::Vec2::ZERO;
                    }
                });
            });
            ui.label("Drag to pan · scroll to zoom · hover a node for its identity");
            ui.small("Grayscale nodes are unreachable.");
            if graph.managed && graph.leader.is_none() {
                ui.small("Waiting for a leader.");
            }
            let size = egui::vec2(ui.available_width(), (ui.available_height() - 25.).max(1.));
            let (response, painter) = ui.allocate_painter(size, egui::Sense::drag());
            let rect = response.rect;
            painter.rect_filled(rect, 8., theme::BACKGROUND);
            if response.dragged() {
                self.network_pan += ui.input(|i| i.pointer.delta());
            }
            if response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                self.network_zoom = (self.network_zoom * (scroll * 0.002).exp()).clamp(0.1, 128.);
            }
            let layout = positions(&graph);
            let extent = layout
                .values()
                .map(|p| p.length() + 65.)
                .fold(160_f32, f32::max);
            let scale = ((rect.width().min(rect.height()) - 50.) / (extent * 2.)).min(1.)
                * self.network_zoom;
            let center = rect.center() + self.network_pan;
            let screen: BTreeMap<_, _> = layout
                .iter()
                .map(|(id, p)| (id.clone(), center + *p * scale))
                .collect();
            for [a, b] in &graph.edges {
                if let (Some(a), Some(b)) = (screen.get(a), screen.get(b)) {
                    painter.line_segment(
                        [*a, *b],
                        egui::Stroke::new(2., Color32::from_rgb(91, 108, 132)),
                    );
                }
            }
            let icons = node_icons(ctx);
            for node in &graph.nodes {
                let Some(point) = screen.get(&node.id).copied() else {
                    continue;
                };
                let leader = node.reachable && graph.leader.as_ref() == Some(&node.id);
                draw_node(&painter, point, node, &icons, leader);
                let color = if node.reachable {
                    theme::TEXT
                } else {
                    Color32::GRAY
                };
                painter.text(
                    point + egui::vec2(0., 31.),
                    egui::Align2::CENTER_TOP,
                    &node.title,
                    egui::FontId::proportional(12.),
                    color,
                );
                let hover = ui.interact(
                    egui::Rect::from_center_size(
                        point + egui::vec2(0., 10.),
                        egui::vec2(120., 75.),
                    ),
                    ui.id().with(&node.id),
                    egui::Sense::hover(),
                );
                hover.on_hover_ui(|ui| {
                    ui.strong(&node.title);
                    ui.monospace(&node.id);
                    if leader {
                        ui.label("Sync leader");
                    }
                    ui.label(if node.local {
                        "This application"
                    } else if !node.reachable {
                        "No longer reachable"
                    } else if node.direct {
                        "Direct connection"
                    } else {
                        "Connected through another peer"
                    });
                });
            }
            if graph.nodes.len() <= 1 {
                ui.small("Connect to a peer in Sync to discover the network.");
            }
        });
    }
}
fn node_icons(ctx: &egui::Context) -> [egui::TextureHandle; 2] {
    let id = egui::Id::new("network_vispace_icons");
    if let Some(icons) = ctx.data_mut(|data| data.get_temp(id)) {
        return icons;
    }

    let mut logo = image::load_from_memory(include_bytes!("../../assets/app-icon.png"))
        .expect("bundled ViSpace app icon")
        .resize_exact(144, 144, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let texture = |name, rgba: &image::RgbaImage| {
        ctx.load_texture(
            name,
            egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            ),
            egui::TextureOptions::LINEAR,
        )
    };
    let reachable = texture("Network ViSpace icon", &logo);
    for pixel in logo.pixels_mut() {
        let [r, g, b, alpha] = pixel.0;
        let gray = grayscale(r, g, b);
        pixel.0 = [gray, gray, gray, alpha];
    }
    let icons = [reachable, texture("Unreachable ViSpace icon", &logo)];
    ctx.data_mut(|data| data.insert_temp(id, icons.clone()));
    icons
}

fn grayscale(r: u8, g: u8, b: u8) -> u8 {
    let luminance = (u16::from(r) * 21 + u16::from(g) * 72 + u16::from(b) * 7) / 100;
    (luminance * 3 / 4) as u8
}

fn draw_node(
    painter: &egui::Painter,
    center: egui::Pos2,
    node: &crate::topology::Node,
    icons: &[egui::TextureHandle; 2],
    leader: bool,
) {
    let at = |x, y| center + egui::vec2(x, y);
    let color = |r, g, b| {
        if node.reachable {
            Color32::from_rgb(r, g, b)
        } else {
            Color32::from_gray(grayscale(r, g, b))
        }
    };
    painter.image(
        icons[usize::from(!node.reachable)].id(),
        egui::Rect::from_center_size(center, egui::vec2(48., 48.)),
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
        Color32::WHITE,
    );

    // Browser peers already advertise this title prefix in the topology protocol.
    if node.title.starts_with("Browser ") {
        let badge = egui::Rect::from_min_size(at(-29., -24.), egui::vec2(19., 18.));
        painter.rect_filled(badge.expand(1.), 3., theme::BACKGROUND);
        painter.rect_filled(badge, 2., color(101, 79, 240));
        // The notch and WA lettering follow the WebAssembly mark.
        painter.circle_filled(badge.center_top(), 2.6, theme::BACKGROUND);
        painter.text(
            badge.center() + egui::vec2(0., 2.),
            egui::Align2::CENTER_CENTER,
            "WA",
            egui::FontId::proportional(8.5),
            color(255, 255, 255),
        );
    }
    if node.local {
        let dot = at(-24., 17.);
        painter.circle_filled(dot, 6., theme::BACKGROUND);
        painter.circle_filled(dot, 4., theme::GREEN);
    }
    if leader {
        let badge = at(29., -17.);
        painter.circle_filled(badge, 10., theme::BACKGROUND);
        painter.add(egui::Shape::closed_line(
            [
                (-6., -4.),
                (-3., -1.),
                (0., -5.),
                (3., -1.),
                (6., -4.),
                (4., 4.),
                (-4., 4.),
            ]
            .map(|(x, y)| badge + egui::vec2(x, y))
            .to_vec(),
            egui::Stroke::new(1.4, theme::YELLOW),
        ));
    }
}

fn positions(graph: &crate::topology::Graph) -> BTreeMap<String, egui::Vec2> {
    let mut distance = BTreeMap::from([(graph.local.clone(), 0usize)]);
    let mut frontier = BTreeSet::from([graph.local.clone()]);
    while !frontier.is_empty() {
        let mut next = BTreeSet::new();
        for [a, b] in &graph.edges {
            for (a, b) in [(a, b), (b, a)] {
                if frontier.contains(a) && !distance.contains_key(b) {
                    distance.insert(b.clone(), distance[a] + 1);
                    next.insert(b.clone());
                }
            }
        }
        frontier = next;
    }
    let outer = distance.values().copied().max().unwrap_or(0) + 1;
    let mut rings: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for n in &graph.nodes {
        rings
            .entry(distance.get(&n.id).copied().unwrap_or(outer))
            .or_default()
            .push(n.id.clone());
    }
    let mut result = BTreeMap::new();
    let mut previous = 0.;
    for (depth, mut ids) in rings {
        ids.sort();
        let radius = if depth == 0 {
            0.
        } else {
            (previous + 135_f32).max(ids.len() as f32 * 115. / std::f32::consts::TAU)
        };
        for (i, id) in ids.iter().enumerate() {
            let angle =
                std::f32::consts::TAU * i as f32 / ids.len() as f32 - std::f32::consts::FRAC_PI_2;
            result.insert(
                id.clone(),
                egui::vec2(angle.cos() * radius, angle.sin() * radius),
            );
        }
        previous = radius;
    }
    result
}
