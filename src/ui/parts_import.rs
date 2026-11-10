use super::*;
use crate::{
    csv_import::{self, CsvImport},
    tasks::Task,
};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::Ordering;

impl InventoryApp {
    #[cfg(target_arch = "wasm32")]
    pub(super) fn import_csv(&mut self, _ctx: &egui::Context) {
        crate::web::pick_file(".csv", |bytes| {
            crate::web::Event::Csv(bytes, "import.csv".into())
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn import_csv(&mut self, ctx: &egui::Context) {
        if self.csv_loading.is_some() || self.import.is_some() {
            return;
        }
        if let Some(path) = crate::platform::dialogs::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .pick_file()
        {
            let filename = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let wake = ctx.clone();
            self.status = "Reading CSV…".into();
            self.csv_loading = Some(Task::spawn(
                move |_| CsvImport::read(crate::platform::fs::File::open(path)?, filename),
                Box::new(move || wake.request_repaint()),
            ));
        }
    }
    pub(super) fn poll_csv_import(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.csv_loading.as_ref().and_then(Task::poll) {
            self.csv_loading = None;
            match result {
                Ok(document) => {
                    self.import = Some(ImportReview {
                        document,
                        error: None,
                        applying: None,
                    });
                    self.status = "CSV ready to review".into();
                }
                Err(error) => self.error = Some(error),
            }
        }
        if self.csv_loading.is_some() || self.import.as_ref().is_some_and(|r| r.applying.is_some())
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    pub(super) fn import_window(&mut self, ctx: &egui::Context) {
        let Some(mut review) = self.import.take() else {
            return;
        };
        if let Some(result) = review.applying.as_ref().and_then(Task::poll) {
            review.applying = None;
            match result {
                Ok(()) => {
                    self.status = "CSV imported".into();
                    ctx.request_repaint();
                    return;
                }
                Err(error) => review.error = Some(error),
            }
        }
        let applying = review.applying.is_some();
        let suggestions = review.document.suggestions(
            self.data_snapshot
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
                ),
        );
        let mut apply = false;
        let mut cancel = false;
        let response=surfaces::sheet("csv_review").show(ctx,|ui|{
            surfaces::configure(ui,(ctx.content_rect().width()-104.).clamp(600.,1100.));
            surfaces::heading(ui,&format!("Import parts — {}",review.document.filename),Icon::Import);
            ui.label("Edit any cell and uncheck rows to skip. Matching part numbers or supplier/SPN pairs increase stock; other rows create new parts. Unknown columns become custom fields.");
            ui.add_enabled_ui(!applying,|ui|{
                ui.horizontal(|ui|{
                    if ui.button("Select all").clicked(){for row in &mut review.document.rows{row.selected=true;}}
                    if ui.button("Deselect all").clicked(){for row in &mut review.document.rows{row.selected=false;}}
                    ui.label(RichText::new(format!("{} of {} selected",review.document.rows.iter().filter(|r|r.selected).count(),review.document.rows.len())).color(theme::MUTED));
                });
                import_table(ui,&mut review.document,&suggestions,(ctx.content_rect().height()-280.).clamp(140.,620.));
            });
            surfaces::error(ui,&review.error);
            if applying{ui.horizontal(|ui|{ui.spinner();ui.label("Importing parts…");});}
            let selected=review.document.rows.iter().any(|r|r.selected);
            let actions=ui.add_enabled_ui(!applying,|ui|surfaces::actions(ui,"Apply",selected)).inner;
            (apply,cancel)=actions;
        });
        if apply {
            let document = review.document.clone();
            let data = self.data_snapshot.clone();
            let store = self.store.clone();
            let revision = self.store_revision;
            let wake = ctx.clone();
            review.error = None;
            #[cfg(not(target_arch = "wasm32"))]
            {
                review.applying = Some(Task::spawn(
                    move |cancel| {
                        let mut document = document;
                        let parts = document.selected_parts(&suggestions)?;
                        ensure!(!cancel.load(Ordering::Relaxed), "Import cancelled");
                        let mut next = (*data).clone();
                        next.import_parts(parts)?;
                        ensure!(!cancel.load(Ordering::Relaxed), "Import cancelled");
                        store
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Inventory is unavailable"))?
                            .commit_if_revision(revision, next)
                    },
                    Box::new(move || wake.request_repaint()),
                ));
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = wake;
                review.applying = Some(Task::browser(
                    crate::web::jobs::Input::CsvApply {
                        document,
                        suggestions: suggestions.clone(),
                        data: (*data).clone(),
                    },
                    move |value| {
                        store
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Inventory unavailable"))?
                            .commit_if_revision(revision, serde_json::from_value(value)?)
                    },
                    ctx.clone(),
                ));
            }
        }
        if applying || review.applying.is_some() || (!cancel && !surfaces::escape(&response)) {
            self.import = Some(review);
        }
    }
}
fn import_table(ui: &mut egui::Ui, doc: &mut CsvImport, suggestions: &[String], height: f32) {
    let label_column = doc.label_column();
    let widths: Vec<_> = doc
        .headers
        .iter()
        .enumerate()
        .map(|(i, _)| if i == label_column { 240. } else { 180. })
        .collect();
    let heights: Vec<_> = doc
        .rows
        .iter()
        .map(|r| {
            let chips = csv_import::labels(&r.values[label_column]);
            let total: f32 = chips
                .iter()
                .map(|s| theme::measure_text(ui, s, 13., 1).x + 40.)
                .sum();
            if chips.is_empty() {
                46.
            } else {
                48. + (total / 220.).ceil().max(1.) * 30.
            }
        })
        .collect();
    let mut edits = Vec::new();
    let mut additions = Vec::new();
    egui::ScrollArea::horizontal().id_salt("csv_columns").show(ui,|ui|{
        ui.set_min_width(82.+widths.iter().sum::<f32>());
        theme::table_header(ui,48.);
        let mut table=TableBuilder::new(ui).id_salt("csv_preview").striped(true).resizable(true)
            .max_scroll_height(height).cell_layout(egui::Layout::top_down(egui::Align::Min))
            .column(Column::exact(40.)).column(Column::exact(32.));
        for width in &widths{table=table.column(Column::initial(*width).at_least(100.).clip(true));}
        table.header(48.,|mut row|{
            row.col(|ui|{ui.label("Import");});row.col(|ui|{ui.label("#");});
            for (column,name) in doc.headers.iter().enumerate(){row.col(|ui|{
                ui.add(egui::Label::new(RichText::new(name).strong().color(theme::TEXT)).truncate());
                if csv_io::part_field(name).is_none(){
                    let mut include=!doc.ignored_custom.contains(&column);
                    if ui.checkbox(&mut include,"Custom field").changed(){if include{doc.ignored_custom.remove(&column);}else{doc.ignored_custom.insert(column);}}
                }
            });}
        }).body(|body|{
            body.heterogeneous_rows(heights.into_iter(),|mut row|{
                let index=row.index();row.set_overline(true);
                row.col(|ui|{ui.checkbox(&mut doc.rows[index].selected,"");});
                row.col(|ui|{ui.label(RichText::new((index+1).to_string()).color(theme::MUTED));});
                for column in 0..doc.headers.len(){row.col(|ui|{
                    if column==label_column{
                        let mut labels=csv_import::labels(&doc.rows[index].values[column]);let original=labels.clone();
                        if !labels.is_empty() { surfaces::chips(ui,&mut labels); }
                        if labels!=original{edits.push((index,column,labels.join(", ")));}
                        let choices=suggestions.iter().filter(|s|!labels.iter().any(|l|key(l)==key(s))).cloned().collect::<Vec<_>>();
                        let (response,chosen)=surfaces::suggestion_choice(ui,("csv_labels",index),&mut doc.rows[index].pending_label,choices);
                        response.clone().on_hover_text("Choose a label or type a new label and press Return. Repeat for multiple labels.");
                        if let Some(name)=chosen{additions.push((index,name));response.request_focus();}
                        else if response.lost_focus() && ui.input(|i|i.key_pressed(egui::Key::Enter)){
                            additions.push((index,doc.rows[index].pending_label.clone()));response.request_focus();
                        }
                    }else{
                        ui.add_enabled_ui(!doc.ignored_custom.contains(&column),|ui|{
                            if ui.add(egui::TextEdit::singleline(&mut doc.rows[index].values[column]).desired_width(f32::INFINITY)).changed(){edits.push((index,column,doc.rows[index].values[column].clone()));}
                        });
                    }
                });}
            });
        });
    });
    for (row, column, value) in edits {
        doc.set_value(row, column, value);
    }
    for (row, text) in additions {
        doc.add_labels(row, &text, suggestions);
    }
}
