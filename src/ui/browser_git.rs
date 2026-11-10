use super::*;
#[derive(Default)]
pub(super) struct State {
    pub project: Option<(String, String)>,
    pub busy: bool,
    pub initialized: bool,
    pub read_only:bool,
    pub changes: Vec<Change>,
    pub error: Option<String>,
    message: String,
}
#[derive(Clone,serde::Deserialize)]
pub(super) struct Change{pub path:String,pub kind:String,#[serde(default)] pub lines:Option<(usize,usize)>}
#[derive(serde::Serialize,serde::Deserialize)]
#[serde(default)]
pub(super) struct Author{pub name:String,pub email:String}
impl Default for Author{fn default()->Self{Self{name:"WASM Application".into(),email:"support@let.run".into()}}}
pub(super) fn author()->Author{
    crate::web::storage().ok().and_then(|storage|storage.get_item("vinventory.git.author").ok().flatten()).and_then(|value|serde_json::from_str(&value).ok()).unwrap_or_default()
}
impl InventoryApp {
    pub(super) fn open_browser_git(&mut self, project: &Project) {
        if let Some(folder) = project
            .kicad
            .as_ref()
            .and_then(|link| link.browser_folder.clone())
        {
            self.browser_git.project = Some((project.id.clone(), folder));
            self.browser_git.changes.clear();
            self.browser_git.error = None;
            self.start_browser_git("status");
        }
    }
    fn start_browser_git(&mut self, operation: &str) {
        if let Some((project, folder)) = &self.browser_git.project {
            self.browser_git.busy = true;
            self.browser_git.error = None;
            let author=author();
            crate::web::local_git(
                project,
                folder,
                operation,
                &self.browser_git.message,
                &author.name,
                &author.email,
            );
        }
    }
    pub(super) fn browser_git_window(&mut self, ctx: &egui::Context) {
        if self.browser_git.project.is_none() {
            return;
        }
        let mut open = true;
        let mut operation = None;
        let project_id=&self.browser_git.project.as_ref().unwrap().0;
        let project=self.data_snapshot.projects.iter().find(|project|&project.id==project_id).map(|project|project.name.clone()).unwrap_or_default();
        let response=surfaces::sheet("browser_git").show(ctx,|ui|{
            surfaces::configure(ui,520.);
            surfaces::heading(ui,&format!("Commit — {project}"),Icon::Commit);
            ui.add_enabled_ui(!self.browser_git.busy,|ui|{
                if self.browser_git.read_only {
                    ui.label("This is a received snapshot without a working Git repository. Choose a writable repository folder in Edit project to enable commits.");
                }else if !self.browser_git.initialized {
                    ui.label("Initialize Git to commit and share this project.");
                    if ui.button("Initialize repository").clicked(){operation=Some("init");}
                }else{
                    use egui_extras::{TableBuilder,Column};
                    let changes=&self.browser_git.changes;
                    if changes.is_empty() && !self.browser_git.busy{ui.label("Nothing to commit");}
                    if !changes.is_empty(){
                        let width=ui.available_width()*0.36;
                        TableBuilder::new(ui).id_salt("browser_git_changes").striped(true).max_scroll_height(232.)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::remainder().clip(true)).column(Column::exact(width).clip(true))
                            .header(24.,|mut row|{row.col(|ui|{ui.strong("Name");});row.col(|ui|{ui.strong("Changes");});})
                            .body(|body|{body.rows(23.,changes.len(),|mut row|{
                                let change=&changes[row.index()];let color=if change.kind=="Removed"{Color32::LIGHT_RED}else{theme::GREEN};
                                row.col(|ui|{ui.add(egui::Label::new(RichText::new(&change.path).size(12.).color(color)).truncate()).on_hover_text(&change.path);});
                                row.col(|ui|{ui.label(RichText::new(&change.kind).size(12.));if let Some((added,removed))=change.lines{ui.label(RichText::new(format!("+{added}")).size(12.).color(theme::GREEN));ui.label(RichText::new(format!("−{removed}")).size(12.).color(Color32::LIGHT_RED));}});
                            });});
                    }
                    surfaces::row(ui,"Commit message",|ui|{surfaces::field(ui,&mut self.browser_git.message);});
                    ui.horizontal(|ui|{
                        if ui.small_button("Refresh changes").clicked(){operation=Some("status");}
                        if ui.small_button("Import received history").clicked(){operation=Some("import");}
                    });
                }
            });
            if self.browser_git.busy{ui.spinner();}
            surfaces::error(ui,&self.browser_git.error);
            ui.add_space(8.);
            ui.add_enabled_ui(!self.browser_git.busy,|ui|{
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                    let author=author();
                    let enabled=self.browser_git.initialized&&!self.browser_git.changes.is_empty()&&!self.browser_git.message.trim().is_empty()&&!author.name.trim().is_empty()&&!author.email.trim().is_empty();
                    if ui.add_enabled(enabled,egui::Button::new(RichText::new("Commit").color(Color32::WHITE)).fill(theme::PURPLE).min_size(egui::vec2(90.,28.))).clicked(){operation=Some("commit");}
                    if ui.add(egui::Button::new("Cancel").fill(theme::RAISED).min_size(egui::vec2(76.,28.))).clicked(){open=false;}
                });
            });
        });
        if surfaces::escape(&response) {
            open = false;
        }
        if let Some(operation) = operation {
            self.start_browser_git(operation);
        }
        if !open && !self.browser_git.busy {
            self.browser_git.project = None;
        }
    }
}
