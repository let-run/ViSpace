use super::*;
use inventory_barcode_camera::Session;

pub(super) struct Scanner {
    session: Session,
    texture: Option<egui::TextureHandle>,
    sequence: u64,
}
impl InventoryApp {
    pub(super) fn start_scanner(&mut self, ctx: &egui::Context) {
        if self.scanner.is_some() {
            return;
        }
        let ctx = ctx.clone();
        self.scanner = Some(Scanner {
            session: Session::start(self.camera_preference.clone(), move || {
                ctx.request_repaint()
            }),
            texture: None,
            sequence: 0,
        });
    }
    pub(super) fn scanner_window(&mut self, ctx: &egui::Context, data: &Inventory) {
        let Some(mut scanner) = self.scanner.take() else {
            return;
        };
        let state = scanner.session.snapshot();
        if let Some(code) = state.code {
            self.apply_scanned_code(code, data);
            return;
        }
        if let Some(frame) = state.frame.filter(|f| f.sequence != scanner.sequence) {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            if let Some(texture) = &mut scanner.texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                scanner.texture =
                    Some(ctx.load_texture("camera-preview", image, egui::TextureOptions::LINEAR));
            }
            scanner.sequence = frame.sequence;
        }
        let mut cancel = false;
        let mut retry = false;
        let mut selected = state.selected.clone();
        let response = surfaces::sheet("barcode_scanner").show(ctx, |ui| {
            let width = (ctx.content_rect().width() - 80.).clamp(240., 560.);
            surfaces::configure(ui, width);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Scan barcode").size(22.).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel = ui.button("Cancel").clicked();
                });
            });
            ui.label("Hold the part’s QR code or barcode in front of the camera.");
            egui::ComboBox::from_id_salt("scanner_camera")
                .width(width)
                .selected_text(
                    state
                        .devices
                        .iter()
                        .find(|d| d.id == selected)
                        .map(|d| d.name.as_str())
                        .unwrap_or("Waiting for camera…"),
                )
                .show_ui(ui, |ui| {
                    for device in &state.devices {
                        ui.selectable_value(&mut selected, device.id.clone(), &device.name);
                    }
                });
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(
                    width,
                    (ctx.content_rect().height() - 240.).clamp(120., 340.),
                ),
                egui::Sense::hover(),
            );
            ui.painter().rect_filled(rect, 4., Color32::BLACK);
            if let Some(texture) = &scanner.texture {
                let size = texture.size_vec2();
                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                let image_rect = egui::Rect::from_center_size(rect.center(), size * scale);
                ui.painter().image(
                    texture.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
                    Color32::WHITE,
                );
            } else {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Waiting for camera…",
                    egui::FontId::proportional(16.),
                    theme::MUTED,
                );
            }
            if let Some(error) = &state.error {
                ui.colored_label(Color32::LIGHT_RED, error);
                retry = ui.button("Retry camera").clicked();
            }
        });
        if !state.selected.is_empty() && self.camera_preference != state.selected {
            self.camera_preference = state.selected;
            if let Ok(bytes) = serde_json::to_vec(&self.camera_preference) {
                let _ = crate::platform::fs::write(&self.camera_settings_path, bytes);
            }
        }
        if selected != self.camera_preference && !selected.is_empty() {
            if state.error.is_some() {
                retry = true;
            } else {
                scanner.session.select(selected.clone());
            }
            scanner.texture = None;
        }
        if retry {
            let wake = ctx.clone();
            scanner.session = Session::start(selected, move || wake.request_repaint());
            scanner.texture = None;
            scanner.sequence = 0;
        }
        if !cancel && !response.should_close() {
            self.scanner = Some(scanner);
        }
    }
    fn apply_scanned_code(&mut self, code: String, data: &Inventory) {
        self.switch_view(View::Parts);
        self.query = code;
        self.labels.clear();
        self.page = 0;
        self.bulk = None;
        self.part_preview = None;
        self.catalog = None;
        self.selected = scan_match(data, &self.query);
        self.status = if self.selected.is_some() {
            "Barcode found".into()
        } else {
            "No matching parts".into()
        };
    }
}

fn scan_match(data: &Inventory, code: &str) -> Option<String> {
    data.parts
        .iter()
        .filter(|p| p.matches(code))
        .min_by(|a, b| part_number_cmp(&a.number, &b.number))
        .map(|p| p.id.clone())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn barcode_uses_normal_inventory_search_and_excludes_cart() {
        let part = Part {
            number: "R2".into(),
            spn: "C210262".into(),
            ..Default::default()
        };
        let barcode = part.barcode.clone();
        let id = part.id.clone();
        let data = Inventory {
            parts: vec![part],
            ..Default::default()
        };
        assert_eq!(scan_match(&data, &barcode), Some(id.clone()));
        assert_eq!(scan_match(&data, "C210262"), Some(id));
        assert_eq!(scan_match(&data, "unmatched"), None);
    }
}
