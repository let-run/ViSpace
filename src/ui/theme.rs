//! ViSpace palette, typography, and controls.
use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextStyle};

pub const BACKGROUND: Color32 = Color32::from_rgb(19, 19, 22);
pub const PANEL: Color32 = Color32::from_rgb(31, 31, 38);
pub const ROW: Color32 = Color32::from_rgb(31, 31, 31);
pub const RAISED: Color32 = Color32::from_rgb(41, 41, 38);
pub const PURPLE: Color32 = Color32::from_rgb(74, 61, 120);
pub const SELECTION: Color32 = Color32::from_rgb(66, 61, 99);
pub const YELLOW: Color32 = Color32::from_rgb(245, 217, 110);
pub const MUTED: Color32 = Color32::from_rgb(176, 173, 194);
pub const GREEN: Color32 = Color32::from_rgb(71, 179, 145);
pub const TEXT: Color32 = Color32::from_rgb(235, 235, 239);
pub const LINE: Color32 = Color32::from_rgb(51, 51, 56);
pub const TOOLBAR_HEIGHT: f32 = 40.;
pub const FOOTER_HEIGHT: f32 = 36.;
pub const ROW_HEIGHT: f32 = 30.;
pub const PAGEBAR_HEIGHT: f32 = 38.;

pub fn apply(ctx: &egui::Context) {
    let mut style = egui::Style {
        text_styles: [
            (TextStyle::Small, FontId::new(11., FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13., FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13., FontFamily::Proportional),
            ),
            (
                TextStyle::Heading,
                FontId::new(22., FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(12., FontFamily::Monospace),
            ),
        ]
        .into(),
        ..Default::default()
    };
    style.spacing.item_spacing = egui::vec2(8., 8.);
    style.spacing.button_padding = egui::vec2(10., 6.);
    style.spacing.interact_size = egui::vec2(32., 28.);
    style.spacing.window_margin = egui::Margin::same(20);
    style.visuals = egui::Visuals::dark();
    let v = &mut style.visuals;
    v.panel_fill = BACKGROUND;
    v.window_fill = PANEL;
    v.extreme_bg_color = BACKGROUND;
    v.text_edit_bg_color = Some(BACKGROUND);
    v.faint_bg_color = ROW;
    v.code_bg_color = ROW;
    v.weak_text_color = Some(MUTED);
    v.hyperlink_color = YELLOW;
    v.selection.bg_fill = SELECTION;
    v.selection.stroke = Stroke::new(1., TEXT);
    v.window_corner_radius = 6.into();
    v.menu_corner_radius = 3.into();
    v.window_stroke = Stroke::new(1., LINE);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1., LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1., TEXT);
    for (widget, fill) in [
        (&mut v.widgets.inactive, RAISED),
        (&mut v.widgets.hovered, SELECTION),
        (&mut v.widgets.active, PURPLE),
        (&mut v.widgets.open, PURPLE),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::NONE;
        widget.fg_stroke = Stroke::new(1., YELLOW);
        widget.corner_radius = 2.into();
        widget.expansion = 0.;
    }
    ctx.set_style(style);
    // Use egui's bundled Ubuntu Light and Hack on every platform.
    ctx.set_fonts(egui::FontDefinitions::default());
}

pub fn frame(fill: Color32) -> egui::Frame {
    egui::Frame::new().fill(fill)
}

#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Lamp,
    Add,
    BulkAdd,
    Edit,
    Trash,
    Filter,
    Search,
    Scan,
    Analyze,
    Import,
    Export,
    Settings,
    Qr,
    Box,
    Left,
    Right,
    First,
    Last,
    Close,
    Cpu,
    Photo,
    Clipboard,
    Folder,
    Notes,
    Git,
    Commit,
    GitLog,
    #[cfg(not(target_arch = "wasm32"))]
    GitPush,
    External,
    Printer,
    Check,
    Sync,
    Mesh,
    Release,
}

