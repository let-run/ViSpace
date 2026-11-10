//! Outer copper is unioned as planar polygons before drill subtraction and
//! extrusion. Only true exterior/hole contours create walls, never triangle seams.
use super::{Geometry, Point};
use glam::{Vec2, Vec3};
use lyon_tessellation::{
    BuffersBuilder, LineCap, LineJoin, StrokeOptions, StrokeTessellator, StrokeVertex,
    VertexBuffers,
    path::{Path, math::point},
};

pub(super) fn stroke(
    points: &[Point],
    width: f64,
    z: f32,
    color: [f32; 3],
    bottom: bool,
) -> Result<Geometry, String> {
    let mut mesh = Geometry::default();
    if points.len() < 2 || width <= 0. {
        return Ok(mesh);
    }
    let mut path = Path::builder();
    path.begin(point(points[0].x as f32, -points[0].y as f32));
    for p in &points[1..] {
        path.line_to(point(p.x as f32, -p.y as f32));
    }
    path.end(false);
    let mut output: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    StrokeTessellator::new()
        .tessellate_path(
            &path.build(),
            &StrokeOptions::default()
                .with_line_width(width as f32)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round)
                .with_tolerance(0.005),
            &mut BuffersBuilder::new(&mut output, |v: StrokeVertex| v.position().to_array()),
        )
        .map_err(|e| format!("Copper trace: {e:?}"))?;
    for t in output.indices.chunks_exact(3) {
        let mut p = [t[0], t[1], t[2]].map(|i| {
            let p = output.vertices[i as usize];
            Vec3::new(p[0], p[1], z)
        });
        if ((p[1] - p[0]).cross(p[2] - p[0]).z < 0.) != bottom {
            p.swap(1, 2);
        }
        mesh.triangle(p[0], p[1], p[2], color);
    }
    Ok(mesh)
}
/// Union the planar triangle coverage, then close it into a 35 µm copper sheet.
/// This removes overlapping trace/pour faces and avoids exporting internal walls.
pub(super) fn solid(
    source: &Geometry,
    z0: f32,
    z1: f32,
    drills: &[Vec<Point>],
) -> Result<Geometry, String> {
    let snap = |v: f32| (v as f64 * 1e5).round() as i64;
    let mut seen = std::collections::BTreeSet::new();
    let mut boundary = std::collections::BTreeMap::new();
    for triangle in source.vertices.chunks_exact(3) {
        let mut p = [triangle[0], triangle[1], triangle[2]]
            .map(|v| (snap(v.position[0]), snap(v.position[1])));
        let mut sorted = p;
        sorted.sort();
        if sorted[0] == sorted[1] || sorted[1] == sorted[2] || !seen.insert(sorted) {
            continue;
        }
        let cross = (p[1].0 - p[0].0) as i128 * (p[2].1 - p[0].1) as i128
            - (p[1].1 - p[0].1) as i128 * (p[2].0 - p[0].0) as i128;
        if cross == 0 {
            continue;
        }
        if cross < 0 {
            p.swap(1, 2);
        }
        for i in 0..3 {
            let a = p[i];
            let b = p[(i + 1) % 3];
            let edge = if a < b { (a, b) } else { (b, a) };
            *boundary.entry(edge).or_insert(0_i32) += if a < b { 1 } else { -1 };
        }
    }
    // Cancel internal triangulation edges before the union sweep. Feeding all
    // triangles directly is much more expensive on boards with large pours.
    let mut next: std::collections::BTreeMap<_, Vec<_>> = std::collections::BTreeMap::new();
    for ((a, b), count) in boundary {
        let (a, b) = if count > 0 { (a, b) } else { (b, a) };
        for _ in 0..count.unsigned_abs() {
            next.entry(a).or_default().push(b);
        }
    }
    let mut contours: Vec<Vec<[f64; 2]>> = Vec::new();
    while let Some((&start, _)) = next.first_key_value() {
        let p = |p: (i64, i64)| [p.0 as f64 / 1e5, p.1 as f64 / 1e5];
        let mut contour = vec![p(start)];
        let mut current = start;
        loop {
            let destinations = next
                .get_mut(&current)
                .ok_or("Copper coverage has an open boundary")?;
            let target = destinations
                .pop()
                .ok_or("Copper coverage has an empty boundary")?;
            if destinations.is_empty() {
                next.remove(&current);
            }
            current = target;
            if current == start {
                break;
            }
            contour.push(p(current));
        }
        if contour.len() >= 3 {
            contours.push(contour);
        }
    }
    use i_overlay::{
        core::{fill_rule::FillRule, overlay_rule::OverlayRule},
        float::single::SingleFloatOverlay,
    };
    let holes: Vec<Vec<[f64; 2]>> = drills
        .iter()
        .map(|h| h.iter().map(|p| [p.x, -p.y]).collect())
        .collect();
    // Subtract drills once, after the union. Clipping individual triangles first
    // creates microscopic seams where their independently rounded cuts meet.
    let shapes = contours.overlay_as::<i64>(&holes, OverlayRule::Difference, FillRule::NonZero);
    let mut result = Geometry::default();
    let color = [0.065, 0.29, 0.145];
    for shape in shapes {
        let mut coordinates = Vec::new();
        let mut holes = Vec::new();
        for (index, contour) in shape.iter().enumerate() {
            if contour.len() < 3 {
                continue;
            }
            if index > 0 {
                holes.push(coordinates.len() / 2);
            }
            for p in contour {
                coordinates.extend_from_slice(p);
            }
            // Only the boolean union's actual outline and hole contours get walls.
            // Triangulation edges (including T-junctions) are never extruded.
            for i in 0..contour.len() {
                let a = contour[i];
                let b = contour[(i + 1) % contour.len()];
                result.quad(
                    Vec3::new(a[0] as f32, a[1] as f32, z0),
                    Vec3::new(b[0] as f32, b[1] as f32, z0),
                    Vec3::new(b[0] as f32, b[1] as f32, z1),
                    Vec3::new(a[0] as f32, a[1] as f32, z1),
                    color,
                );
            }
        }
        let triangles = earcutr::earcut(&coordinates, &holes, 2)
            .map_err(|e| format!("Copper face triangulation: {e:?}"))?;
        for t in triangles.chunks_exact(3) {
            let mut p = [t[0], t[1], t[2]]
                .map(|i| Vec2::new(coordinates[2 * i] as f32, coordinates[2 * i + 1] as f32));
            let cross = (p[1] - p[0]).perp_dot(p[2] - p[0]);
            if cross.abs() < 1e-12 {
                continue;
            }
            if cross < 0. {
                p.swap(1, 2);
            }
            let top = p.map(|p| Vec3::new(p.x, p.y, z1));
            let bottom = p.map(|p| Vec3::new(p.x, p.y, z0));
            result.triangle(top[0], top[1], top[2], color);
            result.triangle(bottom[0], bottom[2], bottom[1], color);
        }
    }
    // Retain indexed vertices rather than three copies for every triangle.
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut shared = std::collections::HashMap::new();
    for vertex in result.vertices {
        let key = [
            vertex.position[0].to_bits(),
            vertex.position[1].to_bits(),
            vertex.position[2].to_bits(),
            vertex.normal[0].to_bits(),
            vertex.normal[1].to_bits(),
            vertex.normal[2].to_bits(),
        ];
        let index = *shared.entry(key).or_insert_with(|| {
            let i = vertices.len() as u32;
            vertices.push(vertex);
            i
        });
        indices.push(index);
    }
    Ok(Geometry { vertices, indices })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn triangulation_t_junctions_never_create_internal_copper_walls() {
        let mut coverage = Geometry::default();
        let p = |x, y| Vec3::new(x, y, 0.);
        let color = [0.065, 0.29, 0.145];
        coverage.triangle(p(-2., -2.), p(2., -2.), p(2., 2.), color);
        coverage.triangle(p(-2., -2.), p(0., 0.), p(-2., 2.), color);
        coverage.triangle(p(0., 0.), p(2., 2.), p(-2., 2.), color);
        let solid = solid(&coverage, 0., 0.035, &[]).unwrap();
        let mut wall_area = 0.;
        for t in solid.indices.chunks_exact(3) {
            let v = [t[0], t[1], t[2]].map(|i| solid.vertices[i as usize]);
            if v[0].normal[2].abs() > 0.1 {
                continue;
            }
            assert!(
                v.iter().all(|v| (v.position[0].abs() - 2.).abs() < 1e-5
                    || (v.position[1].abs() - 2.).abs() < 1e-5),
                "wall inside copper union"
            );
            let p = v.map(|v| Vec3::from_array(v.position));
            wall_area += (p[1] - p[0]).cross(p[2] - p[0]).length() * 0.5;
        }
        assert!(
            (wall_area - 16. * 0.035).abs() < 0.0001,
            "only the outer perimeter may be extruded"
        );
    }
    #[test]
    fn overlapping_copper_is_unioned_into_a_closed_solid_with_drill_walls() {
        let color = [0.065, 0.29, 0.145];
        let mut coverage = stroke(
            &[Point::new(-2., 0.), Point::new(2., 0.)],
            2.,
            0.,
            color,
            false,
        )
        .unwrap();
        let duplicate = coverage.clone();
        coverage.vertices.extend(duplicate.vertices);
        let solid = solid(
            &coverage,
            0.,
            0.035,
            &[super::super::circle(Point::new(0., 0.), 0.5)],
        )
        .unwrap();
        let volume: f32 = solid
            .indices
            .chunks_exact(3)
            .map(|t| {
                let p = [t[0], t[1], t[2]]
                    .map(|i| Vec3::from_array(solid.vertices[i as usize].position));
                p[0].dot(p[1].cross(p[2])) / 6.
            })
            .sum();
        let expected = (8. + std::f32::consts::PI * 0.75) * 0.035;
        assert!(
            (volume - expected).abs() < 0.004,
            "overlaps must not double the volume: {volume} vs {expected}"
        );
        let key = |p: [f32; 3]| p.map(|v| (v as f64 * 1e5).round() as i64);
        let mut edges = std::collections::BTreeMap::new();
        for t in solid.indices.chunks_exact(3) {
            for i in 0..3 {
                let a = key(solid.vertices[t[i] as usize].position);
                let b = key(solid.vertices[t[(i + 1) % 3] as usize].position);
                let edge = if a < b { (a, b) } else { (b, a) };
                *edges.entry(edge).or_insert(0) += 1;
            }
        }
        assert!(
            edges.values().all(|count| *count == 2),
            "solid must have no open boundary edges: {:?}",
            edges
                .iter()
                .filter(|(_, c)| **c != 2)
                .take(12)
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn routed_copper_has_round_caps_and_outward_normals() {
        let source = stroke(
            &[Point::new(-2., 0.), Point::new(2., 0.)],
            2.,
            1.,
            [0.065, 0.29, 0.145],
            false,
        )
        .unwrap();
        let area = |mesh: &Geometry| {
            mesh.vertices
                .chunks_exact(3)
                .map(|t| {
                    let p = [t[0], t[1], t[2]].map(|v| Vec3::from_array(v.position));
                    (p[1] - p[0]).cross(p[2] - p[0]).length() / 2.
                })
                .sum::<f32>()
        };
        assert!((area(&source) - (8. + std::f32::consts::PI)).abs() < 0.08);
        assert!(source.vertices.iter().all(|v| v.normal[2] > 0.99));
        let bottom = stroke(
            &[Point::new(-2., 0.), Point::new(2., 0.)],
            2.,
            -1.,
            [0.065, 0.29, 0.145],
            true,
        )
        .unwrap();
        assert!(bottom.vertices.iter().all(|v| v.normal[2] < -0.99));
        assert!((area(&bottom) - area(&source)).abs() < 0.001);
    }
}
