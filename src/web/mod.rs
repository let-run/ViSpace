mod git_bundle;
pub use crate::browser_jobs as jobs;
pub mod platform;
mod storage_engine;
use std::{
    cell::RefCell,
    sync::{Arc, Mutex},
};
use wasm_bindgen::{JsCast, prelude::*};
thread_local! {
    static STORE:RefCell<Option<crate::store::SharedStore>>=const{RefCell::new(None)};
    static CONTEXT:RefCell<Option<eframe::egui::Context>>=const{RefCell::new(None)};
    static RUNNER:RefCell<Option<eframe::WebRunner>>=const{RefCell::new(None)};
    static FILE_CONTEXT:RefCell<FileContext>=const{RefCell::new(FileContext{part:None,notes:None,project:None})};
    static EVENTS:RefCell<Vec<Event>>=const{RefCell::new(Vec::new())};
}
pub enum Event {
    Csv(Vec<u8>, String),
    PartImage(String, Vec<u8>),
    PartPdf(String, Vec<u8>),
    NotesImage(String, Vec<u8>),
    Bom(String, Vec<u8>),
    Error(String),
    KicadFolder(String, FolderSelection),
    KicadFolderError(String,String),
    GitSyncStatus(String,bool),
    KicadChanged(String),
    ModelsFolder(String, String),
    LocalGit(String, String, Result<serde_json::Value,String>),
}
#[derive(serde::Deserialize)]
pub struct FolderSelection {
    #[serde(default)]
    pub managed_revision:Option<String>,
    #[serde(default)]
    pub git_managed_folder: Option<String>,
    pub folder: Option<String>,
    pub choices: Vec<String>,
    pub files: std::collections::BTreeMap<String, String>,
}
pub fn js_error(e: JsValue) -> anyhow::Error {
    anyhow::anyhow!("{e:?}")
}
pub fn storage() -> anyhow::Result<web_sys::Storage> {
    web_sys::window()
        .ok_or_else(|| anyhow::anyhow!("Window unavailable"))?
        .local_storage()
        .map_err(js_error)?
        .ok_or_else(|| anyhow::anyhow!("Browser storage is disabled"))
}
pub fn notice(message: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.alert_with_message(message);
    }
}
fn wake() {
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.request_repaint();
        }
    });
}
pub fn take_events() -> Vec<Event> {
    EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
}
fn push(event: Event) {
    EVENTS.with(|e| e.borrow_mut().push(event));
    wake();
}
pub fn download(name: &str, bytes: &[u8], mime: &str) -> anyhow::Result<()> {
    let array = js_sys::Array::new();
    array.push(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&array, &options)
        .map_err(js_error)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js_error)?;
    let document = web_sys::window().unwrap().document().unwrap();
    let link = document
        .create_element("a")
        .map_err(js_error)?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|e| js_error(e.into()))?;
    link.set_href(&url);
    link.set_download(name);
    link.click();
    let cleanup = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    web_sys::window()
        .unwrap()
        .set_timeout_with_callback_and_timeout_and_arguments_0(cleanup.unchecked_ref(), 1000)
        .map_err(js_error)?;
    Ok(())
}
pub fn choose_inventory_workspace() {
    let pending = host_select_inventory_workspace();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = wasm_bindgen_futures::JsFuture::from(pending).await {
            let message = js_sys::Reflect::get(&error, &"message".into())
                .ok()
                .and_then(|value| value.as_string())
                .or_else(|| error.as_string())
                .unwrap_or_else(|| "Could not choose an inventory folder.".into());
            push(Event::Error(message));
        }
    });
}
pub fn copy_image(image: platform::clipboard::ImageData<'_>) -> anyhow::Result<()> {
    let rgba = image::RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.into_owned(),
    )
    .ok_or_else(|| anyhow::anyhow!("Invalid clipboard image"))?;
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba).write_to(&mut png, image::ImageFormat::Png)?;
    let parts = js_sys::Array::new();
    parts.push(&js_sys::Uint8Array::from(png.get_ref().as_slice()));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("image/png");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
        .map_err(js_error)?;
    let data = js_sys::Object::new();
    js_sys::Reflect::set(&data, &"image/png".into(), &blob).map_err(js_error)?;
    let constructor = js_sys::Reflect::get(&js_sys::global(), &"ClipboardItem".into())
        .map_err(js_error)?
        .dyn_into::<js_sys::Function>()
        .map_err(js_error)?;
    let args = js_sys::Array::new();
    args.push(&data);
    let item = js_sys::Reflect::construct(&constructor, &args).map_err(js_error)?;
    let items = js_sys::Array::new();
    items.push(&item);
    let pending = web_sys::window()
        .unwrap()
        .navigator()
        .clipboard()
        .write(&items);
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = wasm_bindgen_futures::JsFuture::from(pending).await {
            push(Event::Error(format!("Clipboard permission denied: {e:?}")));
        }
    });
    Ok(())
}
#[wasm_bindgen]
pub async fn start(canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
    let image = Closure::<dyn FnMut(String, js_sys::Uint8Array)>::new(
        |owner, bytes: js_sys::Uint8Array| push(Event::NotesImage(owner, bytes.to_vec())),
    );
    let error = Closure::<dyn FnMut(String)>::new(|message| push(Event::Error(message)));
    host_install_notes_paste(
        image.as_ref().unchecked_ref(),
        error.as_ref().unchecked_ref(),
    );
    image.forget();
    error.forget();
    let changed = Closure::<dyn FnMut(String)>::new(|folder| push(Event::KicadChanged(folder)));
    host_install_kicad_watch(changed.as_ref().unchecked_ref());
    changed.forget();
    wasm_bindgen_futures::JsFuture::from(host_init_inventory_storage()).await?;
    let store = crate::store::Store::open("/browser/inventory.json")
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let store = Arc::new(Mutex::new(store));
    STORE.with(|s| *s.borrow_mut() = Some(store.clone()));
    let runner = eframe::WebRunner::new();
    RUNNER.with(|r| *r.borrow_mut() = Some(runner.clone()));
    runner
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(move |cc| {
                CONTEXT.with(|c| *c.borrow_mut() = Some(cc.egui_ctx.clone()));
                Ok(Box::new(crate::ui::InventoryApp::new(cc, store)))
            }),
        )
        .await?;
    RUNNER.with(|r| *r.borrow_mut() = Some(runner));
    Ok(())
}
#[wasm_bindgen]
pub fn browser_sync_snapshot(actual: String) -> Result<(), JsValue> {
    STORE.with(|s| -> Result<(), JsValue> {
        if let Some(store) = s.borrow().as_ref() {
            store
                .lock()
                .unwrap()
                .accept_remote(actual)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        }
        Ok(())
    })?;
    wake();
    Ok(())
}
#[wasm_bindgen]
pub fn export_inventory() -> Result<String, JsValue> {
    STORE.with(|s| {
        let store = s.borrow();
        let store = store
            .as_ref()
            .ok_or_else(|| JsValue::from_str("App is starting"))?
            .lock()
            .unwrap();
        serde_json::to_string_pretty(&store.data).map_err(|e| JsValue::from_str(&e.to_string()))
    })
}
#[wasm_bindgen]
pub fn import_inventory(json: &str) -> Result<(), JsValue> {
    if json.len() > 64 * 1024 * 1024 {
        return Err("Inventory exceeds the 64 MB browser import limit".into());
    }
    let next: crate::model::Inventory =
        serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    STORE
        .with(|s| {
            s.borrow()
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("App is starting"))?
                .lock()
                .unwrap()
                .change(|data| {
                    *data = next;
                    Ok(())
                })
        })
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    wake();
    Ok(())
}
#[wasm_bindgen]
pub fn import_csv(bytes: Vec<u8>, filename: String) {
    push(Event::Csv(bytes, filename));
}
/// The browser owns asynchronous file selection; the form receives only the selected bytes.
#[wasm_bindgen(module = "/web/host.js")]
extern "C" {
    #[wasm_bindgen(js_name=selectInventoryWorkspace)]
    fn host_select_inventory_workspace() -> js_sys::Promise;
    #[wasm_bindgen(js_name=inventoryWorkspaceStatus)]
    pub(crate) fn inventory_workspace_status() -> String;
    #[wasm_bindgen(js_name=openSyncDialog)]
    pub(crate) fn open_sync_dialog();
    #[wasm_bindgen(js_name=initInventoryStorage)]
    fn host_init_inventory_storage() -> js_sys::Promise;
    #[wasm_bindgen(js_name=flushInventory)]
    fn host_flush_inventory() -> js_sys::Promise;
    #[wasm_bindgen(js_name=isBrowserDemo)]
    pub(crate) fn is_browser_demo() -> bool;
    #[wasm_bindgen(js_name=resetBrowserInventory)]
    fn host_reset_browser_inventory() -> js_sys::Promise;
    #[wasm_bindgen(js_name=inventoryHistoryPage)]
    pub fn host_history_page(before: &str, limit: usize) -> js_sys::Promise;
    #[wasm_bindgen(js_name=inventoryLocalHead)]
    pub fn host_local_head_hash() -> String;
    #[wasm_bindgen(js_name=inventoryPairingPending)]
    pub(crate) fn inventory_pairing_pending() -> bool;
    #[wasm_bindgen(js_name=inventoryNetworkGraph)]
    pub fn host_network_graph() -> String;
    #[wasm_bindgen(js_name=inventorySaveBusy)]
    pub(crate) fn host_inventory_save_busy() -> bool;
    #[wasm_bindgen(js_name=inventorySnapshot)]
    pub(crate) fn host_inventory_snapshot() -> Option<String>;
    #[wasm_bindgen(catch, js_name=saveInventory)]
    pub(crate) fn host_save_inventory(
        next: &str,
        expected: Option<&str>,
    ) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(js_name=installKiCadWatch)]
    fn host_install_kicad_watch(callback: &js_sys::Function);
    #[wasm_bindgen(js_name=setKiCadWatch)]
    pub(crate) fn host_set_kicad_watch(folder: Option<&str>);
    #[wasm_bindgen(js_name=authorizeKiCadFolder)]
    fn host_authorize_kicad_folder(folder: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name=chooseModelsFolder)]
    fn host_choose_models_folder() -> js_sys::Promise;
    #[wasm_bindgen(js_name=pickFile)]
    fn host_pick_file(accept: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name=pickManagedKiCadFolder)]
    fn host_pick_managed_folder(project:&str,tip:&str)->js_sys::Promise;
    #[wasm_bindgen(js_name=pickKiCadFolder)]
    fn host_pick_kicad_folder() -> js_sys::Promise;
    #[wasm_bindgen(js_name=runLocalGit)]
    fn host_local_git(folder: &str, project: &str, operation: &str, message: &str, name: &str, email: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name=pickGitBundles)]
    fn host_pick_git_bundles(project_id: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name=fetchGitBundle)]
    fn host_fetch_git_bundle(project_id: &str, tip: Option<&str>) -> js_sys::Promise;
    #[wasm_bindgen(js_name=keepGitBundles)]
    pub(crate) fn keep_git_bundles() -> bool;
    #[wasm_bindgen(js_name=setKeepGitBundles)]
    pub(crate) fn set_keep_git_bundles(keep: bool);
    #[wasm_bindgen(js_name = setNotesOwner)]
    fn host_set_notes_owner(owner: &str);
    #[wasm_bindgen(js_name = installNotesPaste)]
    fn host_install_notes_paste(image: &js_sys::Function, error: &js_sys::Function);
    #[wasm_bindgen(js_name = readNotesClipboard)]
    fn host_read_notes_clipboard() -> js_sys::Promise;
}
pub fn pick_file(accept: &str, target: impl FnOnce(Vec<u8>) -> Event + 'static) {
    let promise = host_pick_file(accept);
    wasm_bindgen_futures::spawn_local(async move {
        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(value) if !value.is_null() => push(target(js_sys::Uint8Array::new(&value).to_vec())),
            Ok(_) => {}
            Err(error) => push(Event::Error(format!("Could not read file: {error:?}"))),
        }
    });
}

