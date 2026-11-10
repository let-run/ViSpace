use super::*;
use crate::{
    attachments::AttachmentDraft,
    markdown_notes::{self, Row},
    markdown_visual::{Object, Visual},
    notes::Document,
};
use anyhow::Context;
use egui::text::CCursorRange;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};
use surfaces as form;

pub(super) struct NotesEditor {
    target: NotesTarget,
    title: String,
    owner: String,
    original: Notes,
    draft: Option<std::sync::Arc<AttachmentDraft>>,
    load_failed: bool,
    source: String,
    images: BTreeMap<String, String>,
    rows: Vec<Row>,
    selection: Range<usize>,
    source_mode: bool,
    #[cfg(not(target_arch = "wasm32"))]
    paste_handled: bool,
    visual_cursor: Option<Range<usize>>,
    history: Vec<String>,
    history_index: usize,
    restore_focus: bool,
    jump: Option<usize>,
    collapsed: BTreeSet<usize>,
    error: Option<String>,
    parse_error: Option<String>,
    export: Option<crate::tasks::Task<()>>,
}
impl NotesEditor {
    pub fn new(target: NotesTarget, title: String, notes: Notes) -> Self {
        let owner = match &target {
            NotesTarget::Part(id) | NotesTarget::Purchase(id) => id.clone(),
            NotesTarget::Project(_) => format!("project-notes-{}", id()),
        };
        let draft = AttachmentDraft::new();
        let loaded = draft
            .as_ref()
            .map_err(|e| anyhow::anyhow!(e.to_string()))
            .and_then(|d| Document::load(&notes, d));
        let (source, images, error) =
            if let Some(source) = notes.extra.get("markdownSource").and_then(|s| s.as_str()) {
                (
                    source.to_owned(),
                    notes.images.clone(),
                    loaded.err().map(|e| e.to_string()),
                )
            } else {
                match loaded {
                    Ok(document) => {
                        let (s, i) = markdown_notes::encode(&document);
                        (s, i, None)
                    }
                    Err(e) => (
                        notes.text.clone(),
                        notes.images.clone(),
                        Some(e.to_string()),
                    ),
                }
            };
        let mut editor = Self {
            target,
            title,
            owner,
            original: notes,
            load_failed: error.is_some(),
            draft: draft.ok().map(std::sync::Arc::new),
            source: source.clone(),
            images,
            rows: Vec::new(),
            selection: 0..0,
            source_mode: true,
            #[cfg(not(target_arch = "wasm32"))]
            paste_handled: false,
            visual_cursor: None,
            history: vec![source.clone()],
            history_index: 0,
            restore_focus: false,
            jump: None,
            collapsed: BTreeSet::new(),
            error,
            parse_error: None,
            export: None,
        };
        editor.parse();
        editor
    }
    fn parse(&mut self) {
        match markdown_notes::parse(&self.source) {
            Ok(rows) => {
                self.rows = rows;
                self.parse_error = None;
            }
            Err(e) => self.parse_error = Some(e.to_string()),
        }
    }
    fn record(&mut self) {
        if self.history[self.history_index] != self.source {
            self.history.truncate(self.history_index + 1);
            self.history.push(self.source.clone());
            if self.history.len() > 100 {
                self.history.remove(0);
            }
            self.history_index = self.history.len() - 1;
        }
    }
    fn insert(&mut self, text: &str) {
        let mut chars: Vec<_> = self.source.chars().collect();
        let start = self.selection.start.min(chars.len());
        let end = self.selection.end.max(start).min(chars.len());
        chars.splice(start..end, text.chars());
        self.source = chars.into_iter().collect();
        let end = start + text.chars().count();
        self.selection = end..end;
        self.parse();
        self.record();
        self.restore_focus = true;
        self.source_mode = true;
    }
    fn image(&mut self, path: String) {
        let visual = !self.source_mode;
        let key = format!("{}.png", id());
        self.images.insert(key.clone(), path);
        self.insert(&format!("\n\n![Image]({key} \"width=480\")\n\n"));
        if visual {
            self.source_mode = false;
            let model = Visual::new(&self.source);
            let byte = self
                .source
                .char_indices()
                .nth(self.selection.start)
                .map_or(self.source.len(), |(i, _)| i);
            let cursor = model
                .characters
                .iter()
                .position(|c| c.source.start >= byte)
                .unwrap_or(model.characters.len());
            self.visual_cursor = Some(cursor..cursor);
        }
    }
    fn paste_image(&mut self) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        {
            crate::web::paste_notes_image(self.owner.clone());
            Ok(())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let image = self
                .draft
                .as_ref()
                .context("Image draft unavailable")?
                .clipboard_image()?;
            self.image(
                image["originalPath"]
                    .as_str()
                    .context("Image missing")?
                    .into(),
            );
            Ok(())
        }
    }
    #[cfg(target_arch = "wasm32")]
    pub(super) fn browser_owner(&self) -> String {
        self.owner.clone()
    }
    #[cfg(target_arch = "wasm32")]
    pub(super) fn browser_image(&mut self, owner: &str, bytes: &[u8]) -> Result<()> {
        if self.owner == owner {
            let image = self
                .draft
                .as_ref()
                .context("Image draft unavailable")?
                .image_bytes(bytes)?;
            self.image(
                image["originalPath"]
                    .as_str()
                    .context("Image missing")?
                    .into(),
            );
        }
        Ok(())
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.source_mode, true, "Markdown · LaTeX");
            ui.selectable_value(&mut self.source_mode, false, "WYSIWYG");
            ui.separator();
            for (label, marker) in [("B", "**"), ("I", "*")] {
                if ui.button(RichText::new(label).color(GOLD)).clicked() {
                    let selected: String = self
                        .source
                        .chars()
                        .skip(self.selection.start)
                        .take(self.selection.len())
                        .collect();
                    let visual_mode = !self.source_mode;
                    self.insert(&format!("{marker}{selected}{marker}"));
                    if visual_mode {
                        self.source_mode = false;
                        let model = Visual::new(&self.source);
                        let byte = self
                            .source
                            .char_indices()
                            .nth(self.selection.start)
                            .map_or(self.source.len(), |(i, _)| i);
                        let cursor = model
                            .characters
                            .iter()
                            .position(|c| c.source.start >= byte)
                            .unwrap_or(model.characters.len());
                        self.visual_cursor = Some(cursor..cursor);
                    }
                }
            }
            if ui.button("Upload image").clicked()
                && let Some(path) = crate::platform::dialogs::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp", "tiff"])
                    .pick_file()
            {
                match self
                    .draft
                    .as_ref()
                    .context("Image draft unavailable")
                    .and_then(|d| d.import_image(&path))
                {
                    Ok(image) => self.image(image["originalPath"].as_str().unwrap().into()),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            if ui.button("Paste image").clicked()
                && let Err(e) = self.paste_image()
            {
                self.error = Some(e.to_string());
            }
            ui.separator();
            let exporting = self.export.is_some();
            if ui
                .add_enabled(
                    !exporting && self.parse_error.is_none(),
                    egui::Button::new(if exporting {
                        "Exporting…"
                    } else {
                        "Export PDF"
                    }),
                )
                .clicked()
            {
                self.export_pdf(ui.ctx());
            }
        });
        form::hint(
            ui,
            "# Heading · **bold** · $inline math$ · $$display math$$ · Image size: ![Image](name.png \"width=480\") or \"width=75%\". Paste images with Ctrl/⌘V.",
        );
    }
    fn source_field(&mut self, ui: &mut egui::Ui) {
        let widget = egui::Id::new(("notes_markdown_source", &self.owner));
        let restoring = std::mem::take(&mut self.restore_focus);
        if restoring {
            ui.memory_mut(|m| m.request_focus(widget));
            let mut state = egui::TextEdit::load_state(ui.ctx(), widget).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::two(
                egui::text::CCursor::new(self.selection.start),
                egui::text::CCursor::new(self.selection.end),
            )));
            state.store(ui.ctx(), widget);
        }
        let output = egui::TextEdit::multiline(&mut self.source)
            .id(widget)
            .font(egui::FontId::monospace(14.))
            .desired_width(f32::INFINITY)
            .desired_rows(26)
            .frame(false)
            .lock_focus(true)
            .char_limit(2_000_000)
            .hint_text("# Notes\n\nWrite Markdown with $LaTeX$ formulas…")
            .show(ui);
        if restoring {
            let rect = output
                .galley
                .pos_from_cursor(egui::text::CCursor::new(self.selection.start))
                .translate(output.galley_pos.to_vec2());
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        }
        if output.response.has_focus()
            && let Some(range) = output.cursor_range
        {
            self.selection = range.as_sorted_char_range();
        }
        if output.response.changed() {
            self.parse();
            self.record();
        }
        if let Some(index) = self.jump.take()
            && let Some(row) = self.rows.get(index)
        {
            let count = self.source[..row.range.start.min(self.source.len())]
                .chars()
                .count();
            self.selection = count..count;
            self.restore_focus = true;
            let cursor = egui::text::CCursor::new(count);
            let rect = output
                .galley
                .pos_from_cursor(cursor)
                .translate(output.galley_pos.to_vec2());
            ui.scroll_to_rect(rect, Some(egui::Align::Min));
        }
    }
    fn outline(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("CONTENTS").color(theme::MUTED).strong());
        ui.add_space(10.);
        let mut hidden_below = None;
        for index in 0..self.rows.len() {
            let row = &self.rows[index];
            if row.heading == 0 {
                continue;
            }
            if let Some(level) = hidden_below {
                if row.heading > level {
                    continue;
                }
                hidden_below = None;
            }
            let level = row.heading;
            let child = self.rows[index + 1..]
                .iter()
                .find(|r| r.heading > 0)
                .is_some_and(|r| r.heading > level);
            let closed = self.collapsed.contains(&index);
            ui.horizontal(|ui| {
                ui.add_space(f32::from(level.saturating_sub(1)) * 12.);
                if child {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(14., 18.), egui::Sense::click());
                    let center = rect.center();
                    let points = if closed {
                        vec![
                            center + egui::vec2(-2., -4.),
                            center + egui::vec2(2., 0.),
                            center + egui::vec2(-2., 4.),
                        ]
                    } else {
                        vec![
                            center + egui::vec2(-4., -2.),
                            center + egui::vec2(0., 2.),
                            center + egui::vec2(4., -2.),
                        ]
                    };
                    ui.painter().add(egui::Shape::line(
                        points,
                        egui::Stroke::new(1.5, theme::TEXT),
                    ));
                    if response.clicked() {
                        if closed {
                            self.collapsed.remove(&index);
                        } else {
                            self.collapsed.insert(index);
                        }
                    }
                } else {
                    ui.add_space(20.);
                }
                if ui
                    .add(
                        egui::Label::new(RichText::new(row.title()).color(theme::TEXT))
                            .truncate()
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text(row.title())
                    .clicked()
                {
                    self.jump = Some(index);
                }
            });
            if child && closed {
                hidden_below = Some(level);
            }
        }
        if !self.rows.iter().any(|r| r.heading > 0) {
            form::hint(ui, "Use # headings to navigate your notes.");
        }
    }
    fn preview(&mut self, ui: &mut egui::Ui) {
        let visual = Visual::new(&self.source);
        let width = ui.available_width().max(50.);
        let mut objects = BTreeMap::new();
        for (index, object) in &visual.objects {
            let image = match object {
                Object::Math(source, display) => match crate::notes::math_svg(source, *display) {
                    Ok(svg) => egui::Image::from_bytes(
                        format!(
                            "bytes://notes-math-{}.svg",
                            egui::util::hash((source, display))
                        ),
                        svg.into_bytes(),
                    )
                    .fit_to_original_size(1.)
                    .max_width(width)
                    .max_height(300.),
                    Err(_) => continue,
                },
                Object::Image(key, requested, percent) => {
                    let Some(path) = self.images.get(key) else {
                        continue;
                    };
                    let shown = if *percent {
                        width * requested / 100.
                    } else {
                        *requested
                    }
                    .min(width)
                    .max(1.);
                    crate::platform::image_widget(path)
                        .fit_to_original_size(1.)
                        .max_width(shown)
                }
            };
            let size = match image.load_for_size(ui.ctx(), egui::vec2(width, 800.)) {
                Ok(egui::load::TexturePoll::Ready { texture }) => {
                    image.calc_size(egui::vec2(width, 800.), Some(texture.size))
                }
                _ => {
                    ui.ctx().request_repaint();
                    egui::vec2(80., 40.)
                }
            };
            objects.insert(*index, (image, size));
        }
        let widget = egui::Id::new(("notes_visual_editor", &self.owner));
        if let Some(range) = self.visual_cursor.take() {
            ui.memory_mut(|m| m.request_focus(widget));
            let mut state = egui::TextEdit::load_state(ui.ctx(), widget).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::two(
                egui::text::CCursor::new(range.start),
                egui::text::CCursor::new(range.end),
            )));
            state.store(ui.ctx(), widget);
        }
        let mut value = visual.text.clone();
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, width: f32| {
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = width;
            for (index, c) in text.as_str().chars().enumerate() {
                let ch = visual.characters.get(index);
                let size = ch.map_or(15., |c| c.size);
                let mut format = egui::TextFormat {
                    font_id: if ch.is_some_and(|c| c.code) {
                        egui::FontId::monospace(size)
                    } else {
                        egui::FontId::proportional(size)
                    },
                    color: theme::TEXT,
                    italics: ch.is_some_and(|c| c.style.italic),
                    valign: egui::Align::Center,
                    ..Default::default()
                };
                if ch.is_some_and(|c| c.style.bold) {
                    format.extra_letter_spacing = 0.25;
                }
                if c == '\u{fffc}'
                    && let Some((_, dimensions)) = objects.get(&index)
                {
                    let advance = ui
                        .fonts_mut(|f| {
                            f.layout_no_wrap(
                                "\u{fffc}".into(),
                                egui::FontId::proportional(size),
                                Color32::TRANSPARENT,
                            )
                            .size()
                            .x
                        })
                        .max(1.);
                    format.font_id =
                        egui::FontId::proportional(size * (dimensions.x + 4.) / advance);
                    format.line_height = Some(dimensions.y + 4.);
                    format.color = Color32::TRANSPARENT;
                }
                job.append(&c.to_string(), 0., format);
            }
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let output = egui::TextEdit::multiline(&mut value)
            .id(widget)
            .font(egui::FontId::proportional(15.))
            .desired_width(f32::INFINITY)
            .desired_rows(24)
            .frame(false)
            .layouter(&mut layouter)
            .hint_text("Write notes here…")
            .show(ui);
        let mut bold = (*output.galley.job).clone();
        for section in &mut bold.sections {
            if section.format.extra_letter_spacing == 0. {
                section.format.color = Color32::TRANSPARENT;
            }
        }
        let bold = ui.fonts_mut(|f| f.layout_job(bold));
        ui.painter().with_clip_rect(output.text_clip_rect).galley(
            output.galley_pos + egui::vec2(0.4, 0.),
            bold,
            Color32::TRANSPARENT,
        );
        if output.response.has_focus()
            && let Some(range) = output.cursor_range
        {
            let range = range.as_sorted_char_range();
            let bytes = visual.selection(range, &self.source);
            self.selection = self.source[..bytes.start].chars().count()
                ..self.source[..bytes.end].chars().count();
        }
        if output.response.changed() {
            let (source, caret) = visual.edit_at_cursor(
                &self.source,
                &value,
                output.cursor_range.map(|r| r.primary.index),
            );
            let next = Visual::new(&source);
            if next.text != value {
                let cursor = next
                    .characters
                    .iter()
                    .position(|c| c.source.start >= caret)
                    .unwrap_or(next.characters.len());
                self.visual_cursor = Some(cursor..cursor);
            }
            self.source = source;
            self.parse();
            self.record();
        }
        for (index, (image, size)) in objects {
            let at = output
                .galley
                .pos_from_cursor(egui::text::CCursor::new(index))
                .translate(output.galley_pos.to_vec2());
            let rect = egui::Rect::from_min_size(
                egui::pos2(at.min.x + 2., at.center().y - size.y / 2.),
                size,
            );
            image.paint_at(ui, rect);
            if let Some((_, Object::Math(..))) = visual.objects.iter().find(|(i, _)| *i == index)
                && ui
                    .interact(rect, widget.with(index), egui::Sense::click())
                    .on_hover_text("Edit formula in Markdown · LaTeX")
                    .clicked()
            {
                let byte = visual.characters[index].source.start;
                let count = self.source[..byte].chars().count();
                self.selection = count..count;
                self.source_mode = true;
                self.restore_focus = true;
            }
        }
        if let Some(index) = self.jump.take()
            && let Some(row) = self.rows.get(index)
        {
            let cursor = visual
                .characters
                .iter()
                .position(|c| c.source.start >= row.range.start)
                .unwrap_or(0);
            ui.scroll_to_rect(
                output
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(cursor))
                    .translate(output.galley_pos.to_vec2()),
                Some(egui::Align::Min),
            );
        }
    }
    fn export_pdf(&mut self, ctx: &egui::Context) {
        let Some(path) = crate::platform::dialogs::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name("notes.pdf")
            .save_file()
        else {
            return;
        };
        let source = self.source.clone();
        let images = self.images.clone();
        #[cfg(not(target_arch = "wasm32"))]
        let draft = self.draft.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let ctx = ctx.clone();
            self.export = Some(crate::tasks::Task::spawn(
                move |_| {
                    let _keep_draft_alive = draft;
                    let bytes = crate::notes_pdf::render(&source, &images)?;
                    crate::platform::fs::write(path, bytes)?;
                    Ok(())
                },
                Box::new(move || ctx.request_repaint()),
            ));
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.export = Some(crate::tasks::Task::browser(
                crate::browser_jobs::Input::NotesPdf { source, images },
                move |value| {
                    use base64::Engine;
                    let encoded = value.as_str().context("Invalid PDF worker response")?;
                    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                    crate::platform::fs::write(path, bytes)?;
                    Ok(())
                },
                ctx.clone(),
            ));
        }
    }
}
impl InventoryApp {
    pub(super) fn notes_window(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.notes_editor.take() else {
            return;
        };
        if let Some(task) = &editor.export
            && let Some(result) = task.poll()
        {
            editor.export = None;
            match result {
                Ok(()) => self.status = "Notes PDF exported".into(),
                Err(e) => editor.error = Some(e),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // egui-winit consumes the press for Cmd/Ctrl+V and emits no Paste event
            // for an image-only clipboard. Its key release is still delivered.
            let paste = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
            let release=ctx.input(|i|i.events.iter().any(|e|matches!(e,egui::Event::Key{key:egui::Key::V,pressed:false,modifiers,..} if modifiers.command)));
            if (paste || release) && !editor.paste_handled && editor.paste_image().is_ok() {
                editor.paste_handled = true;
                ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Paste(_))));
            }
            if release {
                editor.paste_handled = false;
            }
        }
        if !editor.source_mode {
            let redo = ctx.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    egui::Key::Z,
                )
            });
            let undo = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z));
            if redo && editor.history_index + 1 < editor.history.len() {
                editor.history_index += 1;
            } else if undo && editor.history_index > 0 {
                editor.history_index -= 1;
            }
            if redo || undo {
                editor.source = editor.history[editor.history_index].clone();
                editor.parse();
            }
        }
        let response = form::sheet("notes_editor_markdown").show(ctx, |ui| {
            form::configure(ui, (ctx.content_rect().width() - 80.).clamp(600., 1280.));
            form::heading(ui, &editor.title, Icon::Notes);
            editor.toolbar(ui);
            let height = (ctx.content_rect().height() - 240.).clamp(220., 720.);
            let width = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(220., height + 28.),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::new()
                            .fill(theme::PANEL)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.set_width(200.);
                                egui::ScrollArea::vertical()
                                    .id_salt("notes_outline")
                                    .max_height(height)
                                    .show(ui, |ui| editor.outline(ui));
                            });
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2((width - 240.).max(200.), height + 28.),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        theme::frame(theme::PANEL)
                            .inner_margin(14)
                            .corner_radius(4)
                            .stroke(egui::Stroke::new(1., theme::LINE))
                            .show(ui, |ui| {
                                ui.set_width((width - 268.).max(180.));
                                egui::ScrollArea::vertical()
                                    .id_salt(("notes_document", editor.source_mode))
                                    .auto_shrink([false, false])
                                    .max_height(height)
                                    .show(ui, |ui| {
                                        if editor.source_mode {
                                            editor.source_field(ui);
                                        } else {
                                            editor.preview(ui);
                                        }
                                    });
                            });
                    },
                );
            });
            form::error(ui, &editor.parse_error);
            form::error(ui, &editor.error);
            form::actions(
                ui,
                "Save notes",
                editor.parse_error.is_none() && !editor.load_failed,
            )
        });
        if response.inner.0 {
            match self.save_portable_notes(&editor) {
                Ok(()) => {
                    self.status = "Notes saved".into();
                    return;
                }
                Err(error) if queued_for_approval(&error) => {
                    self.status = "Edit queued for leader approval".into();
                    return;
                }
                Err(e) => editor.error = Some(e.to_string()),
            }
        }
        if response.inner.1 || form::escape(&response) {
            return;
        }
        self.notes_editor = Some(editor);
    }
    fn save_portable_notes(&mut self, editor: &NotesEditor) -> Result<()> {
        let document = markdown_notes::document(&editor.source, &editor.images)?;
        let mut notes = document.save(&editor.original);
        notes.extra.remove("latexSource");
        notes.extra.insert(
            "markdownSource".into(),
            serde_json::Value::String(editor.source.clone()),
        );
        // Canonical image keys must remain stable when temporary images are promoted.
        notes.images.extend(editor.images.clone());
        let mut created = Vec::new();
        let result = (|| -> Result<()> {
            let mut store = self.store.lock().unwrap();
            if store.uses_write_approval() {
                let latest = match &editor.target {
                    NotesTarget::Part(id) => store
                        .data
                        .parts
                        .iter()
                        .find(|p| &p.id == id)
                        .map(|p| &p.notes),
                    NotesTarget::Purchase(id) => store
                        .data
                        .purchases
                        .iter()
                        .find(|p| &p.part.id == id)
                        .map(|p| &p.part.notes),
                    NotesTarget::Project(name) => store
                        .data
                        .projects
                        .iter()
                        .find(|p| key(&p.name) == key(name))
                        .map(|p| &p.notes),
                }
                .context("The item was removed while you were editing its notes")?;
                ensure!(
                    serde_json::to_value(latest)? == serde_json::to_value(&editor.original)?,
                    "Notes changed while you were editing. Your draft is still open."
                );
            }
            let mut promoted = BTreeMap::<String, String>::new();
            for path in notes.images.values_mut() {
                if let Some(saved) = promoted.get(path) {
                    *path = saved.clone();
                    continue;
                }
                if let Some(draft) = &editor.draft
                    && std::path::Path::new(path.as_str()).starts_with(draft.root())
                {
                    let saved = store.attach(std::path::Path::new(path), &editor.owner)?;
                    let old = path.clone();
                    *path = saved.to_string_lossy().into();
                    promoted.insert(old, path.clone());
                    created.push(saved);
                }
            }
            store.change(|data| data.save_notes(&editor.target, notes))
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
}
