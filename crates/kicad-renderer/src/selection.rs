use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Clone, Default, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Selection {
    pub net: Option<String>,
    #[serde(default)]
    pub nets: BTreeSet<String>,
    pub references: BTreeSet<String>,
}
impl Selection {
    pub fn contains_net(&self, net: &str) -> bool {
        if self.nets.is_empty() {
            self.net.as_deref() == Some(net)
        } else {
            self.nets.contains(net)
        }
    }
    pub fn is_empty(&self) -> bool {
        self.net.is_none() && self.nets.is_empty() && self.references.is_empty()
    }
}
pub fn layer_color(name: &str) -> [f32; 3] {
    let rgb = match name {
        "F.Cu" => [220_u8, 65, 65],
        "B.Cu" => [75, 100, 235],
        _ => [180, 165, 60],
    };
    rgb.map(|v| v as f32 / 255.)
}
/// Use saved stackup depths; otherwise distribute layers through board thickness.
pub fn copper_depth(board: &Pcb, name: &str) -> f32 {
    if let Some(z) = board.copper_depths.get(name) {
        return *z as f32;
    }
    let layers: Vec<_> = board
        .layers
        .iter()
        .filter(|l| l.name.ends_with(".Cu"))
        .collect();
    let i = layers.iter().position(|l| l.name == name).unwrap_or(0);
    board.thickness as f32 * (0.5 - i as f32 / layers.len().saturating_sub(1).max(1) as f32)
}
/// Portable background-built X-ray overlay. Base model buffers remain unchanged.
pub fn build_selection(
    board: &Pcb,
    selection: &Selection,
    cancel: impl Fn() -> bool,
) -> Result<Arc<Scene>, String> {
    let mut scene = Scene {
        id: next_id(),
        meshes: Vec::new(),
        instances: Vec::new(),
        center: [0.; 3],
        radius: 1.,
        statistics: Statistics::default(),
        warnings: Vec::new(),
    };
    if selection.is_empty() {
        return Ok(Arc::new(scene));
    }
    let matches = |net: &str, reference: &str| {
        selection.contains_net(net) || selection.references.contains(reference)
    };
    let mut holes = board.holes.clone();
    holes.extend(
        board
            .vias
            .iter()
            .filter(|v| v.drill > 0.)
            .map(|v| circle(v.at, v.drill / 2.)),
    );
    for layer in board.layers.iter().filter(|l| l.name.ends_with(".Cu")) {
        if cancel() {
            return Err("Selection cancelled".into());
        }
        let z = copper_depth(board, &layer.name);
        let mut coverage = Geometry::default();
        for item in &board.items {
            if !selection.contains_net(&item.net) || !item.layers.contains(&layer.name) {
                continue;
            }
            if let PcbShape::Path {
                points,
                width,
                filled,
                ..
            } = &item.shape
            {
                if *filled {
                    fill(
                        std::slice::from_ref(points),
                        0.,
                        [1.; 3],
                        &mut coverage,
                        false,
                    )?;
                } else {
                    coverage
                        .vertices
                        .extend(copper::stroke(points, *width, 0., [1.; 3], false)?.vertices);
                }
            }
        }
        for pad in board
            .pads
            .iter()
            .filter(|p| matches(&p.net, &p.reference) && p.layers.contains(&layer.name))
        {
            fill(
                std::slice::from_ref(&pad.contour),
                0.,
                [1.; 3],
                &mut coverage,
                false,
            )?;
        }
        for via in board
            .vias
            .iter()
            .filter(|v| selection.contains_net(&v.net) && v.layers.contains(&layer.name))
        {
            fill(
                &[circle(via.at, via.size / 2.)],
                0.,
                [1.; 3],
                &mut coverage,
                false,
            )?;
        }
        if coverage.vertices.is_empty() {
            continue;
        }
        let mut mesh = copper::solid(&coverage, z - 0.0175, z + 0.0175, &holes)?;
        for vertex in &mut mesh.vertices {
            vertex.color = layer_color(&layer.name);
        }
        let index = scene.meshes.len();
        scene.meshes.push(mesh);
        scene.instances.push(Instance {
            mesh: index,
            transform: Mat4::IDENTITY.to_cols_array(),
            reference: layer.name.clone(),
        });
    }
    // Via barrels connect selected copper planes visibly through the board.
    let mut barrels = Geometry::default();
    for via in board.vias.iter().filter(|v| selection.contains_net(&v.net)) {
        for layers in via.layers.windows(2) {
            let z0 = copper_depth(board, &layers[0]);
            let z1 = copper_depth(board, &layers[1]);
            walls(
                &circle(via.at, via.size / 2.),
                z0.min(z1),
                z0.max(z1),
                layer_color(&layers[0]),
                &mut barrels,
            );
        }
    }
    if !barrels.vertices.is_empty() {
        let mesh = scene.meshes.len();
        scene.meshes.push(barrels);
        scene.instances.push(Instance {
            mesh,
            transform: Mat4::IDENTITY.to_cols_array(),
            reference: String::new(),
        });
    }
    scene.statistics.triangles = scene.meshes.iter().map(|m| m.indices.len() / 3).sum();
    Ok(Arc::new(scene))
}
/// Ray pick model bounds first; net mode instead picks pads/vias/tracks on the facing surface.
pub fn pick(
    scene: &Scene,
    board: &Pcb,
    origin: [f32; 3],
    direction: [f32; 3],
    nets: bool,
) -> Selection {
    let o = Vec3::from_array(origin);
    let d = Vec3::from_array(direction);
    if !nets {
        let mut hit = None;
        let mut nearest = f32::INFINITY;
        for instance in scene.instances.iter().filter(|i| {
            board
                .footprint_list
                .iter()
                .any(|f| f.reference == i.reference)
        }) {
            let m = Mat4::from_cols_array(&instance.transform).inverse();
            let o = m.transform_point3(o);
            let d = m.transform_vector3(d);
            let mesh = &scene.meshes[instance.mesh];
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for v in &mesh.vertices {
                let p = Vec3::from_array(v.position);
                min = min.min(p);
                max = max.max(p);
            }
            let a = (min - o) / d;
            let b = (max - o) / d;
            let near = a.min(b).max_element().max(0.);
            let far = a.max(b).min_element();
            if near <= far && near < nearest {
                nearest = near;
                hit = Some(instance.reference.clone());
            }
        }
        if let Some(reference) = hit {
            return Selection {
                nets: std::collections::BTreeSet::new(),
                net: None,
                references: BTreeSet::from([reference]),
            };
        }
    }
    if d.z.abs() < 1e-7 {
        return Selection::default();
    }
    let z = if d.z < 0. {
        board.thickness as f32 * 0.5
    } else {
        -board.thickness as f32 * 0.5
    };
    let t = (z - o.z) / d.z;
    if t < 0. {
        return Selection::default();
    }
    let at = o + d * t;
    let p = Point::new(at.x as f64, -at.y as f64);
    let layer = if d.z < 0. { "F.Cu" } else { "B.Cu" };
    let net = board
        .vias
        .iter()
        .find(|v| {
            v.layers.iter().any(|l| l == layer) && (v.at.x - p.x).hypot(v.at.y - p.y) < v.size / 2.
        })
        .map(|v| v.net.clone())
        .or_else(|| {
            board
                .pads
                .iter()
                .find(|v| v.layers.iter().any(|l| l == layer) && inside(&v.contour, p))
                .map(|v| v.net.clone())
        })
        .or_else(|| {
            board
                .items
                .iter()
                .filter(|i| !i.net.is_empty() && i.layers.iter().any(|l| l == layer))
                .filter_map(|i| {
                    if let PcbShape::Path {
                        points,
                        width,
                        filled,
                        ..
                    } = &i.shape
                    {
                        let distance = points
                            .windows(2)
                            .map(|s| {
                                let a = Vec3::new(s[0].x as f32, s[0].y as f32, 0.);
                                let b = Vec3::new(s[1].x as f32, s[1].y as f32, 0.);
                                let p = Vec3::new(p.x as f32, p.y as f32, 0.);
                                let ab = b - a;
                                let t =
                                    ((p - a).dot(ab) / ab.length_squared().max(1e-9)).clamp(0., 1.);
                                (p - a - ab * t).length()
                            })
                            .fold(f32::INFINITY, f32::min);
                        if (!filled && distance < (*width as f32 / 2. + 0.08))
                            || (*filled && inside(points, p))
                        {
                            return Some((if *filled { 1000. } else { distance }, i.net.clone()));
                        }
                    }
                    None
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, n)| n)
        });
    Selection {
        nets: std::collections::BTreeSet::new(),
        net: net.filter(|n| !n.is_empty()),
        references: BTreeSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn board() -> Pcb {
        Pcb::parse(r#"(kicad_pcb (general (thickness 1.6))
      (layers (0 "F.Cu" signal) (4 "In1.Cu" power) (2 "B.Cu" signal))
      (setup (stackup (layer "F.Cu" (thickness 0.035)) (layer "dielectric 1" (thickness 0.2)) (layer "In1.Cu" (thickness 0.035)) (layer "dielectric 2" (thickness 1.295)) (layer "B.Cu" (thickness 0.035))))
      (net 1 "SIG") (net 2 "OTHER")
      (segment (start 0 0) (end 4 0) (width 0.4) (layer "F.Cu") (net 1))
      (segment (start 0 0) (end 4 0) (width 0.4) (layer "In1.Cu") (net 1))
      (segment (start 0 0) (end 4 0) (width 0.4) (layer "B.Cu") (net 1))
      (segment (start 10 0) (end 14 0) (width 0.4) (layer "F.Cu") (net 2))
      (via (at 3 0) (size 0.8) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1)))"#).unwrap()
    }
    #[test]
    fn net_overlay_contains_internal_copper_with_stackup_depth_and_layer_colours() {
        let board = board();
        let selected = Selection {
            nets: std::collections::BTreeSet::new(),
            net: Some("SIG".into()),
            references: BTreeSet::new(),
        };
        let scene = build_selection(&board, &selected, || false).unwrap();
        for layer in ["F.Cu", "In1.Cu", "B.Cu"] {
            let instance = scene
                .instances
                .iter()
                .find(|i| i.reference == layer)
                .unwrap();
            let vertices = &scene.meshes[instance.mesh].vertices;
            assert!(!vertices.is_empty());
            assert!(
                vertices
                    .iter()
                    .all(|v| v.color == layer_color(layer) && v.position[0] < 5.)
            );
        }
        assert!((copper_depth(&board, "In1.Cu") - 0.5475).abs() < 0.001);
        assert!(build_selection(&board, &selected, || true).is_err());
        assert!(
            build_selection(&board, &Selection::default(), || false)
                .unwrap()
                .meshes
                .is_empty()
        );
        assert_eq!(
            pick(&scene, &board, [1., 0., 10.], [0., 0., -1.], true)
                .net
                .as_deref(),
            Some("SIG")
        );
        assert_eq!(
            pick(&scene, &board, [11., 0., 10.], [0., 0., -1.], true)
                .net
                .as_deref(),
            Some("OTHER")
        );
    }
    #[test]
    fn group_selection_renders_every_selected_net() {
        let selection = Selection {
            net: Some("Net class: Signals".into()),
            nets: BTreeSet::from(["SIG".into(), "OTHER".into()]),
            references: BTreeSet::new(),
        };
        let scene = build_selection(&board(), &selection, || false).unwrap();
        let vertices: Vec<_> = scene.meshes.iter().flat_map(|m| &m.vertices).collect();
        assert!(vertices.iter().any(|v| v.position[0] < 5.));
        assert!(vertices.iter().any(|v| v.position[0] > 10.));
        assert!(!selection.is_empty());
        assert!(!selection.contains_net("Net class: Signals"));
    }
    #[test]
    fn model_picking_selects_its_designator() {
        let mut board = board();
        board.footprint_list.push(inventory_kicad::PcbFootprint {
            angle: 0.,
            models: vec![],
            properties: BTreeMap::new(),
            library: String::new(),
            reference: "U1".into(),
            layer: "F.Cu".into(),
            at: Point::new(2., 2.),
            bounds: inventory_kicad::Bounds {
                min: Point::new(1., 1.),
                max: Point::new(3., 3.),
            },
        });
        let mut scene = build_selection(&board, &Selection::default(), || false).unwrap();
        let scene = Arc::get_mut(&mut scene).unwrap();
        let mut mesh = Geometry::default();
        mesh.triangle(
            Vec3::new(1., -1., 1.),
            Vec3::new(3., -1., 1.),
            Vec3::new(2., -3., 2.),
            [1.; 3],
        );
        scene.meshes.push(mesh);
        scene.instances.push(Instance {
            mesh: 0,
            transform: Mat4::IDENTITY.to_cols_array(),
            reference: "U1".into(),
        });
        assert!(
            pick(scene, &board, [2., -2., 10.], [0., 0., -1.], false)
                .references
                .contains("U1")
        );
    }
}