/// Small vector controls avoid platform-dependent Unicode symbol glyphs.
pub fn icon(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    paint_icon(ui.painter(), icon, rect, color);
    response
}
pub fn selection_lamp(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(24., 26.), egui::Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, 4., Color32::from_rgb(235, 229, 245));
        }
        paint_icon(
            ui.painter(),
            Icon::Lamp,
            rect.shrink(3.),
            if enabled {
                Color32::from_rgb(180, 40, 205)
            } else {
                MUTED
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "Blink selection")
        });
        response.on_hover_text("Blink selection")
    })
    .inner
}
pub fn tool(ui: &mut egui::Ui, icon: Icon, title: &str) -> egui::Response {
    tool_sized(ui, icon, title, egui::vec2(38., 32.), false)
}
pub fn tool_sized(
    ui: &mut egui::Ui,
    icon: Icon,
    title: &str,
    size: egui::Vec2,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if selected {
        PURPLE
    } else if response.hovered() {
        SELECTION
    } else {
        RAISED
    };
    ui.painter().rect_filled(rect, 2., fill);
    let color = if ui.is_enabled() {
        YELLOW
    } else {
        MUTED.gamma_multiply(0.4)
    };
    paint_icon(
        ui.painter(),
        icon,
        egui::Rect::from_center_size(rect.center(), egui::vec2(17., 17.)),
        color,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), title)
    });
    response.on_hover_text(title)
}
pub(super) fn paint_icon(p: &egui::Painter, icon: Icon, r: egui::Rect, color: Color32) {
    let asset: Option<(&str, &[u8])> = match icon {
        Icon::Add => Some(("add", include_bytes!("../../assets/icons/add.png"))),
        Icon::Edit => Some(("edit", include_bytes!("../../assets/icons/edit.png"))),
        Icon::Trash => Some(("trash", include_bytes!("../../assets/icons/trash.png"))),
        Icon::BulkAdd => Some((
            "bulk-add",
            include_bytes!("../../assets/icons/bulk-add.png"),
        )),
        Icon::Notes => Some(("notes", include_bytes!("../../assets/icons/notes.png"))),
        Icon::Qr => Some(("qr", include_bytes!("../../assets/icons/qr.png"))),
        _ => None,
    };
    if let Some((name, bytes)) = asset {
        let id = egui::Id::new(("symbol_texture", name));
        let cached = p
            .ctx()
            .data_mut(|data| data.get_temp::<egui::TextureHandle>(id));
        let texture = cached.unwrap_or_else(|| {
            let image = image::load_from_memory(bytes)
                .expect("bundled icon")
                .into_rgba8();
            let texture = p.ctx().load_texture(
                name,
                egui::ColorImage::from_rgba_unmultiplied(
                    [image.width() as usize, image.height() as usize],
                    image.as_raw(),
                ),
                egui::TextureOptions::LINEAR,
            );
            p.ctx()
                .data_mut(|data| data.insert_temp(id, texture.clone()));
            texture
        });
        p.image(
            texture.id(),
            r,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
            color,
        );
        return;
    }
    let pos = |x: f32, y: f32| egui::pos2(r.left() + r.width() * x, r.top() + r.height() * y);
    let stroke = Stroke::new(1.5, color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([pos(a.0, a.1), pos(b.0, b.1)], stroke);
    };
    let path = |points: &[(f32, f32)]| {
        p.add(egui::Shape::line(
            points.iter().map(|(x, y)| pos(*x, *y)).collect(),
            stroke,
        ));
    };
    match icon {
        Icon::Mesh => {
            let nodes = [(0.5, 0.19), (0.18, 0.76), (0.82, 0.76)];
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                let a = pos(nodes[a].0, nodes[a].1);
                let b = pos(nodes[b].0, nodes[b].1);
                let inset = (b - a).normalized() * r.width() * 0.12;
                p.line_segment([a + inset, b - inset], stroke);
            }
            for (x, y) in nodes {
                p.circle_stroke(pos(x, y), r.width() * 0.12, stroke);
            }
        }
        Icon::Release => {
            // An arrow leaving an open reservation box.
            path(&[
                (0.55, 0.15),
                (0.16, 0.15),
                (0.16, 0.86),
                (0.84, 0.86),
                (0.84, 0.55),
            ]);
            line((0.42, 0.58), (0.9, 0.1));
            path(&[(0.62, 0.1), (0.9, 0.1), (0.9, 0.38)]);
        }
        Icon::Cpu => {
            p.rect_stroke(
                egui::Rect::from_min_max(pos(0.25, 0.25), pos(0.75, 0.75)),
                1.,
                stroke,
                egui::StrokeKind::Inside,
            );
            p.rect_stroke(
                egui::Rect::from_min_max(pos(0.4, 0.4), pos(0.6, 0.6)),
                0.,
                stroke,
                egui::StrokeKind::Inside,
            );
            for x in [0.33, 0.5, 0.67] {
                line((x, 0.08), (x, 0.25));
                line((x, 0.75), (x, 0.92));
                line((0.08, x), (0.25, x));
                line((0.75, x), (0.92, x));
            }
        }
        Icon::Photo => {
            p.rect_stroke(
                egui::Rect::from_min_max(pos(0.08, 0.15), pos(0.85, 0.85)),
                2.,
                stroke,
                egui::StrokeKind::Inside,
            );
            p.circle_stroke(pos(0.3, 0.35), r.width() * 0.08, stroke);
            path(&[
                (0.12, 0.75),
                (0.4, 0.5),
                (0.55, 0.65),
                (0.7, 0.48),
                (0.84, 0.65),
            ]);
            line((0.82, 0.02), (0.82, 0.3));
            line((0.68, 0.16), (0.96, 0.16));
        }
        Icon::Clipboard => {
            path(&[
                (0.3, 0.17),
                (0.17, 0.17),
                (0.17, 0.9),
                (0.82, 0.9),
                (0.82, 0.17),
                (0.68, 0.17),
            ]);
            p.rect_stroke(
                egui::Rect::from_min_max(pos(0.32, 0.08), pos(0.68, 0.29)),
                2.,
                stroke,
                egui::StrokeKind::Inside,
            );
            line((0.32, 0.48), (0.68, 0.48));
            line((0.32, 0.65), (0.68, 0.65));
        }
        Icon::Lamp => {
            p.circle_stroke(pos(0.5, 0.38), r.width() * 0.22, stroke);
            path(&[(0.34, 0.55), (0.36, 0.72), (0.64, 0.72), (0.66, 0.55)]);
            line((0.38, 0.82), (0.62, 0.82));
            line((0.43, 0.9), (0.57, 0.9));
            for (a, b) in [
                ((0.5, 0.01), (0.5, 0.08)),
                ((0.1, 0.12), (0.18, 0.2)),
                ((0.82, 0.2), (0.9, 0.12)),
                ((0.02, 0.38), (0.12, 0.38)),
                ((0.88, 0.38), (0.98, 0.38)),
            ] {
                line(a, b);
            }
        }
        Icon::Folder => {
            path(&[
                (0.07, 0.2),
                (0.4, 0.2),
                (0.5, 0.32),
                (0.9, 0.32),
                (0.84, 0.84),
                (0.07, 0.84),
                (0.07, 0.2),
            ]);
            line((0.1, 0.43), (0.85, 0.43));
        }
        Icon::Git => {
            let branch = Stroke::new(r.width() * 0.075, color);
            p.line_segment([pos(0.27, 0.17), pos(0.27, 0.85)], branch);
            for points in [
                [
                    pos(0.27, 0.68),
                    pos(0.27, 0.58),
                    pos(0.35, 0.50),
                    pos(0.45, 0.50),
                ],
                [
                    pos(0.55, 0.50),
                    pos(0.65, 0.50),
                    pos(0.73, 0.42),
                    pos(0.73, 0.32),
                ],
            ] {
                p.add(egui::epaint::CubicBezierShape::from_points_stroke(
                    points,
                    false,
                    Color32::TRANSPARENT,
                    branch,
                ));
            }
            p.line_segment([pos(0.45, 0.50), pos(0.55, 0.50)], branch);
            p.line_segment([pos(0.73, 0.32), pos(0.73, 0.17)], branch);
            for (x, y) in [(0.27, 0.17), (0.27, 0.85), (0.73, 0.17)] {
                p.circle_filled(pos(x, y), r.width() * 0.115, color);
            }
        }
        Icon::GitLog => {
            line((0.22,0.15),(0.22,0.85));
            for y in [0.2,0.5,0.8]{p.circle_filled(pos(0.22,y),r.width()*0.07,color);line((0.42,y),(0.9,y));}
        }
        Icon::Commit => {
            let thick = Stroke::new(r.width() * 0.075, color);
            p.line_segment([pos(0.21, 0.21), pos(0.21, 0.66)], thick);
            p.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [
                    pos(0.21, 0.66),
                    pos(0.21, 0.75),
                    pos(0.26, 0.82),
                    pos(0.35, 0.82),
                ],
                false,
                Color32::TRANSPARENT,
                thick,
            ));
            p.line_segment([pos(0.35, 0.82), pos(0.56, 0.82)], thick);
            p.add(egui::Shape::line(
                vec![pos(0.44, 0.70), pos(0.56, 0.82), pos(0.44, 0.94)],
                thick,
            ));
            for (start, end) in [(0.21, 0.40), (0.48, 0.55), (0.63, 0.82)] {
                p.line_segment([pos(0.8, start), pos(0.8, end)], thick);
            }
            for (x, y) in [(0.21, 0.21), (0.8, 0.21), (0.8, 0.82)] {
                p.circle_filled(pos(x, y), r.width() * 0.145, color);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        Icon::GitPush => {
            let thick = Stroke::new(r.width() * 0.085, color);
            for points in [
                [
                    pos(0.23, 0.61),
                    pos(0.23, 0.73),
                    pos(0.29, 0.80),
                    pos(0.42, 0.80),
                ],
                [
                    pos(0.77, 0.39),
                    pos(0.77, 0.27),
                    pos(0.71, 0.20),
                    pos(0.58, 0.20),
                ],
            ] {
                p.add(egui::epaint::CubicBezierShape::from_points_stroke(
                    points,
                    false,
                    Color32::TRANSPARENT,
                    thick,
                ));
            }
            for (a, b) in [
                (pos(0.23, 0.31), pos(0.23, 0.61)),
                (pos(0.77, 0.69), pos(0.77, 0.39)),
                (pos(0.42, 0.80), pos(0.56, 0.80)),
                (pos(0.58, 0.20), pos(0.44, 0.20)),
            ] {
                p.line_segment([a, b], thick);
            }
            p.add(egui::Shape::line(
                vec![pos(0.44, 0.68), pos(0.56, 0.80), pos(0.44, 0.92)],
                thick,
            ));
            p.add(egui::Shape::line(
                vec![pos(0.56, 0.08), pos(0.44, 0.20), pos(0.56, 0.32)],
                thick,
            ));
            p.circle_stroke(pos(0.23, 0.20), r.width() * 0.10, thick);
            p.circle_stroke(pos(0.77, 0.80), r.width() * 0.10, thick);
        }
        Icon::Notes | Icon::BulkAdd => unreachable!("handled by PNG assets"),
        Icon::Sync => {
            path(&[
                (0.82, 0.35),
                (0.7, 0.17),
                (0.42, 0.1),
                (0.2, 0.23),
                (0.1, 0.45),
            ]);
            path(&[(0.65, 0.36), (0.84, 0.4), (0.92, 0.21)]);
            path(&[
                (0.18, 0.65),
                (0.3, 0.83),
                (0.58, 0.9),
                (0.8, 0.77),
                (0.9, 0.55),
            ]);
            path(&[(0.35, 0.64), (0.16, 0.6), (0.08, 0.79)]);
        }
        Icon::Check => {
            path(&[(0.15, 0.5), (0.4, 0.75), (0.9, 0.15)]);
        }
        Icon::Printer => {
            path(&[(0.25, 0.35), (0.25, 0.1), (0.75, 0.1), (0.75, 0.35)]);
            path(&[
                (0.25, 0.72),
                (0.1, 0.72),
                (0.1, 0.35),
                (0.9, 0.35),
                (0.9, 0.72),
                (0.75, 0.72),
            ]);
            path(&[
                (0.25, 0.57),
                (0.75, 0.57),
                (0.75, 0.9),
                (0.25, 0.9),
                (0.25, 0.57),
            ]);
            line((0.35, 0.7), (0.65, 0.7));
            p.circle_filled(pos(0.77, 0.46), r.width() * 0.035, color);
        }
        Icon::External => {
            line((0.2, 0.8), (0.8, 0.2));
            path(&[(0.3, 0.2), (0.8, 0.2), (0.8, 0.7)]);
        }
        Icon::Add => {
            line((0.15, 0.5), (0.85, 0.5));
            line((0.5, 0.15), (0.5, 0.85));
        }
        Icon::Edit => {
            path(&[
                (0.17, 0.65),
                (0.65, 0.17),
                (0.83, 0.35),
                (0.35, 0.83),
                (0.13, 0.87),
                (0.17, 0.65),
            ]);
            line((0.56, 0.26), (0.74, 0.44));
        }
        Icon::Trash => {
            path(&[(0.25, 0.3), (0.29, 0.87), (0.71, 0.87), (0.75, 0.3)]);
            line((0.12, 0.24), (0.88, 0.24));
            path(&[(0.36, 0.24), (0.36, 0.11), (0.64, 0.11), (0.64, 0.24)]);
            line((0.43, 0.4), (0.43, 0.74));
            line((0.59, 0.4), (0.59, 0.74));
        }
        Icon::Filter => {
            for (y, inset) in [(0.25, 0.1), (0.5, 0.25), (0.75, 0.4)] {
                line((inset, y), (1. - inset, y));
            }
        }
        Icon::Analyze => {
            line((0.2, 0.8), (0.7, 0.3));
            line((0.32, 0.68), (0.43, 0.79));
            for (x, y, s) in [(0.25, 0.2, 0.12), (0.78, 0.7, 0.1), (0.73, 0.1, 0.08)] {
                line((x - s, y), (x + s, y));
                line((x, y - s), (x, y + s));
            }
        }
        Icon::Scan => {
            for (x, y, dx, dy) in [
                (0.1, 0.1, 0.2, 0.2),
                (0.9, 0.1, -0.2, 0.2),
                (0.1, 0.9, 0.2, -0.2),
                (0.9, 0.9, -0.2, -0.2),
            ] {
                path(&[(x + dx, y), (x, y), (x, y + dy)]);
            }
            for x in [0.3, 0.43, 0.5, 0.65, 0.73] {
                line((x, 0.32), (x, 0.68));
            }
        }
        Icon::Search => {
            p.circle_stroke(pos(0.42, 0.42), r.width() * 0.28, stroke);
            line((0.64, 0.64), (0.9, 0.9));
        }
        Icon::Import | Icon::Export => {
            let (a, b) = if matches!(icon, Icon::Import) {
                (0.12, 0.67)
            } else {
                (0.67, 0.12)
            };
            line((0.5, a), (0.5, b));
            let d = if b > a { -0.2 } else { 0.2 };
            path(&[(0.28, b + d), (0.5, b), (0.72, b + d)]);
            path(&[(0.15, 0.68), (0.15, 0.9), (0.85, 0.9), (0.85, 0.68)]);
        }
        Icon::Settings => {
            p.circle_stroke(pos(0.5, 0.5), r.width() * 0.27, stroke);
            p.circle_stroke(pos(0.5, 0.5), r.width() * 0.08, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.;
                line(
                    (0.5 + a.cos() * 0.28, 0.5 + a.sin() * 0.28),
                    (0.5 + a.cos() * 0.42, 0.5 + a.sin() * 0.42),
                );
            }
        }
        Icon::Qr => unreachable!("handled by PNG asset"),
        Icon::Box => {
            path(&[
                (0.1, 0.28),
                (0.5, 0.08),
                (0.9, 0.28),
                (0.9, 0.74),
                (0.5, 0.94),
                (0.1, 0.74),
                (0.1, 0.28),
                (0.5, 0.48),
                (0.9, 0.28),
            ]);
            line((0.5, 0.48), (0.5, 0.94));
            line((0.3, 0.18), (0.7, 0.38));
        }
        Icon::Left | Icon::First => {
            path(&[(0.65, 0.18), (0.32, 0.5), (0.65, 0.82)]);
            if matches!(icon, Icon::First) {
                line((0.15, 0.18), (0.15, 0.82));
            }
        }
        Icon::Right | Icon::Last => {
            path(&[(0.35, 0.18), (0.68, 0.5), (0.35, 0.82)]);
            if matches!(icon, Icon::Last) {
                line((0.85, 0.18), (0.85, 0.82));
            }
        }
        Icon::Close => {
            line((0.2, 0.2), (0.8, 0.8));
            line((0.8, 0.2), (0.2, 0.8));
        }
    }
}
pub fn empty(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    let area = ui.available_rect_before_wrap();
    ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
        ui.add_space((area.height() * 0.4 - 55.).max(20.));
        ui.vertical_centered(|ui| {
            icon(ui, Icon::Box, 36., YELLOW);
            ui.add_space(8.);
            ui.label(egui::RichText::new(title).size(17.).strong().color(TEXT));
            ui.add_space(4.);
            ui.label(egui::RichText::new(subtitle).color(MUTED));
        });
    });
}