#[derive(Clone)]
pub struct FileContext {
    pub part: Option<String>,
    pub notes: Option<String>,
    pub project: Option<String>,
}
pub fn set_file_context(context: FileContext) {
    host_set_notes_owner(context.notes.as_deref().unwrap_or(""));
    FILE_CONTEXT.with(|c| *c.borrow_mut() = context);
}
pub fn choose_file(extensions: &[String]) {
    let context = FILE_CONTEXT.with(|c| c.borrow().clone());
    let accept = extensions
        .iter()
        .map(|s| format!(".{s}"))
        .collect::<Vec<_>>()
        .join(",");
    if extensions.iter().any(|s| s == "csv") {
        pick_file(&accept, move |bytes| {
            if let Some(project) = context.project {
                Event::Bom(project, bytes)
            } else {
                Event::Csv(bytes, "import.csv".into())
            }
        });
    } else if extensions.iter().any(|s| s == "pdf") {
        if let Some(id) = context.part {
            pick_file(&accept, move |bytes| Event::PartPdf(id, bytes));
        }
    } else if extensions
        .iter()
        .any(|s| matches!(s.as_str(), "png" | "jpg" | "jpeg" | "webp" | "tiff"))
    {
        if let Some(owner) = context.notes {
            pick_file(&accept, move |bytes| Event::NotesImage(owner, bytes));
        } else if let Some(id) = context.part {
            pick_file(&accept, move |bytes| Event::PartImage(id, bytes));
        }
    } else {
        notice("Export parts as CSV in the desktop app, then import that CSV in the browser.");
    }
}

