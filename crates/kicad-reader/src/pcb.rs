//! Portable KiCad 6+ board scene reader. All coordinates are millimetres.
use crate::{
    Bounds, Error, Point, Result,
    schematic::arc_points,
    syntax::{self, Expr},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PcbLayer {
    pub id: i32,
    pub name: String,
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PcbShape {
    Path {
        points: Vec<Point>,
        width: f64,
        filled: bool,
        triangles: Vec<[u32; 3]>,
    },
    Text {
        at: Point,
        text: String,
        size: f64,
        angle: f64,
        mirrored: bool,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PcbItem {
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub net: String,
    pub zone: bool,
    pub layers: Vec<String>,
    pub shape: PcbShape,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PcbVia {
    pub at: Point,
    pub size: f64,
    pub drill: f64,
    pub layers: Vec<String>,
    pub net: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PcbPad {
    #[serde(default)]
    pub reference: String,
    pub at: Point,
    pub size: Point,
    pub angle: f64,
    pub number: String,
    pub net: String,
    pub layers: Vec<String>,
    pub local_clearance: Option<f64>,
    pub footprint_clearance: Option<f64>,
    pub clearance: f64,
    pub clearance_source: String,
    pub contour: Vec<Point>,
    pub clearance_contour: Vec<Point>,
}
/// Footprint-local 3D model placement. Offsets are millimetres; rotations are degrees.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PcbModel {
    pub path: String,
    pub offset: [f64; 3],
    pub scale: [f64; 3],
    pub rotate: [f64; 3],
    pub hidden: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PcbFootprint {
    #[serde(default)]
    pub angle: f64,
    #[serde(default)]
    pub models: Vec<PcbModel>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    #[serde(default)]
    pub library: String,
    pub reference: String,
    pub layer: String,
    pub at: Point,
    pub bounds: Bounds,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pcb {
    /// Copper layer centre depths in mm, from the saved stackup (Z up).
    #[serde(default)]
    pub copper_depths: BTreeMap<String, f64>,
    #[serde(default = "default_thickness")]
    pub thickness: f64,
    /// Encoded KiCad embedded-file payloads, decoded by the model loader on demand.
    #[serde(default)]
    pub embedded_files: BTreeMap<String, String>,
    #[serde(default)]
    pub footprint_list: Vec<PcbFootprint>,
    #[serde(default)]
    pub pads: Vec<PcbPad>,
    #[serde(default)]
    pub net_names: BTreeMap<String, String>,
    #[serde(default)]
    pub vias: Vec<PcbVia>,
    pub layers: Vec<PcbLayer>,
    pub items: Vec<PcbItem>,
    pub holes: Vec<Vec<Point>>,
    pub bounds: Bounds,
    pub footprints: usize,
    pub warnings: Vec<String>,
}
fn default_thickness() -> f64 {
    1.6
}
fn xyz(e: &Expr, name: &str, default: f64) -> [f64; 3] {
    let xyz = e.child(name).and_then(|e| e.child("xyz"));
    [1, 2, 3].map(|i| xyz.and_then(|e| e.number(i)).unwrap_or(default))
}
fn n(e: &Expr, key: &str, i: usize, default: f64) -> f64 {
    e.child(key).and_then(|c| c.number(i)).unwrap_or(default)
}
fn p(e: &Expr, key: &str) -> Point {
    Point::new(n(e, key, 1, 0.), n(e, key, 2, 0.))
}
fn value<'a>(e: &'a Expr, key: &str) -> &'a str {
    e.child(key).and_then(|c| c.value(1)).unwrap_or("")
}
#[derive(Clone, Copy, Default)]
struct Transform {
    at: Point,
    angle: f64,
}
impl Transform {
    fn point(self, p: Point) -> Point {
        let a = self.angle.to_radians();
        Point::new(
            self.at.x + p.x * a.cos() + p.y * a.sin(),
            self.at.y - p.x * a.sin() + p.y * a.cos(),
        )
    }
}
fn oval(size: Point) -> Vec<Point> {
    let r = size.x.min(size.y) / 2.;
    let dx = (size.x / 2. - r).max(0.);
    let dy = (size.y / 2. - r).max(0.);
    (0..=64)
        .map(|i| {
            let a = i as f64 * std::f64::consts::TAU / 64.;
            Point::new(
                a.cos() * r + dx * a.cos().signum(),
                a.sin() * r + dy * a.sin().signum(),
            )
        })
        .collect()
}
impl Pcb {
    pub fn parse(source: &str) -> Result<Self> {
        let root = syntax::parse(source)?;
        if root.tag() != Some("kicad_pcb") {
            return Err(Error::new("Expected kicad_pcb board"));
        }
        let stackup: Vec<_> = root
            .child("setup")
            .and_then(|s| s.child("stackup"))
            .map(|s| s.children("layer").collect())
            .unwrap_or_default();
        let total: f64 = stackup.iter().map(|l| n(l, "thickness", 1, 0.)).sum();
        let mut depth = total / 2.;
        let mut copper_depths = BTreeMap::new();
        for layer in stackup {
            let thickness = n(layer, "thickness", 1, 0.);
            if let Some(name) = layer.value(1).filter(|s| s.ends_with(".Cu")) {
                copper_depths.insert(name.to_owned(), depth - thickness / 2.);
            }
            depth -= thickness;
        }
        let mut board = Self {
            copper_depths,
            thickness: root
                .child("general")
                .map(|e| n(e, "thickness", 1, 1.6))
                .unwrap_or(1.6)
                .clamp(0.1, 20.),
            embedded_files: root
                .child("embedded_files")
                .into_iter()
                .flat_map(|e| e.children("file"))
                .filter_map(|e| {
                    Some((
                        value(e, "name").to_owned(),
                        e.child("data")?
                            .items()
                            .iter()
                            .skip(1)
                            .filter_map(Expr::atom)
                            .collect::<String>(),
                    ))
                })
                .collect(),
            pads: vec![],
            net_names: root
                .children("net")
                .filter_map(|e| Some((e.value(1)?.to_owned(), e.value(2)?.to_owned())))
                .collect(),
            vias: vec![],
            layers: vec![],
            items: vec![],
            holes: vec![],
            bounds: Bounds::default(),
            footprints: 0,
            footprint_list: vec![],
            warnings: vec![],
        };
        if let Some(layers) = root.child("layers") {
            for layer in layers.items().iter().skip(1) {
                if let (Some(id), Some(name)) = (layer.number(0), layer.value(1)) {
                    board.layers.push(PcbLayer {
                        id: id as i32,
                        name: name.into(),
                        kind: layer.value(2).unwrap_or("").into(),
                    });
                }
            }
        }
        for e in root.items().iter().skip(1) {
            match e.tag().unwrap_or("") {
                "footprint" | "module" => {
                    board.footprints += 1;
                    let t = Transform {
                        at: p(e, "at"),
                        angle: n(e, "at", 3, 0.),
                    };
                    let reference = e
                        .children("property")
                        .find(|p| p.value(1) == Some("Reference"))
                        .and_then(|p| p.value(2))
                        .or_else(|| {
                            e.children("fp_text")
                                .find(|text| text.value(1) == Some("reference"))
                                .and_then(|text| text.value(2))
                        })
                        .unwrap_or("");
                    let val = e
                        .children("property")
                        .find(|p| p.value(1) == Some("Value"))
                        .and_then(|p| p.value(2))
                        .unwrap_or("");
                    let first_item = board.items.len();
                    for child in e.items() {
                        if child.tag() == Some("pad") {
                            board.pad(
                                child,
                                t,
                                reference,
                                local_clearance(e, n(&root, "version", 1, 0.)),
                                n(&root, "version", 1, 0.),
                            );
                        } else {
                            board.graphic(child, t, reference, val);
                        }
                    }
                    let mut min = Point::new(f64::INFINITY, f64::INFINITY);
                    let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
                    for item in &mut board.items[first_item..] {
                        item.reference = reference.into();
                        if let PcbShape::Path { points, width, .. } = &item.shape {
                            for point in points {
                                min.x = min.x.min(point.x - width / 2.);
                                min.y = min.y.min(point.y - width / 2.);
                                max.x = max.x.max(point.x + width / 2.);
                                max.y = max.y.max(point.y + width / 2.);
                            }
                        }
                    }
                    if !min.x.is_finite() {
                        min = Point::new(t.at.x - 1., t.at.y - 1.);
                        max = Point::new(t.at.x + 1., t.at.y + 1.);
                    }
                    board.footprint_list.push(PcbFootprint {
                        angle: t.angle,
                        models: e
                            .children("model")
                            .filter_map(|m| {
                                Some(PcbModel {
                                    path: m.value(1)?.to_owned(),
                                    offset: xyz(m, "offset", 0.),
                                    scale: xyz(m, "scale", 1.),
                                    rotate: xyz(m, "rotate", 0.),
                                    hidden: m
                                        .child("hide")
                                        .is_some_and(|hide| hide.value(1) != Some("no")),
                                })
                            })
                            .collect(),
                        library: e.value(1).unwrap_or("").into(),
                        properties: e
                            .children("property")
                            .filter_map(|p| Some((p.value(1)?.to_owned(), p.value(2)?.to_owned())))
                            .collect(),
                        reference: reference.into(),
                        layer: value(e, "layer").into(),
                        at: t.at,
                        bounds: Bounds { min, max },
                    });
                }
                "via" => {
                    let at = p(e, "at");
                    let size = n(e, "size", 1, 0.6);
                    let copper: Vec<_> = board
                        .layers
                        .iter()
                        .filter(|l| l.name.ends_with(".Cu"))
                        .collect();
                    let ends = board.item_layers(e);
                    let first = copper
                        .iter()
                        .position(|l| ends.contains(&l.name))
                        .unwrap_or(0);
                    let last = copper
                        .iter()
                        .rposition(|l| ends.contains(&l.name))
                        .unwrap_or(copper.len().saturating_sub(1));
                    let layers = copper
                        .iter()
                        .skip(first)
                        .take(last.saturating_sub(first) + 1)
                        .map(|l| l.name.clone())
                        .collect();
                    let net = value(e, "net");
                    let net = root
                        .children("net")
                        .find(|n| n.value(1) == Some(net))
                        .and_then(|n| n.value(2))
                        .unwrap_or(net)
                        .to_owned();
                    board.vias.push(PcbVia {
                        at,
                        size,
                        drill: n(e, "drill", 1, 0.3),
                        layers,
                        net,
                    });
                }
                "zone" => {
                    for poly in e.children("filled_polygon") {
                        let layer = value(poly, "layer");
                        let layers = if layer.is_empty() {
                            board.item_layers(e)
                        } else {
                            vec![layer.into()]
                        };
                        let before = board.items.len();
                        board.path(
                            poly.child("pts")
                                .map(|pts| {
                                    pts.children("xy")
                                        .map(|v| {
                                            Point::new(
                                                v.number(1).unwrap_or(0.),
                                                v.number(2).unwrap_or(0.),
                                            )
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                            0.,
                            true,
                            layers,
                        );
                        if board.items.len() > before {
                            let net = board.net(e);
                            let item = board.items.last_mut().unwrap();
                            item.zone = true;
                            item.net = net;
                        }
                    }
                }
                _ => board.graphic(e, Transform::default(), "", ""),
            }
        }
        // Older standalone board files can embed net classes directly.
        for pad in &mut board.pads {
            if pad.local_clearance.is_some() || pad.footprint_clearance.is_some() {
                continue;
            }
            let class = root
                .children("net_class")
                .find(|c| c.children("add_net").any(|n| n.value(1) == Some(&pad.net)))
                .or_else(|| {
                    root.children("net_class")
                        .find(|c| c.value(1) == Some("Default"))
                });
            if let Some(class) = class
                && let Some(v) = class
                    .child("clearance")
                    .and_then(|c| c.number(1))
                    .filter(|v| *v >= 0.)
            {
                pad.clearance = v;
                pad.clearance_source = format!("Net class: {}", class.value(1).unwrap_or(""));
                pad.clearance_contour = offset_convex(&pad.contour, v);
            }
        }
        // Board outline is the useful fit region; remote fabrication notes should not shrink it.
        let outline: Vec<_> = board
            .items
            .iter()
            .filter(|i| i.layers.iter().any(|l| l == "Edge.Cuts"))
            .collect();
        let items: Vec<_> = if outline.is_empty() {
            board.items.iter().collect()
        } else {
            outline
        };
        let points: Vec<_> = items
            .iter()
            .flat_map(|i| match &i.shape {
                PcbShape::Path { points, .. } => points.clone(),
                PcbShape::Text { at, .. } => vec![*at],
            })
            .collect();
        if let Some(first) = points.first() {
            let mut bounds = Bounds {
                min: *first,
                max: *first,
            };
            for p in points {
                bounds.min.x = bounds.min.x.min(p.x);
                bounds.min.y = bounds.min.y.min(p.y);
                bounds.max.x = bounds.max.x.max(p.x);
                bounds.max.y = bounds.max.y.max(p.y);
            }
            board.bounds = bounds;
        }
        Ok(board)
    }
    fn item_layers(&self, e: &Expr) -> Vec<String> {
        let names: Vec<_> = if let Some(l) = e.child("layers") {
            l.items().iter().skip(1).filter_map(Expr::atom).collect()
        } else {
            vec![value(e, "layer")]
        };
        self.layers
            .iter()
            .filter(|l| {
                names.iter().any(|n| {
                    *n == l.name
                        || *n == "*.Cu" && l.name.ends_with(".Cu")
                        || *n == "*.Mask" && l.name.ends_with(".Mask")
                        || *n == "F&B.Cu" && matches!(l.name.as_str(), "F.Cu" | "B.Cu")
                })
            })
            .map(|l| l.name.clone())
            .collect()
    }
    fn path(&mut self, points: Vec<Point>, width: f64, filled: bool, layers: Vec<String>) {
        if points.len() >= 2 && !layers.is_empty() {
            let triangles = if filled { triangulate(&points) } else { vec![] };
            if filled && triangles.is_empty() {
                self.warnings.push(format!(
                    "Could not triangulate filled polygon ({} vertices)",
                    points.len()
                ));
            }
            self.items.push(PcbItem {
                reference: String::new(),
                net: String::new(),
                zone: false,
                layers,
                shape: PcbShape::Path {
                    points,
                    width,
                    filled,
                    triangles,
                },
            });
        }
    }
    fn graphic(&mut self, e: &Expr, t: Transform, reference: &str, val: &str) {
        let tag = e.tag().unwrap_or("");
        let layers = self.item_layers(e);
        let width = n(e, "stroke", 1, 0.); // modern stroke stores a nested width
        let width = e
            .child("stroke")
            .map(|s| n(s, "width", 1, 0.15))
            .unwrap_or_else(|| n(e, "width", 1, width.max(0.15)));
        let filled = matches!(value(e, "fill"), "solid" | "yes");
        let points = match tag {
            "segment" | "gr_line" | "fp_line" => vec![p(e, "start"), p(e, "end")],
            "arc" | "gr_arc" | "fp_arc" => {
                if e.child("mid").is_some() {
                    arc_points(p(e, "start"), p(e, "mid"), p(e, "end"))
                } else {
                    self.warnings
                        .push("Legacy arc without midpoint is unsupported".into());
                    vec![]
                }
            }
            "gr_rect" | "fp_rect" => {
                let a = p(e, "start");
                let b = p(e, "end");
                vec![a, Point::new(b.x, a.y), b, Point::new(a.x, b.y), a]
            }
            "gr_circle" | "fp_circle" => {
                let c = p(e, "center");
                let end = p(e, "end");
                let r = (end.x - c.x).hypot(end.y - c.y);
                oval(Point::new(2. * r, 2. * r))
                    .into_iter()
                    .map(|p| Point::new(c.x + p.x, c.y + p.y))
                    .collect()
            }
            "gr_poly" | "fp_poly" => e
                .child("pts")
                .map(|pts| {
                    pts.children("xy")
                        .map(|v| Point::new(v.number(1).unwrap_or(0.), v.number(2).unwrap_or(0.)))
                        .collect()
                })
                .unwrap_or_default(),
            "gr_curve" | "fp_curve" => {
                let pts: Vec<_> = e
                    .child("pts")
                    .map(|p| {
                        p.children("xy")
                            .map(|v| {
                                Point::new(v.number(1).unwrap_or(0.), v.number(2).unwrap_or(0.))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if pts.len() == 4 {
                    (0..=48)
                        .map(|i| {
                            let a = i as f64 / 48.;
                            let b = 1. - a;
                            Point::new(
                                b * b * b * pts[0].x
                                    + 3. * b * b * a * pts[1].x
                                    + 3. * b * a * a * pts[2].x
                                    + a * a * a * pts[3].x,
                                b * b * b * pts[0].y
                                    + 3. * b * b * a * pts[1].y
                                    + 3. * b * a * a * pts[2].y
                                    + a * a * a * pts[3].y,
                            )
                        })
                        .collect()
                } else {
                    vec![]
                }
            }
            "gr_text" | "fp_text" | "property" => {
                if e.has_atom("hide") || value(e, "hide") == "yes" || layers.is_empty() {
                    return;
                }
                let effects = e.child("effects");
                let font = effects.and_then(|e| e.child("font"));
                if effects.is_some_and(|e| e.has_atom("hide") || value(e, "hide") == "yes") {
                    return;
                }
                let text = if tag == "fp_text" {
                    e.value(2)
                } else if tag == "property" {
                    if !matches!(e.value(1), Some("Reference" | "Value")) {
                        return;
                    }
                    e.value(2)
                } else {
                    e.value(1)
                }
                .unwrap_or("")
                .replace("${REFERENCE}", reference)
                .replace("${VALUE}", val);
                // KiCad's outline-font cache is already in board coordinates, including
                // justification, font weight, rotation and mirroring. Do not apply the
                // footprint transform again. Ignore stale caches after text substitution.
                if let Some(cache) = e.child("render_cache")
                    && cache.value(1) == Some(text.as_str())
                {
                    let polygons: Vec<Vec<Point>> = cache
                        .children("polygon")
                        .filter_map(|polygon| polygon.child("pts"))
                        .map(|pts| {
                            pts.children("xy")
                                .filter_map(|xy| Some(Point::new(xy.number(1)?, xy.number(2)?)))
                                .collect::<Vec<_>>()
                        })
                        .filter(|points| points.len() >= 3)
                        .collect();
                    if !polygons.is_empty() {
                        for points in polygons {
                            self.path(points, 0., true, layers.clone());
                        }
                        return;
                    }
                }
                self.items.push(PcbItem {
                    reference: String::new(),
                    net: String::new(),
                    zone: false,
                    layers,
                    shape: PcbShape::Text {
                        at: t.point(p(e, "at")),
                        text,
                        size: font.map(|f| n(f, "size", 1, 1.)).unwrap_or(1.),
                        angle: n(e, "at", 3, 0.),
                        mirrored: effects
                            .and_then(|e| e.child("justify"))
                            .is_some_and(|e| e.has_atom("mirror")),
                    },
                });
                return;
            }
            _ => {
                if tag.starts_with("gr_") || tag.starts_with("fp_") || tag == "dimension" {
                    self.warnings
                        .push(format!("Unsupported PCB graphic: {tag}"));
                }
                return;
            }
        };
        let net = self.net(e);
        let before = self.items.len();
        self.path(
            points.into_iter().map(|p| t.point(p)).collect(),
            width,
            filled,
            layers,
        );
        if self.items.len() > before && matches!(tag, "segment" | "arc") {
            self.items.last_mut().unwrap().net = net;
        }
    }
    fn net(&self, e: &Expr) -> String {
        let name = value(e, "net");
        self.net_names
            .get(name)
            .map(String::as_str)
            .unwrap_or(name)
            .into()
    }
    fn pad(
        &mut self,
        e: &Expr,
        t: Transform,
        reference: &str,
        footprint_clearance: Option<f64>,
        version: f64,
    ) {
        let first_item = self.items.len();
        let anchor = t.point(p(e, "at"));
        let size = p(e, "size");
        // Pad positions are footprint-local, but saved pad orientation is absolute.
        // An omitted angle is zero, even when the footprint itself is rotated.
        let angle = n(e, "at", 3, 0.);
        let drill = e.child("drill");
        // KiCad stores the copper offset inside the drill clause, even for SMD pads.
        // The hole itself stays at the pad anchor.
        let anchor_transform = Transform { at: anchor, angle };
        let at = anchor_transform.point(drill.map(|drill| p(drill, "offset")).unwrap_or_default());
        let transform = Transform { at, angle };
        let shape = e.value(3).unwrap_or("rect");
        let points = match shape {
            "circle" | "oval" => oval(size),
            "roundrect" => {
                let r = n(e, "roundrect_rratio", 1, 0.25) * size.x.min(size.y);
                let mut pts = vec![];
                for (x, y, start) in [
                    (size.x / 2. - r, size.y / 2. - r, 0.),
                    (-size.x / 2. + r, size.y / 2. - r, 90.),
                    (-size.x / 2. + r, -size.y / 2. + r, 180.),
                    (size.x / 2. - r, -size.y / 2. + r, 270.),
                ] {
                    for i in 0..=8 {
                        let a = (start + i as f64 * 90. / 8.).to_radians();
                        pts.push(Point::new(x + r * a.cos(), y + r * a.sin()));
                    }
                }
                pts
            }
            _ => {
                let d = p(e, "rect_delta");
                vec![
                    Point::new(-size.x / 2. - d.y / 2., -size.y / 2. + d.x / 2.),
                    Point::new(size.x / 2. + d.y / 2., -size.y / 2. - d.x / 2.),
                    Point::new(size.x / 2. - d.y / 2., size.y / 2. + d.x / 2.),
                    Point::new(-size.x / 2. + d.y / 2., size.y / 2. - d.x / 2.),
                ]
            }
        };
        let layers = self.item_layers(e);
        self.path(
            points.into_iter().map(|p| transform.point(p)).collect(),
            0.,
            true,
            layers,
        );
        if shape == "custom"
            && let Some(primitives) = e.child("primitives")
        {
            for primitive in primitives.items().iter().skip(1) {
                let mut primitive = primitive.clone();
                if let Expr::List(items) = &mut primitive {
                    items.push(Expr::List(
                        std::iter::once(Expr::Atom("layers".into()))
                            .chain(self.item_layers(e).into_iter().map(Expr::Atom))
                            .collect(),
                    ));
                }
                self.graphic(&primitive, transform, "", "");
            }
        }
        let net = self.net(e);
        for item in &mut self.items[first_item..] {
            item.net = net.clone();
        }
        let mut points = vec![];
        for item in &self.items[first_item..] {
            if let PcbShape::Path { points: p, .. } = &item.shape {
                points.extend(p.iter().copied());
            }
        }
        let contour = convex_hull(points);
        let local = local_clearance(e, version);
        let clearance = local.or(footprint_clearance).unwrap_or(0.2);
        if shape == "custom" {
            self.warnings
                .push("Custom pad clearance uses an approximate convex hull".into());
        }
        self.pads.push(PcbPad {
            reference: reference.into(),
            at,
            size,
            angle,
            number: e.value(1).unwrap_or("").into(),
            net: self.net(e),
            layers: self.item_layers(e),
            local_clearance: local,
            footprint_clearance,
            clearance,
            clearance_source: if local.is_some() {
                "Pad"
            } else if footprint_clearance.is_some() {
                "Footprint"
            } else {
                "Default (no project rules)"
            }
            .into(),
            clearance_contour: offset_convex(&contour, clearance),
            contour,
        });
        if let Some(drill) = drill {
            let oval_drill = drill.has_atom("oval");
            let i = if oval_drill { 2 } else { 1 };
            let w = drill.number(i).unwrap_or(0.);
            let h = if oval_drill {
                drill.number(i + 1).unwrap_or(w)
            } else {
                w
            };
            if w > 0. {
                self.holes.push(
                    oval(Point::new(w, h))
                        .into_iter()
                        .map(|p| anchor_transform.point(p))
                        .collect(),
                );
            }
        }
    }
}

// Ear clipping is performed while parsing on the background worker, never per frame.
fn triangulate(points: &[Point]) -> Vec<[u32; 3]> {
    let len = points.len() - usize::from(points.len() > 1 && points.first() == points.last());
    if !(3..=32768).contains(&len) {
        return vec![];
    }
    let cross =
        |a: Point, b: Point, c: Point| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let area: f64 = (0..len)
        .map(|i| points[i].x * points[(i + 1) % len].y - points[(i + 1) % len].x * points[i].y)
        .sum();
    let direction = if area >= 0. { 1. } else { -1. };
    if (0..len).all(|i| {
        cross(points[i], points[(i + 1) % len], points[(i + 2) % len]) * direction >= -1e-10
    }) {
        return (1..len - 1)
            .map(|i| [0, i as u32, (i + 1) as u32])
            .collect();
    }
    let mut ids: Vec<usize> = (0..len).collect();
    let mut triangles = vec![];
    while ids.len() > 2 {
        let mut clipped = false;
        for i in 0..ids.len() {
            let a = ids[(i + ids.len() - 1) % ids.len()];
            let b = ids[i];
            let c = ids[(i + 1) % ids.len()];
            let turn = cross(points[a], points[b], points[c]) * direction;
            if turn.abs() < 1e-10 {
                ids.remove(i);
                clipped = true;
                break;
            }
            if turn < 0. {
                continue;
            }
            if ids.iter().any(|&j| {
                j != a
                    && j != b
                    && j != c
                    && points[j] != points[a]
                    && points[j] != points[b]
                    && points[j] != points[c]
                    && cross(points[a], points[b], points[j]) * direction >= 0.
                    && cross(points[b], points[c], points[j]) * direction >= 0.
                    && cross(points[c], points[a], points[j]) * direction >= 0.
            }) {
                continue;
            }
            triangles.push([a as u32, b as u32, c as u32]);
            ids.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            return vec![];
        }
    }
    triangles
}

fn local_clearance(e: &Expr, version: f64) -> Option<f64> {
    let v = e.child("clearance")?.number(1)?;
    (v >= 0. && (v > 0. || version >= 20241229.)).then_some(v)
}
fn convex_hull(mut points: Vec<Point>) -> Vec<Point> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross =
        |a: Point, b: Point, c: Point| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let mut lower: Vec<Point> = vec![];
    for p in &points {
        while lower.len() > 1 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 1e-12
        {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<Point> = vec![];
    for p in points.iter().rev() {
        while upper.len() > 1 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 1e-12
        {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}
fn offset_convex(points: &[Point], clearance: f64) -> Vec<Point> {
    if points.len() < 3 || clearance <= 0. {
        return points.to_vec();
    }
    let mut out = vec![];
    for i in 0..points.len() {
        let a = points[(i + points.len() - 1) % points.len()];
        let b = points[i];
        let c = points[(i + 1) % points.len()];
        let start = (b.y - a.y).atan2(b.x - a.x) - std::f64::consts::FRAC_PI_2;
        let end = (c.y - b.y).atan2(c.x - b.x) - std::f64::consts::FRAC_PI_2;
        let sweep = (end - start).rem_euclid(std::f64::consts::TAU);
        let steps = (sweep / (std::f64::consts::PI / 16.)).ceil().max(1.) as usize;
        for step in 0..=steps {
            let angle = start + sweep * step as f64 / steps as f64;
            out.push(Point::new(
                b.x + clearance * angle.cos(),
                b.y + clearance * angle.sin(),
            ));
        }
    }
    out
}
pub(crate) fn wildcard(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
fn json_number(v: Option<&crate::json::Value>) -> Option<f64> {
    if let Some(crate::json::Value::Number(n)) = v {
        Some(*n)
    } else {
        None
    }
}
impl Pcb {
    /// Resolve project net-class clearances after reading standalone board geometry.
    /// This does not evaluate custom .kicad_dru condition expressions.
    pub fn apply_project_settings(&mut self, source: &str) -> Result<()> {
        let project = crate::json::parse(source)?;
        let settings = project.get("net_settings");
        let mut classes: Vec<_> = settings
            .and_then(|s| s.get("classes"))
            .map(|c| c.array().to_vec())
            .unwrap_or_default();
        classes.sort_by(|a, b| {
            json_number(a.get("priority"))
                .unwrap_or(f64::MAX)
                .total_cmp(&json_number(b.get("priority")).unwrap_or(f64::MAX))
        });
        let default = classes
            .iter()
            .find(|c| c.get("name").and_then(crate::json::Value::string) == Some("Default"))
            .and_then(|c| json_number(c.get("clearance")))
            .filter(|v| *v >= 0.);
        let minimum = project
            .get("board")
            .and_then(|b| b.get("design_settings"))
            .and_then(|s| s.get("rules"))
            .and_then(|r| json_number(r.get("min_clearance")))
            .unwrap_or(0.);
        for pad in &mut self.pads {
            let mut assigned = vec![];
            if let Some(v) = settings
                .and_then(|s| s.get("netclass_assignments"))
                .and_then(|s| s.get(&pad.net))
            {
                if let Some(name) = v.string() {
                    assigned.push(name.to_owned());
                }
                for name in v.array().iter().filter_map(crate::json::Value::string) {
                    assigned.push(name.into());
                }
            }
            if let Some(patterns) = settings.and_then(|s| s.get("netclass_patterns")) {
                for pattern in patterns.array() {
                    if let (Some(pattern), Some(class)) = (
                        pattern.get("pattern").and_then(crate::json::Value::string),
                        pattern.get("netclass").and_then(crate::json::Value::string),
                    ) && wildcard(pattern, &pad.net)
                    {
                        assigned.push(class.into());
                    }
                }
            }
            let class = classes.iter().find(|c| {
                c.get("name")
                    .and_then(crate::json::Value::string)
                    .is_some_and(|name| assigned.iter().any(|s| s == name))
                    && json_number(c.get("clearance")).is_some_and(|v| v >= 0.)
            });
            let resolved = pad
                .local_clearance
                .map(|v| (v, "Pad".to_owned()))
                .or_else(|| pad.footprint_clearance.map(|v| (v, "Footprint".to_owned())))
                .or_else(|| {
                    class.map(|c| {
                        (
                            json_number(c.get("clearance")).unwrap().max(minimum),
                            format!(
                                "Net class: {}",
                                c.get("name")
                                    .and_then(crate::json::Value::string)
                                    .unwrap_or("")
                            ),
                        )
                    })
                })
                .or_else(|| default.map(|v| (v.max(minimum), "Net class: Default".into())));
            if let Some((clearance, source)) = resolved {
                pad.clearance = clearance;
                pad.clearance_source = source;
                pad.clearance_contour = offset_convex(&pad.contour, clearance);
            }
        }
        Ok(())
    }
}

/// Read project-local footprint-library URIs without performing filesystem I/O.
pub fn footprint_model_libraries(source: &str) -> Result<BTreeMap<String, String>> {
    let root = syntax::parse(source)?;
    if root.tag() != Some("fp_lib_table") {
        return Err(Error::new("Expected fp_lib_table"));
    }
    Ok(root
        .children("lib")
        .filter_map(|lib| {
            let name = value(lib, "name");
            let uri = value(lib, "uri");
            (!name.is_empty() && !uri.is_empty()).then(|| (name.to_owned(), uri.to_owned()))
        })
        .collect())
}
