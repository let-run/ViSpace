//! Read-only project Git history and receiver diagnostics.
use super::*;
#[derive(Default)]
pub(super) struct State {
    pub project:Option<String>,
    pub report:Option<serde_json::Value>,
    pub error:Option<String>,
    pub busy:bool,
    pub blocked:std::collections::BTreeSet<String>,
    diagnostics:bool,
    limit:usize,
    #[cfg(not(target_arch="wasm32"))]
    pending:Option<crate::tasks::Task<serde_json::Value>>,
}
impl InventoryApp {
    pub(super) fn project_git_blocked(&self,project:&Project)->bool{
        #[cfg(not(target_arch="wasm32"))]
        {crate::git_bundle::sync_blocked(project)}
        #[cfg(target_arch="wasm32")]
        {project.unresolved_git_tips().len()>1||self.git_history.blocked.contains(&project.id)}
    }
    pub(super) fn open_git_history(&mut self,project:&Project,ctx:&egui::Context){
        self.git_history.project=Some(project.id.clone());self.git_history.report=None;
        self.git_history.diagnostics=false;self.git_history.limit=30;
        self.request_git_history(project,ctx);
    }
    fn request_git_history(&mut self,project:&Project,ctx:&egui::Context){
        self.git_history.error=None;self.git_history.busy=true;
        let limit=self.git_history.limit;
        #[cfg(target_arch="wasm32")]
        {let folder=project.kicad.as_ref().and_then(|link|link.browser_folder.as_deref()).unwrap_or("");crate::web::local_git(&project.id,folder,"diagnostics",&limit.to_string(),"","");let _=ctx;}
        #[cfg(not(target_arch="wasm32"))]
        {let store=self.store.clone();let id=project.id.clone();let wake=ctx.clone();self.git_history.pending=Some(crate::tasks::Task::spawn(move |_|crate::git_bundle::diagnostics_limit(&store,&id,limit),Box::new(move||wake.request_repaint())));}
    }
    pub(super) fn git_history_window(&mut self,ctx:&egui::Context){
        let Some(id)=self.git_history.project.clone() else{return;};
        let Some(project)=self.data_snapshot.projects.iter().find(|p|p.id==id).cloned() else{self.git_history.project=None;return;};
        #[cfg(not(target_arch="wasm32"))]
        if let Some(result)=self.git_history.pending.as_ref().and_then(crate::tasks::Task::poll){self.git_history.pending=None;self.git_history.busy=false;match result{Ok(report)=>self.git_history.report=Some(report),Err(error)=>self.git_history.error=Some(error)}}
        let mut close=false;let mut refresh=false;let mut more=false;
        let response=surfaces::sheet("project_git_history").show(ctx,|ui|{
            surfaces::configure(ui,700.);surfaces::heading(ui,&format!("Git history — {}",project.name),Icon::GitLog);
            ui.horizontal(|ui|{
                ui.selectable_value(&mut self.git_history.diagnostics,false,"Git log");
                ui.selectable_value(&mut self.git_history.diagnostics,true,"Diagnostics");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                    if ui.add_enabled(!self.git_history.busy,egui::Button::new("Refresh")).clicked(){refresh=true;}
                    if self.git_history.busy{ui.spinner();}
                });
            });
            ui.separator();
            if self.git_history.diagnostics{
                if ui.button("Copy diagnostics").clicked(){ui.ctx().copy_text(serde_json::json!({"project":project.name,"announcements":project.git_tips,"binding":project.kicad,"local":self.git_history.report,"error":self.git_history.error}).to_string());}
                egui::ScrollArea::vertical().id_salt("git_diagnostics").max_height(470.).show(ui,|ui|{
                    let tips=project.unresolved_git_tips();
                    let applied=project.kicad.as_ref().and_then(|link|link.revision.strip_prefix("git-snapshot:")).and_then(|rest|rest.split_once(':').map(|(_,tip)|tip));
                    let actual=self.git_history.report.as_ref().and_then(|r|r["head"].as_str());
                    let blocked=self.project_git_blocked(&project);
                    let status=if tips.len()>1{"Competing tips — manual resolution required"}else if blocked{"Update blocked — manual resolution required"}else if let Some(tip)=tips.iter().next(){if actual.is_some_and(|head|head!=*tip){"Update pending — checkout HEAD differs from advertised commit"}else if actual==Some(*tip)&&applied.is_some_and(|head|head!=*tip){"Checkout updated — viewer refresh pending"}else if actual==Some(*tip)||applied==Some(*tip){"Latest advertised commit applied"}else{"Update pending — advertised commit differs from checkout"}}else{"No Git commit announced"};
                    ui.label(RichText::new(status).color(if blocked{Color32::LIGHT_RED}else if status.contains("pending"){GOLD}else{theme::GREEN}));
                    ui.label(RichText::new("Viewer applied commit").color(theme::MUTED));ui.monospace(applied.unwrap_or("Not recorded (local folder or older attachment)"));
                    ui.label(RichText::new("Checkout HEAD").color(theme::MUTED));ui.monospace(actual.unwrap_or(if self.git_history.busy{"Reading checkout…"}else{"No local Git checkout / not available"}));
                    ui.separator();ui.strong("Peer announcements");
                    for (node,tip) in &project.git_tips{ui.label(RichText::new(format!("{} · {}",node,tip.reference)).size(12.).color(theme::MUTED));ui.monospace(&tip.commit);}
                    if let Some(report)=&self.git_history.report{
                        if let Some(connections)=report["connections"].as_array(){for connection in connections{ui.label(format!("{} — {}",connection["url"].as_str().unwrap_or(""),connection["status"].as_str().unwrap_or("")));}}
                        if let Some(peers)=report["peers"].as_array(){ui.label(format!("{} connected bundle sources · {} pending sync comparisons",peers.len(),report["pending_comparisons"]));for peer in peers{ui.label(RichText::new(peer.as_str().unwrap_or("")).size(12.).color(theme::MUTED));}}
                        if let Some(error)=report["error"].as_str(){ui.colored_label(Color32::LIGHT_RED,error);}
                        if let Some(events)=report["events"].as_array(){ui.separator();ui.strong("Local sync activity");if events.is_empty(){ui.label("No activity recorded in this session.");}for event in events.iter().rev(){ui.label(RichText::new(format!("{} · {}",event["time"].as_str().unwrap_or(""),event["phase"].as_str().unwrap_or(""))).size(12.).color(GOLD));ui.label(event["detail"].as_str().unwrap_or(""));}}
                        if let Some(bundles)=report["bundles"].as_array(){ui.separator();ui.label(format!("{} received bundle ranges",bundles.len()));for range in bundles{ui.monospace(format!("{} → {}",range["bases"],range["tips"]));}}
                    }
                });
            }else{
                egui::ScrollArea::vertical().id_salt(("git_commits",&id)).max_height(470.).show(ui,|ui|{
                    if let Some(report)=&self.git_history.report{
                        if let Some(source)=report["history_source"].as_str(){ui.label(RichText::new(source).color(GOLD));}
                        if let Some(commits)=report["history"].as_array(){
                            for commit in commits{ui.monospace(commit["hash"].as_str().unwrap_or(""));ui.label(commit["message"].as_str().unwrap_or(""));ui.label(RichText::new(commit["author"].as_str().unwrap_or("")).size(12.).color(theme::MUTED));ui.separator();}
                            if commits.is_empty()&&!self.git_history.busy{ui.label("No readable local commit history.");}
                        }
                        let end=ui.allocate_space(egui::vec2(1.,1.)).1;
                        more=report["history_more"].as_bool().unwrap_or(false)&&ui.is_rect_visible(end)&&!self.git_history.busy&&self.git_history.error.is_none();
                    }
                    if self.git_history.busy{ui.spinner();}
                });
            }
            surfaces::error(ui,&self.git_history.error);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{if ui.button("Close").clicked(){close=true;}});
        });
        if surfaces::escape(&response){close=true;}
        if close{self.git_history.project=None;}else if refresh{self.git_history.limit=30;self.request_git_history(&project,ctx);}else if more{self.git_history.limit=self.git_history.limit.saturating_add(30);self.request_git_history(&project,ctx);}
    }
}
