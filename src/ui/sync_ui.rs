use super::*;
use crate::sync::{ClientStatus, SyncClient, SyncServer};
use surfaces as form;

const DEFAULT_LISTENER: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 8824);
#[derive(serde::Serialize, serde::Deserialize)]
struct HostPreferences {
    address: std::net::SocketAddr,
    enabled: bool,
}

fn host_preferences_path(store: &SharedStore) -> std::path::PathBuf {
    store.lock().unwrap().path.with_extension("mesh-sync.json")
}

fn save_host_preferences(store: &SharedStore, settings: &HostPreferences) -> Result<()> {
    use std::io::Write;
    let path = host_preferences_path(store);
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, settings)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

#[derive(Default, PartialEq)]
enum SyncTab {
    #[default]
    Discover,
    Connect,
}

#[derive(Default)]
struct PeerSnapshot {
    local_id: String,
    known: Vec<crate::network_identity::DiscoverPeer>,
    online: std::collections::BTreeSet<String>,
    direct: std::collections::BTreeSet<String>,
    pairing: std::collections::BTreeSet<String>,
    pairing_codes: std::collections::BTreeMap<String, Option<String>>,
}

pub(super) struct SyncUi {
    pub open: bool,
    tab: SyncTab,
    peers: PeerSnapshot,
    revoking_id: Option<String>,
    listen_ip: String,
    listen_port: String,
    host_error: Option<String>,
    tls_paths: Option<(String, String)>,
    autostart_address: Option<std::net::SocketAddr>,
    peer: String,
    server: Option<SyncServer>,
    clients: Vec<(String, SyncClient)>,
    error: Option<String>,
    merge_edit: Option<(String, String)>,
    merge_note_base: Option<serde_json::Value>,
    bridge_task: Option<crate::tasks::Task<Option<(String, SyncClient)>>>,
    membership_task: Option<crate::tasks::Task<()>>,
    discovery: Option<crate::discovery::Discovery>,
    nearby: Vec<crate::discovery::NearbyNode>,
    discovery_error: Option<String>,
    auto_connect: bool,
    next_discovery: Option<std::time::Instant>,
    attempts: std::collections::BTreeMap<String,std::time::Instant>,
    disconnected: std::collections::BTreeSet<String>,
}
impl Default for SyncUi {
    fn default() -> Self {
        Self {
            open: false,
            tab: SyncTab::default(),
            peers: PeerSnapshot::default(),
            revoking_id: None,
            listen_ip: DEFAULT_LISTENER.ip().to_string(),
            listen_port: DEFAULT_LISTENER.port().to_string(),
            host_error: None,
            tls_paths: None,
            autostart_address: None,
            peer: format!("wss://127.0.0.1:{}", DEFAULT_LISTENER.port()),
            server: None,
            clients: vec![],
            error: None,
            merge_edit: None,
            merge_note_base: None,
            bridge_task: None,
            membership_task: None,
            discovery:None,nearby:vec![],discovery_error:None,auto_connect:true,next_discovery:None,
            attempts:Default::default(),disconnected:Default::default(),
        }
    }
}
impl SyncUi {
    pub fn restore_host(&mut self, store: &SharedStore) {
        match crate::discovery::Discovery::new(){Ok(discovery)=>self.discovery=Some(discovery),Err(e)=>self.discovery_error=Some(e.to_string())}
        if let Ok(bytes)=std::fs::read(store.lock().unwrap().path.with_extension("discovery.json")){
            self.auto_connect=serde_json::from_slice(&bytes).unwrap_or(true);
        }
        let restore = || -> Result<HostPreferences> {
            match std::fs::read(host_preferences_path(store)) {
                Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HostPreferences {
                    address: DEFAULT_LISTENER,
                    enabled: true,
                }),
                Err(error) => Err(error.into()),
            }
        };
        match restore() {
            Ok(settings) => {
                self.listen_ip = settings.address.ip().to_string();
                self.listen_port = settings.address.port().to_string();
                if settings.enabled {
                    self.autostart_address = Some(settings.address);
                    match SyncServer::start(store.clone(), settings.address) {
                        Ok(server) => self.server = Some(server),
                        Err(error) => {
                            self.host_error = Some(format!("Could not resume listener: {error}"))
                        }
                    }
                }
            }
            Err(error) => {
                self.host_error = Some(format!("Could not read Mesh sync preferences: {error}"))
            }
        }
    }

    pub fn poll_discovery(&mut self,ctx:&egui::Context,store:&SharedStore){
        let Some(discovery)=self.discovery.as_mut() else{return};
        let now=std::time::Instant::now();let interval=std::time::Duration::from_secs(1);
        if let Some(next)=self.next_discovery.filter(|next|*next>now){ctx.request_repaint_after(next-now);return}
        self.next_discovery=Some(now+interval);ctx.request_repaint_after(interval);
        let Ok(mut locked)=store.try_lock()else{return};
        let id=locked.application_id().to_owned();let listen=self.server.as_ref().filter(|s|s.discovery_hostname.is_some()).map(|s|s.address);
        self.discovery_error=discovery.advertise(&id,listen).err().map(|e|e.to_string());
        self.nearby=discovery.nodes().into_iter().filter(|n|n.node_id!=id).collect();
        if !self.auto_connect{return}
        let direct=locked.network_graph().nodes.into_iter().filter(|n|n.direct).map(|n|n.id).collect::<std::collections::BTreeSet<_>>();
        self.attempts.retain(|_,time|now.duration_since(*time)<std::time::Duration::from_secs(30));
        let next=self.nearby.iter().find(|n|locked.network_trusted(&n.node_id)
            && !direct.contains(&n.node_id)&&!self.disconnected.contains(&n.node_id)&&!self.attempts.contains_key(&n.node_id)
            && !self.clients.iter().any(|(url,c)|url==&n.address&&!c.is_finished())
            // Two listeners choose one dial direction; client-only nodes may always dial.
            && (listen.is_none_or(|a|a.ip().is_loopback())||id<n.node_id)).cloned();
        drop(locked);
        if let Some(node)=next.filter(|_|self.clients.iter().filter(|(_,c)|!c.is_finished()).count()<16){
            self.attempts.insert(node.node_id.clone(),now);
            self.clients.retain(|(url,c)|url!=&node.address||!c.is_finished());
            match SyncClient::start_expected(store.clone(),&node.address,Some(node.node_id)){Ok(client)=>self.clients.push((node.address,client)),Err(e)=>self.discovery_error=Some(e.to_string())}
        }
    }
    fn connect(&mut self, store: &SharedStore, address: String, expected: Option<String>, replace_pin: bool) {
        if self.clients.iter().any(|(url, client)| url == &address && !client.is_finished()) {
            self.error = Some("Already connected to this address".into());
            return;
        }
        if let Some(id) = &expected {
            self.disconnected.remove(id);
        }
        self.clients.retain(|(url, _)| url != &address);
        self.error = match SyncClient::start_code_pairing(store.clone(), &address, expected, replace_pin) {
            Ok(client) => {
                self.clients.push((address, client));
                None
            }
            Err(error) => Some(error.to_string()),
        };
    }

    fn discover(&mut self, ui: &mut egui::Ui, store: &SharedStore) -> Option<(String, bool)> {
        if ui.checkbox(&mut self.auto_connect, "Auto-connect to approved nearby nodes").changed() {
            let saved = store.try_lock().ok().map(|store| {
                std::fs::write(store.path.with_extension("discovery.json"), self.auto_connect.to_string())
            });
            self.error = match saved {
                Some(Ok(())) => None,
                Some(Err(error)) => Some(error.to_string()),
                None => Some("Inventory is busy. Try changing auto-connect again.".into()),
            };
            if self.error.is_some() {
                self.auto_connect = !self.auto_connect;
            }
        }
        if let Some(error) = &self.discovery_error {
            ui.colored_label(Color32::LIGHT_RED, format!("Discovery: {error}"));
        }
        if let Some(server) = &self.server
            && !server.address.ip().is_loopback()
            && server.discovery_hostname.is_none()
        {
            ui.small("The listener needs a certificate for this node’s discovery hostname to appear on the network.");
        }

        let known: std::collections::BTreeMap<_, _> = self.peers.known.iter()
            .map(|node| (node.id.as_str(), node)).collect();
        let nearby: std::collections::BTreeMap<_, _> = self.nearby.iter()
            .map(|node| (node.node_id.as_str(), node.address.as_str())).collect();
        let ids: std::collections::BTreeSet<_> = known.keys().chain(nearby.keys()).copied().collect();
        let mut ids: Vec<_> = ids.into_iter().collect();
        let now = crate::action_log::timestamp_now();
        ids.retain(|id| {
            let Some(node) = known.get(id) else { return true; };
            node.expires_at.is_none_or(|expires| expires > now)
                || self.peers.pairing.contains(*id)
                || nearby.contains_key(id)
        });
        ids.sort_by_key(|id| (!known.get(id).is_some_and(|node| node.trusted), *id));
        if let Some(id) = self.revoking_id.as_deref()
            && !known.get(id).is_some_and(|node| node.trusted && !node.revoked)
        {
            self.revoking_id = None;
        }
        if ids.is_empty() {
            ui.label("No nearby or approved nodes yet.");
            ui.small("Use Connect to enter a peer address manually.");
            return None;
        }

        let mut membership = None;
        let mut show_code = None;
        let mut connect = None;
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 0)).show(ui, |ui| {
            use egui_extras::{Column, TableBuilder};

            ui.spacing_mut().item_spacing = egui::vec2(8., 3.);
            TableBuilder::new(ui)
                .id_salt("sync_nodes")
                .striped(true)
                .vscroll(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().clip(true))
                .column(Column::exact(100.))
                .column(Column::exact(90.))
                .column(Column::exact(154.))
                .header(21., |mut row| {
                    for label in ["Node", "Status", "Pairing", "Actions"] {
                        row.col(|ui| { ui.strong(label); });
                    }
                })
                .body(|body| {
                    body.rows(25., ids.len(), |mut row| {
                        let id = ids[row.index()];
                        let address = nearby.get(id).copied();
                        let known = known.get(id).copied();
                        let approved = known.is_some_and(|node| node.trusted);
                        let revoked = known.is_some_and(|node| node.revoked);
                        let connected = self.peers.direct.contains(id);
                        let online = address.is_some() || self.peers.online.contains(id);
                        let attempt = address.and_then(|address| {
                            self.clients.iter().rev().find(|(url, _)| url == address).map(|(_, client)| client)
                        });
                        let connecting = attempt.is_some_and(|client| !client.is_finished());

                        let pairing = self.peers.pairing.contains(id);
                        row.col(|ui| {
                            let mut label = RichText::new(id).monospace();
                            if pairing {
                                label = label.color(theme::GREEN);
                            }
                            ui.add(egui::Label::new(label).truncate())
                                .on_hover_text(address.unwrap_or(id));
                        });
                        row.col(|ui| {
                            let status = if connected { "Connected" } else if pairing { "Pairing" } else if connecting { "Connecting…" }
                                else if online { "Online" } else { "Offline" };
                            let status = ui.label(match status {
                                "Connected" | "Online" => RichText::new(status).color(theme::GREEN),
                                "Offline" => RichText::new(status).color(theme::MUTED),
                                _ => RichText::new(status),
                            });
                            if let Some(ClientStatus::Failed(error)) = attempt.map(SyncClient::status) {
                                status.on_hover_text(error);
                            }
                        });
                        row.col(|ui| { ui.label(if approved { "Approved" } else if revoked { "Revoked" } else { "Unpaired" }); });
                        row.col(|ui| {
                            if ui.add_enabled(address.is_some() && !revoked && !connected && !connecting && !pairing, egui::Button::new("Connect").small()).clicked()
                                && let Some(address) = address
                            {
                                connect = Some((address.to_owned(), id.to_owned()));
                            }
                            if let Some(node) = known {
                                if let Some(code) = self.peers.pairing_codes.get(id) {
                                    if let Some(code) = code {
                                        ui.label(RichText::new(code).monospace().strong().color(GOLD))
                                            .on_hover_text("Give this code to the connecting desktop");
                                    } else if ui.small_button("Approve").on_hover_text("Show the pairing code").clicked() {
                                        show_code = Some(id.to_owned());
                                    }
                                } else if approved && self.revoking_id.as_deref() == Some(id) {
                                    let size = egui::vec2(24., 22.);
                                    let confirm = theme::tool_sized(ui, Icon::Check, "Confirm revoke", size, false);
                                    if confirm.clicked() && !confirm.double_clicked() {
                                        self.revoking_id = None;
                                        membership = Some((node.public_key.clone(), true));
                                    }
                                    if theme::tool_sized(ui, Icon::Close, "Cancel revoke", size, false).clicked() {
                                        self.revoking_id = None;
                                    }
                                } else if approved {
                                    if ui.small_button("Revoke").on_hover_text("Revoke sync access").clicked() {
                                        self.revoking_id = Some(id.to_owned());
                                    }
                                } else if ui.small_button(if revoked { "Approve again" } else { "Approve" }).on_hover_text("Verify the node ID before approving").clicked() {
                                    membership = Some((node.public_key.clone(), false));
                                }
                            }
                        });
                    });
                });
        });
        if let Some(id) = show_code {
            store.lock().unwrap().show_pairing_code(&id);
        }
        if let Some((address, id)) = connect {
            self.connect(store, address, Some(id), false);
        }
        membership
    }

    pub fn active(&self) -> bool {
        self.server.is_some() || self.clients.iter().any(|(_, c)| !c.is_finished())
    }
}
impl InventoryApp {
    pub(super) fn mesh_host_controls(&mut self, ui: &mut egui::Ui) {
        let state = &mut self.sync;
        if state.tls_paths.is_none() {
            if let Ok(store) = self.store.try_lock() {
                match crate::network_tls::load(&store) {
                    Ok(tls) => {
                        state.tls_paths = Some((
                            tls.certificate.to_string_lossy().into_owned(),
                            tls.private_key.to_string_lossy().into_owned(),
                        ));
                    }
                    Err(error) => {
                        ui.colored_label(Color32::LIGHT_RED, error.to_string());
                    }
                }
            }
        }
        let mut automatic = false;
        if let Some((certificate, private_key)) = &mut state.tls_paths {
            ui.label("Secure WebSocket listener (WSS)");
            form::row(ui, "Certificate PEM", |ui| form::field(ui, certificate));
            form::row(ui, "Private key PEM", |ui| form::field(ui, private_key));
            if ui.button("Save TLS files").clicked() {
                let settings = crate::network_tls::Settings {
                    certificate: certificate.trim().into(),
                    private_key: private_key.trim().into(),
                };
                state.host_error = self.store.try_lock().ok().map_or_else(
                    || Some("Inventory is busy. Try saving the TLS files again.".into()),
                    |store| crate::network_tls::save(&store, &settings).err().map(|e| e.to_string()),
                );
            }
            automatic = certificate.trim().is_empty() && private_key.trim().is_empty();
            if !automatic && ui.button("Use automatic certificates").clicked() {
                let result = crate::network_tls::save(
                    &self.store.lock().unwrap(),
                    &crate::network_tls::Settings::default(),
                );
                state.host_error = result.err().map(|error| error.to_string());
                if state.host_error.is_none() {
                    certificate.clear();
                    private_key.clear();
                    automatic = true;
                }
            }
            ui.small("Leave both paths blank for automatic certificates. Restart the listener after saving changes.");
        }
        if automatic {
            ui.small("Connecting devices must trust the exported CA certificate once.");
            if ui.button("Export CA certificate…").clicked() {
                let ca = crate::network_tls::public_ca(&self.store.lock().unwrap());
                state.host_error = ca.and_then(|ca| {
                    if let Some(path) = rfd::FileDialog::new().set_file_name("ViSpace-Sync-CA.pem").save_file() {
                        std::fs::write(path, ca)?;
                    }
                    Ok(())
                }).err().map(|error| error.to_string());
            }
        }
        ui.add_enabled_ui(state.server.is_none(), |ui| {
            form::row(ui, "Server address", |ui| {
                form::field(ui, &mut state.listen_ip)
            });
            form::row(ui, "Port", |ui| form::field(ui, &mut state.listen_port));
        });
        if let Some(server) = &state.server {
            ui.label(format!("Listening at wss://{}", server.address));
        }
        if let Some(address) = state.autostart_address {
            let label = if state.server.is_some() {
                "Stop listening"
            } else {
                "Disable autostart"
            };
            if ui.button(label).clicked() {
                match save_host_preferences(
                    &self.store,
                    &HostPreferences {
                        address,
                        enabled: false,
                    },
                ) {
                    Ok(()) => {
                        state.server = None;
                        state.autostart_address = None;
                        state.host_error = None;
                    }
                    Err(error) => {
                        state.host_error =
                            Some(format!("Could not save Mesh sync preferences: {error}"))
                    }
                }
            }
        }
        if state.server.is_none() && ui.button("Start listening").clicked() {
            let start = || -> Result<(SyncServer, std::net::SocketAddr)> {
                let ip = state
                    .listen_ip
                    .trim()
                    .parse::<std::net::IpAddr>()
                    .map_err(|_| anyhow::anyhow!("Enter a valid IPv4 or IPv6 address"))?;
                let port = state
                    .listen_port
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| anyhow::anyhow!("Enter a valid port number"))?;
                let address = std::net::SocketAddr::new(ip, port);
                let server = SyncServer::start(self.store.clone(), address)?;
                save_host_preferences(
                    &self.store,
                    &HostPreferences {
                        address,
                        enabled: true,
                    },
                )?;
                Ok((server, address))
            };
            match start() {
                Ok((server, address)) => {
                    state.server = Some(server);
                    state.autostart_address = Some(address);
                    state.host_error = None;
                }
                Err(error) => state.host_error = Some(error.to_string()),
            }
        }
        form::hint(
            ui,
            "Use 0.0.0.0 to listen on the local network. Share this computer’s IP address and port with peers.",
        );
        form::error(ui, &state.host_error);
    }

    pub(super) fn sync_window(&mut self, ctx: &egui::Context) {
        let state = &mut self.sync;
        if !state.open {
            return;
        }
        if let Some(result) = state.membership_task.as_ref().and_then(crate::tasks::Task::poll) {
            state.membership_task = None;
            state.error = result.err();
        }
        if let Some(result) = state
            .bridge_task
            .as_ref()
            .and_then(crate::tasks::Task::poll)
        {
            state.bridge_task = None;
            state.error = match result {
                Ok(Some(link)) => {
                    state.clients.push(link);
                    None
                }
                Ok(None) => None,
                Err(error) => Some(error),
            };
        }
        let mut resolution = None;
        let mut enable_component = false;
        let mut component = false;
        let mut fetch_bridge = None;
        let mut connect_after_review = false;
        let mut finish_bridge = false;
        let mut clear_bridge = false;
        let mut pending_action = None;
        let mut membership_action = None;
        if let Ok(mut store) = self.store.try_lock() {
            let local_id = store.application_id().to_owned();
            let known = store.network_discover_peers();
            let graph = store.network_graph();
            let pairing = store.network_pairing_ids();
            let pairing_codes = pairing.iter().filter_map(|id| store.pairing_code(id)
                .map(|code| (id.clone(), code))).collect();
            state.peers = PeerSnapshot {
                local_id,
                known,
                pairing,
                pairing_codes,
                online: graph.nodes.iter().filter(|node| node.reachable || node.direct)
                    .map(|node| node.id.clone()).collect(),
                direct: graph.nodes.into_iter().filter(|node| node.direct)
                    .map(|node| node.id).collect(),
            };
        }
        let width = (ctx.content_rect().width() - 76.).clamp(280., 680.);
        let height = (ctx.content_rect().height() - 190.).clamp(160., 450.);
        let mut close = false;
        let response = form::sheet_with_margin("sync_dialog", egui::Margin::symmetric(22, 12)).show(ctx, |ui| {
            form::configure(ui, width);
            ui.spacing_mut().item_spacing.y = 6.;
            let title = if state.peers.local_id.is_empty() {
                "Sync".to_owned()
            } else {
                format!("Sync · {}", state.peers.local_id)
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(18.).strong().color(theme::TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    theme::icon(ui, Icon::Mesh, 16., GOLD);
                });
            });
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.tab, SyncTab::Discover, "Discover");
                ui.selectable_value(&mut state.tab, SyncTab::Connect, "Connect");
            });
            if state.tab != SyncTab::Discover {
                state.revoking_id = None;
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt(if state.tab == SyncTab::Discover { "discover" } else { "connect" })
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_height(height);
                    if state.tab == SyncTab::Discover {
                        membership_action = state.discover(ui, &self.store);
                        return;
                    }

                    ui.label("Peer address");
                    let address = form::field(ui, &mut state.peer);
                    let submitted = address.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if ui.button("Connect").clicked() || submitted {
                        state.connect(&self.store, state.peer.trim().to_owned(), None, false);
                    }
                    if state.bridge_task.is_some() {
                        ui.small("Checking branch histories…");
                    }

                    if let Ok(store)=self.store.try_lock(){
                        component = store.uses_component();
                        let managed=store.uses_write_approval();
                        if !managed && ui.button("Enable partition sync").clicked() {
                            enable_component=true;
                        }
                        let mut reviews=if component { Vec::new() } else { store.merge_reviews() };
                        if managed {
                            if component && store.component_pending_count() > 0 {
                                ui.label(format!("{} edits awaiting leader approval", store.component_pending_count()));
                            }
                            match store.pending_proposal(){
                                Ok(Some(draft))=>{
                                    use crate::component_sync::SaveResult;
                                    match &draft.outcome {
                                        SaveResult::Unavailable=>{
                                            ui.label("Your draft is saved and awaiting confirmation.");
                                            if ui.button("Retry draft").clicked(){pending_action=Some(true);}
                                        }
                                        SaveResult::Stale(_)=>{
                                            match store.proposal_reviews(&draft){
                                                Ok(pending) if pending.is_empty()=>{ui.label("Receiving the latest shared values…");}
                                                Ok(pending)=>{reviews=pending.into_iter().filter(|r|!draft.choices.contains_key(&r.id)).collect();}
                                                Err(error)=>{ui.colored_label(Color32::LIGHT_RED,error.to_string());}
                                            }
                                            if ui.button("Discard draft").clicked(){pending_action=Some(false);}
                                        }
                                        SaveResult::Invalid(message)=>{
                                            ui.colored_label(Color32::LIGHT_RED,message);
                                            if ui.button("Discard draft").clicked(){pending_action=Some(false);}
                                        }
                                        SaveResult::Committed=>{}
                                    }
                                }
                                Ok(None)=>{}
                                Err(error)=>{ui.colored_label(Color32::LIGHT_RED,error.to_string());}
                            }
                        }
                        let mut bridge_review=false;
                        if let Some(bridge)=store.component_bridge() {
                            ui.separator();
                            ui.label(format!("Reconnect review for {}",bridge.address));
                            ui.small("Both branches can keep saving while you review.");
                            match store.component_bridge_reviews() {
                                Ok(comparisons) => {
                                    if comparisons.is_empty() {
                                        if ui.add_enabled(state.bridge_task.is_none(),egui::Button::new("Apply merge and connect")).clicked(){finish_bridge=true;}
                                    } else {
                                        reviews=comparisons;
                                        bridge_review=true;
                                    }
                                }
                                Err(error)=>{ui.colored_label(Color32::LIGHT_RED,error.to_string());}
                            }
                            ui.horizontal(|ui|{
                                if ui.add_enabled(state.bridge_task.is_none(),egui::Button::new("Refresh comparison")).clicked(){fetch_bridge=Some(bridge.address.clone());}
                                if ui.add_enabled(state.bridge_task.is_none(),egui::Button::new("Cancel reconnect")).clicked(){clear_bridge=true;}
                            });
                        }
                        if let Some(review)=reviews.first(){
                            ui.colored_label(Color32::YELLOW,format!("{} conflicts to review",reviews.len()));
                            ui.label(format!("{} · {} · {}",review.entity,review.key,review.field));
                            ui.columns(3,|columns|{for (column,(label,value)) in columns.iter_mut().zip([("Common version",&review.base),("Ours",&review.ours),("Theirs",&review.theirs)]){column.strong(label);let mut text=if review.markdown{crate::merge_review::markdown(value).to_owned()}else{crate::merge_review::display(value)};column.add(egui::TextEdit::multiline(&mut text).interactive(false).desired_rows(5).desired_width(f32::INFINITY));}});
                            if state.merge_edit.as_ref().is_none_or(|(id,_)|id!=&review.id){let text=if review.markdown{crate::merge_review::merge_text(crate::merge_review::markdown(&review.base),crate::merge_review::markdown(&review.ours),crate::merge_review::markdown(&review.theirs))}else{crate::merge_review::display(&review.ours)};state.merge_note_base=Some(review.ours.clone());state.merge_edit=Some((review.id.clone(),store.merge_draft(&review.id).unwrap_or(text)));}
                            let edit=&mut state.merge_edit.as_mut().unwrap().1;
                            ui.horizontal(|ui|{if ui.button("Use ours").clicked(){state.merge_note_base=Some(review.ours.clone());*edit=if review.markdown{crate::merge_review::markdown(&review.ours).into()}else{crate::merge_review::display(&review.ours)};}if ui.button("Use theirs").clicked(){state.merge_note_base=Some(review.theirs.clone());*edit=if review.markdown{crate::merge_review::markdown(&review.theirs).into()}else{crate::merge_review::display(&review.theirs)};}});
                            ui.label(if review.markdown{"Merged Markdown — edit the result and remove conflict markers"}else if review.ours.is_string()&&review.theirs.is_string(){"Merged text"}else{"Merged value (JSON)"});
                            if ui.add(egui::TextEdit::multiline(edit).desired_rows(10).desired_width(f32::INFINITY)).changed(){if let Err(e)=store.save_merge_draft(&review.id,edit){state.error=Some(e.to_string());}}
                            if review.markdown {egui::CollapsingHeader::new("Preview").show(ui,|ui|{if let Ok(rows)=crate::markdown_notes::parse(edit){for row in rows{let text=row.title();if row.heading>0{ui.strong(text);}else{ui.label(text);}}}});}
                            if ui.button("Resolve").clicked(){let value=if review.markdown{let mut value=state.merge_note_base.clone().unwrap_or_else(||review.ours.clone());value["text"]=edit.clone().into();value["markdownSource"]=edit.clone().into();value["rtf"]="".into();if let Some(images)=review.theirs["images"].as_object(){for(k,v)in images{if value["images"].get(k).is_none(){value["images"][k]=v.clone();}}}Ok(value)}else if review.ours.is_string()&&review.theirs.is_string(){Ok(serde_json::Value::String(edit.clone()))}else{serde_json::from_str(edit)};match value{Ok(value)=>resolution=Some((review.id.clone(),value,managed,bridge_review)),Err(e)=>state.error=Some(e.to_string())} }
                        }
                        if !store.sync_conflicts().is_empty(){
                            ui.colored_label(Color32::YELLOW,format!("{} inventory quantity/relationship warnings",store.sync_conflicts().len()));
                            egui::ScrollArea::vertical().max_height(140.).show(ui,|ui|{for conflict in store.sync_conflicts(){ui.small(format!("{} · {} · {}",conflict.entity,conflict.id,conflict.message));}});
                        }
                    }
                    let mut remove = None;
                    let mut re_pair = None;
                    for (index, (url, client)) in state.clients.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(url);
                            if ui.button("Disconnect").clicked() {
                                remove = Some(index);
                            }
                        });
                        match client.status() {
                            ClientStatus::Connecting => { ui.small("Connecting…"); }
                            ClientStatus::WaitingForApproval | ClientStatus::PairingCode { .. } => {}
                            ClientStatus::Connected { peer_id } => {
                                ui.small(format!("Connected to {peer_id}"));
                            }
                            ClientStatus::Failed(error) => {
                                ui.colored_label(Color32::LIGHT_RED, &error);
                                if error.contains("Paired server TLS key changed") && ui.button("Pair again").clicked() {
                                    re_pair = Some(url.clone());
                                }
                                if component && error.contains("diverged")
                                    && ui.add_enabled(state.bridge_task.is_none(), egui::Button::new("Review branches")).clicked()
                                {
                                    fetch_bridge = Some(url.clone());
                                    connect_after_review = true;
                                }
                            }
                        }
                    }
                    if let Some(url) = re_pair {
                        state.connect(&self.store, url, None, true);
                    }
                    if let Some(index) = remove {
                        let (url, client) = state.clients.remove(index);
                        if let ClientStatus::Connected { peer_id } = client.status() {
                            state.disconnected.insert(peer_id);
                        }
                        if let Some(node) = state.nearby.iter().find(|node| node.address == url) {
                            state.disconnected.insert(node.node_id.clone());
                        }
                    }
                    if let Ok(store) = self.store.try_lock() {
                        egui::CollapsingHeader::new("Signed network action log").show(ui, |ui| {
                            for action in store.network_actions() {
                                let verb = if action.revoked { "Revoked" } else { "Approved" };
                                ui.small(format!("{verb} {} · signed by {}", action.subject, action.issuer));
                            }
                        });
                    }
                });
            for (url, client) in &state.clients {
                match client.status() {
                    ClientStatus::WaitingForApproval => {
                        ui.small(format!("Waiting for host approval · {url}"));
                    }
                    ClientStatus::PairingCode { remaining } => {
                        ui.horizontal(|ui| {
                            let id = ui.id().with(("pairing_code", url));
                            let mut code = ui.data_mut(|data| data.get_temp::<String>(id).unwrap_or_default());
                            ui.label("Enter the host’s four-digit code");
                            let input = ui.add(egui::TextEdit::singleline(&mut code).desired_width(64.).char_limit(4).id(id.with("input")));
                            code.retain(|character| character.is_ascii_digit());
                            let valid = code.len() == 4;
                            let enter = input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                            if ui.add_enabled(valid, egui::Button::new("Pair")).clicked() || (valid && enter) {
                                client.enter_pairing_code(std::mem::take(&mut code));
                            }
                            ui.data_mut(|data| data.insert_temp(id, code));
                            if remaining < 3 {
                                ui.small(format!("Incorrect code · {remaining} attempts left"));
                            }
                        });
                    }
                    _ => {}
                }
            }
            if let Some(error) = &state.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.add(egui::Button::new("Done").min_size(egui::vec2(62., 23.))).clicked();
            });
        });
        if close || response.should_close() {
            state.open = false;
            state.revoking_id = None;
        }
        if let Some((public, revoke)) = membership_action
            && state.membership_task.is_none()
        {
            let pairing = !revoke && crate::network_identity::node_id(&public).is_ok_and(|id|
                self.store.lock().unwrap().show_pairing_code(&id));
            if !pairing {
                let store = std::sync::Arc::downgrade(&self.store);
                let wake = ctx.clone();
                state.membership_task = Some(crate::tasks::Task::spawn(
                    move |_| crate::async_io::runtime()?.block_on(
                        crate::component_sync::authorize_member(store, public, revoke),
                    ),
                    Box::new(move || wake.request_repaint()),
                ));
            }
        }
        if enable_component {
            state.error = crate::store::Store::enable_component(&self.store)
                .err()
                .map(|error| error.to_string());
        }
        if clear_bridge {
            self.store.lock().unwrap().clear_component_bridge();
        }
        if fetch_bridge.is_some() || finish_bridge {
            let store = self.store.clone();
            let wake = ctx.clone();
            state.error = None;
            state.bridge_task = Some(crate::tasks::Task::spawn(
                move |_| {
                    let runtime = crate::async_io::runtime()?;
                    if let Some(address) = fetch_bridge {
                        runtime.block_on(crate::component_sync::fetch_branch(
                            store.clone(),
                            address,
                        ))?;
                        if !connect_after_review
                            || !store.lock().unwrap().component_bridge_reviews()?.is_empty()
                        {
                            return Ok(None);
                        }
                    }
                    let address = store
                        .lock()
                        .unwrap()
                        .component_bridge()
                        .map(|bridge| bridge.address);
                    let result =
                        runtime.block_on(crate::component_sync::finish_bridge(store.clone()));
                    if result.is_err()
                        && let Some(address) = address
                    {
                        let _ =
                            runtime.block_on(crate::component_sync::fetch_branch(store, address));
                    }
                    result.map(Some)
                },
                Box::new(move || wake.request_repaint()),
            ));
        }
        if let Some(retry) = pending_action {
            let mut store = self.store.lock().unwrap();
            let result = if retry {
                store.retry_proposal()
            } else {
                store.discard_proposal()
            };
            state.error = result.err().map(|error| error.to_string());
        }
        if let Some((id, value, managed, bridge)) = resolution {
            let mut store = self.store.lock().unwrap();
            let result = if bridge {
                store.choose_component_merge(&id, value)
            } else if managed {
                store.resolve_proposal(&id, value)
            } else {
                store.resolve_merge(&id, value)
            };
            match result {
                Ok(()) => {
                    state.merge_edit = None;
                    state.error = None;
                    if !managed && store.merge_reviews().is_empty() {
                        drop(store);
                        state.error = crate::store::Store::enable_component(&self.store)
                            .err()
                            .map(|error| error.to_string());
                    }
                }
                Err(e) => state.error = Some(e.to_string()),
            }
        }
    }
}