pub(super) fn image_tile(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    path: Option<&str>,
    placeholder: Icon,
) -> egui::Response {
    ui.painter().rect_filled(rect, 4., RAISED);
    if let Some(path) = path {
        ui.put(
            rect,
            crate::platform::image_widget(path)
                .fit_to_exact_size(rect.size())
                .maintain_aspect_ratio(true)
                .corner_radius(4)
                .sense(egui::Sense::click()),
        )
    } else {
        paint_icon(
            ui.painter(),
            placeholder,
            egui::Rect::from_center_size(rect.center(), egui::vec2(36., 36.)),
            MUTED.gamma_multiply(0.5),
        );
        ui.interact(
            rect,
            ui.id().with("image_placeholder"),
            egui::Sense::hover(),
        )
    }
}

pub fn text(
    ui: &mut egui::Ui,
    value: &str,
    size: f32,
    weight: i32,
    color: Color32,
) -> egui::Response {
    let rich = egui::RichText::new(value).size(size).color(color);
    ui.label(if weight > 0 { rich.strong() } else { rich })
}
pub fn plain_action(ui: &mut egui::Ui, symbol: Icon, title: &str) -> egui::Response {
    let text_width = measure_text(ui, title, 12., 0).x;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(text_width + 20., 22.), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 2., SELECTION);
    }
    paint_icon(
        ui.painter(),
        symbol,
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 6., rect.center().y),
            egui::vec2(12., 12.),
        ),
        YELLOW,
    );
    paint_text(
        ui,
        title,
        12.,
        0,
        YELLOW,
        egui::pos2(rect.left() + 20., rect.center().y),
    );
    response.on_hover_text(title)
}
pub fn measure_text(ui: &egui::Ui, value: &str, size: f32, _weight: i32) -> egui::Vec2 {
    ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(value.to_owned(), FontId::proportional(size), TEXT)
            .size()
    })
}
pub fn paint_text(
    ui: &egui::Ui,
    value: &str,
    size: f32,
    weight: i32,
    color: Color32,
    left_center: egui::Pos2,
) {
    if weight > 0 {
        ui.painter().text(
            left_center + egui::vec2(0.3, 0.),
            egui::Align2::LEFT_CENTER,
            value,
            FontId::proportional(size),
            color,
        );
    }
    ui.painter().text(
        left_center,
        egui::Align2::LEFT_CENTER,
        value,
        FontId::proportional(size),
        color,
    );
}

