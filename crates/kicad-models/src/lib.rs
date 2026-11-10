//! Filesystem-independent model loading. Applications supply bytes and variable mappings.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
pub const MAX_MODEL_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_TRIANGLES: usize = 500_000;
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Triangle {
    pub points: [[f32; 3]; 3],
    pub color: [f32; 3],
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Mesh {
    pub triangles: Vec<Triangle>,
}
impl Mesh {
    pub fn triangle(&mut self, points: [[f32; 3]; 3], color: [f32; 3]) -> Result<(), String> {
        if self.triangles.len() >= MAX_TRIANGLES {
            return Err("Model exceeds 500,000 triangles".into());
        }
        if !points
            .iter()
            .flatten()
            .all(|n| n.is_finite() && n.abs() < 1e6)
        {
            return Err("Invalid model coordinates".into());
        }
        self.triangles.push(Triangle { points, color });
        Ok(())
    }
}
/// Resolve variables explicitly instead of using process environment in portable code.
pub fn candidates(
    path: &str,
    project: &str,
    library: Option<&str>,
    variables: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut path = path.replace('\\', "/");
    for _ in 0..16 {
        let Some(start) = path.find("${") else { break };
        let Some(end) = path[start..].find('}').map(|i| start + i) else {
            return vec![];
        };
        let name = &path[start + 2..end];
        let value = if name == "KIPRJMOD" {
            Some(project)
        } else if name.starts_with("KICAD") && name.ends_with("_3DMODEL_DIR") {
            library.or_else(|| variables.get(name).map(String::as_str))
        } else {
            variables.get(name).map(String::as_str)
        };
        let Some(value) = value else { return vec![] };
        path.replace_range(start..=end, value);
    }
    if path.contains("${") {
        return vec![];
    }
    let absolute = path.starts_with('/') || (path.len() > 2 && path.as_bytes()[1] == b':');
    if !absolute && !project.is_empty() {
        path = format!("{}/{path}", project.trim_end_matches('/'));
    }
    let mut output = vec![path.clone()];
    if let Some((stem, ext)) = path.rsplit_once('.') {
        for alternate in if ext.eq_ignore_ascii_case("wrl") {
            &["step", "stp"][..]
        } else {
            &["wrl"][..]
        } {
            output.push(format!("{stem}.{alternate}"));
        }
    }
    output
}
/// Bounded cache shared by all placements of the same model. No process-global GPU state.
#[derive(Default)]
pub struct Cache {
    entries: BTreeMap<String, Arc<Mesh>>,
    triangles: usize,
}
impl Cache {
    pub fn load(&mut self, key: &str, bytes: &[u8]) -> Result<Arc<Mesh>, String> {
        if let Some(mesh) = self.entries.get(key) {
            return Ok(mesh.clone());
        }
        if bytes.len() > MAX_MODEL_BYTES {
            return Err("3D model exceeds 32 MB".into());
        }
        let source = std::str::from_utf8(bytes).map_err(|_| "3D model is not UTF-8")?;
        let mesh = if source.trim_start().starts_with("ISO-10303-21") {
            step(source)?
        } else if source.trim_start().starts_with("#VRML") {
            vrml::read(source)?
        } else {
            return Err("Expected STEP or VRML model".into());
        };
        if mesh.triangles.is_empty() {
            return Err("3D model contains no supported faces".into());
        }
        if self.triangles + mesh.triangles.len() > 1_000_000 {
            self.entries.clear();
            self.triangles = 0;
        }
        self.triangles += mesh.triangles.len();
        let mesh = Arc::new(mesh);
        self.entries.insert(key.into(), mesh.clone());
        Ok(mesh)
    }
}
/// Only units assigned to the representation context apply to coordinates.
/// Exporters often include an unused metre unit alongside the active millimetre unit.
fn step_unit(entities: &[ruststep::ast::EntityInstance]) -> f64 {
    use ruststep::ast::{EntityInstance, Name, Parameter, Record};
    fn records(entity: &EntityInstance) -> Vec<&Record> {
        match entity {
            EntityInstance::Simple { record, .. } => vec![record],
            EntityInstance::Complex { subsuper, .. } => subsuper.0.iter().collect(),
        }
    }
    let mut assigned = Vec::new();
    fn refs(parameter: &Parameter, output: &mut Vec<u64>) {
        match parameter {
            Parameter::Ref(Name::Entity(id)) => output.push(*id),
            Parameter::List(items) => items.iter().for_each(|p| refs(p, output)),
            _ => {}
        }
    }
    for entity in entities {
        for record in records(entity) {
            if record.name == "GLOBAL_UNIT_ASSIGNED_CONTEXT" {
                refs(&record.parameter, &mut assigned);
            }
        }
    }
    for id in assigned {
        let Some(entity) = entities.iter().find(|e| match e {
            EntityInstance::Simple { id: n, .. } | EntityInstance::Complex { id: n, .. } => {
                *n == id
            }
        }) else {
            continue;
        };
        let records = records(entity);
        if !records.iter().any(|r| r.name == "LENGTH_UNIT") {
            continue;
        }
        for record in records {
            if record.name == "SI_UNIT"
                && let Parameter::List(parameters) = &record.parameter
                && matches!(parameters.get(1),Some(Parameter::Enumeration(unit)) if unit == "METRE")
            {
                return match parameters.first() {
                    Some(Parameter::Enumeration(prefix)) => match prefix.as_str() {
                        "MILLI" => 1.,
                        "CENTI" => 10.,
                        "MICRO" => 0.001,
                        "DECI" => 100.,
                        "KILO" => 1_000_000.,
                        _ => 1000.,
                    },
                    _ => 1000.,
                };
            }
            if record.name == "CONVERSION_BASED_UNIT"
                && let Parameter::List(parameters) = &record.parameter
                && matches!(parameters.first(),Some(Parameter::String(unit)) if unit.eq_ignore_ascii_case("inch"))
            {
                return 25.4;
            }
        }
    }
    1.
}

// STEP exporters may style a face, a shell, or the solid owning that shell.
// Keep explicit face/shell styles authoritative over inherited body materials.
fn step_colors(records: &BTreeMap<u64, &ruststep::ast::Record>) -> BTreeMap<u64, [f32; 3]> {
    use ruststep::ast::{Name, Parameter};
    fn refs(p: &Parameter, output: &mut Vec<u64>) {
        match p {
            Parameter::Ref(Name::Entity(id)) => output.push(*id),
            Parameter::List(list) => {
                for p in list {
                    refs(p, output)
                }
            }
            _ => {}
        }
    }
    fn color(
        id: u64,
        records: &BTreeMap<u64, &ruststep::ast::Record>,
        depth: usize,
    ) -> Option<[f32; 3]> {
        if depth > 12 {
            return None;
        }
        let record = records.get(&id)?;
        if record.name == "COLOUR_RGB"
            && let Parameter::List(params) = &record.parameter
        {
            let numbers: Vec<f32> = params
                .iter()
                .filter_map(|p| match p {
                    Parameter::Real(n) => Some(*n as f32),
                    Parameter::Integer(n) => Some(*n as f32),
                    _ => None,
                })
                .collect();
            if numbers.len() == 3 {
                return Some([numbers[0], numbers[1], numbers[2]]);
            }
        }
        let mut children = vec![];
        refs(&record.parameter, &mut children);
        children
            .into_iter()
            .find_map(|id| color(id, records, depth + 1))
    }
    let mut colors = BTreeMap::new();
    for record in records
        .values()
        .filter(|r| r.name == "STYLED_ITEM" || r.name == "OVER_RIDING_STYLED_ITEM")
    {
        if let Parameter::List(params) = &record.parameter
            && params.len() >= 3
        {
            let mut styles = vec![];
            refs(&params[1], &mut styles);
            if let (Some(color), Parameter::Ref(Name::Entity(target))) = (
                styles.into_iter().find_map(|id| color(id, records, 0)),
                &params[2],
            ) {
                colors.insert(*target, color);
            }
        }
    }

    for (id, record) in records {
        if matches!(
            record.name.as_str(),
            "MANIFOLD_SOLID_BREP" | "BREP_WITH_VOIDS" | "FACETED_BREP"
        ) && let Some(material) = colors.get(id).copied()
        {
            let mut shells = Vec::new();
            refs(&record.parameter, &mut shells);
            for shell in shells {
                colors.entry(shell).or_insert(material);
            }
        }
    }
    colors
}

fn step(source: &str) -> Result<Mesh, String> {
    use ruststep::{
        ast::{EntityInstance, Name},
        tables::PlaceHolder,
    };
    use truck_meshalgo::prelude::*;
    let exchange = ruststep::parser::parse(source).map_err(|e| format!("STEP syntax: {e}"))?;
    let mut result = Mesh::default();
    for data in &exchange.data {
        let unit = step_unit(&data.entities);
        let records: BTreeMap<_, _> = data
            .entities
            .iter()
            .filter_map(|e| match e {
                EntityInstance::Simple { id, record } => Some((*id, record)),
                _ => None,
            })
            .collect();
        let colors = step_colors(&records);
        let table = truck_stepio::r#in::Table::from_data_section(data);
        for (shell_id, holder) in &table.shell {
            let shell = table
                .to_compressed_shell(holder)
                .map_err(|e| format!("STEP shell: {e}"))?;
            let polygon = bounded::shell(shell, 0.08 / unit).robust_triangulation(0.08 / unit);
            for (face_index, face) in polygon.faces.iter().enumerate() {
                let Some(surface) = &face.surface else {
                    continue;
                };
                let face_id = holder.cfs_faces.get(face_index).and_then(|f| match f {
                    PlaceHolder::Ref(Name::Entity(id)) => Some(id),
                    _ => None,
                });
                let color = face_id
                    .and_then(|id| colors.get(id))
                    .or_else(|| colors.get(shell_id))
                    .copied()
                    .unwrap_or([0.32, 0.34, 0.37]);
                for polygon_face in surface.face_iter() {
                    for i in 1..polygon_face.len().saturating_sub(1) {
                        let mut points = [polygon_face[0], polygon_face[i], polygon_face[i + 1]]
                            .map(|v| {
                                let p = surface.positions()[v.pos];
                                [
                                    (p.x * unit) as f32,
                                    (p.y * unit) as f32,
                                    (p.z * unit) as f32,
                                ]
                            });
                        if !face.orientation {
                            points.swap(1, 2);
                        }
                        result.triangle(points, color)?;
                    }
                }
            }
        }
    }
    Ok(result)
}

mod bounded;
mod vrml;
/// KiCad 9/10 embeds base64-encoded Zstandard data; uncompressed payloads are accepted too.
pub fn embedded(encoded: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    use std::io::Read;
    if encoded.len() > MAX_MODEL_BYTES * 2 {
        return Err("Embedded model too large".into());
    }
    let encoded = encoded.trim();
    let encoded = encoded
        .strip_prefix('|')
        .and_then(|s| s.strip_suffix('|'))
        .unwrap_or(encoded);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.split_whitespace().collect::<String>())
        .map_err(|e| e.to_string())?;
    let decoded = if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let decoder =
            ruzstd::decoding::StreamingDecoder::new(bytes.as_slice()).map_err(|e| e.to_string())?;
        let mut output = vec![];
        decoder
            .take(MAX_MODEL_BYTES as u64 + 1)
            .read_to_end(&mut output)
            .map_err(|e| e.to_string())?;
        output
    } else {
        bytes
    };
    if decoded.len() > MAX_MODEL_BYTES {
        return Err("Embedded model exceeds 32 MB after decompression".into());
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "RT7272 boundary-curve regression; set KICAD_3D_MODEL"]
    fn rt7272_surface_curves_mesh_with_finite_bounded_geometry() {
        let path = std::env::var("KICAD_3D_MODEL").unwrap();
        let start = std::time::Instant::now();
        let mesh = Cache::default()
            .load(&path, &std::fs::read(&path).unwrap())
            .unwrap();
        assert!(mesh.triangles.len() > 500 && mesh.triangles.len() < 20_000);
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for t in &mesh.triangles {
            for p in &t.points {
                for a in 0..3 {
                    assert!(p[a].is_finite());
                    min[a] = min[a].min(p[a]);
                    max[a] = max[a].max(p[a]);
                }
            }
        }
        assert!(
            min[0] < -2.8 && min[1] < -2.3 && max[0] > 2.8 && max[1] > 2.3 && max[2] > 1.7,
            "missing leads or body: {min:?} .. {max:?}"
        );
        assert!(
            mesh.triangles
                .iter()
                .any(|t| t.color.iter().all(|c| (*c - 0.3).abs() < 0.001)),
            "missing moulded body"
        );
        assert!(
            mesh.triangles
                .iter()
                .any(|t| t.color[0] > 0.7 && t.color[2] > 0.79),
            "missing metal leads"
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        eprintln!(
            "RT7272 prepared in {:?}: {} triangles, bounds {min:?} .. {max:?}",
            start.elapsed(),
            mesh.triangles.len()
        );
    }
    #[test]
    fn step_body_materials_reach_shells_without_overriding_explicit_styles() {
        use ruststep::ast::EntityInstance;
        use std::str::FromStr;
        let entities: Vec<_> = [
            "#1=MANIFOLD_SOLID_BREP('body',#2);",
            "#2=CLOSED_SHELL('',(#3,#4));",
            "#5=MANIFOLD_SOLID_BREP('other body',#6);",
            "#6=CLOSED_SHELL('',(#7));",
            "#10=STYLED_ITEM('',(#11),#1);",
            "#11=PRESENTATION_STYLE_ASSIGNMENT((#12));",
            "#12=SURFACE_STYLE_USAGE(.BOTH.,#13);",
            "#13=SURFACE_SIDE_STYLE('',(#14));",
            "#14=SURFACE_STYLE_FILL_AREA(#15);",
            "#15=FILL_AREA_STYLE('',(#16));",
            "#16=FILL_AREA_STYLE_COLOUR('',#17);",
            "#17=COLOUR_RGB('',0.97,0.96,0.93);",
            "#20=STYLED_ITEM('',(#21),#3);",
            "#21=PRESENTATION_STYLE_ASSIGNMENT((#22));",
            "#22=COLOUR_RGB('',1.,0.,0.);",
            "#30=STYLED_ITEM('',(#31),#5);",
            "#31=PRESENTATION_STYLE_ASSIGNMENT((#32));",
            "#32=COLOUR_RGB('',0.1,0.1,0.1);",
            "#40=STYLED_ITEM('',(#21),#6);",
        ]
        .iter()
        .map(|s| EntityInstance::from_str(s).unwrap())
        .collect();
        let records = entities
            .iter()
            .filter_map(|e| match e {
                EntityInstance::Simple { id, record } => Some((*id, record)),
                _ => None,
            })
            .collect();
        let colors = step_colors(&records);
        assert_eq!(colors[&2], [0.97, 0.96, 0.93]);
        assert_eq!(colors[&3], [1., 0., 0.]);
        assert_eq!(colors[&5], [0.1, 0.1, 0.1]);
        assert_eq!(colors[&6], [1., 0., 0.]);
    }
    #[test]
    #[ignore = "Local CLN17 LED fixture; set KICAD_3D_LED"]
    fn cln17_led_preserves_body_colors() {
        let path = std::env::var("KICAD_3D_LED").unwrap();
        let mesh = Cache::default()
            .load(&path, &std::fs::read(&path).unwrap())
            .unwrap();
        for expected in [
            [0.8156863, 0.8156863, 0.8156863],
            [0.09803922, 0.09803922, 0.09803922],
            [0.972549, 0.96862745, 0.92941177],
        ] {
            assert!(
                mesh.triangles.iter().any(|t| t
                    .color
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < 0.0001)),
                "missing material {expected:?}"
            );
        }
        assert!(
            mesh.triangles.iter().all(|t| t.color != [0.32, 0.34, 0.37]),
            "body material fell back to grey"
        );
    }
    #[test]
    #[ignore = "Manual model profiling fixture; set KICAD_3D_MODEL"]
    fn profile_model_fixture() {
        let path = std::env::var("KICAD_3D_MODEL").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let start = std::time::Instant::now();
        let mesh = Cache::default().load(&path, &bytes).unwrap();
        let tall = mesh
            .triangles
            .iter()
            .filter(|t| {
                let min = t.points.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
                let max = t
                    .points
                    .iter()
                    .map(|p| p[2])
                    .fold(f32::NEG_INFINITY, f32::max);
                max - min > 2.
            })
            .count();
        eprintln!("Tall wall triangles: {tall}");
        eprintln!(
            "Model prepared in {:?}: {} triangles",
            start.elapsed(),
            mesh.triangles.len()
        );
    }
    #[test]
    #[ignore = "Requires KiCad model library; set KICAD_3D_CAPACITOR"]
    fn electrolytic_capacitor_has_cylindrical_walls_on_every_side() {
        let path = std::env::var("KICAD_3D_CAPACITOR").unwrap();
        let mesh = Cache::default()
            .load(&path, &std::fs::read(&path).unwrap())
            .unwrap();
        let mut sectors = [0; 8];
        let mut walls = 0;
        for triangle in &mesh.triangles {
            let low = triangle
                .points
                .iter()
                .map(|p| p[2])
                .fold(f32::INFINITY, f32::min);
            let high = triangle
                .points
                .iter()
                .map(|p| p[2])
                .fold(f32::NEG_INFINITY, f32::max);
            if high - low < 2. {
                continue;
            }
            let x = triangle.points.iter().map(|p| p[0]).sum::<f32>() / 3.;
            let y = triangle.points.iter().map(|p| p[1]).sum::<f32>() / 3.;
            let angle = y.atan2(x).rem_euclid(std::f32::consts::TAU);
            sectors[(angle / std::f32::consts::TAU * 8.) as usize % 8] += 1;
            walls += 1;
        }
        assert!(walls >= 24, "capacitor side walls were omitted: {walls}");
        assert!(
            sectors.iter().all(|n| *n > 0),
            "wall coverage around the cylinder: {sectors:?}"
        );
    }
    #[test]
    fn step_uses_assigned_units_instead_of_unused_metre_declarations() {
        use ruststep::ast::EntityInstance;
        use std::str::FromStr;
        let entities: Vec<_> = [
            "#1=(GLOBAL_UNIT_ASSIGNED_CONTEXT((#2)) REPRESENTATION_CONTEXT('','3D'));",
            "#2=(LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.));",
            "#3=(LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT($,.METRE.));",
        ]
        .iter()
        .map(|s| EntityInstance::from_str(s).unwrap())
        .collect();
        assert_eq!(step_unit(&entities), 1.);
        let mut metres = entities.clone();
        metres[0] = EntityInstance::from_str(
            "#1=(GLOBAL_UNIT_ASSIGNED_CONTEXT((#3)) REPRESENTATION_CONTEXT('','3D'));",
        )
        .unwrap();
        assert_eq!(step_unit(&metres), 1000.);
    }
    #[test]
    fn embedded_raw_and_zstandard_payloads_decode_with_limits() {
        use base64::Engine;
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        assert_eq!(embedded(&encode(b"ABCDEF")).unwrap(), b"ABCDEF");
        let zstd = [
            0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x06, 0x31, 0, 0, b'A', b'B', b'C', b'D', b'E', b'F',
        ];
        assert_eq!(embedded(&encode(&zstd)).unwrap(), b"ABCDEF");
        assert_eq!(
            embedded(&format!("|{}|", encode(&zstd))).unwrap(),
            b"ABCDEF"
        );
        assert!(embedded("not base64!").is_err());
    }
    #[test]
    fn vrml_metadata_def_use_and_nested_transforms_are_applied() {
        let source=b"#VRML V2.0 utf8\nWorldInfo {info [\"example model\"]} Transform {translation 2 0 0 scale 2 2 2 children [DEF SHAPE Shape {geometry IndexedFaceSet {coord Coordinate {point [0 0 0,1 0 0,0 1 0]} coordIndex [0,1,2,-1]}} USE SHAPE]}";
        let mesh = Cache::default().load("example.wrl", source).unwrap();
        assert_eq!(mesh.triangles.len(), 2);
        assert_eq!(mesh.triangles[0].points[0], [2., 0., 0.]);
        assert_eq!(mesh.triangles[0].points[1], [4., 0., 0.]);
    }
    #[test]
    fn paths_resolve_project_and_library_variables_and_fallback_extensions() {
        assert_eq!(
            candidates(
                "${KICAD10_3DMODEL_DIR}/R.3dshapes/R.wrl",
                "/project",
                Some("/models"),
                &BTreeMap::new()
            ),
            vec![
                "/models/R.3dshapes/R.wrl",
                "/models/R.3dshapes/R.step",
                "/models/R.3dshapes/R.stp"
            ]
        );
        assert_eq!(
            candidates(
                "${KIPRJMOD}/parts/U.step",
                "/project",
                None,
                &BTreeMap::new()
            )[0],
            "/project/parts/U.step"
        );
        assert!(candidates("${UNKNOWN}/U.step", "", None, &BTreeMap::new()).is_empty());
    }
    #[test]
    fn vrml_triangles_material_and_cache_reuse() {
        let source=b"#VRML V2.0 utf8\nShape { appearance Appearance { material Material { diffuseColor 1 0 0 } } geometry IndexedFaceSet { coord Coordinate { point [0 0 0,1 0 0,1 1 0,0 1 0] } coordIndex [0,1,2,3,-1] } }";
        let mut cache = Cache::default();
        let a = cache.load("square.wrl", source).unwrap();
        let b = cache.load("square.wrl", source).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.triangles.len(), 2);
        assert_eq!(a.triangles[0].color, [1., 0., 0.]);
    }
}
