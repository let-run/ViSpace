use super::*;
use inventory_niimbot::{Device, Transport};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct State {
    devices: Vec<Device>,
    connected: bool,
    busy: bool,
    scanning: bool,
    status: String,
    error: Option<String>,
    connected_id: String,
}
impl Default for State {
    fn default() -> Self {
        Self {
            devices: vec![],
            connected: false,
            busy: false,
            scanning: false,
            status: "Connect your Niimbot B1".into(),
            error: None,
            connected_id: String::new(),
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    selected: String,
    density: u8,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            selected: String::new(),
            density: 3,
        }
    }
}
enum Command {
    Scan,
    Connect(String),
    Disconnect,
    Print(Box<Part>, u8, u8),
}
pub(super) struct PrinterUi {
    state: Arc<Mutex<State>>,
    #[cfg(not(target_arch = "wasm32"))]
    sender: Option<tokio::sync::mpsc::Sender<Command>>,
    #[cfg(target_arch = "wasm32")]
    browser: std::rc::Rc<std::cell::RefCell<Option<BrowserHost>>>,
    settings: Settings,
    path: std::path::PathBuf,
    copies: u8,
}
impl PrinterUi {
    pub(super) fn new(path: std::path::PathBuf) -> Self {
        let mut settings: Settings = crate::platform::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        settings.density = settings.density.clamp(1, 5);
        Self {
            state: Arc::new(Mutex::new(State::default())),
            #[cfg(not(target_arch = "wasm32"))]
            sender: None,
            #[cfg(target_arch = "wasm32")]
            browser: std::rc::Rc::new(std::cell::RefCell::new(Some(BrowserHost::default()))),
            settings,
            path,
            copies: 1,
        }
    }
    fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec(&self.settings) {
            let _ = crate::platform::fs::write(&self.path, bytes);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn send(&mut self, command: Command, ctx: &egui::Context) {
        if self.state.lock().unwrap().busy {
            return;
        }
        if self.sender.is_none() {
            let (sender, receiver) = tokio::sync::mpsc::channel(8);
            let state = self.state.clone();
            let wake = ctx.clone();
            if let Err(error) = crate::async_io::spawn(actor(receiver, state, wake)) {
                self.state.lock().unwrap().error = Some(error.to_string());
                return;
            }
            self.sender = Some(sender);
        }
        let scanning = matches!(&command, Command::Scan);
        {
            let mut state = self.state.lock().unwrap();
            state.busy = true;
            state.scanning = scanning;
            state.error = None;
        }
        if let Err(error) = self.sender.as_ref().unwrap().try_send(command) {
            let mut state = self.state.lock().unwrap();
            state.busy = false;
            state.scanning = false;
            state.error = Some(error.to_string());
        }
    }
    #[cfg(target_arch = "wasm32")]
    fn send(&mut self, command: Command, ctx: &egui::Context) {
        if self.state.lock().unwrap().busy {
            return;
        }
        let Some(mut host) = self.browser.borrow_mut().take() else {
            return;
        };
        {
            let mut state = self.state.lock().unwrap();
            state.busy = true;
            state.scanning = matches!(&command, Command::Scan);
            state.error = None;
        }
        let slot = self.browser.clone();
        let shared = self.state.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result: Result<()> = async {
                match command {
                    Command::Scan => {
                        shared.lock().unwrap().status =
                            "Choose your B1 in the browser’s Bluetooth dialog…".into();
                        ctx.request_repaint();
                        let devices = host
                            .driver
                            .scan(|devices| {
                                shared.lock().unwrap().devices = devices;
                                ctx.request_repaint();
                            })
                            .await?;
                        let mut state = shared.lock().unwrap();
                        state.devices = devices;
                        state.status = "Select your B1 and connect.".into();
                    }
                    Command::Connect(id) => {
                        if let Some(mut old) = host.printer.take() {
                            let _ = old.transport.disconnect().await;
                        }
                        {
                            let mut state = shared.lock().unwrap();
                            state.connected = false;
                            state.status = "Connecting and identifying printer…".into();
                        }
                        ctx.request_repaint();
                        host.printer = Some(host.driver.connect(&id).await?);
                        let mut state = shared.lock().unwrap();
                        state.connected = true;
                        state.connected_id = id;
                        state.status = "Niimbot B1 connected • 203 dpi".into();
                    }
                    Command::Disconnect => {
                        if let Some(mut old) = host.printer.take() {
                            old.transport.disconnect().await?;
                        }
                        let mut state = shared.lock().unwrap();
                        state.connected = false;
                        state.connected_id.clear();
                        state.status = "Disconnected".into();
                    }
                    Command::Print(part, copies, density) => {
                        let raster = crate::label::printer_raster(&part)?;
                        host.printer
                            .as_mut()
                            .ok_or_else(|| anyhow::anyhow!("Printer disconnected"))?
                            .print(&raster, copies, density, |message| {
                                shared.lock().unwrap().status = message;
                                ctx.request_repaint();
                            })
                            .await?;
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                if let Some(active) = &host.printer
                    && !active.transport.is_connected().await
                {
                    host.printer = None;
                }
                let mut state = shared.lock().unwrap();
                state.error = Some(error.to_string());
                state.connected = host.printer.is_some();
                state.status = if state.connected {
                    "Print not confirmed"
                } else {
                    "Printer unavailable"
                }
                .into();
            }
            {
                let mut state = shared.lock().unwrap();
                state.busy = false;
                state.scanning = false;
            }
            *slot.borrow_mut() = Some(host);
            ctx.request_repaint();
        });
    }
    pub(super) fn controls(&mut self, ui: &mut egui::Ui, part: &Part) {
        let state = self.state.lock().unwrap().clone();
        if state.busy {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        if state.connected
            && !state.connected_id.is_empty()
            && self.settings.selected != state.connected_id
        {
            self.settings.selected = state.connected_id.clone();
            self.save();
        }
        if !state.devices.is_empty()
            && !state.devices.iter().any(|d| d.id == self.settings.selected)
        {
            self.settings.selected = state.devices[0].id.clone();
        }
        theme::frame(theme::PANEL).inner_margin(12).show(ui, |ui| {
            surfaces::configure(ui, 376.);
            let visuals = ui.visuals_mut();
            for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered] {
                widget.bg_fill = theme::RAISED;
                widget.weak_bg_fill = theme::RAISED;
            }
            ui.label(
                RichText::new("Niimbot B1 • 50 × 30 mm • 203 dpi")
                    .strong()
                    .color(theme::TEXT),
            );
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!state.busy && !state.connected, |ui| {
                    egui::ComboBox::from_id_salt("niimbot_printer")
                        .width(270.)
                        .selected_text(
                            state
                                .devices
                                .iter()
                                .find(|d| d.id == self.settings.selected)
                                .map(|d| d.name.as_str())
                                .unwrap_or("Scan for printers"),
                        )
                        .show_ui(ui, |ui| {
                            for device in &state.devices {
                                ui.selectable_value(
                                    &mut self.settings.selected,
                                    device.id.clone(),
                                    &device.name,
                                );
                            }
                        });
                });
                if ui
                    .add_enabled(
                        !state.busy && !state.connected,
                        egui::Button::new(if state.scanning {
                            "Scanning…"
                        } else {
                            "Scan"
                        }),
                    )
                    .clicked()
                {
                    self.send(Command::Scan, ui.ctx());
                }
            });
            ui.add_enabled_ui(!state.busy, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Copies:");
                    ui.add(egui::DragValue::new(&mut self.copies).range(1..=20));
                    ui.add_space(16.);
                    ui.label("Density:");
                    if ui
                        .add(egui::DragValue::new(&mut self.settings.density).range(1..=5))
                        .changed()
                    {
                        self.save();
                    }
                });
            });
            ui.label(RichText::new(&state.status).size(11.).color(theme::MUTED));
            if let Some(error) = &state.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            ui.horizontal(|ui| {
                if state.connected {
                    if ui
                        .add_enabled(!state.busy, egui::Button::new("Disconnect"))
                        .clicked()
                    {
                        self.send(Command::Disconnect, ui.ctx());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!state.busy, primary("Print label"))
                            .clicked()
                        {
                            self.send(
                                Command::Print(
                                    Box::new(part.clone()),
                                    self.copies,
                                    self.settings.density,
                                ),
                                ui.ctx(),
                            );
                        }
                    });
                } else if ui
                    .add_enabled(
                        !state.busy && !state.devices.is_empty(),
                        egui::Button::new("Connect"),
                    )
                    .clicked()
                {
                    self.send(Command::Connect(self.settings.selected.clone()), ui.ctx());
                }
                if state.busy {
                    ui.spinner();
                }
            });
        });
    }
}
#[cfg(not(target_arch = "wasm32"))]
async fn actor(
    mut receiver: tokio::sync::mpsc::Receiver<Command>,
    state: Arc<Mutex<State>>,
    ctx: egui::Context,
) {
    let mut driver = inventory_niimbot::Driver::default();
    let mut printer: Option<inventory_niimbot::Printer<inventory_niimbot::Connection>> = None;
    let mut monitor = tokio::time::interval(std::time::Duration::from_secs(3));
    loop {
        let command = tokio::select! {
            command=receiver.recv()=>{let Some(command)=command else {break;};command},
            _=monitor.tick()=>{
                if let Some(active)=&mut printer && !active.transport.is_connected().await {
                    printer=None;let mut state=state.lock().unwrap();state.connected=false;state.connected_id.clear();state.status="Disconnected".into();ctx.request_repaint();
                }continue;
            }
        };
        let result: Result<()> = async {
            match command {
                Command::Scan => {
                    state.lock().unwrap().status = "Searching for B1 printers…".into();
                    ctx.request_repaint();
                    let devices = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        driver.scan(|devices| {
                            state.lock().unwrap().devices = devices;
                            ctx.request_repaint();
                        }),
                    )
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!("Bluetooth scan timed out. Check power and permissions.")
                    })??;
                    let mut state = state.lock().unwrap();
                    state.status = if devices.is_empty() {
                        "No B1 found. Turn it on and close other printer apps."
                    } else {
                        "Select your B1 and connect."
                    }
                    .into();
                    state.devices = devices;
                }
                Command::Connect(id) => {
                    if let Some(mut old) = printer.take() {
                        let _ = old.transport.disconnect().await;
                    }
                    state.lock().unwrap().connected = false;
                    state.lock().unwrap().status = "Connecting and identifying printer…".into();
                    ctx.request_repaint();
                    printer = Some(driver.connect(&id).await?);
                    let mut state = state.lock().unwrap();
                    state.connected = true;
                    state.connected_id = id;
                    state.status = "Niimbot B1 connected • 203 dpi".into();
                }
                Command::Disconnect => {
                    if let Some(mut old) = printer.take() {
                        old.transport.disconnect().await?;
                    }
                    let mut state = state.lock().unwrap();
                    state.connected = false;
                    state.connected_id.clear();
                    state.status = "Disconnected".into();
                }
                Command::Print(part, copies, density) => {
                    let active = printer
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("Printer disconnected"))?;
                    let raster =
                        crate::tasks::compute_async(move || crate::label::printer_raster(&part))
                            .await??;
                    active
                        .print(&raster, copies, density, |message| {
                            state.lock().unwrap().status = message;
                            ctx.request_repaint();
                        })
                        .await?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if let Some(active) = &mut printer
                && !active.transport.is_connected().await
            {
                printer = None;
            }
            let mut state = state.lock().unwrap();
            state.error = Some(error.to_string());
            state.status = if printer.is_some() {
                "Print not confirmed"
            } else {
                "Printer unavailable"
            }
            .into();
            state.connected = printer.is_some();
        }
        {
            let mut state = state.lock().unwrap();
            state.busy = false;
            state.scanning = false;
        }
        ctx.request_repaint();
    }
    if let Some(mut printer) = printer {
        let _ = printer.transport.disconnect().await;
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct BrowserHost {
    driver: inventory_niimbot::Driver,
    printer: Option<inventory_niimbot::Printer<inventory_niimbot::Connection>>,
}