#[wasm_bindgen]
pub fn runtime_error() -> Option<String> {
    RUNNER.with(|r| {
        r.borrow()
            .as_ref()
            .and_then(|runner| runner.panic_summary().map(|panic| panic.message()))
    })
}

pub fn reset_browser_inventory() {
    let pending = host_reset_browser_inventory();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = wasm_bindgen_futures::JsFuture::from(pending).await {
            push(Event::Error(format!(
                "Could not reset browser inventory: {error:?}"
            )));
        }
    });
}

pub fn pick_managed_folder(token:String,project:&str,tip:&str){accept_kicad_selection(token,host_pick_managed_folder(project,tip));}
pub fn pick_kicad_folder(token: String) {
    accept_kicad_selection(token, host_pick_kicad_folder());
}

pub fn pick_git_bundles(token: String, project_id: &str) {
    accept_kicad_selection(token, host_pick_git_bundles(project_id));
}

pub fn fetch_git_bundle(token: String, project_id: &str, tip: Option<&str>) {
    accept_kicad_selection(token, host_fetch_git_bundle(project_id, tip));
}

fn accept_kicad_selection(token: String, pending: js_sys::Promise) {
    wasm_bindgen_futures::spawn_local(async move {
        match wasm_bindgen_futures::JsFuture::from(pending).await {
            Ok(value) if value.is_null() => {}
            Ok(value) => match value
                .as_string()
                .and_then(|s| serde_json::from_str(&s).ok())
            {
                Some(files) => push(Event::KicadFolder(token, files)),
                None => push(Event::KicadFolderError(token,"Could not read the selected project folder".into())),
            },
            Err(error) => push(Event::KicadFolderError(token,format!("Could not read project: {error:?}"))),
        }
    });
}

