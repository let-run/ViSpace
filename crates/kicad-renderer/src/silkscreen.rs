//! Tessellate bundled font outlines once per glyph during scene preparation.
//! No text rasterization or font atlas updates are needed when orbiting/zooming.
use super::{Geometry, Point};
use ab_glyph::{Font, FontArc, OutlineCurve};
use glam::Vec3;
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers,
    path::{Path, math::point},
};
use std::collections::BTreeMap;
struct Glyph {
    vertices: Vec<[f32; 2]>,
    indices: Vec<u32>,
    advance: f32,
}
pub(super) struct TextGeometry {
    font: FontArc,
    height: f32,
    glyphs: BTreeMap<char, Glyph>,
}
impl TextGeometry {
    pub fn new() -> Self {
        let font =
            FontArc::try_from_slice(epaint_default_fonts::HACK_REGULAR).expect("bundled font");
        // KiCad specifies capital-letter height, rather than typographic em size.
        let height = font
            .outline(font.glyph_id('H'))
            .map(|o| o.bounds.max.y - o.bounds.min.y)
            .unwrap_or(1500.);
        Self {
            font,
            height,
            glyphs: BTreeMap::new(),
        }
    }
    fn glyph(&mut self, ch: char) -> Result<&Glyph, String> {
        if !self.glyphs.contains_key(&ch) {
            let id = self.font.glyph_id(ch);
            let mut buffers: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
            if let Some(outline) = self.font.outline(id) {
                let p = |p: ab_glyph::Point| point(p.x / self.height, -p.y / self.height);
                let mut builder = Path::builder();
                let mut current = None;
                for curve in outline.curves {
                    let a = match curve {
                        OutlineCurve::Line(a, _)
                        | OutlineCurve::Quad(a, _, _)
                        | OutlineCurve::Cubic(a, _, _, _) => p(a),
                    };
                    if current != Some(a) {
                        if current.is_some() {
                            builder.end(true);
                        }
                        builder.begin(a);
                    }
                    let end = match curve {
                        OutlineCurve::Line(_, b) => {
                            let b = p(b);
                            builder.line_to(b);
                            b
                        }
                        OutlineCurve::Quad(_, b, c) => {
                            let c = p(c);
                            builder.quadratic_bezier_to(p(b), c);
                            c
                        }
                        OutlineCurve::Cubic(_, b, c, d) => {
                            let d = p(d);
                            builder.cubic_bezier_to(p(b), p(c), d);
                            d
                        }
                    };
                    current = Some(end);
                }
                if current.is_some() {
                    builder.end(true);
                }
                FillTessellator::new()
                    .tessellate_path(
                        &builder.build(),
                        &FillOptions::non_zero().with_tolerance(0.002),
                        &mut BuffersBuilder::new(&mut buffers, |v: FillVertex| {
                            v.position().to_array()
                        }),
                    )
                    .map_err(|e| format!("Silkscreen glyph: {e:?}"))?;
            }
            self.glyphs.insert(
                ch,
                Glyph {
                    vertices: buffers.vertices,
                    indices: buffers.indices,
                    advance: self.font.h_advance_unscaled(id) / self.height,
                },
            );
        }
        Ok(&self.glyphs[&ch])
    }
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &mut self,
        text: &str,
        at: Point,
        size: f64,
        angle: f64,
        mirrored: bool,
        z: f32,
        bottom: bool,
        mesh: &mut Geometry,
    ) -> Result<(), String> {
        if !size.is_finite() || size <= 0. {
            return Ok(());
        }
        let lines: Vec<_> = text.lines().collect();
        let (sin, cos) = angle.to_radians().sin_cos();
        for (line_index, line) in lines.iter().enumerate() {
            let mut width = 0.;
            for ch in line.chars() {
                width += self.glyph(ch)?.advance;
            }
            let mut x = -width / 2.;
            let y = (lines.len().saturating_sub(1) as f32 / 2. - line_index as f32) * 1.5 - 0.5;
            for ch in line.chars() {
                let glyph = self.glyph(ch)?;
                for t in glyph.indices.chunks_exact(3) {
                    let mut points = [t[0], t[1], t[2]].map(|i| {
                        let p = glyph.vertices[i as usize];
                        let dx = (p[0] + x) as f64 * size * if mirrored { -1. } else { 1. };
                        let dy = (p[1] + y) as f64 * size;
                        Vec3::new(
                            (at.x + cos * dx - sin * dy) as f32,
                            (-at.y + sin * dx + cos * dy) as f32,
                            z,
                        )
                    });
                    // Keep outward normals for both surfaces, including mirrored text.
                    let normal = (points[1] - points[0]).cross(points[2] - points[0]).z;
                    if (normal < 0.) != bottom {
                        points.swap(1, 2);
                    }
                    mesh.triangle(points[0], points[1], points[2], [0.86, 0.86, 0.78]);
                }
                x += glyph.advance;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_is_cached_centered_mirrored_and_faces_outward_on_both_sides() {
        let mut text = TextGeometry::new();
        let mut top = Geometry::default();
        let mut bottom = Geometry::default();
        text.append("H8", Point::new(4., 5.), 1., 0., false, 1., false, &mut top)
            .unwrap();
        let count = text.glyphs.len();
        text.append(
            "H8",
            Point::new(4., 5.),
            1.,
            0.,
            true,
            -1.,
            true,
            &mut bottom,
        )
        .unwrap();
        assert_eq!(count, text.glyphs.len());
        assert!(!top.vertices.is_empty());
        assert_eq!(top.vertices.len(), bottom.vertices.len());
        let bounds = |mesh: &Geometry| {
            mesh.vertices.iter().fold(
                (
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                ),
                |(x0, x1, y0, y1), v| {
                    (
                        x0.min(v.position[0]),
                        x1.max(v.position[0]),
                        y0.min(v.position[1]),
                        y1.max(v.position[1]),
                    )
                },
            )
        };
        let a = bounds(&top);
        let b = bounds(&bottom);
        assert!((a.0 + b.1 - 8.).abs() < 0.001);
        assert!((a.1 + b.0 - 8.).abs() < 0.001);
        assert!(
            (a.2 + a.3 + 10.).abs() < 0.05,
            "capital height must be centered: {a:?}"
        );
        assert!(top.vertices.iter().all(|v| v.normal[2] > 0.99));
        assert!(bottom.vertices.iter().all(|v| v.normal[2] < -0.99));
        let mut rotated = Geometry::default();
        text.append(
            "H8",
            Point::new(4., 5.),
            1.,
            90.,
            false,
            1.,
            false,
            &mut rotated,
        )
        .unwrap();
        let r = bounds(&rotated);
        assert!(((r.3 - r.2) - (a.1 - a.0)).abs() < 0.001);
    }
}