pub fn label_tool(ui: &mut egui::Ui, icon: Icon, title: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(30., 26.), egui::Sense::click());
    ui.painter().rect_filled(
        rect,
        5.,
        Color32::from_rgb(135, 119, 179).gamma_multiply(if ui.is_enabled() { 1. } else { 0.5 }),
    );
    paint_icon(
        ui.painter(),
        icon,
        egui::Rect::from_center_size(rect.center(), egui::vec2(16., 16.)),
        Color32::WHITE.gamma_multiply(if ui.is_enabled() { 1. } else { 0.4 }),
    );
    response.on_hover_text(title)
}

/// Fill the full table header before egui lays out column cells.
pub fn table_style(ui: &mut egui::Ui) {
    ui.visuals_mut().widgets.noninteractive.bg_stroke = Stroke::new(1., Color32::from_gray(19));
}
pub fn table_header(ui: &mut egui::Ui, height: f32) {
    table_style(ui);
    ui.painter().rect_filled(
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), height)),
        0.,
        RAISED,
    );
}

/// Keep the notes marker at the trailing edge without expanding the number column.
pub fn part_number_cell(ui: &mut egui::Ui, part: &crate::model::Part) {
    let rect = ui.available_rect_before_wrap();
    let has_notes = !part.notes.is_empty();
    let text_rect = egui::Rect::from_min_max(
        rect.min + egui::vec2(7., 0.),
        rect.max - egui::vec2(if has_notes { 24. } else { 4. }, 0.),
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        part.number.clone(),
        egui::FontId::proportional(13.),
        TEXT,
    );
    job.wrap.max_width = text_rect.width().max(0.);
    job.wrap.max_rows = 1;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let position = egui::pos2(
        text_rect.left(),
        text_rect.center().y - galley.size().y / 2.,
    );
    ui.painter()
        .with_clip_rect(text_rect.intersect(ui.clip_rect()))
        .galley(position, galley, TEXT);
    ui.interact(
        text_rect,
        ui.id().with("part_number_text"),
        egui::Sense::hover(),
    )
    .on_hover_text(&part.number);
    if has_notes {
        let icon = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 12., rect.center().y),
            egui::vec2(12., 12.),
        );
        paint_icon(ui.painter(), Icon::Notes, icon, YELLOW);
        ui.interact(icon, ui.id().with("notes_marker"), egui::Sense::hover())
            .on_hover_text("Notes attached");
    }
}