pub fn paste_notes_image(owner: String) {
    let promise = host_read_notes_clipboard();
    wasm_bindgen_futures::spawn_local(async move {
        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(value) => push(Event::NotesImage(
                owner,
                js_sys::Uint8Array::new(&value).to_vec(),
            )),
            Err(error) => push(Event::Error(format!("Could not paste image: {error:?}"))),
        }
    });
}

pub(crate) fn persist_inventory(next: &str, expected: Option<&str>) -> anyhow::Result<()> {
    let pending = host_save_inventory(next, expected).map_err(js_error)?;
    let expected = next.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        let result = wasm_bindgen_futures::JsFuture::from(pending).await;
        if let Ok(value) = &result {
            if let Some(actual) = value.as_string() {
                STORE.with(|s| {
                    if let Some(store) = s.borrow().as_ref() {
                        if let Ok(mut store) = store.lock() {
                            if let Err(error) = store.accept_persisted(&expected, actual) {
                                push(Event::Error(error.to_string()));
                            }
                        }
                    }
                });
                wake();
            }
        }
        if let Err(error) = result {
            push(Event::Error(format!(
                "Inventory has not been saved. Export your inventory before reloading. {error:?}"
            )));
        }
    });
    Ok(())
}
pub(crate) fn authorize_kicad_folder(folder: String) {
    let pending = host_authorize_kicad_folder(&folder);
    wasm_bindgen_futures::spawn_local(async move {
        match wasm_bindgen_futures::JsFuture::from(pending).await {
            Ok(_) => push(Event::KicadChanged(folder)),
            Err(error) => push(Event::Error(format!("Could not access folder: {error:?}"))),
        }
    });
}

