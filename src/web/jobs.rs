use crate::{csv_import::CsvImport, model::Inventory};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
pub enum Input {
    Board3dSelection {
        board: std::sync::Arc<inventory_kicad::Pcb>,
        selection: inventory_kicad_renderer::Selection,
    },
    Board3d {
        board: std::sync::Arc<inventory_kicad::Pcb>,
        path: String,
        folder: Option<String>,
        models_folder: Option<String>,
        #[serde(default)]
        demo: bool,
        assets: std::collections::BTreeMap<String, (String, String)>,
        #[serde(default)]
        cache: Option<String>,
    },
    CadScene {
        request: crate::ui::cad_jobs::Request,
    },
    NotesPdf {
        source: String,
        images: std::collections::BTreeMap<String, String>,
    },
    KicadBoard {
        path: String,
        #[serde(default)]
        folder: Option<String>,
        files: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        stamps: Vec<crate::pcb_cache::SourceStamp>,
        #[serde(default)]
        cache: Option<String>,
    },
    Kicad {
        path: String,
        #[serde(default)]
        folder: Option<String>,
        files: std::collections::BTreeMap<String, String>,
    },
    Csv {
        bytes: Vec<u8>,
        filename: String,
    },
    CsvApply {
        document: CsvImport,
        suggestions: Vec<String>,
        data: Inventory,
    },
    Review {
        data: Inventory,
        project: String,
    },
    Apply {
        data: Inventory,
        project: String,
        matches: std::collections::BTreeMap<String, String>,
    },
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn run_worker_job(request: &str) -> String {
    let result = (|| -> anyhow::Result<serde_json::Value> {
        match serde_json::from_str::<Input>(request)? {
            Input::Board3dSelection { board, selection } => {
                crate::ui::board3d::worker_selection(board, selection)
            }
            Input::Board3d {
                board,
                assets,
                cache,
                ..
            } => crate::ui::board3d::worker_scene_cached(board, assets, cache),
            Input::CadScene { request } => crate::ui::cad_jobs::run_worker(request),
            Input::NotesPdf { source, images } => {
                use base64::Engine;
                Ok(serde_json::json!(
                    base64::engine::general_purpose::STANDARD
                        .encode(crate::notes_pdf::render(&source, &images)?)
                ))
            }
            Input::KicadBoard {
                path,
                files,
                stamps,
                cache,
                ..
            } => crate::pcb_cache::worker(&path, &files, &stamps, cache.as_deref()),
            Input::Kicad { path, files, .. } => {
                let bundle = inventory_kicad::load_schematics(&path, |name| {
                    files.get(name).cloned().ok_or_else(||inventory_kicad::Error::new(format!("Sheet not supplied: {name}. Choose the project folder again to include its sheets.")))
                })?;
                Ok(serde_json::to_value(bundle)?)
            }
            Input::Csv { bytes, filename } => Ok(serde_json::to_value(CsvImport::read(
                bytes.as_slice(),
                filename,
            )?)?),
            Input::CsvApply {
                mut document,
                suggestions,
                mut data,
            } => {
                data.import_parts(document.selected_parts(&suggestions)?)?;
                Ok(serde_json::to_value(data)?)
            }
            Input::Review { data, project } => Ok(serde_json::to_value(
                crate::requests::RequestReview::build(&data, &project, || false)?,
            )?),
            Input::Apply {
                mut data,
                project,
                matches,
            } => {
                let count = data.sync_requests_cancellable(&project, &matches, || false)?;
                Ok(serde_json::json!({"data":data,"count":count}))
            }
        }
    })();
    match result {
        Ok(value) => serde_json::json!({"value":value}),
        Err(e) => serde_json::json!({"error":e.to_string()}),
    }
    .to_string()
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn pcb_cache_version() -> u32 {
    crate::pcb_cache::PARSER_VERSION
}
