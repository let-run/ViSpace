//! Hybrid CAD text: reusable bitmap glyphs below 32 points, cached outline
//! triangles above it. All curves are tessellated once per glyph, never per zoom.
use ab_glyph::{Font, FontArc, OutlineCurve};
use eframe::egui::{self, Color32};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers,
    path::{Path, PathEvent, iterator::PathIterator, math::point},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
};

const VECTOR_THRESHOLD: f32 = 32.;
const TOLERANCE: f32 = 1. / 4096.;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) struct OutlineVertex {
    pub position: egui::Pos2,
    /// Screen-space AA extrusion. Zero for fill and inner fringe vertices.
    pub normal: egui::Vec2,
    pub outer: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct GlyphGeometry {
    pub vertices: Vec<OutlineVertex>,
    pub indices: Vec<u32>,
    pub bottom: f32,
}
struct VectorCache {
    font: FontArc,
    glyphs: BTreeMap<char, Option<Arc<GlyphGeometry>>>,
    bytes: usize,
    fallbacks: Vec<(FontArc, f32)>,
    fallback_glyphs: BTreeMap<char, Option<Arc<GlyphGeometry>>>,
}
impl VectorCache {
    fn new() -> Self {
        Self {
            font: FontArc::try_from_slice(epaint_default_fonts::HACK_REGULAR)
                .expect("bundled Hack font"),
            glyphs: BTreeMap::new(),
            bytes: 0,
            fallbacks: [
                (epaint_default_fonts::UBUNTU_LIGHT, 1.),
                (epaint_default_fonts::NOTO_EMOJI_REGULAR, 0.81),
                (epaint_default_fonts::EMOJI_ICON, 0.9),
            ]
            .into_iter()
            .map(|(bytes, scale)| {
                (
                    FontArc::try_from_slice(bytes).expect("bundled fallback font"),
                    scale,
                )
            })
            .collect(),
            fallback_glyphs: BTreeMap::new(),
        }
    }
    fn glyph(&mut self, ch: char) -> Option<Arc<GlyphGeometry>> {
        if let Some(g) = self.glyphs.get(&ch) {
            return g.clone();
        }
        let id = self.font.glyph_id(ch);
        let geometry = (id.0 != 0)
            .then(|| self.font.outline(id))
            .flatten()
            .and_then(|outline| {
                build_glyph(&outline.curves, self.font.units_per_em().unwrap_or(2048.))
            })
            .map(Arc::new);
        // Bound growth from arbitrary imported Unicode text. Existing Arc users stay valid.
        let bytes = geometry.as_ref().map_or(0, |g| {
            g.vertices.len() * std::mem::size_of::<OutlineVertex>()
                + g.indices.len() * std::mem::size_of::<u32>()
        });
        if self.glyphs.len() >= 4096 || self.bytes + bytes > 32 * 1024 * 1024 {
            self.glyphs.clear();
            self.bytes = 0;
        }
        self.bytes += bytes;
        self.glyphs.insert(ch, geometry.clone());
        geometry
    }
    fn vector_fallback(&mut self, ch: char) -> Option<Arc<GlyphGeometry>> {
        if let Some(g) = self.fallback_glyphs.get(&ch) {
            return g.clone();
        }
        let result = self.fallbacks.iter().find_map(|(font, scale)| {
            let id = font.glyph_id(ch);
            if id.0 == 0 {
                return None;
            }
            font.outline(id)
                .and_then(|o| build_glyph(&o.curves, font.units_per_em().unwrap_or(2048.) / scale))
                .map(Arc::new)
        });
        if self.fallback_glyphs.len() >= 512 {
            self.fallback_glyphs.clear();
        }
        self.fallback_glyphs.insert(ch, result.clone());
        result
    }
}
fn cache() -> &'static Mutex<VectorCache> {
    static CACHE: OnceLock<Mutex<VectorCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VectorCache::new()))
}

