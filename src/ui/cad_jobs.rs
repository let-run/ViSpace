//! CPU-only scene preparation: Rayon on desktop, Web Workers in the browser.
use super::{cad_gpu::Scene, *};
use inventory_kicad::{Pcb, Schematic};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct PcbSettings {
    pub active: String,
    pub hidden: BTreeSet<String>,
    pub opacity: f32,
    pub labels: bool,
    pub clearance: bool,
    pub net_colors: std::collections::BTreeMap<String, [u8; 3]>,
}
#[derive(Serialize, Deserialize)]
pub enum Request {
    PcbFootprints {
        board: Arc<Pcb>,
        settings: PcbSettings,
        references: BTreeSet<String>,
    },
    PcbNet {
        board: Arc<Pcb>,
        settings: PcbSettings,
        net: String,
        nets: BTreeSet<String>,
    },
    Pcb {
        board: Arc<Pcb>,
        settings: PcbSettings,
    },
    Schematic {
        source: Arc<Schematic>,
        variables: std::collections::BTreeMap<String, String>,
        instance: String,
    },
}
impl Request {
    fn prepare(self, cancel: Arc<AtomicBool>) -> anyhow::Result<Arc<Scene>> {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Preparation cancelled");
        match self {
            Self::PcbFootprints {
                board,
                settings,
                references,
            } => super::pcb::PcbViewer::prepare_footprints(board, settings, references, cancel),
            Self::PcbNet {
                board,
                settings,
                nets,
                ..
            } => super::pcb::PcbViewer::prepare_net(board, settings, &nets, cancel),
            Self::Pcb { board, settings } => {
                super::pcb::PcbViewer::prepare(board, settings, cancel)
            }
            Self::Schematic {
                source,
                variables,
                instance,
            } => super::kicad::prepare_document(source, variables, instance, cancel),
        }
    }
}
pub(super) fn spawn(request: Request, ctx: &egui::Context) -> crate::tasks::Task<Arc<Scene>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let wake = ctx.clone();
        crate::tasks::Task::spawn_shared(
            move |cancel| request.prepare(cancel),
            Box::new(move || wake.request_repaint()),
        )
    }
    #[cfg(target_arch = "wasm32")]
    {
        crate::tasks::Task::browser(
            crate::web::jobs::Input::CadScene { request },
            decode_worker_scene,
            ctx.clone(),
        )
    }
}
fn encode_scene(scene: &Scene) -> anyhow::Result<String> {
    use base64::Engine;
    use std::io::Write;
    let encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut writer = std::io::BufWriter::with_capacity(64 * 1024, encoder);
    bincode::serde::encode_into_std_write(
        scene,
        &mut writer,
        bincode::config::standard().with_fixed_int_encoding(),
    )?;
    writer.flush()?;
    let bytes = writer.into_inner().map_err(|e| e.into_error())?.finish()?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}
#[cfg(any(target_arch = "wasm32", test))]
fn decode_worker_scene(value: serde_json::Value) -> anyhow::Result<Arc<Scene>> {
    use base64::Engine;
    let text = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid prepared scene"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(text)?;
    let mut reader = std::io::BufReader::with_capacity(
        64 * 1024,
        flate2::read::ZlibDecoder::new(bytes.as_slice()),
    );
    let mut scene: Scene = bincode::serde::decode_from_std_read(
        &mut reader,
        bincode::config::standard()
            .with_fixed_int_encoding()
            .with_limit::<{ 512 * 1024 * 1024 }>(),
    )?;
    scene.share_glyphs();
    Ok(Arc::new(scene))
}
pub fn run_worker(request: Request) -> anyhow::Result<serde_json::Value> {
    let scene = request.prepare(Arc::new(AtomicBool::new(false)))?;
    Ok(serde_json::Value::String(encode_scene(&scene)?))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_footprint_group_preparation_keeps_every_selected_footprint() {
        let board = Arc::new(
            Pcb::parse(
                r#"(kicad_pcb (layers (0 "F.Cu" signal))
            (footprint "R" (layer "F.Cu") (at 0 0) (property "Reference" "R1")
                (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu")))
            (footprint "R" (layer "F.Cu") (at 10 10) (property "Reference" "R2")
                (pad "2" smd rect (at 0 0) (size 2 2) (layers "F.Cu"))))"#,
            )
            .unwrap(),
        );
        let settings = PcbSettings {
            active: "F.Cu".into(),
            hidden: BTreeSet::new(),
            opacity: 0.18,
            labels: true,
            clearance: true,
            net_colors: Default::default(),
        };
        let one = decode_worker_scene(
            run_worker(Request::PcbFootprints {
                board: board.clone(),
                settings: settings.clone(),
                references: BTreeSet::from(["R1".into()]),
            })
            .unwrap(),
        )
        .unwrap();
        let group = decode_worker_scene(
            run_worker(Request::PcbFootprints {
                board,
                settings,
                references: BTreeSet::from(["R1".into(), "R2".into()]),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            group.profile().1 > one.profile().1,
            "both footprints and pad labels reach the browser renderer"
        );
    }
    #[test]
    fn browser_scene_transport_preserves_instanced_text_and_cancellation_does_no_work() {
        let board=Arc::new(Pcb::parse(r#"(kicad_pcb (layers (0 "F.Cu" signal)) (gr_text "RRR" (at 1 1) (layer "F.Cu") (effects (font (size 1 1)))))"#).unwrap());
        let settings = PcbSettings {
            active: "F.Cu".into(),
            hidden: Default::default(),
            opacity: 0.18,
            labels: true,
            clearance: true,
            net_colors: Default::default(),
        };
        let cancelled = Request::Pcb {
            board: board.clone(),
            settings: settings.clone(),
        }
        .prepare(Arc::new(AtomicBool::new(true)));
        assert!(cancelled.is_err());
        let value = run_worker(Request::Pcb { board, settings }).unwrap();
        let scene = decode_worker_scene(value).unwrap();
        assert_eq!(scene.profile().1, 3);
    }
}
