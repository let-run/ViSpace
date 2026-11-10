use super::*;
impl InventoryApp {
    pub(super) fn browser_events(&mut self, ctx: &egui::Context) {
        crate::web::set_file_context(crate::web::FileContext {
            part: self.editor.as_ref().map(|e| e.part.id.clone()),
            notes: self.notes_editor.as_ref().map(|e| e.browser_owner()),
            project: if self.view == View::Projects {
                self.catalog.clone()
            } else {
                None
            },
        });
        for event in crate::web::take_events() {
            let result: Result<()> = (|| {
                match event {
                    crate::web::Event::GitSyncStatus(project,blocked)=>{if blocked{self.git_history.blocked.insert(project);}else{self.git_history.blocked.remove(&project);}},
                    crate::web::Event::Error(error) => anyhow::bail!(error),
                    crate::web::Event::LocalGit(project, operation, result) => {
                        if operation=="diagnostics"{
                            if self.git_history.project.as_deref()==Some(project.as_str()){
                                self.git_history.busy=false;match result{Ok(report)=>self.git_history.report=Some(report),Err(error)=>self.git_history.error=Some(error)}
                            }
                            return Ok(());
                        }
                        self.browser_git.busy=false;
                        match result {
                            Err(error)=>{if operation=="share"{self.status=format!("Git sharing failed: {error}");}self.browser_git.error=Some(error)},
                            Ok(value)=>{
                                self.browser_git.read_only=value["read_only"].as_bool().unwrap_or(false);
                                self.browser_git.initialized=value["initialized"].as_bool().unwrap_or(false);
                                self.browser_git.changes=serde_json::from_value(value["change_details"].clone()).unwrap_or_else(|_|value["changes"].as_array().map(|items|items.iter().filter_map(|v|v.as_str().map(|path|super::browser_git::Change{path:path.into(),kind:"Modified".into(),lines:None})).collect()).unwrap_or_default());
                                if let (Some(commit),Some(reference),Some(publisher))=(value["tip"].as_str(),value["reference"].as_str(),value["publisher"].as_str()) {
                                    let commit=commit.to_owned();let reference=reference.to_owned();let publisher=publisher.to_owned();
                                    let includes=value["ancestors"].as_array().map(|items|items.iter().filter_map(|v|v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                                    self.change(if operation=="share"{"Local Git repository shared"}else{"Local Git commit shared"},move|data|{
                                        if let Some(project)=data.projects.iter_mut().find(|p|p.id==project){
                                            if let Some(link)=&mut project.kicad && link.revision.starts_with("git-snapshot:"){
                                                link.revision=format!("git-snapshot:{}:{commit}",project.id);
                                            }
                                            project.git_tips.insert(publisher,GitTip{commit,reference,includes});
                                        }
                                        Ok(())
                                    });
                                    self.browser_git.changes.clear();
                                }
                                if operation=="share" && value["tip"].as_str().is_none(){self.status="KiCad folder attached. Initialize Git and create a commit to share its files.".into();}
                                if operation=="init" || operation=="import" {if let Some((project,folder))=&self.browser_git.project {self.browser_git.busy=true;crate::web::local_git(project,folder,"status","","","");}}
                            }
                        }
                    },
                    crate::web::Event::KicadFolderError(token,error)=>{
                        if let Some(editor)=self.catalog_editor.as_mut().filter(|editor|editor.token==token){editor.error=Some(error);}else{self.status=error;}
                    },
                    crate::web::Event::KicadFolder(token, selection) => {
                        if let Some(editor) =
                            self.catalog_editor.as_mut().filter(|e| e.token == token)
                        {
                            let choices = selection.choices;
                            let path = choices
                                .first()
                                .ok_or_else(|| anyhow::anyhow!("No .kicad_pro file was selected"))?
                                .clone();
                            editor.kicad = Some(KicadProject {
                                path,
                                revision: selection.managed_revision.unwrap_or_else(id),
                                files: selection.files,
                                browser_folder: selection.folder,
                                ..Default::default()
                            });
                            editor.kicad_choices = choices;
                            editor.error = None;
                        }
                    }
                    crate::web::Event::ModelsFolder(project, folder) => {
                        self.change("3D model library updated", move |data| {
                            if let Some(link) = data
                                .projects
                                .iter_mut()
                                .find(|p| p.name == project)
                                .and_then(|p| p.kicad.as_mut())
                            {
                                link.browser_models_folder = Some(folder);
                            }
                            Ok(())
                        });
                    }
                    crate::web::Event::KicadChanged(folder) => {
                        if let Some(link) = self
                            .data_snapshot
                            .projects
                            .iter()
                            .find(|p| {
                                self.view == View::Projects
                                    && self.catalog.as_ref() == Some(&p.name)
                            })
                            .and_then(|p| p.kicad.as_ref())
                            .filter(|l| l.browser_folder.as_deref() == Some(&folder))
                        {
                            self.kicad.refresh(link, ctx);
                        }
                    }
                    crate::web::Event::Csv(bytes, filename) => {
                        self.csv_loading = Some(crate::tasks::Task::browser(
                            crate::web::jobs::Input::Csv { bytes, filename },
                            |v| Ok(serde_json::from_value(v)?),
                            ctx.clone(),
                        ));
                    }
                    crate::web::Event::PartImage(id, bytes) => {
                        if let Some(editor) = self.editor.as_mut().filter(|e| e.part.id == id) {
                            editor.part.image = Some(editor.draft.image_bytes(&bytes)?);
                        }
                    }
                    crate::web::Event::PartPdf(id, bytes) => {
                        ensure!(bytes.starts_with(b"%PDF-"), "Choose a PDF file");
                        use base64::Engine;
                        if let Some(editor) = self.editor.as_mut().filter(|e| e.part.id == id) {
                            editor.part.datasheet = format!(
                                "data:application/pdf;base64,{}",
                                base64::engine::general_purpose::STANDARD.encode(bytes)
                            );
                        }
                    }
                    crate::web::Event::NotesImage(owner, bytes) => {
                        if let Some(editor) = self.notes_editor.as_mut() {
                            editor.browser_image(&owner, &bytes)?;
                        }
                    }
                    crate::web::Event::Bom(project, bytes) => {
                        let rows =
                            csv_io::read_bom(bytes.as_slice(), &self.data_snapshot, &project)?;
                        self.bom = Some(BomReview { project, rows });
                    }
                }
                Ok(())
            })();
            if let Err(e) = result {
                self.error = Some(e.to_string());
            }
        }
    }
}