fn build_glyph(curves: &[OutlineCurve], em: f32) -> Option<GlyphGeometry> {
    let p = |p: ab_glyph::Point| point(p.x / em, -p.y / em);
    let mut builder = Path::builder();
    let mut current = None;
    let mut start = None;
    for curve in curves {
        let (a, b) = match *curve {
            OutlineCurve::Line(a, b)
            | OutlineCurve::Quad(a, _, b)
            | OutlineCurve::Cubic(a, _, _, b) => (p(a), p(b)),
        };
        if current != Some(a) {
            if current.is_some() {
                builder.end(true);
            }
            builder.begin(a);
            start = Some(a);
        }
        match *curve {
            OutlineCurve::Line(_, _) => {
                builder.line_to(b);
            }
            OutlineCurve::Quad(_, c, _) => {
                builder.quadratic_bezier_to(p(c), b);
            }
            OutlineCurve::Cubic(_, c, d, _) => {
                builder.cubic_bezier_to(p(c), p(d), b);
            }
        }
        if Some(b) == start {
            builder.end(true);
            current = None;
            start = None;
        } else {
            current = Some(b);
        }
    }
    if current.is_some() {
        builder.end(true);
    }
    let path = builder.build();
    let mut buffers: VertexBuffers<egui::Pos2, u32> = VertexBuffers::new();
    FillTessellator::new()
        .tessellate_path(
            &path,
            &FillOptions::non_zero().with_tolerance(TOLERANCE),
            &mut BuffersBuilder::new(&mut buffers, |v: FillVertex<'_>| {
                egui::pos2(v.position().x, v.position().y)
            }),
        )
        .ok()?;
    let mut vertices: Vec<_> = buffers
        .vertices
        .into_iter()
        .map(|position| OutlineVertex {
            position,
            normal: egui::Vec2::ZERO,
            outer: false,
        })
        .collect();
    let mut indices = buffers.indices;
    let bottom = vertices
        .iter()
        .map(|v| v.position.y)
        .fold(f32::NEG_INFINITY, f32::max);

    // A one-physical-pixel transparent fringe gives vector edges the same smooth
    // coverage as egui text without requiring MSAA or another GPU pipeline.
    let mut contours = Vec::<Vec<egui::Pos2>>::new();
    let mut contour = Vec::new();
    for event in path.iter().flattened(TOLERANCE) {
        match event {
            PathEvent::Begin { at } => {
                contour.clear();
                contour.push(egui::pos2(at.x, at.y));
            }
            PathEvent::Line { to, .. } => contour.push(egui::pos2(to.x, to.y)),
            PathEvent::End { .. } => {
                if contour.first() == contour.last() {
                    contour.pop();
                }
                if contour.len() >= 3 {
                    contours.push(std::mem::take(&mut contour));
                }
            }
            _ => unreachable!("flattened path"),
        }
    }
    let area = |c: &[egui::Pos2]| {
        c.iter()
            .zip(c.iter().cycle().skip(1))
            .take(c.len())
            .map(|(a, b)| a.x * b.y - b.x * a.y)
            .sum::<f32>()
    };
    let orientation = contours
        .iter()
        .map(|c| area(c))
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap_or(1.)
        .signum();
    for c in contours {
        let base = vertices.len() as u32;
        for i in 0..c.len() {
            let a = c[i] - c[(i + c.len() - 1) % c.len()];
            let b = c[(i + 1) % c.len()] - c[i];
            let outward = |d: egui::Vec2| egui::vec2(d.y, -d.x).normalized() * orientation;
            let (na, nb) = (outward(a), outward(b));
            let bisector = (na + nb).normalized();
            let normal = bisector / bisector.dot(nb).max(0.33);
            vertices.push(OutlineVertex {
                position: c[i],
                normal: egui::Vec2::ZERO,
                outer: false,
            });
            vertices.push(OutlineVertex {
                position: c[i],
                normal,
                outer: true,
            });
        }
        for i in 0..c.len() as u32 {
            let a = base + i * 2;
            let b = base + ((i + 1) % c.len() as u32) * 2;
            indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    Some(GlyphGeometry {
        vertices,
        indices,
        bottom,
    })
}

pub(super) fn zoom_galley(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    raster_size: f32,
    color: Color32,
) -> Arc<egui::Galley> {
    let mut galley =
        painter.layout_no_wrap(text.into(), egui::FontId::monospace(raster_size), color);
    prepare_galley(
        &mut galley,
        size / raster_size,
        painter.ctx().pixels_per_point(),
    );
    super::cad_gpu::tag_text(&galley, size, 0., f32::INFINITY);
    galley
}

/// Preserve egui's wrapping, alignment and fallback font layout. Replace only the
/// glyph visuals; unsupported characters retain their cached bitmap quads.
pub(super) fn prepare_galley(galley: &mut Arc<egui::Galley>, scale: f32, pixels_per_point: f32) {
    let size = galley
        .job
        .sections
        .first()
        .map_or(0., |s| s.format.font_id.size)
        * scale;
    let italic = galley
        .job
        .sections
        .first()
        .is_some_and(|s| s.format.italics);
    scale_galley(galley, scale);
    let capturing = super::cad_gpu::capturing();
    if capturing {
        // Retained scenes instance shared glyph meshes; do not expand a mesh per letter.
        super::cad_gpu::tag_text(galley, size, 0., f32::INFINITY);
        return;
    }
    if size < VECTOR_THRESHOLD {
        return;
    }
    let mut cache = cache().lock().unwrap_or_else(|p| p.into_inner());
    let g = Arc::make_mut(galley);
    g.mesh_bounds = egui::Rect::NOTHING;
    g.num_vertices = 0;
    g.num_indices = 0;
    let fringe = 0.75 / pixels_per_point;
    for placed in &mut g.rows {
        let row = Arc::make_mut(&mut placed.row);
        let original = &row.visuals.mesh;
        let mut mesh = egui::Mesh::default();
        for glyph in &row.glyphs {
            if glyph.uv_rect.is_nothing() {
                continue;
            }
            let first = glyph.first_vertex as usize;
            let color = original.vertices[first].color;
            let base = mesh.vertices.len() as u32;
            let geometry = cache.glyph(glyph.chr).or_else(|| {
                capturing
                    .then(|| {
                        cache
                            .vector_fallback(glyph.chr)
                            .or_else(|| cache.glyph('?'))
                    })
                    .flatten()
            });
            if let Some(geometry) = geometry {
                for v in &geometry.vertices {
                    let mut offset = v.position.to_vec2() * size
                        + if capturing {
                            egui::Vec2::ZERO
                        } else {
                            v.normal * fringe
                        };
                    if italic {
                        offset.x += (geometry.bottom * size - offset.y) * 0.25;
                    }
                    mesh.colored_vertex(
                        glyph.pos + offset,
                        if v.outer { Color32::TRANSPARENT } else { color },
                    );
                    if capturing {
                        let mut normal = v.normal * 0.75;
                        if italic {
                            normal.x -= normal.y * 0.25;
                        }
                        mesh.vertices.last_mut().unwrap().uv = egui::pos2(normal.x, normal.y);
                    }
                }
                mesh.indices
                    .extend(geometry.indices.iter().map(|i| i + base));
            } else {
                // Emoji, symbols or future custom fallback fonts use egui's atlas.
                mesh.vertices
                    .extend_from_slice(&original.vertices[first..first + 4]);
                mesh.indices.extend_from_slice(&[
                    base,
                    base + 1,
                    base + 2,
                    base + 2,
                    base + 1,
                    base + 3,
                ]);
            }
        }
        let bounds =
            egui::Rect::from_points(&mesh.vertices.iter().map(|v| v.pos).collect::<Vec<_>>());
        row.visuals.mesh_bounds = bounds;
        row.visuals.glyph_vertex_range = 0..mesh.vertices.len();
        row.visuals.glyph_index_start = 0;
        row.visuals.mesh = mesh;
        g.mesh_bounds = g.mesh_bounds.union(bounds.translate(placed.pos.to_vec2()));
        g.num_vertices += row.visuals.mesh.vertices.len();
        g.num_indices += row.visuals.mesh.indices.len();
    }
}

fn scale_galley(galley: &mut Arc<egui::Galley>, scale: f32) {
    let g = Arc::make_mut(galley);
    g.rect = g.rect * scale;
    g.mesh_bounds = g.mesh_bounds * scale;
    for placed in &mut g.rows {
        placed.pos *= scale;
        let row = Arc::make_mut(&mut placed.row);
        row.size *= scale;
        row.visuals.mesh_bounds = row.visuals.mesh_bounds * scale;
        for glyph in &mut row.glyphs {
            glyph.pos *= scale;
        }
        for vertex in &mut row.visuals.mesh.vertices {
            vertex.pos *= scale;
        }
    }
}

pub(super) fn retained_glyph(ch: char) -> Option<Arc<GlyphGeometry>> {
    let mut cache = cache().lock().unwrap_or_else(|p| p.into_inner());
    cache
        .glyph(ch)
        .or_else(|| cache.vector_fallback(ch))
        .or_else(|| cache.glyph('?'))
}

/// Captured geometry must retain world dimensions rather than freeze a screen cap.
pub(super) fn limited_size(size: f64, min: f64, max: f64) -> f64 {
    if super::cad_gpu::capturing() {
        size
    } else {
        size.clamp(min, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inside(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2, c: egui::Pos2) -> bool {
        let cross = |a: egui::Pos2, b: egui::Pos2, p: egui::Pos2| {
            (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
        };
        let area = cross(a, b, c);
        if area.abs() < 1e-10 {
            return false;
        }
        let sign = area.signum();
        cross(a, b, p) * sign >= 0. && cross(b, c, p) * sign >= 0. && cross(c, a, p) * sign >= 0.
    }
    #[test]
    fn vector_counters_and_curves_match_the_bundled_font_and_geometry_is_reused() {
        let mut cache = VectorCache::new();
        for ch in "BO8Ωµ".chars() {
            let geometry = cache.glyph(ch).unwrap();
            assert!(Arc::ptr_eq(&geometry, &cache.glyph(ch).unwrap()));
            let font = &cache.font;
            let pixels = 96.;
            let glyph = font
                .glyph_id(ch)
                .with_scale(pixels * font.height_unscaled() / font.units_per_em().unwrap());
            let outlined = font.outline_glyph(glyph).unwrap();
            let bounds = outlined.px_bounds();
            let mut ink = 0;
            let mut mismatches = 0;
            outlined.draw(|x, y, coverage| {
                let p = egui::pos2(
                    (bounds.min.x + x as f32 + 0.5) / pixels,
                    (bounds.min.y + y as f32 + 0.5) / pixels,
                );
                let filled = geometry.indices.chunks_exact(3).any(|t| {
                    let a = geometry.vertices[t[0] as usize];
                    let b = geometry.vertices[t[1] as usize];
                    let c = geometry.vertices[t[2] as usize];
                    !a.outer
                        && !b.outer
                        && !c.outer
                        && inside(p, a.position, b.position, c.position)
                });
                if coverage >= 0.5 {
                    ink += 1;
                }
                if filled != (coverage >= 0.5) {
                    mismatches += 1;
                }
            });
            assert!(
                mismatches < ink / 20 + 4,
                "{ch}: {mismatches} mismatches / {ink} ink pixels"
            );
        }
    }
    #[test]
    fn hybrid_preserves_wrapping_baselines_fallback_and_has_no_zoom_atlas_uploads() {
        let ctx = egui::Context::default();
        let mut small_size = egui::Vec2::ZERO;
        for (frame, size) in [16., 256., 512.].into_iter().enumerate() {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let g = zoom_galley(ui.painter(), "R12 Ω\nGND 😀", size, 128., Color32::BLACK);
                    assert_eq!(g.rows.len(), 2);
                    if frame == 0 {
                        small_size = g.size();
                    } else {
                        assert!((g.size() - small_size * (size / 16.)).length() < 0.01);
                        let verts = &g.rows[0].visuals.mesh.vertices;
                        assert!(verts.iter().any(|v| v.color == Color32::TRANSPARENT));
                        assert!(verts.iter().all(|v| v.uv == egui::Pos2::ZERO));
                        assert!(
                            g.rows[1]
                                .visuals
                                .mesh
                                .vertices
                                .iter()
                                .any(|v| v.uv != egui::Pos2::ZERO),
                            "fallback bitmap must survive"
                        );
                    }
                    ui.painter().add(egui::epaint::TextShape::new(
                        egui::Pos2::ZERO,
                        g,
                        Color32::BLACK,
                    ));
                });
            });
            if frame > 0 {
                assert!(output.textures_delta.set.is_empty());
            }
            std::hint::black_box(ctx.tessellate(output.shapes, output.pixels_per_point));
        }
    }
    #[test]
    #[ignore = "Manual software rendering preview; set CAD_TEXT_PREVIEW"]
    fn render_vector_preview() {
        let path = std::env::var("CAD_TEXT_PREVIEW").expect("CAD_TEXT_PREVIEW output PNG path");
        let ctx = egui::Context::default();
        let mut image = image::RgbaImage::from_pixel(2200, 1000, image::Rgba([255, 255, 245, 255]));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for (size, y) in [(48., 10.), (128., 90.), (512., 270.)] {
                    let galley = zoom_galley(ui.painter(), "R12 Ω GND", size, 128., Color32::BLACK);
                    for row in &galley.rows {
                        let mesh = &row.visuals.mesh;
                        for t in mesh.indices.chunks_exact(3) {
                            let v = [
                                mesh.vertices[t[0] as usize],
                                mesh.vertices[t[1] as usize],
                                mesh.vertices[t[2] as usize],
                            ];
                            let positions =
                                v.map(|v| v.pos + row.pos.to_vec2() + egui::vec2(10., y));
                            let bounds = egui::Rect::from_points(&positions);
                            let [a, b, c] = positions;
                            let area = (b - a).rot90().dot(c - a);
                            if area.abs() < 1e-8 {
                                continue;
                            }
                            for y in (bounds.min.y.floor().max(0.) as u32)
                                ..(bounds.max.y.ceil().min(1000.) as u32)
                            {
                                for x in (bounds.min.x.floor().max(0.) as u32)
                                    ..(bounds.max.x.ceil().min(2200.) as u32)
                                {
                                    let p = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                                    if !inside(p, a, b, c) {
                                        continue;
                                    }
                                    let u = (b - p).rot90().dot(c - p) / area;
                                    let w = (c - p).rot90().dot(a - p) / area;
                                    let z = 1. - u - w;
                                    let alpha = (u * v[0].color.a() as f32
                                        + w * v[1].color.a() as f32
                                        + z * v[2].color.a() as f32)
                                        / 255.;
                                    let pixel = image.get_pixel_mut(x, y);
                                    for c in 0..3 {
                                        pixel[c] = (pixel[c] as f32 * (1. - alpha)) as u8;
                                    }
                                }
                            }
                        }
                    }
                }
            });
        });
        image.save(path).unwrap();
    }
}
