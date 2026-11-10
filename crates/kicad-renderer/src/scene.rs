#[path = "selection.rs"]
mod selection;
pub use selection::*;
#[path = "copper.rs"]
mod copper;
#[path = "silkscreen.rs"]
mod silkscreen;
use glam::{Mat4, Vec3};
use inventory_kicad::{Pcb, PcbShape, Point};
use inventory_kicad_models::{Cache, Mesh};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
#[repr(C)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Geometry {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}
impl Geometry {
    fn triangle(&mut self, a: Vec3, b: Vec3, c: Vec3, color: [f32; 3]) {
        let normal = (b - a).cross(c - a).normalize_or_zero().to_array();
        let base = self.vertices.len() as u32;
        self.vertices.extend([a, b, c].map(|p| Vertex {
            position: p.to_array(),
            normal,
            color,
        }));
        self.indices.extend([base, base + 1, base + 2]);
    }
    fn quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3, color: [f32; 3]) {
        self.triangle(a, b, c, color);
        self.triangle(a, c, d, color);
    }
    fn from_model(model: &Mesh) -> Self {
        let mut geometry = Self::default();
        for t in &model.triangles {
            let [a, b, c] = t.points.map(Vec3::from_array);
            geometry.triangle(a, b, c, t.color);
        }
        geometry
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Instance {
    pub mesh: usize,
    pub transform: [f32; 16],
    pub reference: String,
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Statistics {
    pub models: usize,
    pub missing: usize,
    pub unassigned: usize,
    pub footprints: usize,
    pub triangles: usize,
}
#[derive(Serialize, Deserialize)]
pub struct Scene {
    #[serde(skip, default = "next_id")]
    pub(crate) id: usize,
    pub meshes: Vec<Geometry>,
    pub instances: Vec<Instance>,
    pub center: [f32; 3],
    pub radius: f32,
    pub statistics: Statistics,
    pub warnings: Vec<String>,
}
impl Scene {
    /// Stable identifier for renderer-side caches during this scene's lifetime.
    pub fn id(&self) -> usize {
        self.id
    }
}
fn next_id() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
fn close(a: Point, b: Point) -> bool {
    (a.x - b.x).hypot(a.y - b.y) < 0.02
}
fn outlines(board: &Pcb) -> Vec<Vec<Point>> {
    let mut segments: Vec<Vec<Point>> = board
        .items
        .iter()
        .filter(|i| i.layers.iter().any(|l| l == "Edge.Cuts"))
        .filter_map(|i| match &i.shape {
            PcbShape::Path { points, .. } if points.len() > 1 => Some(points.clone()),
            _ => None,
        })
        .collect();
    let mut contours = vec![];
    while let Some(mut chain) = segments.pop() {
        for _ in 0..=segments.len() {
            if close(chain[0], *chain.last().unwrap()) {
                break;
            }
            let end = *chain.last().unwrap();
            let next = segments
                .iter()
                .position(|s| close(s[0], end) || close(*s.last().unwrap(), end));
            let Some(index) = next else { break };
            let mut next = segments.swap_remove(index);
            if !close(next[0], end) {
                next.reverse();
            }
            chain.extend(next.into_iter().skip(1));
        }
        if chain.len() > 3 && close(chain[0], *chain.last().unwrap()) {
            chain.pop();
            contours.push(chain);
        }
    }
    contours
}
fn fill(
    contours: &[Vec<Point>],
    z: f32,
    color: [f32; 3],
    mesh: &mut Geometry,
    flip: bool,
) -> Result<(), String> {
    use lyon_tessellation::{
        BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers,
        math::point, path::Path,
    };
    let mut builder = Path::builder();
    for contour in contours {
        if contour.len() < 3 {
            continue;
        }
        builder.begin(point(contour[0].x as f32, -contour[0].y as f32));
        for p in &contour[1..] {
            builder.line_to(point(p.x as f32, -p.y as f32));
        }
        builder.close();
    }
    let mut output: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    FillTessellator::new()
        .tessellate_path(
            &builder.build(),
            &FillOptions::default().with_fill_rule(FillRule::EvenOdd),
            &mut BuffersBuilder::new(&mut output, |v: FillVertex| v.position().to_array()),
        )
        .map_err(|e| format!("Board outline: {e:?}"))?;
    for t in output.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| {
            let p = output.vertices[i as usize];
            Vec3::new(p[0], p[1], z)
        });
        if flip {
            mesh.triangle(a, c, b, color)
        } else {
            mesh.triangle(a, b, c, color)
        }
    }
    Ok(())
}
fn walls(contour: &[Point], z0: f32, z1: f32, color: [f32; 3], mesh: &mut Geometry) {
    for i in 0..contour.len() {
        let a = contour[i];
        let b = contour[(i + 1) % contour.len()];
        mesh.quad(
            Vec3::new(a.x as f32, -a.y as f32, z0),
            Vec3::new(b.x as f32, -b.y as f32, z0),
            Vec3::new(b.x as f32, -b.y as f32, z1),
            Vec3::new(a.x as f32, -a.y as f32, z1),
            color,
        );
    }
}
fn circle(center: Point, radius: f64) -> Vec<Point> {
    (0..32)
        .map(|i| {
            let a = i as f64 / 32. * std::f64::consts::TAU;
            Point::new(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}
fn trace(points: &[Point], width: f64, z: f32, color: [f32; 3], mesh: &mut Geometry) {
    for pair in points.windows(2) {
        let a = Vec3::new(pair[0].x as f32, -pair[0].y as f32, z);
        let b = Vec3::new(pair[1].x as f32, -pair[1].y as f32, z);
        let d = (b - a).normalize_or_zero();
        let n = Vec3::new(-d.y, d.x, 0.) * width as f32 * 0.5;
        mesh.quad(a - n, b - n, b + n, a + n, color);
    }
}
/// KiCad local model coordinates are right-handed/Z-up. Back-side placement mirrors Y/Z.
pub fn model_transform(
    at: Point,
    angle: f64,
    bottom: bool,
    thickness: f64,
    model: &inventory_kicad::PcbModel,
    vrml: bool,
) -> Mat4 {
    let (offset, scale, rotate) = (model.offset, model.scale, model.rotate);
    let v = |p: [f64; 3]| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32);
    let placement = Mat4::from_translation(Vec3::new(
        at.x as f32,
        -at.y as f32,
        if bottom {
            -(thickness as f32) * 0.5
        } else {
            thickness as f32 * 0.5
        },
    )) * Mat4::from_rotation_z((angle as f32).to_radians())
        * if bottom {
            Mat4::from_rotation_x(std::f32::consts::PI)
        } else {
            Mat4::IDENTITY
        };
    // KiCad model orientation is clockwise: apply negative Z, Y, X angles.
    placement
        * Mat4::from_translation(v(offset))
        * Mat4::from_rotation_z(-(rotate[2] as f32).to_radians())
        * Mat4::from_rotation_y(-(rotate[1] as f32).to_radians())
        * Mat4::from_rotation_x(-(rotate[0] as f32).to_radians())
        * Mat4::from_scale(v(scale) * if vrml { 2.54 } else { 1. })
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ModelAsset {
    pub name: String,
    pub bytes: Vec<u8>,
}
/// Builds once off the UI thread. Repeated footprints share one model mesh and separate transforms.
pub fn build(
    board: &Pcb,
    assets: &BTreeMap<String, ModelAsset>,
    cancel: impl Fn() -> bool,
) -> Result<Arc<Scene>, String> {
    build_with_progress(board, assets, cancel, |_| {})
}
/// Reports coarse progress without rebuilding scene geometry on the UI thread.
pub fn build_with_progress(
    board: &Pcb,
    assets: &BTreeMap<String, ModelAsset>,
    cancel: impl Fn() -> bool,
    progress: impl Fn(&str),
) -> Result<Arc<Scene>, String> {
    build_internal(
        board,
        assets,
        cancel,
        progress,
        true,
        None,
        &mut BTreeMap::new(),
    )
}
/// Quick substrate/pad/silkscreen preview while detailed copper and models prepare.
pub fn build_preview(board: &Pcb, cancel: impl Fn() -> bool) -> Result<Arc<Scene>, String> {
    build_internal(
        board,
        &BTreeMap::new(),
        cancel,
        |_| {},
        false,
        None,
        &mut BTreeMap::new(),
    )
}
pub(crate) fn build_internal(
    board: &Pcb,
    assets: &BTreeMap<String, ModelAsset>,
    cancel: impl Fn() -> bool,
    progress: impl Fn(&str),
    include_copper: bool,
    prepared: Option<&crate::cache::PreparedCache>,
    model_keys: &mut BTreeMap<[u64; 2], usize>,
) -> Result<Arc<Scene>, String> {
    if cancel() {
        return Err("3D preparation cancelled".into());
    }
    progress("Building board outline and drills…");
    let mut contours = outlines(board);
    let mut warnings = vec![];
    if contours.is_empty() {
        let b = board.bounds;
        contours.push(vec![
            b.min,
            Point::new(b.max.x, b.min.y),
            b.max,
            Point::new(b.min.x, b.max.y),
        ]);
        warnings.push("No closed Edge.Cuts outline: using board bounds".into());
    }
    let mut cuts = board.holes.clone();
    cuts.extend(
        board
            .vias
            .iter()
            .filter(|v| v.drill > 0.)
            .map(|v| circle(v.at, v.drill / 2.)),
    );
    // Coincident drills (e.g. stacked vias) must be subtracted only once with even-odd filling.
    let mut distinct = BTreeMap::new();
    for cut in cuts {
        if cut.is_empty() {
            continue;
        }
        let center = cut.iter().fold(Point::default(), |a, p| {
            Point::new(a.x + p.x / cut.len() as f64, a.y + p.y / cut.len() as f64)
        });
        let radius = (cut[0].x - center.x).hypot(cut[0].y - center.y);
        distinct
            .entry((
                (center.x * 10000.).round() as i64,
                (center.y * 10000.).round() as i64,
                (radius * 10000.).round() as i64,
            ))
            .or_insert(cut);
    }
    let holes: Vec<_> = distinct.into_values().collect();
    let mut all = contours.clone();
    all.extend(holes.iter().cloned());
    let h = board.thickness as f32 * 0.5;
    let mut surface = Geometry::default();
    fill(&all, h, [0.035, 0.21, 0.10], &mut surface, false)?;
    fill(&all, -h, [0.035, 0.21, 0.10], &mut surface, true)?;
    for contour in &contours {
        walls(contour, -h, h, [0.22, 0.26, 0.10], &mut surface);
    }
    for hole in &holes {
        walls(hole, -h, h, [0.46, 0.37, 0.18], &mut surface);
    }
    progress("Building outer copper traces and pours…");
    let mut copper_layers = [Geometry::default(), Geometry::default()];
    for item in board.items.iter().filter(|_| include_copper) {
        if cancel() {
            return Err("3D preparation cancelled".into());
        }
        let PcbShape::Path {
            points,
            width,
            filled,
            ..
        } = &item.shape
        else {
            continue;
        };
        for (layer, bottom) in [("F.Cu", false), ("B.Cu", true)] {
            if !item.layers.iter().any(|l| l == layer) {
                continue;
            }
            let z = if bottom { -h - 0.004 } else { h + 0.004 };
            let color = [0.065, 0.29, 0.145];
            let mut copper = Geometry::default();
            if *filled {
                fill(std::slice::from_ref(points), z, color, &mut copper, bottom)?;
            } else {
                copper = copper::stroke(points, *width, z, color, bottom)?;
            }
            copper_layers[usize::from(bottom)]
                .vertices
                .extend(copper.vertices);
        }
    }
    progress("Building pads…");
    for pad in &board.pads {
        if cancel() {
            return Err("3D preparation cancelled".into());
        }
        let mut shape = vec![pad.contour.clone()];
        for hole in &holes {
            if inside(
                &pad.contour,
                hole.iter().fold(Point::default(), |a, p| {
                    Point::new(a.x + p.x / hole.len() as f64, a.y + p.y / hole.len() as f64)
                }),
            ) {
                shape.push(hole.clone());
            }
        }
        if pad.layers.iter().any(|l| l == "F.Cu") {
            fill(&shape, h + 0.05, [0.72, 0.55, 0.22], &mut surface, false)?;
        }
        if pad.layers.iter().any(|l| l == "B.Cu") {
            fill(&shape, -h - 0.05, [0.72, 0.55, 0.22], &mut surface, true)?;
        }
    }
    progress("Building vias…");
    for via in &board.vias {
        let shape = vec![
            circle(via.at, via.size / 2.),
            circle(via.at, via.drill / 2.),
        ];
        if via.layers.iter().any(|l| l == "F.Cu") {
            fill(&shape, h + 0.045, [0.64, 0.48, 0.20], &mut surface, false)?;
        }
        if via.layers.iter().any(|l| l == "B.Cu") {
            fill(&shape, -h - 0.045, [0.64, 0.48, 0.20], &mut surface, true)?;
        }
    }
    let mut text = silkscreen::TextGeometry::new();
    for item in &board.items {
        let top = item.layers.iter().any(|l| l == "F.SilkS");
        let bottom = item.layers.iter().any(|l| l == "B.SilkS");
        for underside in [false, true] {
            if (!underside && !top) || (underside && !bottom) {
                continue;
            }
            let z = if underside { -h - 0.07 } else { h + 0.07 };
            let color = [0.86, 0.86, 0.78];
            match &item.shape {
                PcbShape::Path {
                    points,
                    width,
                    filled,
                    ..
                } => {
                    if *filled {
                        fill(
                            std::slice::from_ref(points),
                            z,
                            color,
                            &mut surface,
                            underside,
                        )?;
                    } else {
                        trace(points, *width, z, color, &mut surface);
                    }
                }
                PcbShape::Text {
                    at,
                    text: value,
                    size,
                    angle,
                    mirrored,
                } => {
                    text.append(
                        value,
                        *at,
                        *size,
                        *angle,
                        *mirrored,
                        z,
                        underside,
                        &mut surface,
                    )?;
                }
            }
        }
    }
    let mut scene = Scene {
        id: next_id(),
        meshes: vec![surface],
        instances: vec![Instance {
            mesh: 0,
            transform: Mat4::IDENTITY.to_cols_array(),
            reference: String::new(),
        }],
        center: [0.; 3],
        radius: 1.,
        statistics: Statistics {
            footprints: board.footprints,
            ..Default::default()
        },
        warnings,
    };
    for (index, coverage) in copper_layers.into_iter().enumerate() {
        if coverage.vertices.is_empty() {
            continue;
        }
        if cancel() {
            return Err("3D preparation cancelled".into());
        }
        let (name, z0, z1) = if index == 0 {
            ("F.Cu", h, h + 0.035)
        } else {
            ("B.Cu", -h - 0.035, -h)
        };
        progress(&format!("Building {name} copper solid…"));
        let geometry = copper::solid(&coverage, z0, z1, &holes)?;
        let mesh = scene.meshes.len();
        scene.meshes.push(geometry);
        scene.instances.push(Instance {
            mesh,
            transform: Mat4::IDENTITY.to_cols_array(),
            reference: name.into(),
        });
    }
    let mut cache = Cache::default();
    let mut meshes = BTreeMap::new();
    let mut failed = BTreeMap::new();
    progress("Loading component models…");
    for footprint in &board.footprint_list {
        if cancel() {
            return Err("3D preparation cancelled".into());
        }
        if footprint.models.is_empty() {
            scene.statistics.unassigned += 1;
            continue;
        }
        let mut loaded = false;
        for model in footprint.models.iter().filter(|m| !m.hidden) {
            if let Some(asset) = assets.get(&model.path) {
                if failed.contains_key(&asset.name) {
                    continue;
                }
                let index = if let Some(index) = meshes.get(&asset.name) {
                    *index
                } else {
                    let key = crate::cache::content_key(&asset.bytes);
                    let cached = prepared.and_then(|c| c.model(&key));
                    let geometry = if let Some(cached) = cached {
                        progress(&format!("Reusing model: {}", footprint.reference));
                        Ok(cached.clone())
                    } else {
                        progress(&format!("Meshing {}: {}", footprint.reference, asset.name));
                        cache
                            .load(&asset.name, &asset.bytes)
                            .map(|mesh| Geometry::from_model(&mesh))
                    };
                    match geometry {
                        Ok(geometry) => {
                            let index = scene.meshes.len();
                            scene.meshes.push(geometry);
                            meshes.insert(asset.name.clone(), index);
                            model_keys.insert(key, index);
                            index
                        }
                        Err(error) => {
                            failed.insert(asset.name.clone(), error.clone());
                            if scene.warnings.len() < 30 {
                                scene.warnings.push(format!("{}: {error}", asset.name));
                            }
                            continue;
                        }
                    }
                };
                let vrml = std::str::from_utf8(&asset.bytes)
                    .is_ok_and(|s| s.trim_start().starts_with("#VRML"));
                let transform = model_transform(
                    footprint.at,
                    footprint.angle,
                    footprint.layer == "B.Cu",
                    board.thickness,
                    model,
                    vrml,
                );
                scene.instances.push(Instance {
                    mesh: index,
                    transform: transform.to_cols_array(),
                    reference: footprint.reference.clone(),
                });
                scene.statistics.models += 1;
                loaded = true;
            }
        }
        if !loaded && !footprint.models.iter().all(|m| m.hidden) {
            scene.statistics.missing += 1;
            if scene.warnings.len() < 30 {
                scene.warnings.push(format!(
                    "{}: model unavailable; omitted from 3D view",
                    footprint.reference
                ));
            }
        }
        if scene.meshes.iter().map(|m| m.vertices.len()).sum::<usize>() > 4_500_000 {
            return Err("3D board exceeds the mesh memory budget".into());
        }
    }
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for instance in &scene.instances {
        let matrix = Mat4::from_cols_array(&instance.transform);
        for vertex in &scene.meshes[instance.mesh].vertices {
            let p = matrix.transform_point3(Vec3::from_array(vertex.position));
            min = min.min(p);
            max = max.max(p);
        }
    }
    scene.center = ((min + max) * 0.5).to_array();
    scene.radius = ((max - min).length() * 0.5).max(1.);
    scene.statistics.triangles = scene.meshes.iter().map(|m| m.indices.len() / 3).sum();
    Ok(Arc::new(scene))
}
fn inside(contour: &[Point], point: Point) -> bool {
    if contour.is_empty() {
        return false;
    }
    let mut hit = false;
    let mut j = contour.len() - 1;
    for i in 0..contour.len() {
        let a = contour[i];
        let b = contour[j];
        if (a.y > point.y) != (b.y > point.y)
            && point.x < (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x
        {
            hit = !hit;
        }
        j = i;
    }
    hit
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn board() -> Pcb {
        Pcb::parse(r#"(kicad_pcb (general (thickness 2))
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (44 "Edge.Cuts" user))
        (gr_rect (start 0 0) (end 10 10) (stroke (width 0.05) (type default)) (fill none) (layer "Edge.Cuts"))
        (via (at 5 5) (size 2) (drill 1) (layers "F.Cu" "B.Cu")))"#).unwrap()
    }
    #[test]
    fn board_silkscreen_text_is_present_on_each_surface() {
        let mut board = board();
        let baseline = build(&board, &BTreeMap::new(), || false).unwrap().meshes[0]
            .vertices
            .len();
        for (layer, mirrored) in [("F.SilkS", false), ("B.SilkS", true)] {
            board.items.push(inventory_kicad::PcbItem {
                reference: String::new(),
                net: String::new(),
                zone: false,
                layers: vec![layer.into()],
                shape: PcbShape::Text {
                    at: Point::new(5., 5.),
                    text: "CLN17".into(),
                    size: 1.,
                    angle: 0.,
                    mirrored,
                },
            });
        }
        let scene = build(&board, &BTreeMap::new(), || false).unwrap();
        let vertices = &scene.meshes[0].vertices[baseline..];
        assert!(
            vertices
                .iter()
                .any(|v| v.position[2] > 1. && v.color == [0.86, 0.86, 0.78])
        );
        assert!(
            vertices
                .iter()
                .any(|v| v.position[2] < -1. && v.color == [0.86, 0.86, 0.78])
        );
    }
    #[test]
    fn only_outer_layer_copper_is_visible_below_pads_and_silkscreen() {
        let board=Pcb::parse(r#"(kicad_pcb (general (thickness 2))
            (layers (0 "F.Cu" signal) (1 "In1.Cu" signal) (2 "B.Cu" signal))
            (segment (start 0 1) (end 8 1) (width 0.3) (layer "F.Cu"))
            (segment (start 0 2) (end 8 2) (width 0.3) (layer "B.Cu"))
            (segment (start 0 3) (end 8 3) (width 0.3) (layer "In1.Cu"))
            (zone (layer "F.Cu") (filled_polygon (layer "F.Cu") (pts (xy 0 4) (xy 8 4) (xy 8 5) (xy 0 5)))))"#).unwrap();
        let scene = build(&board, &BTreeMap::new(), || false).unwrap();
        let copper: Vec<_> = scene
            .meshes
            .iter()
            .skip(1)
            .flat_map(|m| &m.vertices)
            .filter(|v| v.color == [0.065, 0.29, 0.145])
            .collect();
        assert!(
            copper
                .iter()
                .any(|v| v.position[2] > 1. && v.position[1] < -4.)
        );
        assert!(copper.iter().any(|v| v.position[2] < -1.));
        assert!(!copper.iter().any(|v| (v.position[1] + 3.).abs() < 0.2));
        assert!(copper.iter().all(|v| v.position[2].abs() <= 1.036));
    }
    #[test]
    fn substrate_is_extruded_to_board_thickness_and_drilled() {
        let scene = build(&board(), &BTreeMap::new(), || false).unwrap();
        let mesh = &scene.meshes[0];
        let mut area = 0.;
        for triangle in mesh.vertices.chunks_exact(3) {
            if triangle
                .iter()
                .all(|v| v.position[2] == 1. && v.color == [0.035, 0.21, 0.10])
            {
                let [a, b, c] =
                    [triangle[0], triangle[1], triangle[2]].map(|v| Vec3::from_array(v.position));
                area += (b - a).cross(c - a).length() / 2.;
            }
        }
        assert!(
            (area - (100. - std::f32::consts::PI * 0.25)).abs() < 0.03,
            "hole area is subtracted, got {area}"
        );
        assert!(mesh.vertices.iter().any(|v| v.position[2] == -1.));
        assert!(
            mesh.vertices
                .iter()
                .all(|v| v.normal.iter().all(|n| n.is_finite()))
        );
    }
    #[test]
    fn kicad_model_orientation_uses_clockwise_angles() {
        let model = inventory_kicad::PcbModel {
            path: String::new(),
            offset: [0.; 3],
            scale: [1.; 3],
            rotate: [-90., 0., 0.],
            hidden: false,
        };
        let matrix = model_transform(Point::new(0., 0.), 0., false, 1.6, &model, false);
        assert!((matrix.transform_point3(Vec3::Y) - Vec3::new(0., 0., 1.8)).length() < 1e-5);
        let back = model_transform(Point::new(0., 0.), 0., true, 1.6, &model, false);
        assert!((back.transform_point3(Vec3::Y) - Vec3::new(0., 0., -1.8)).length() < 1e-5);
    }
    #[test]
    fn model_placements_share_meshes_and_bottom_components_face_outward() {
        let mut board = board();
        for (reference, layer, x) in [("U1", "F.Cu", 2.), ("U2", "B.Cu", 8.)] {
            board.footprint_list.push(inventory_kicad::PcbFootprint {
                angle: 0.,
                models: vec![inventory_kicad::PcbModel {
                    path: "part.wrl".into(),
                    offset: [0.; 3],
                    scale: [1.; 3],
                    rotate: [0.; 3],
                    hidden: false,
                }],
                properties: BTreeMap::new(),
                library: "Demo".into(),
                reference: reference.into(),
                layer: layer.into(),
                at: Point::new(x, 5.),
                bounds: inventory_kicad::Bounds {
                    min: Point::new(x - 1., 4.),
                    max: Point::new(x + 1., 6.),
                },
            });
        }
        let assets=BTreeMap::from([("part.wrl".into(),ModelAsset{name:"part.wrl".into(),bytes:b"#VRML V2.0 utf8\nShape {geometry IndexedFaceSet {coord Coordinate {point [0 0 0,1 0 0,0 0 1]} coordIndex [0,1,2,-1]}}".to_vec()})]);
        let preview = build(&board, &BTreeMap::new(), || false).unwrap();
        assert_eq!(
            preview.meshes.len(),
            1,
            "missing models must not generate boxes"
        );
        assert_eq!(preview.instances.len(), 1);
        assert_eq!(preview.statistics.missing, 2);
        let invalid = BTreeMap::from([(
            "part.wrl".into(),
            ModelAsset {
                name: "broken.wrl".into(),
                bytes: b"invalid model".to_vec(),
            },
        )]);
        assert_eq!(
            build(&board, &invalid, || false).unwrap().instances.len(),
            1
        );
        let scene = build(&board, &assets, || false).unwrap();
        assert_eq!(scene.meshes.len(), 2);
        assert_eq!(scene.statistics.models, 2);
        assert_eq!(scene.instances[1].mesh, scene.instances[2].mesh);
        let top = Mat4::from_cols_array(&scene.instances[1].transform);
        let bottom = Mat4::from_cols_array(&scene.instances[2].transform);
        assert!((top.transform_point3(Vec3::Z).z - 3.54).abs() < 1e-5);
        assert!((bottom.transform_point3(Vec3::Z).z + 3.54).abs() < 1e-5);
        assert!(build(&board, &assets, || true).is_err());
    }
}
