use super::*;

pub(super) const PALETTE: [[u8; 3]; 10] = [
    [239, 83, 80],
    [255, 167, 38],
    [255, 225, 70],
    [102, 187, 106],
    [38, 198, 218],
    [66, 165, 245],
    [92, 107, 192],
    [171, 71, 188],
    [236, 64, 122],
    [224, 224, 224],
];
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct ClassStyle {
    pub color: [u8; 3],
    pub enabled: bool,
}
impl Default for ClassStyle {
    fn default() -> Self {
        Self {
            color: PALETTE[4],
            enabled: false,
        }
    }
}
/// Allocate one bounded row. Trailing padding belongs inside this rectangle,
/// so child controls cannot increase the sidebar's minimum width.
pub(super) fn row(
    ui: &mut egui::Ui,
    text: String,
    selected: bool,
    enabled: bool,
    style: &mut ClassStyle,
) -> egui::Response {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.), egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    child.spacing_mut().item_spacing.x = 3.;
    let response = class_button(
        &mut child,
        text,
        selected,
        enabled,
        (rect.width() - 62.).max(1.),
    );
    controls(&mut child, style);
    response
}
pub(super) fn class_button(
    ui: &mut egui::Ui,
    text: String,
    selected: bool,
    enabled: bool,
    width: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, 22.),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            3,
            if selected {
                theme::SELECTION
            } else {
                theme::RAISED
            },
        );
    }
    let galley = egui::WidgetText::from(RichText::new(text).size(11.).color(if enabled {
        theme::YELLOW
    } else {
        theme::MUTED
    }))
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (width - 12.).max(1.),
        egui::FontSelection::Default,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 6., rect.center().y - galley.size().y * 0.5),
        galley,
        theme::YELLOW,
    );
    response
}
/// Eye outline, pupil, and a slash for the disabled state; no platform font symbols.
fn eye(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22., 22.), egui::Sense::click());
    let color = if enabled { theme::YELLOW } else { theme::MUTED };
    let center = rect.center();
    let points = (0..=24)
        .map(|i| {
            let t = i as f32 / 24. * std::f32::consts::TAU;
            center + egui::vec2(t.cos() * 8., t.sin() * 4.5)
        })
        .collect();
    ui.painter()
        .add(egui::Shape::line(points, egui::Stroke::new(1.2, color)));
    ui.painter().circle_filled(center, 2., color);
    if !enabled {
        ui.painter().line_segment(
            [center + egui::vec2(-8., 7.), center + egui::vec2(8., -7.)],
            egui::Stroke::new(1.5, color),
        );
    }
    response.on_hover_text(if enabled {
        "Disable class copper color"
    } else {
        "Enable class copper color"
    })
}
pub(super) fn controls(ui: &mut egui::Ui, style: &mut ClassStyle) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22., 22.), egui::Sense::click());
    let color = Color32::from_rgb(style.color[0], style.color[1], style.color[2]);
    ui.painter().rect(
        rect.shrink(3.),
        4,
        color,
        egui::Stroke::new(1., theme::MUTED),
        egui::StrokeKind::Inside,
    );
    let response = response.on_hover_text("PCB copper color");
    egui::Popup::menu(&response).show(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4., 4.);
        for row in PALETTE.chunks(5) {
            ui.horizontal(|ui| {
                for color in row {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(24., 24.), egui::Sense::click());
                    let rgb = Color32::from_rgb(color[0], color[1], color[2]);
                    ui.painter().rect(
                        rect.shrink(2.),
                        4,
                        rgb,
                        egui::Stroke::new(
                            if style.color == *color { 2. } else { 1. },
                            if style.color == *color {
                                Color32::WHITE
                            } else {
                                theme::MUTED
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    if response.clicked() {
                        style.color = *color;
                        style.enabled = true;
                        ui.close();
                    }
                }
            });
        }
    });
    if eye(ui, style.enabled).clicked() {
        style.enabled = !style.enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn class_rows_shrink_with_sidebar_and_keep_trailing_controls_aligned() {
        let ctx = egui::Context::default();
        for width in [400., 250., 330., 260.] {
            let mut panel_rect = egui::Rect::NOTHING;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900., 600.),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    panel_rect = egui::SidePanel::left("test_classes")
                        .exact_width(width)
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            for (index, name) in ["Default (72)", "HV_100V (1)", "HV_60V (9)"]
                                .iter()
                                .enumerate()
                            {
                                egui::collapsing_header::CollapsingState::load_with_default_open(
                                    ctx,
                                    egui::Id::new(index),
                                    false,
                                )
                                .show_header(ui, |ui| {
                                    row(ui, (*name).into(), false, true, &mut ClassStyle::default())
                                })
                                .body(|_| {});
                            }
                        })
                        .response
                        .rect;
                },
            );
            assert!(
                (panel_rect.width() - width).abs() < 0.1,
                "class rows must not force the sidebar wider"
            );
            let swatches: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| {
                    if let egui::Shape::Rect(r) = &s.shape
                        && r.fill == Color32::from_rgb(38, 198, 218)
                    {
                        Some(r.rect)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(swatches.len(), 3);
            assert!(
                swatches
                    .iter()
                    .all(|r| (r.center().x - swatches[0].center().x).abs() < 0.1)
            );
            assert!(
                swatches
                    .iter()
                    .all(|r| panel_rect.right() - r.right() >= 34.)
            );
        }
    }
}