/// Narrow page bridge: supplier purchases and project names only; no HTTP server.
#[wasm_bindgen]
pub async fn supplier_request(operation: &str, payload: &str) -> Result<String, JsValue> {
    use anyhow::ensure;
    use base64::Engine;
    let result = (|| -> anyhow::Result<serde_json::Value> {
        ensure!(payload.len() <= 1_000_000, "Purchase payload exceeds 1 MB");
        STORE.with(|s| {
            let store = s.borrow();
            let mut store = store
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("App is starting"))?
                .lock()
                .unwrap();
            match operation {
                "health" => Ok(serde_json::json!({"application":"bom-inventory", "protocol":1})),
                "projects" => Ok(serde_json::json!(
                    store
                        .data
                        .projects
                        .iter()
                        .map(|p| serde_json::json!({"name":p.name,"description":p.description}))
                        .collect::<Vec<_>>()
                )),
                "purchase" => {
                    let body: serde_json::Value = serde_json::from_str(payload)?;
                    if let Some(existing) = crate::purchase_import::existing(&store.data, &body)? {
                        return Ok(serde_json::json!({"id":existing.part.id}));
                    }
                    let image = if let Some(encoded) = body.get("imageBase64") {
                        let encoded = encoded
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid imageBase64"))?;
                        ensure!(encoded.len() <= 800_000, "Image exceeds 600 KB");
                        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                        ensure!(
                            !bytes.is_empty() && bytes.len() <= 600_000,
                            "Image exceeds 600 KB"
                        );
                        Some(crate::attachments::AttachmentDraft::new()?.image_bytes(&bytes)?)
                    } else {
                        None
                    };
                    let id = store.change(|data| {
                        let (_, purchase) =
                            crate::purchase_import::import(data, &body, image.as_ref())?;
                        Ok(purchase.part.id)
                    })?;
                    Ok(serde_json::json!({"id":id}))
                }
                _ => anyhow::bail!("Unsupported supplier bridge operation"),
            }
        })
    })();
    let value = result.map_err(|e| JsValue::from_str(&e.to_string()))?;
    if operation == "purchase" {
        wasm_bindgen_futures::JsFuture::from(host_flush_inventory()).await?;
    }
    wake();
    Ok(value.to_string())
}

pub fn pick_models_folder(project: String) {
    wasm_bindgen_futures::spawn_local(async move {
        match wasm_bindgen_futures::JsFuture::from(host_choose_models_folder()).await {
            Ok(value) => {
                if let Some(folder) = value.as_string() {
                    push(Event::ModelsFolder(project, folder));
                }
            }
            Err(error) => push(Event::Error(format!(
                "Could not choose 3D model folder: {error:?}"
            ))),
        }
    });
}

#[wasm_bindgen]
pub fn parse_footprint_model_libraries(source: &str) -> Result<String, JsValue> {
    let libraries = inventory_kicad::footprint_model_libraries(source)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    serde_json::to_string(&libraries).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen]
pub fn report_git_sync_status(project:&str,blocked:bool){push(Event::GitSyncStatus(project.into(),blocked));wake();}

/// Host file polling reports committed HEAD changes; Rust owns announcements.
#[wasm_bindgen]
pub fn report_local_git_tip(project:&str,result:&str){
    let value=serde_json::from_str(result).map_err(|error|error.to_string());
    push(Event::LocalGit(project.into(),"share".into(),value));wake();
}
pub fn local_git(project: &str, folder: &str, operation: &str, message: &str, name: &str, email: &str) {
    let pending=host_local_git(folder,project,operation,message,name,email);
    let project=project.to_owned();let operation=operation.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        let result=match wasm_bindgen_futures::JsFuture::from(pending).await {
            Ok(value)=>js_sys::JSON::stringify(&value).map_err(|e|format!("{e:?}")).and_then(|s|serde_json::from_str(&String::from(s)).map_err(|e|e.to_string())),
            Err(error)=>Err(format!("{error:?}")),
        };
        push(Event::LocalGit(project,operation,result));
    });
}
