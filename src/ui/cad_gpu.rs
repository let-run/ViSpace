//! Retained 2D CAD scenes shared by PCB and schematic viewers. Camera-only frames
//! never tessellate or upload geometry; identical frames only blit the last image.
use super::*;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor, wgpu};
use std::{cell::RefCell, collections::HashMap, sync::Arc};
use wgpu::util::DeviceExt;

pub(super) const REFERENCE: f32 = 64.;
#[derive(Clone, Copy)]
struct TextRule {
    font: f32,
    min: f32,
    max: f32,
    mirrored: bool,
}
thread_local! { static CAPTURE: RefCell<Option<HashMap<usize,TextRule>>> = const { RefCell::new(None) }; }
thread_local! {static CANCEL:RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>=const{RefCell::new(None)};}
pub(super) fn with_cancel<T>(
    token: Arc<std::sync::atomic::AtomicBool>,
    work: impl FnOnce() -> T,
) -> T {
    CANCEL.with(|c| *c.borrow_mut() = Some(token));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CANCEL.with(|c| *c.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    work()
}
pub(super) fn cancelled() -> bool {
    CANCEL.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    })
}
pub(super) fn capturing() -> bool {
    CAPTURE.with(|c| c.borrow().is_some())
}
pub(super) fn tag_text(g: &Arc<egui::Galley>, font: f32, min: f32, max: f32) {
    CAPTURE.with(|c| {
        if let Some(c) = c.borrow_mut().as_mut() {
            let key = Arc::as_ptr(g) as usize;
            let mirrored = c.get(&key).is_some_and(|r| r.mirrored);
            c.insert(
                key,
                TextRule {
                    font,
                    min,
                    max,
                    mirrored,
                },
            );
        }
    });
}
pub(super) fn tag_mirror(g: &Arc<egui::Galley>) {
    CAPTURE.with(|c| {
        if let Some(rule) = c
            .borrow_mut()
            .as_mut()
            .and_then(|c| c.get_mut(&(Arc::as_ptr(g) as usize)))
        {
            rule.mirrored = true;
        }
    });
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct GlyphInstance {
    origin: egui::Pos2,
    basis: egui::Vec2,
    anchor: egui::Pos2,
    min: f32,
    #[serde(with = "float_bits")]
    max: f32,
    color: Color32,
    tint: f32,
    style: u32,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct GlyphGroup {
    geometry: Arc<super::cad_text::GlyphGeometry>,
    instances: Vec<GlyphInstance>,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Vertex {
    p: egui::Pos2,
    color: Color32,
    anchor: egui::Pos2,
    min: f32,
    #[serde(with = "float_bits")]
    max: f32,
    offset: egui::Vec2,
    tint: f32,
    uv: egui::Pos2,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Batch {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    #[serde(with = "rect_bits")]
    clip: egui::Rect,
    #[serde(with = "image_bytes")]
    image: Option<([u32; 2], Arc<Vec<u8>>)>,
    glyphs: std::collections::BTreeMap<char, GlyphGroup>,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Scene {
    #[serde(skip, default = "next_scene_id")]
    id: usize,
    #[serde(skip)]
    ready: std::sync::atomic::AtomicBool,
    groups: Vec<(bool, Vec<Arc<Batch>>)>,
}
mod float_bits {
    use serde::{Deserialize, Serialize};
    pub fn serialize<S: serde::Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
        v.to_bits().serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(f32::from_bits(u32::deserialize(d)?))
    }
}
mod rect_bits {
    use eframe::egui;
    use serde::{Deserialize, Serialize};
    pub fn serialize<S: serde::Serializer>(v: &egui::Rect, s: S) -> Result<S::Ok, S::Error> {
        [
            v.min.x.to_bits(),
            v.min.y.to_bits(),
            v.max.x.to_bits(),
            v.max.y.to_bits(),
        ]
        .serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<egui::Rect, D::Error> {
        let v = <[u32; 4]>::deserialize(d)?;
        Ok(egui::Rect::from_min_max(
            egui::pos2(f32::from_bits(v[0]), f32::from_bits(v[1])),
            egui::pos2(f32::from_bits(v[2]), f32::from_bits(v[3])),
        ))
    }
}
mod image_bytes {
    use base64::Engine;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    type Image = Option<([u32; 2], Arc<Vec<u8>>)>;
    pub fn serialize<S: serde::Serializer>(v: &Image, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref()
            .map(|(size, bytes)| {
                (
                    *size,
                    base64::engine::general_purpose::STANDARD.encode(bytes.as_slice()),
                )
            })
            .serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Image, D::Error> {
        Option::<([u32; 2], String)>::deserialize(d)?
            .map(|(size, text)| {
                base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .map(|bytes| (size, Arc::new(bytes)))
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }
}
impl Scene {
    #[cfg(any(target_arch = "wasm32", test))]
    pub(super) fn share_glyphs(&mut self) {
        let mut shared = HashMap::new();
        for (_, batches) in &mut self.groups {
            for batch in batches {
                if let Some(batch) = Arc::get_mut(batch) {
                    for (&ch, group) in &mut batch.glyphs {
                        group.geometry = shared
                            .entry(ch)
                            .or_insert_with(|| group.geometry.clone())
                            .clone();
                    }
                }
            }
        }
    }
    #[cfg(test)]
    pub(super) fn profile(&self) -> (usize, usize, usize) {
        let mut letters = 0;
        let mut expanded = 0;
        for (_, batches) in &self.groups {
            for b in batches {
                for g in b.glyphs.values() {
                    letters += g.instances.len();
                    expanded += g.instances.len()
                        * (g.geometry.vertices.len() * 48 + g.geometry.indices.len() * 4);
                }
            }
        }
        (self.resident_bytes(), letters, expanded)
    }
    pub(super) fn resident_bytes(&self) -> usize {
        let mut glyphs = std::collections::HashSet::new();
        let mut bytes = 0;
        for (_, batches) in &self.groups {
            for batch in batches {
                bytes += batch.vertices.capacity() * std::mem::size_of::<Vertex>()
                    + batch.indices.capacity() * 4
                    + batch.image.as_ref().map_or(0, |i| i.1.capacity());
                for group in batch.glyphs.values() {
                    bytes += group.instances.capacity() * std::mem::size_of::<GlyphInstance>();
                    if glyphs.insert(Arc::as_ptr(&group.geometry) as usize) {
                        bytes += group.geometry.vertices.len()
                            * std::mem::size_of::<super::cad_text::OutlineVertex>()
                            + group.geometry.indices.len() * 4;
                    }
                }
            }
        }
        bytes
    }
}
fn next_scene_id() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
#[derive(Default)]
pub(super) struct Builder {
    groups: Vec<(bool, Vec<Arc<Batch>>)>,
}
impl Builder {
    pub fn finish(self) -> Arc<Scene> {
        Arc::new(Scene {
            id: next_scene_id(),
            ready: std::sync::atomic::AtomicBool::new(false),
            groups: self.groups,
        })
    }
    pub fn mesh_layer(&mut self, meshes: &[egui::Mesh]) {
        let mut batches = vec![];
        let mut batch = Batch::empty(egui::Rect::EVERYTHING);
        for mesh in meshes {
            let mut remap = HashMap::<u32, u32>::new();
            for triangle in mesh.indices.chunks_exact(3) {
                if cancelled() {
                    return;
                }
                if batch.vertices.len() + 3 > 65_536 {
                    batches.push(Arc::new(batch));
                    batch = Batch::empty(egui::Rect::EVERYTHING);
                    remap.clear();
                }
                for &index in triangle {
                    let mapped = *remap.entry(index).or_insert_with(|| {
                        let v = &mesh.vertices[index as usize];
                        let mapped = batch.vertices.len() as u32;
                        batch.vertices.push(Vertex::plain(v.pos, v.color));
                        mapped
                    });
                    batch.indices.push(mapped);
                }
            }
        }
        if !batch.indices.is_empty() {
            batches.push(Arc::new(batch));
        }
        if !batches.is_empty() {
            self.groups.push((true, batches));
        }
    }
    fn push(&mut self, batch: Batch) {
        if batch.indices.is_empty() && batch.glyphs.is_empty() {
            return;
        }
        if self.groups.last().is_none_or(|g| g.0) {
            self.groups.push((false, vec![]));
        }
        let list = &mut self.groups.last_mut().unwrap().1;
        // Merge adjacent untextured/unclipped draws, keeping painter order.
        if let Some(last) = list.last_mut()
            && last.image.is_none()
            && batch.image.is_none()
            && last.clip == batch.clip
            && last.vertices.len() + batch.vertices.len() <= 65_536
            && let Some(last) = Arc::get_mut(last)
        {
            let base = last.vertices.len() as u32;
            last.vertices.extend(batch.vertices);
            for (ch, group) in batch.glyphs {
                last.glyphs
                    .entry(ch)
                    .or_insert_with(|| GlyphGroup {
                        geometry: group.geometry.clone(),
                        instances: vec![],
                    })
                    .instances
                    .extend(group.instances);
            }
            last.indices
                .extend(batch.indices.into_iter().map(|i| i + base));
        } else {
            list.push(Arc::new(batch));
        }
    }
    pub fn capture(&mut self, ui: &egui::Ui, tint: bool, draw: impl FnOnce(&egui::Painter)) {
        self.capture_impl(ui, tint, false, draw);
    }
    pub fn capture_schematic(
        &mut self,
        ui: &egui::Ui,
        tint: bool,
        draw: impl FnOnce(&egui::Painter),
    ) {
        self.capture_impl(ui, tint, true, draw);
    }
    fn capture_impl(
        &mut self,
        ui: &egui::Ui,
        tint: bool,
        schematic: bool,
        draw: impl FnOnce(&egui::Painter),
    ) {
        let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::new("cad scene capture"));
        let painter = egui::Painter::new(ui.ctx().clone(), layer, egui::Rect::EVERYTHING);
        CAPTURE.with(|c| *c.borrow_mut() = Some(HashMap::new()));
        draw(&painter);
        let tags = CAPTURE.with(|c| c.borrow_mut().take().unwrap());
        let entries = ui
            .ctx()
            .graphics_mut(|g| g.get_mut(layer).map(std::mem::take).unwrap_or_default());
        let mut options = ui.ctx().tessellation_options(|o| *o);
        options.prerasterized_discs = false;
        options.coarse_tessellation_culling = false;
        for source in entries.all_entries() {
            if cancelled() {
                break;
            }
            let mut entry = source.clone();
            let mut outline = None;
            if schematic {
                match &mut entry.shape {
                    egui::Shape::Path(path) => {
                        if let egui::epaint::ColorMode::Solid(color) = path.stroke.color
                            && color.a() > 0
                        {
                            let mut points = path.points.clone();
                            if path.closed
                                && let Some(first) = points.first().copied()
                                && points.last() != Some(&first)
                            {
                                points.push(first);
                            }
                            outline = Some((points, path.stroke.width, color));
                            path.stroke = egui::epaint::PathStroke::NONE;
                        }
                    }
                    egui::Shape::LineSegment { points, stroke } if stroke.color.a() > 0 => {
                        outline = Some((points.to_vec(), stroke.width, stroke.color));
                        entry.shape = egui::Shape::Noop;
                    }
                    egui::Shape::Circle(circle)
                        if circle.fill.a() > 0 && circle.stroke.color.a() == 0 =>
                    {
                        self.junction(
                            circle.center / REFERENCE,
                            circle.radius / REFERENCE,
                            circle.fill,
                            tint,
                            entry.clip_rect / REFERENCE,
                        );
                        continue;
                    }
                    _ => {}
                }
            }
            let mut batch = Batch::empty(entry.clip_rect / REFERENCE);
            if let egui::Shape::Text(text) = &entry.shape {
                let rule = tags
                    .get(&(Arc::as_ptr(&text.galley) as usize))
                    .copied()
                    .unwrap_or(TextRule {
                        font: REFERENCE,
                        min: 0.,
                        max: f32::INFINITY,
                        mirrored: false,
                    });
                batch.text(text, rule, tint);
                self.push(batch);
                continue;
            }
            let rule = if let egui::Shape::Text(t) = &entry.shape {
                let font = t
                    .galley
                    .rows
                    .first()
                    .and_then(|r| r.glyphs.first())
                    .map_or(REFERENCE, |g| g.font_height);
                Some((
                    tags.get(&(Arc::as_ptr(&t.galley) as usize))
                        .copied()
                        .unwrap_or(TextRule {
                            font,
                            min: 0.,
                            max: f32::INFINITY,
                            mirrored: false,
                        }),
                    t.pos + egui::emath::Rot2::from_angle(t.angle) * (t.galley.size() * 0.5),
                    t.angle,
                ))
            } else {
                None
            };
            let primitives = egui::epaint::Tessellator::new(1., options, [1, 1], vec![])
                .tessellate_shapes(vec![entry.clone()]);
            for primitive in primitives {
                if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive {
                    batch.mesh(&mesh, REFERENCE, rule, tint);
                }
            }
            self.push(batch);
            if let Some((points, width, color)) = outline {
                self.stroke(
                    &points,
                    width / REFERENCE,
                    color,
                    tint,
                    entry.clip_rect / REFERENCE,
                );
            }
        }
    }
    // Extrude centerlines in the vertex shader. A negative maximum marks stroke
    // vertices; `min` then stores world diameter rather than text visibility.
    fn stroke(
        &mut self,
        points: &[egui::Pos2],
        width: f32,
        color: Color32,
        tint: bool,
        clip: egui::Rect,
    ) {
        let mut points: Vec<_> = points.iter().map(|p| *p / REFERENCE).collect();
        points.dedup();
        let closed = points.len() > 2 && points.first() == points.last();
        if closed {
            points.pop();
        }
        if points.len() < 2 {
            return;
        }
        let width = width.max(0.001);
        let mut batch = Batch::empty(clip);
        for i in 0..points.len() {
            let prev = if i > 0 {
                i - 1
            } else if closed {
                points.len() - 1
            } else {
                0
            };
            let next = if i + 1 < points.len() {
                i + 1
            } else if closed {
                0
            } else {
                i
            };
            let normal = |d: egui::Vec2| egui::vec2(-d.y, d.x).normalized();
            let a = normal(points[i] - points[prev]);
            let b = normal(points[next] - points[i]);
            let sum = (a + b).normalized();
            let n = if a == egui::Vec2::ZERO {
                b
            } else if b == egui::Vec2::ZERO {
                a
            } else if sum == egui::Vec2::ZERO {
                b
            } else {
                sum / sum.dot(b).max(0.25)
            };
            for (side, outer) in [(-1., false), (1., false), (-1., true), (1., true)] {
                let color = if outer { Color32::TRANSPARENT } else { color };
                batch.vertices.push(Vertex {
                    p: points[i] + n * side * width * 0.5,
                    anchor: points[i],
                    min: width,
                    max: -1.,
                    offset: if outer {
                        n * side * 0.75
                    } else {
                        egui::Vec2::ZERO
                    },
                    tint: if tint && color == Color32::WHITE {
                        1.
                    } else {
                        0.
                    },
                    ..Vertex::plain(points[i], color)
                });
            }
        }
        for i in 0..if closed {
            points.len()
        } else {
            points.len() - 1
        } {
            let a = (i * 4) as u32;
            let b = (((i + 1) % points.len()) * 4) as u32;
            batch.indices.extend_from_slice(&[
                a,
                b,
                a + 1,
                a + 1,
                b,
                b + 1,
                a + 2,
                b + 2,
                a,
                a,
                b + 2,
                b,
                a + 1,
                b + 1,
                a + 3,
                a + 3,
                b + 1,
                b + 3,
            ]);
        }
        self.push(batch);
    }
    fn junction(
        &mut self,
        center: egui::Pos2,
        radius: f32,
        color: Color32,
        tint: bool,
        clip: egui::Rect,
    ) {
        let radius = radius.max(0.001);
        let mut batch = Batch::empty(clip);
        let vertex = |p, offset, outer: bool| Vertex {
            p,
            anchor: center,
            min: radius * 2.,
            max: -2.,
            offset,
            tint: if tint && color == Color32::WHITE && !outer {
                1.
            } else {
                0.
            },
            ..Vertex::plain(p, if outer { Color32::TRANSPARENT } else { color })
        };
        batch.vertices.push(vertex(center, egui::Vec2::ZERO, false));
        for i in 0..32 {
            let angle = i as f32 * std::f32::consts::TAU / 32.;
            let n = egui::vec2(angle.cos(), angle.sin());
            batch
                .vertices
                .push(vertex(center + n * radius, egui::Vec2::ZERO, false));
            batch
                .vertices
                .push(vertex(center + n * radius, n * 0.75, true));
        }
        for i in 0..32 {
            let a = 1 + i * 2;
            let b = 1 + ((i + 1) % 32) * 2;
            batch
                .indices
                .extend_from_slice(&[0, a, b, a, b, a + 1, a + 1, b, b + 1]);
        }
        self.push(batch);
    }
    pub fn image(
        &mut self,
        center: inventory_kicad::Point,
        size: inventory_kicad::Point,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        let mut b = Batch::empty(egui::Rect::EVERYTHING);
        let rect = egui::Rect::from_center_size(
            egui::pos2(center.x as f32, center.y as f32),
            egui::vec2(size.x as f32, size.y as f32),
        );
        for (p, uv) in [
            (rect.left_top(), egui::pos2(0., 0.)),
            (rect.right_top(), egui::pos2(1., 0.)),
            (rect.left_bottom(), egui::pos2(0., 1.)),
            (rect.right_bottom(), egui::pos2(1., 1.)),
        ] {
            b.vertices.push(Vertex {
                p,
                uv,
                ..Vertex::plain(p, Color32::WHITE)
            });
        }
        b.indices = vec![0, 1, 2, 2, 1, 3];
        b.image = Some(([width, height], Arc::new(rgba.to_vec())));
        self.push(b);
    }
    /// Constant screen-space outline plus a world-space core width (for glow and clearances).
    pub fn line(
        &mut self,
        points: &[inventory_kicad::Point],
        world_width: f32,
        pixel_width: f32,
        color: Color32,
        min_zoom: f32,
    ) {
        let mut b = Batch::empty(egui::Rect::EVERYTHING);
        for pair in points.windows(2) {
            let a = egui::pos2(pair[0].x as f32, pair[0].y as f32);
            let z = egui::pos2(pair[1].x as f32, pair[1].y as f32);
            let d = (z - a).normalized();
            if d == egui::Vec2::ZERO {
                continue;
            }
            let n = egui::vec2(-d.y, d.x);
            let base = b.vertices.len() as u32;
            for (side, outer) in [(-1., false), (1., false), (-1., true), (1., true)] {
                for p in [a, z] {
                    let offset = n * side * (pixel_width * 0.5 + if outer { 0.75 } else { 0. });
                    let p = p + n * side * world_width * 0.5;
                    b.vertices.push(Vertex {
                        offset,
                        min: min_zoom,
                        ..Vertex::plain(p, if outer { Color32::TRANSPARENT } else { color })
                    });
                }
            }
            b.indices
                .extend([0, 1, 2, 2, 1, 3, 4, 5, 0, 0, 5, 1, 2, 3, 6, 6, 3, 7].map(|i| i + base));
        }
        self.push(b);
    }
}
impl Vertex {
    fn plain(p: egui::Pos2, color: Color32) -> Self {
        Self {
            p,
            color,
            anchor: p,
            min: 0.,
            max: f32::INFINITY,
            offset: egui::Vec2::ZERO,
            tint: 0.,
            uv: egui::Pos2::ZERO,
        }
    }
}
impl Batch {
    fn empty(clip: egui::Rect) -> Self {
        Self {
            vertices: vec![],
            indices: vec![],
            clip,
            image: None,
            glyphs: Default::default(),
        }
    }
    fn text(&mut self, text: &egui::epaint::TextShape, rule: TextRule, tint: bool) {
        let rot = egui::emath::Rot2::from_angle(text.angle);
        let font = rule.font / REFERENCE;
        let anchor = (text.pos + rot * (text.galley.size() * 0.5)) / REFERENCE;
        let italic = text
            .galley
            .job
            .sections
            .first()
            .is_some_and(|s| s.format.italics);
        for row in &text.galley.rows {
            for glyph in &row.glyphs {
                if glyph.uv_rect.is_nothing() {
                    continue;
                }
                let Some(geometry) = super::cad_text::retained_glyph(glyph.chr) else {
                    continue;
                };
                let mut local = row.pos + glyph.pos.to_vec2();
                if rule.mirrored {
                    local.x = text.galley.size().x - local.x;
                }
                let color = row.visuals.mesh.vertices[glyph.first_vertex as usize].color;
                let instance = GlyphInstance {
                    origin: (text.pos + rot * local.to_vec2()) / REFERENCE,
                    basis: rot * egui::vec2(font, 0.),
                    anchor,
                    min: rule.min / font.max(1e-6),
                    max: rule.max / font.max(1e-6),
                    color,
                    tint: if tint
                        && color.r() == color.a()
                        && color.g() == color.a()
                        && color.b() == color.a()
                    {
                        1.
                    } else {
                        0.
                    },
                    style: u32::from(rule.mirrored) | (u32::from(italic) << 1),
                };
                self.glyphs
                    .entry(glyph.chr)
                    .or_insert_with(|| GlyphGroup {
                        geometry,
                        instances: vec![],
                    })
                    .instances
                    .push(instance);
            }
        }
    }
    fn mesh(
        &mut self,
        mesh: &egui::Mesh,
        reference: f32,
        rule: Option<(TextRule, egui::Pos2, f32)>,
        tint: bool,
    ) {
        let base = self.vertices.len() as u32;
        for v in &mesh.vertices {
            let p = v.pos / reference;
            let mut vertex = Vertex::plain(p, v.color);
            if let Some((rule, anchor, angle)) = rule {
                vertex.offset = egui::emath::Rot2::from_angle(angle) * v.uv.to_vec2();
                vertex.anchor = anchor / reference;
                vertex.min = rule.min * reference / rule.font.max(0.001);
                vertex.max = rule.max * reference / rule.font.max(0.001);
            }
            if tint && v.color.r() == v.color.g() && v.color.g() == v.color.b() && v.color.a() > 0 {
                vertex.tint = 1.;
            }
            self.vertices.push(vertex);
        }
        self.indices.extend(mesh.indices.iter().map(|i| i + base));
    }
}

pub(super) fn maintain(state: &egui_wgpu::RenderState) {
    if let Some(renderer) = state
        .renderer
        .write()
        .callback_resources
        .get_mut::<Renderer>()
    {
        renderer.prune();
    }
}
pub(super) fn available(ctx: &egui::Context) -> bool {
    ctx.data(|d| {
        d.get_temp::<bool>(egui::Id::new("cad gpu available"))
            .unwrap_or(false)
    })
}
pub(super) fn register(state: &egui_wgpu::RenderState, ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("cad gpu available"), true));
    state
        .renderer
        .write()
        .callback_resources
        .insert(Renderer::new(&state.device, state.target_format));
}
pub(super) struct View {
    pub center: inventory_kicad::Point,
    pub scale: f32,
    pub tint: Color32,
    pub background: Color32,
    pub grayscale_base: bool,
    pub glow_time: Option<f32>,
}
pub(super) fn callback(
    ctx: &egui::Context,
    rect: egui::Rect,
    base: Arc<Scene>,
    glow: Option<Arc<Scene>>,
    highlight: Option<Arc<Scene>>,
    view: View,
) -> egui::Shape {
    let View {
        center,
        scale,
        tint,
        background,
        grayscale_base,
        glow_time,
    } = view;
    egui_wgpu::Callback::new_paint_callback(
        rect,
        Frame {
            base,
            glow,
            highlight,
            center,
            scale,
            tint,
            background,
            grayscale_base,
            glow_time,
            size: rect.size(),
            repaint: Some(ctx.clone()),
        },
    )
    .into()
}
struct Frame {
    base: Arc<Scene>,
    glow: Option<Arc<Scene>>,
    highlight: Option<Arc<Scene>>,
    center: inventory_kicad::Point,
    scale: f32,
    tint: Color32,
    background: Color32,
    grayscale_base: bool,
    glow_time: Option<f32>,
    size: egui::Vec2,
    repaint: Option<egui::Context>,
}
struct Uploaded {
    _source: Arc<Batch>,
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    count: u32,
    glyphs: Vec<(char, wgpu::Buffer, u32)>,
    image: Option<(wgpu::Texture, wgpu::BindGroup)>,
}
struct GlyphGpu {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    count: u32,
}
struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}
#[derive(Clone, PartialEq)]
struct FrameKey {
    glow_time: Option<u32>,
    grayscale_base: bool,
    scenes: [usize; 3],
    camera: [u32; 5],
    size: [u32; 2],
    ppp: u32,
    tint: Color32,
    background: Color32,
}
struct Renderer {
    solid: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    glyph_buffers: HashMap<char, GlyphGpu>,
    union: wgpu::RenderPipeline,
    image: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    paint: wgpu::RenderPipeline,
    dummy: wgpu::BindGroup,
    camera: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    gray_camera: wgpu::Buffer,
    gray_camera_bind: wgpu::BindGroup,
    glow_camera: wgpu::Buffer,
    glow_camera_bind: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    uploaded: HashMap<usize, Uploaded>,
    live_scenes: Vec<std::sync::Weak<Scene>>,
    output: Option<Target>,
    mask: Option<Target>,
    size: [u32; 2],
    finished: Option<FrameKey>,
    uploads: usize,
    renders: usize,
    hits: usize,
}
const SHADER: &str = r#"
struct Camera {transform:vec4<f32>,view:vec4<f32>,tint:vec4<f32>}
@group(0) @binding(0) var<uniform> camera:Camera;
@group(1) @binding(0) var image:texture_2d<f32>;
@group(1) @binding(1) var image_sampler:sampler;
struct Out {@builtin(position) p:vec4<f32>,@location(0) c:vec4<f32>,@location(1) uv:vec2<f32>}
@vertex fn world(@location(0) p:vec2<f32>,@location(1) c:u32,@location(2) anchor:vec2<f32>,@location(3) limits:vec2<f32>,@location(4) offset:vec2<f32>,@location(5) tint:f32,@location(6) uv:vec2<f32>)->Out{
 let zoom=camera.transform.z;
 let stroke=limits.y<0.;
 let local=select(min(zoom,limits.y),max(zoom,-limits.y/(max(limits.x,0.000001)*camera.transform.w)),stroke);
 let s=(anchor-camera.transform.xy)*zoom+(p-anchor)*local+offset/camera.transform.w+camera.view.xy*0.5;
 var out:Out;out.p=vec4<f32>(s.x/camera.view.x*2.-1.,1.-s.y/camera.view.y*2.,0.,1.);
 let color=unpack4x8unorm(c);out.c=mix(color,vec4<f32>(camera.tint.rgb*color.a,color.a),tint)*select(0.,1.,(zoom>=limits.x)||stroke);
 if zoom<limits.x && !stroke {out.p=vec4<f32>(2.,2.,2.,1.);}
 out.uv=uv;return out;
}
@vertex fn glyph(@location(0) p:vec2<f32>,@location(1) c:u32,@location(4) offset:vec2<f32>,@location(6) uv:vec2<f32>,
 @location(7) origin:vec2<f32>,@location(8) basis:vec2<f32>,@location(9) anchor:vec2<f32>,@location(10) limits:vec2<f32>,@location(11) color:u32,@location(12) tint:f32,@location(13) style:u32)->Out {
 var q=p; var n=offset;
 if (style&2u)!=0u {q.x+=(uv.x-q.y)*0.25;n.x-=n.y*0.25;}
 if (style&1u)!=0u {q.x=-q.x;n.x=-n.x;}
 let world=origin+vec2<f32>(basis.x*q.x-basis.y*q.y,basis.y*q.x+basis.x*q.y);
 let rot=basis/max(length(basis),0.000001);
 let aa=vec2<f32>(rot.x*n.x-rot.y*n.y,rot.y*n.x+rot.x*n.y);
 let zoom=camera.transform.z;let local=min(zoom,limits.y);
 let s=(anchor-camera.transform.xy)*zoom+(world-anchor)*local+aa/camera.transform.w+camera.view.xy*0.5;
 var out:Out;out.p=vec4<f32>(s.x/camera.view.x*2.-1.,1.-s.y/camera.view.y*2.,0.,1.);
 let rgba=unpack4x8unorm(color)*unpack4x8unorm(c).a;
 out.c=mix(rgba,vec4<f32>(camera.tint.rgb*rgba.a,rgba.a),tint);
 if zoom<limits.x {out.p=vec4<f32>(2.,2.,2.,1.);}
 out.uv=vec2<f32>(0.);return out;
}
fn appearance(c:vec4<f32>, p:vec2<f32>)->vec4<f32>{
 let gray=dot(c.rgb,vec3<f32>(0.2126,0.7152,0.0722));
 var color=vec4<f32>(mix(c.rgb,vec3<f32>(gray),camera.view.z),c.a);
 if camera.view.w>0. {
  let t=camera.view.w-1.;let logical=p/camera.transform.w;
  let shine=pow(max(0.,cos(logical.x*0.012+logical.y*0.006-t*2.4)),12.);
  let intensity=0.55+0.25*(0.5+0.5*sin(t*3.))+0.2*shine;
  color=vec4<f32>(mix(color.rgb,vec3<f32>(0.94,0.82,1.)*color.a,shine*0.5),color.a)*intensity;

 }
 return color;
}
@fragment fn solid(in:Out)->@location(0) vec4<f32>{return appearance(in.c,in.p.xy);}
@fragment fn textured(in:Out)->@location(0) vec4<f32>{let c=textureSample(image,image_sampler,in.uv);return appearance(vec4<f32>(c.rgb*c.a,c.a)*in.c,in.p.xy);}
@vertex fn full(@builtin(vertex_index) i:u32)->Out{
 let p=array<vec2<f32>,3>(vec2<f32>(-1.,-1.),vec2<f32>(3.,-1.),vec2<f32>(-1.,3.));
 var out:Out;out.p=vec4<f32>(p[i],0.,1.);out.c=vec4<f32>(1.);out.uv=vec2<f32>((p[i].x+1.)*0.5,(1.-p[i].y)*0.5);return out;
}
@fragment fn blit(in:Out)->@location(0) vec4<f32>{return textureSample(image,image_sampler,in.uv);}
@fragment fn linear(in:Out)->@location(0) vec4<f32>{let c=textureSample(image,image_sampler,in.uv);let rgb=select(c.rgb/12.92,pow((c.rgb+vec3<f32>(0.055))/1.055,vec3<f32>(2.4)),c.rgb>vec3<f32>(0.04045));return vec4<f32>(rgb,c.a);}
"#;
impl Renderer {
    fn prune(&mut self) {
        self.uploaded
            .retain(|_, u| Arc::strong_count(&u._source) > 1);
        let mut chars: std::collections::HashSet<_> = self
            .uploaded
            .values()
            .flat_map(|u| u._source.glyphs.keys().copied())
            .collect();
        self.live_scenes.retain(|s| s.strong_count() > 0);
        for scene in self.live_scenes.iter().filter_map(|s| s.upgrade()) {
            chars.extend(
                scene
                    .groups
                    .iter()
                    .flat_map(|g| g.1.iter().flat_map(|b| b.glyphs.keys().copied())),
            );
        }
        self.glyph_buffers.retain(|ch, _| chars.contains(ch));
        if self.uploaded.is_empty() && self.live_scenes.is_empty() {
            self.output = None;
            self.mask = None;
            self.size = [0, 0];
            self.finished = None;
        }
    }
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Retained CAD camera"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("CAD camera and highlight"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            }],
        });
        let gray_camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("CAD grayscale base camera"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let gray_camera_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: gray_camera.as_entire_binding(),
            }],
        });
        let glow_camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("CAD animated glow camera"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let glow_camera_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: glow_camera.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&camera_layout, &texture_layout],
            push_constant_ranges: &[],
        });
        let attributes = wgpu::vertex_attr_array![0=>Float32x2,1=>Uint32,2=>Float32x2,3=>Float32x2,4=>Float32x2,5=>Float32,6=>Float32x2];
        let buffers = [wgpu::VertexBufferLayout {
            array_stride: 48,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        }];
        let instance_attributes = wgpu::vertex_attr_array![7=>Float32x2,8=>Float32x2,9=>Float32x2,10=>Float32x2,11=>Uint32,12=>Float32,13=>Uint32];
        let glyph_layout = [
            buffers[0].clone(),
            wgpu::VertexBufferLayout {
                array_stride: 44,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &instance_attributes,
            },
        ];
        let pipeline = |vertex: &str, fragment: &str, format, blend, geometry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fragment),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex),
                    compilation_options: Default::default(),
                    buffers: match geometry {
                        1 => &buffers,
                        2 => &glyph_layout,
                        _ => &[],
                    },
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        let alpha = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;
        let max = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let solid = pipeline("world", "solid", wgpu::TextureFormat::Rgba8Unorm, alpha, 1);
        let union = pipeline(
            "world",
            "solid",
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::BlendState {
                color: max,
                alpha: max,
            },
            1,
        );
        let image = pipeline(
            "world",
            "textured",
            wgpu::TextureFormat::Rgba8Unorm,
            alpha,
            1,
        );
        let composite = pipeline("full", "blit", wgpu::TextureFormat::Rgba8Unorm, alpha, 0);
        let paint = pipeline(
            "full",
            if format.is_srgb() { "linear" } else { "blit" },
            format,
            alpha,
            0,
        );
        let glyph_pipeline = pipeline("glyph", "solid", wgpu::TextureFormat::Rgba8Unorm, alpha, 2);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("CAD unused solid texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let sampler = device.create_sampler(&Default::default());
        let dummy = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Self {
            solid,
            glyph_pipeline,
            glyph_buffers: HashMap::new(),
            union,
            image,
            composite,
            paint,
            dummy,
            camera,
            camera_bind,
            gray_camera,
            gray_camera_bind,
            glow_camera,
            glow_camera_bind,
            texture_layout,
            uploaded: HashMap::new(),
            live_scenes: vec![],
            output: None,
            mask: None,
            size: [0, 0],
            finished: None,
            uploads: 0,
            renders: 0,
            hits: 0,
        }
    }
    fn target(&self, device: &wgpu::Device, size: [u32; 2]) -> Target {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("CAD cached frame"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind = self.texture_bind(device, &view);
        Target {
            _texture: texture,
            view,
            bind,
        }
    }
    fn texture_bind(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> wgpu::BindGroup {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        })
    }
    fn upload_glyph(
        &mut self,
        device: &wgpu::Device,
        ch: char,
        geometry: &super::cad_text::GlyphGeometry,
    ) {
        if self.glyph_buffers.contains_key(&ch) {
            return;
        }
        let glyph = {
            let mut bytes = vec![];
            for v in &geometry.vertices {
                for f in [v.position.x, v.position.y] {
                    bytes.extend_from_slice(&f.to_ne_bytes());
                }
                bytes.extend_from_slice(
                    &if v.outer {
                        Color32::TRANSPARENT
                    } else {
                        Color32::WHITE
                    }
                    .to_array(),
                );
                for f in [
                    0f32,
                    0.,
                    0.,
                    f32::INFINITY,
                    v.normal.x * 0.75,
                    v.normal.y * 0.75,
                    0.,
                    geometry.bottom,
                    0.,
                ] {
                    bytes.extend_from_slice(&f.to_ne_bytes());
                }
            }
            GlyphGpu {
                vb: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("CAD shared glyph vertices"),
                    contents: &bytes,
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                ib: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("CAD shared glyph indices"),
                    contents: &geometry
                        .indices
                        .iter()
                        .flat_map(|i| i.to_ne_bytes())
                        .collect::<Vec<_>>(),
                    usage: wgpu::BufferUsages::INDEX,
                }),
                count: geometry.indices.len() as u32,
            }
        };
        self.glyph_buffers.insert(ch, glyph);
    }
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, batch: &Arc<Batch>) {
        let key = Arc::as_ptr(batch) as usize;
        if self.uploaded.contains_key(&key) {
            return;
        }
        let mut vertices = Vec::with_capacity(batch.vertices.len() * 48);
        for v in &batch.vertices {
            for f in [v.p.x, v.p.y] {
                vertices.extend_from_slice(&f.to_ne_bytes());
            }
            vertices.extend_from_slice(&v.color.to_array());
            for f in [
                v.anchor.x, v.anchor.y, v.min, v.max, v.offset.x, v.offset.y, v.tint, v.uv.x,
                v.uv.y,
            ] {
                vertices.extend_from_slice(&f.to_ne_bytes());
            }
        }
        let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("CAD retained vertices"),
            contents: if vertices.is_empty() {
                &[0; 48]
            } else {
                &vertices
            },
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("CAD retained indices"),
            contents: &if batch.indices.is_empty() {
                vec![0; 4]
            } else {
                batch
                    .indices
                    .iter()
                    .flat_map(|i| i.to_ne_bytes())
                    .collect::<Vec<_>>()
            },
            usage: wgpu::BufferUsages::INDEX,
        });
        let image = batch.image.as_ref().map(|(size, rgba)| {
            let max = device.limits().max_texture_dimension_2d;
            let resized = if size[0] > max || size[1] > max {
                let factor = max as f64 / size[0].max(size[1]) as f64;
                Some(image::imageops::resize(
                    &image::RgbaImage::from_raw(size[0], size[1], (**rgba).clone()).unwrap(),
                    (size[0] as f64 * factor).floor().max(1.) as u32,
                    (size[1] as f64 * factor).floor().max(1.) as u32,
                    image::imageops::FilterType::Triangle,
                ))
            } else {
                None
            };
            let size = resized.as_ref().map_or(*size, |i| [i.width(), i.height()]);
            let rgba = resized
                .as_ref()
                .map_or(rgba.as_slice(), |i| i.as_raw().as_slice());
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("CAD retained bitmap"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                texture.as_image_copy(),
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size[0] * 4),
                    rows_per_image: Some(size[1]),
                },
                texture.size(),
            );
            let bind = self.texture_bind(device, &texture.create_view(&Default::default()));
            (texture, bind)
        });
        let mut glyphs = vec![];
        for (&ch, group) in &batch.glyphs {
            let mut bytes = Vec::with_capacity(group.instances.len() * 44);
            for i in &group.instances {
                for f in [
                    i.origin.x, i.origin.y, i.basis.x, i.basis.y, i.anchor.x, i.anchor.y, i.min,
                    i.max,
                ] {
                    bytes.extend_from_slice(&f.to_ne_bytes());
                }
                bytes.extend_from_slice(&i.color.to_array());
                bytes.extend_from_slice(&i.tint.to_ne_bytes());
                bytes.extend_from_slice(&i.style.to_ne_bytes());
            }
            glyphs.push((
                ch,
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("CAD glyph instances"),
                    contents: &bytes,
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                group.instances.len() as u32,
            ));
        }
        self.uploaded.insert(
            key,
            Uploaded {
                _source: batch.clone(),
                vb,
                ib,
                count: batch.indices.len() as u32,
                glyphs,
                image,
            },
        );
        self.uploads += 1;
    }
    fn prepare(
        &mut self,
        frame: &Frame,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        logical: egui::Vec2,
        ppp: f32,
    ) -> bool {
        // Two RGBA frame targets share a 128 MiB budget. Large/HiDPI windows
        // downsample proportionally rather than exceeding WebGL/device limits.
        let requested = logical * ppp;
        let max = device.limits().max_texture_dimension_2d as f32;
        let factor = (max / requested.x.max(requested.y))
            .min(((128. * 1024. * 1024.) / (requested.x * requested.y * 8.)).sqrt())
            .min(1.);
        let size = [
            (requested.x * factor).floor().max(1.) as u32,
            (requested.y * factor).floor().max(1.) as u32,
        ];
        let raster = egui::vec2(size[0] as f32 / logical.x, size[1] as f32 / logical.y);
        let scenes = [
            Some(&frame.base),
            frame.glow.as_ref(),
            frame.highlight.as_ref(),
        ];
        self.live_scenes = scenes.iter().flatten().map(|s| Arc::downgrade(s)).collect();
        let key = FrameKey {
            glow_time: frame
                .glow_time
                .filter(|_| frame.glow.is_some())
                .map(f32::to_bits),
            grayscale_base: frame.grayscale_base,
            scenes: scenes.map(|s| s.map_or(0, |s| s.id)),
            camera: [
                (frame.center.x as f32).to_bits(),
                (frame.center.y as f32).to_bits(),
                frame.scale.to_bits(),
                logical.x.to_bits(),
                logical.y.to_bits(),
            ],
            size,
            ppp: ppp.to_bits(),
            tint: frame.tint,
            background: frame.background,
        };
        if self.finished.as_ref() == Some(&key) {
            self.hits += 1;
            return true;
        }
        if size != self.size {
            self.output = Some(self.target(device, size));
            self.mask = Some(self.target(device, size));
            self.size = size;
        }
        let current: std::collections::HashSet<_> = scenes
            .iter()
            .flatten()
            .flat_map(|s| &s.groups)
            .flat_map(|g| &g.1)
            .map(|b| Arc::as_ptr(b) as usize)
            .collect();
        // Keep live document batches between sheet switches, with a bounded GPU cache.
        self.uploaded
            .retain(|_, u| Arc::strong_count(&u._source) > 1);
        let bytes: usize = self
            .uploaded
            .values()
            .map(|u| {
                u._source.vertices.len() * 48
                    + u._source.indices.len() * 4
                    + u._source
                        .glyphs
                        .values()
                        .map(|g| g.instances.len() * 44)
                        .sum::<usize>()
                    + u._source.image.as_ref().map_or(0, |i| i.1.len())
            })
            .sum();
        if bytes > 192 * 1024 * 1024 {
            self.uploaded.retain(|id, _| current.contains(id));
        }
        let used_glyphs: std::collections::HashSet<_> = self
            .uploaded
            .values()
            .flat_map(|u| u._source.glyphs.keys().copied())
            .chain(scenes.iter().flatten().flat_map(|s| {
                s.groups
                    .iter()
                    .flat_map(|g| g.1.iter().flat_map(|b| b.glyphs.keys().copied()))
            }))
            .collect();
        self.glyph_buffers.retain(|ch, _| used_glyphs.contains(ch));
        let started = web_time::Instant::now();
        let mut uploaded_bytes = 0usize;
        let mut pending = false;
        for scene in scenes.iter().flatten() {
            for (_, batches) in &scene.groups {
                for batch in batches {
                    for (&ch, group) in &batch.glyphs {
                        if self.glyph_buffers.contains_key(&ch) {
                            continue;
                        }
                        if uploaded_bytes > 0
                            && (uploaded_bytes >= 4 * 1024 * 1024
                                || started.elapsed().as_millis() >= 4)
                        {
                            pending = true;
                            continue;
                        }
                        self.upload_glyph(device, ch, &group.geometry);
                        uploaded_bytes +=
                            group.geometry.vertices.len() * 48 + group.geometry.indices.len() * 4;
                    }
                    if self.uploaded.contains_key(&(Arc::as_ptr(batch) as usize)) {
                        continue;
                    }
                    if batch
                        .glyphs
                        .keys()
                        .any(|ch| !self.glyph_buffers.contains_key(ch))
                        || (uploaded_bytes > 0
                            && (uploaded_bytes >= 4 * 1024 * 1024
                                || started.elapsed().as_millis() >= 4))
                    {
                        pending = true;
                        continue;
                    }
                    self.upload(device, queue, batch);
                    uploaded_bytes += batch.vertices.len() * 48
                        + batch.indices.len() * 4
                        + batch
                            .glyphs
                            .values()
                            .map(|g| g.instances.len() * 44)
                            .sum::<usize>()
                        + batch.image.as_ref().map_or(0, |i| i.1.len());
                }
            }
            scene.ready.store(
                scene
                    .groups
                    .iter()
                    .flat_map(|g| &g.1)
                    .all(|b| self.uploaded.contains_key(&(Arc::as_ptr(b) as usize))),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        let tint = frame.tint.to_array();
        let values = [
            frame.center.x as f32,
            frame.center.y as f32,
            frame.scale,
            ppp,
            logical.x,
            logical.y,
            0.,
            0.,
            tint[0] as f32 / 255.,
            tint[1] as f32 / 255.,
            tint[2] as f32 / 255.,
            tint[3] as f32 / 255.,
        ];
        queue.write_buffer(
            &self.camera,
            0,
            &values
                .into_iter()
                .flat_map(f32::to_ne_bytes)
                .collect::<Vec<_>>(),
        );
        let mut gray_values = values;
        gray_values[6] = 1.;
        queue.write_buffer(
            &self.gray_camera,
            0,
            &gray_values
                .into_iter()
                .flat_map(f32::to_ne_bytes)
                .collect::<Vec<_>>(),
        );
        let mut glow_values = values;
        glow_values[7] = frame.glow_time.map_or(0., |t| t + 1.);
        queue.write_buffer(
            &self.glow_camera,
            0,
            &glow_values
                .into_iter()
                .flat_map(f32::to_ne_bytes)
                .collect::<Vec<_>>(),
        );
        let output = self.output.as_ref().unwrap();
        let mask = self.mask.as_ref().unwrap();
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("CAD clear frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: frame.background.r() as f64 / 255.,
                            g: frame.background.g() as f64 / 255.,
                            b: frame.background.b() as f64 / 255.,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        // Component glow is behind the document; net highlighting is above it.
        for scene in [
            frame.glow.as_ref(),
            Some(&frame.base),
            frame.highlight.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            for (union, batches) in &scene.groups {
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("CAD retained scene"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: if *union { &mask.view } else { &output.view },
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: if *union {
                                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                                } else {
                                    wgpu::LoadOp::Load
                                },
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    pass.set_bind_group(
                        0,
                        if frame.glow_time.is_some()
                            && frame
                                .glow
                                .as_ref()
                                .is_some_and(|glow| Arc::ptr_eq(scene, glow))
                        {
                            &self.glow_camera_bind
                        } else if frame.grayscale_base && Arc::ptr_eq(scene, &frame.base) {
                            &self.gray_camera_bind
                        } else {
                            &self.camera_bind
                        },
                        &[],
                    );
                    // The solid pipeline shares its layout with images; bind an unused texture.
                    pass.set_bind_group(1, &self.dummy, &[]);
                    for batch in batches {
                        let Some(u) = self.uploaded.get(&(Arc::as_ptr(batch) as usize)) else {
                            continue;
                        };
                        let clip = batch.clip;
                        let a = egui::pos2(
                            (clip.min.x - frame.center.x as f32) * frame.scale + logical.x * 0.5,
                            (clip.min.y - frame.center.y as f32) * frame.scale + logical.y * 0.5,
                        );
                        let b = egui::pos2(
                            (clip.max.x - frame.center.x as f32) * frame.scale + logical.x * 0.5,
                            (clip.max.y - frame.center.y as f32) * frame.scale + logical.y * 0.5,
                        );
                        let clip = egui::Rect::from_min_max(a, b)
                            .intersect(egui::Rect::from_min_size(egui::Pos2::ZERO, logical));
                        if !clip.is_positive() {
                            continue;
                        }
                        let x = (clip.min.x * raster.x).floor().max(0.) as u32;
                        let y = (clip.min.y * raster.y).floor().max(0.) as u32;
                        let w = ((clip.max.x * raster.x).ceil() as u32)
                            .min(size[0])
                            .saturating_sub(x);
                        let h = ((clip.max.y * raster.y).ceil() as u32)
                            .min(size[1])
                            .saturating_sub(y);
                        if w == 0 || h == 0 {
                            continue;
                        }
                        pass.set_scissor_rect(x, y, w, h);
                        if let Some((_, bind)) = &u.image {
                            pass.set_pipeline(&self.image);
                            pass.set_bind_group(1, bind, &[]);
                        } else {
                            pass.set_pipeline(if *union { &self.union } else { &self.solid });
                            pass.set_bind_group(1, &self.dummy, &[]);
                        }
                        pass.set_vertex_buffer(0, u.vb.slice(..));
                        pass.set_index_buffer(u.ib.slice(..), wgpu::IndexFormat::Uint32);
                        if u.count > 0 {
                            pass.draw_indexed(0..u.count, 0, 0..1);
                        }
                        if !u.glyphs.is_empty() {
                            pass.set_pipeline(&self.glyph_pipeline);
                            pass.set_bind_group(1, &self.dummy, &[]);
                            for (ch, instances, count) in &u.glyphs {
                                let glyph = &self.glyph_buffers[ch];
                                pass.set_vertex_buffer(0, glyph.vb.slice(..));
                                pass.set_vertex_buffer(1, instances.slice(..));
                                pass.set_index_buffer(
                                    glyph.ib.slice(..),
                                    wgpu::IndexFormat::Uint32,
                                );
                                pass.draw_indexed(0..glyph.count, 0, 0..*count);
                            }
                        }
                    }
                }
                if *union {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("CAD layer union composite"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &output.view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.composite);
                    pass.set_bind_group(0, &self.camera_bind, &[]);
                    pass.set_bind_group(1, &mask.bind, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }
        self.finished = (!pending).then_some(key);
        self.renders += 1;
        !pending
    }
}
impl CallbackTrait for Frame {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Render in local viewport coordinates; moving the panel only changes its final blit.
        if let Some(r) = resources.get_mut::<Renderer>() {
            let ready = r.prepare(
                self,
                device,
                queue,
                encoder,
                self.size,
                screen.pixels_per_point,
            );
            if !ready && let Some(ctx) = &self.repaint {
                ctx.request_repaint();
            }
        }
        vec![]
    }
    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        if let Some(r) = resources.get::<Renderer>()
            && let Some(output) = &r.output
        {
            let v = info.viewport_in_pixels();
            pass.set_viewport(
                v.left_px as f32,
                v.top_px as f32,
                v.width_px as f32,
                v.height_px as f32,
                0.,
                1.,
            );
            pass.set_pipeline(&r.paint);
            pass.set_bind_group(0, &r.camera_bind, &[]);
            pass.set_bind_group(1, &output.bind, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(super) fn render_preview(
    base: Arc<Scene>,
    highlight: Option<Arc<Scene>>,
    center: inventory_kicad::Point,
    scale: f32,
    size: [u32; 2],
    background: Color32,
    path: &str,
) {
    crate::async_io::runtime().unwrap().block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance.request_adapter(&Default::default()).await.unwrap();
        let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
        let mut renderer = Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let frame = Frame {
            base,
            glow: None,
            highlight,
            center,
            scale,
            tint: Color32::from_rgb(220, 0, 220),
            background,
            grayscale_base: std::env::var_os("CAD_GPU_GRAYSCALE").is_some(),
            glow_time: None,
            size: egui::vec2(size[0] as f32, size[1] as f32),
            repaint: None,
        };
        let stride = (size[0] * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (stride * size[1]) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = loop {
            let mut encoder = device.create_command_encoder(&Default::default());
            if renderer.prepare(&frame, &device, &queue, &mut encoder, frame.size, 1.) {
                break encoder;
            }
            queue.submit([encoder.finish()]);
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        };
        encoder.copy_texture_to_buffer(
            renderer.output.as_ref().unwrap()._texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(size[1]),
                },
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let pixels = buffer.slice(..).get_mapped_range();
        let mut rgba = Vec::new();
        for row in pixels.chunks_exact(stride as usize) {
            rgba.extend_from_slice(&row[..(size[0] * 4) as usize]);
        }
        if std::env::var_os("CAD_GPU_PROFILE").is_some() {
            let buffers:u64=renderer.uploaded.values().map(|u|u.vb.size()+u.ib.size()+u.glyphs.iter().map(|(_,b,_)|b.size()).sum::<u64>()).sum::<u64>()+renderer.glyph_buffers.values().map(|g|g.vb.size()+g.ib.size()).sum::<u64>();
            eprintln!("GPU buffer data {:.1} MiB; {} shared glyph meshes; {} batches uploaded over {} frames",buffers as f64/1048576.,renderer.glyph_buffers.len(),renderer.uploads,renderer.renders);
            #[cfg(target_os="macos")]
            if let Ok(output)=std::process::Command::new("ps").args(["-o","rss=","-p",&std::process::id().to_string()]).output(){eprintln!("Test-process RSS: {} KiB",String::from_utf8_lossy(&output.stdout).trim());}
        }
        image::RgbaImage::from_raw(size[0], size[1], rgba)
            .unwrap()
            .save(path)
            .unwrap();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schematic_strokes_have_a_physical_pixel_floor_and_keep_world_width_at_high_zoom() {
        let mut b = Builder::default();
        b.stroke(
            &[egui::pos2(-64., 0.), egui::pos2(64., 0.)],
            0.15,
            Color32::BLACK,
            false,
            egui::Rect::EVERYTHING,
        );
        let scene = b.finish();
        let vertices = &scene.groups[0].1[0].vertices;
        for zoom in [0.1f32, 1., 20., 200.] {
            for dpi in [1f32, 2., 3.] {
                let v = vertices[0];
                let opposite = vertices[1];
                let factor = zoom.max(-v.max / (v.min * dpi));
                let physical = (v.p - opposite.p).length() * factor * dpi;
                let expected = (0.15 * zoom * dpi).max(1.);
                assert!((physical - expected).abs() < 0.001);
            }
        }
        assert!(vertices.iter().any(|v| v.offset.length() >= 0.75));
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[ignore = "Requires a working GPU adapter"]
    fn zoomed_out_schematic_strokes_remain_visible() {
        let ctx = egui::Context::default();
        let mut scene = None;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut b = Builder::default();
                b.capture_schematic(ui, false, |p| {
                    p.add(egui::Shape::line_segment(
                        [egui::pos2(-6400., 0.), egui::pos2(6400., 0.)],
                        egui::Stroke::new(0.15 * REFERENCE, Color32::BLACK),
                    ));
                });
                scene = Some(b.finish());
            });
        });
        let file = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        for zoom in [0.1, 0.5, 20.] {
            render_preview(
                scene.as_ref().unwrap().clone(),
                None,
                inventory_kicad::Point::default(),
                zoom,
                [128, 128],
                Color32::WHITE,
                file.path().to_str().unwrap(),
            );
            let image = image::open(file.path()).unwrap().to_rgba8();
            let darkest = (60..68).map(|y| image.get_pixel(64, y)[0]).min().unwrap();
            assert!(
                darkest < 90,
                "wire faded at zoom {zoom}: darkest pixel {darkest}"
            );
        }
    }
    #[test]
    fn repeated_text_uses_one_glyph_mesh_and_linear_small_instance_storage() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut builder = Builder::default();
                builder.capture(ui, false, |p| {
                    let g = super::super::cad_text::zoom_galley(
                        p,
                        &"R".repeat(10_000),
                        24.,
                        128.,
                        Color32::WHITE,
                    );
                    p.add(egui::epaint::TextShape::new(
                        egui::Pos2::ZERO,
                        g,
                        Color32::WHITE,
                    ));
                });
                let scene = builder.finish();
                let batch = &scene.groups[0].1[0];
                assert!(batch.vertices.is_empty());
                assert_eq!(batch.glyphs.len(), 1);
                assert_eq!(batch.glyphs[&'R'].instances.len(), 10_000);
                let (bytes, _, expanded) = scene.profile();
                assert!(bytes < 2 * 1024 * 1024);
                assert!(expanded > bytes * 20);
            });
        });
    }
    #[test]
    fn captured_text_has_pixel_aa_and_zoom_limits_without_atlas_dependency() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut builder = Builder::default();
                builder.capture(ui, false, |p| {
                    let g = super::super::cad_text::zoom_galley(
                        p,
                        "R12 Ω 😀",
                        24.,
                        128.,
                        Color32::WHITE,
                    );
                    tag_text(&g, 24., 7., 48.);
                    p.add(egui::epaint::TextShape::new(
                        egui::Pos2::ZERO,
                        g,
                        Color32::WHITE,
                    ));
                });
                let scene = builder.finish();
                let batch = &scene.groups[0].1[0];
                assert!(
                    batch.vertices.is_empty(),
                    "letters must use shared meshes, not expanded vertices"
                );
                assert!(
                    batch.glyphs.values().any(|g| g
                        .geometry
                        .vertices
                        .iter()
                        .any(|v| v.normal.length() > 0.5))
                );
                let instances = batch
                    .glyphs
                    .values()
                    .flat_map(|g| &g.instances)
                    .collect::<Vec<_>>();
                assert!(
                    instances
                        .iter()
                        .all(|v| (v.min - 7. * REFERENCE / 24.).abs() < 0.001)
                );
                assert!(
                    instances
                        .iter()
                        .all(|v| (v.max - 48. * REFERENCE / 24.).abs() < 0.001)
                );
                let bytes = serde_json::to_vec(scene.as_ref()).unwrap();
                let decoded: Scene = serde_json::from_slice(&bytes).unwrap();
                assert_ne!(scene.id, decoded.id);
                assert!(
                    decoded.groups[0].1[0]
                        .glyphs
                        .values()
                        .all(|g| !g.instances.is_empty())
                );
                assert!(!capturing());
            });
        });
    }
    #[test]
    #[ignore = "Requires a working GPU adapter"]
    fn retained_gpu_frames_reuse_buffers_and_images_and_invalidate_correctly() {
        crate::async_io::runtime().unwrap().block_on(async {
            let instance = wgpu::Instance::default();
            let adapter = instance.request_adapter(&Default::default()).await.unwrap();
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
            let mut renderer = Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
            let mut mesh = egui::Mesh::default();
            mesh.add_colored_rect(
                egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(12., 12.)),
                Color32::RED,
            );
            let mut builder = Builder::default();
            builder.mesh_layer(&[mesh]);
            builder.image(
                inventory_kicad::Point::new(-15., 0.),
                inventory_kicad::Point::new(8., 8.),
                1,
                1,
                &[0, 255, 0, 255],
            );
            let ctx = egui::Context::default();
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    builder.capture(ui, false, |p| {
                        let g = super::super::cad_text::zoom_galley(
                            p,
                            "R1",
                            4. * REFERENCE,
                            128.,
                            Color32::WHITE,
                        );
                        p.add(egui::epaint::TextShape::new(
                            egui::pos2(-30. * REFERENCE, 20. * REFERENCE),
                            g,
                            Color32::WHITE,
                        ));
                    })
                });
            });
            let scene = builder.finish();
            let mut frame = Frame {
                base: scene,
                glow: None,
                highlight: None,
                center: inventory_kicad::Point::default(),
                scale: 2.,
                tint: Color32::WHITE,
                grayscale_base: false,
                glow_time: None,
                background: Color32::BLACK,
                size: egui::vec2(128., 128.),
                repaint: None,
            };
            let draw = |renderer: &mut Renderer, frame: &Frame, ppp: f32| {
                for _ in 0..1000 {
                    let mut encoder = device.create_command_encoder(&Default::default());
                    let ready =
                        renderer.prepare(frame, &device, &queue, &mut encoder, frame.size, ppp);
                    queue.submit([encoder.finish()]);
                    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    if ready {
                        return;
                    }
                }
                panic!("GPU upload did not finish");
            };
            draw(&mut renderer, &frame, 1.);
            assert_eq!(renderer.uploads, 3);
            let renders = renderer.renders;
            assert!(renders >= 1);
            draw(&mut renderer, &frame, 1.);
            assert_eq!(renderer.uploads, 3);
            assert_eq!(renderer.renders, renders);
            assert_eq!(renderer.hits, 1);
            frame.center.x = 10.;
            draw(&mut renderer, &frame, 1.);
            frame.scale = 3.;
            draw(&mut renderer, &frame, 1.);
            assert_eq!(renderer.uploads, 3);
            assert_eq!(renderer.renders, renders + 2);
            let mut highlight = Builder::default();
            highlight.line(
                &[
                    inventory_kicad::Point::new(0., 0.),
                    inventory_kicad::Point::new(12., 0.),
                ],
                0.,
                4.,
                Color32::WHITE,
                0.,
            );
            // Mark the line as a tintable net highlight.
            for (_, batches) in &mut highlight.groups {
                for b in batches {
                    for v in &mut Arc::get_mut(b).unwrap().vertices {
                        v.tint = 1.;
                    }
                }
            }
            frame.highlight = Some(highlight.finish());
            frame.tint = Color32::YELLOW;
            draw(&mut renderer, &frame, 1.);
            let uploads = renderer.uploads;
            frame.tint = Color32::from_rgb(220, 0, 220);
            draw(&mut renderer, &frame, 1.);
            assert_eq!(
                renderer.uploads, uploads,
                "blink must not upload highlight vertices"
            );
            frame.size = egui::vec2(160., 128.);
            draw(&mut renderer, &frame, 1.);
            draw(&mut renderer, &frame, 2.);
            assert_eq!(renderer.size, [320, 256]);
            assert_eq!(renderer.uploads, uploads);
            frame.highlight = None;
            frame.size = egui::vec2(128., 128.);
            frame.center = Default::default();
            frame.scale = 2.;
            draw(&mut renderer, &frame, 1.);
            // Pixel readback verifies layer compositing and bitmap sampling, not just counters.
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 512 * 128,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                renderer.output.as_ref().unwrap()._texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(512),
                        rows_per_image: Some(128),
                    },
                },
                wgpu::Extent3d {
                    width: 128,
                    height: 128,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            let (sender, receiver) = std::sync::mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| sender.send(r).unwrap());
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            receiver.recv().unwrap().unwrap();
            let pixels = buffer.slice(..).get_mapped_range();
            let pixel = |x: usize, y: usize| &pixels[y * 512 + x * 4..y * 512 + x * 4 + 4];
            assert_eq!(pixel(70, 70), [255, 0, 0, 255]);
            assert_eq!(pixel(34, 64), [0, 255, 0, 255]);
            assert!(
                (100..124).any(|y| (0..22).any(|x| pixel(x, y)[0] > 180 && pixel(x, y)[1] > 180)),
                "instanced text must render"
            );
            assert!(!renderer.glyph_buffers.is_empty());
            drop(pixels);
            buffer.unmap();
            let mut colored = Builder::default();
            let mut mesh = egui::Mesh::default();
            mesh.add_colored_rect(
                egui::Rect::from_min_max(egui::pos2(2., 2.), egui::pos2(4., 4.)),
                Color32::BLUE,
            );
            colored.mesh_layer(&[mesh]);
            frame.highlight = Some(colored.finish());
            frame.grayscale_base = true;
            let base_uploads = renderer.uploads;
            draw(&mut renderer, &frame, 1.);
            assert_eq!(
                renderer.uploads,
                base_uploads + 1,
                "selection must reuse base buffers"
            );
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                renderer.output.as_ref().unwrap()._texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(512),
                        rows_per_image: Some(128),
                    },
                },
                wgpu::Extent3d {
                    width: 128,
                    height: 128,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            let (sender, receiver) = std::sync::mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| sender.send(r).unwrap());
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            receiver.recv().unwrap().unwrap();
            let pixels = buffer.slice(..).get_mapped_range();
            let pixel = |x: usize, y: usize| &pixels[y * 512 + x * 4..y * 512 + x * 4 + 4];
            assert_eq!(
                pixel(70, 70),
                [0, 0, 255, 255],
                "selected net keeps its layer color"
            );
            let gray = pixel(80, 80);
            assert!(
                gray[0] > 0 && gray[0] == gray[1] && gray[1] == gray[2],
                "base copper is grayscale: {gray:?}"
            );
            let image_gray = pixel(34, 64);
            assert_eq!(image_gray[0], image_gray[1]);
            assert_eq!(image_gray[1], image_gray[2]);
            drop(pixels);
            buffer.unmap();
            frame.grayscale_base = false;
            frame.highlight = None;
            let mut glow = Builder::default();
            let mut mesh = egui::Mesh::default();
            mesh.add_colored_rect(
                egui::Rect::from_min_max(egui::pos2(-6., -6.), egui::pos2(-4., -4.)),
                Color32::from_rgba_unmultiplied(155, 60, 235, 180),
            );
            glow.mesh_layer(&[mesh]);
            frame.glow = Some(glow.finish());
            frame.glow_time = Some(0.);
            draw(&mut renderer, &frame, 1.);
            let sample = |renderer: &Renderer, x, y| {
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 256,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                let mut source = renderer.output.as_ref().unwrap()._texture.as_image_copy();
                source.origin = wgpu::Origin3d { x, y, z: 0 };
                encoder.copy_texture_to_buffer(
                    source,
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(256),
                            rows_per_image: Some(1),
                        },
                    },
                    wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit([encoder.finish()]);
                let (sender, receiver) = std::sync::mpsc::channel();
                buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| sender.send(r).unwrap());
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                receiver.recv().unwrap().unwrap();
                let bytes = buffer.slice(..).get_mapped_range();
                let pixel: [u8; 4] = bytes[..4].try_into().unwrap();
                drop(bytes);
                buffer.unmap();
                pixel
            };
            let before = sample(&renderer, 54, 54);
            let uploaded = renderer.uploads;
            frame.glow_time = Some(0.5);
            draw(&mut renderer, &frame, 1.);
            assert_ne!(
                before,
                sample(&renderer, 54, 54),
                "glow pixels pulse and shine over time"
            );
            assert_eq!(
                sample(&renderer, 70, 70),
                [255, 0, 0, 255],
                "animation affects only glow"
            );
            assert_eq!(
                uploaded, renderer.uploads,
                "animation must not reupload geometry"
            );
            frame.glow = None;
            frame.glow_time = None;
            let mut new = Builder::default();
            new.line(
                &[
                    inventory_kicad::Point::default(),
                    inventory_kicad::Point::new(1., 1.),
                ],
                0.,
                1.,
                Color32::WHITE,
                0.,
            );
            let old_id = frame.base.id;
            frame.base = new.finish();
            assert_ne!(frame.base.id, old_id);
            draw(&mut renderer, &frame, 1.);
            assert!(renderer.uploads > uploads);
            drop(frame);
            renderer.prune();
            assert!(renderer.uploaded.is_empty());
            assert!(renderer.glyph_buffers.is_empty());
            assert!(renderer.output.is_none() && renderer.mask.is_none());
        });
    }
}
