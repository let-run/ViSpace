use crate::{
    Error, Result,
    syntax::{self, Expr},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    fn plus(self, p: Self) -> Self {
        Self::new(self.x + p.x, self.y + p.y)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub min: Point,
    pub max: Point,
}
impl Default for Bounds {
    fn default() -> Self {
        Self {
            min: Point::new(0., 0.),
            max: Point::new(297., 210.),
        }
    }
}
impl Bounds {
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }
    pub fn width(&self) -> f64 {
        self.max.x - self.min.x
    }
    pub fn height(&self) -> f64 {
        self.max.y - self.min.y
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Ink {
    Wire,
    Bus,
    Symbol,
    Pin,
    Text,
    Note,
    Label,
    Sheet,
    Paper,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Primitive {
    TextBox {
        at: Point,
        size: Point,
        angle: f64,
        text: String,
        font_size: f64,
        align: [i8; 2],
        bold: bool,
        italic: bool,
        margins: [f64; 4],
        stroke_width: f64,
        stroke_color: [u8; 4],
        fill_color: Option<[u8; 4]>,
    },
    Bitmap {
        center: Point,
        size: Point,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Path {
        points: Vec<Point>,
        width: f64,
        ink: Ink,
        fill: bool,
        solid: bool,
        #[serde(default)]
        fill_color: Option<[u8; 4]>,
        #[serde(default)]
        stroke_color: Option<[u8; 4]>,
    },
    Dot {
        center: Point,
        radius: f64,
        ink: Ink,
    },
    Text {
        at: Point,
        text: String,
        size: f64,
        angle: f64,
        align: [i8; 2],
        ink: Ink,
        bold: bool,
        instances: BTreeMap<String, String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SheetRef {
    #[serde(default)]
    pub excluded_from_bom: bool,
    pub uuid: String,
    pub name: String,
    pub file: String,
    pub bounds: Bounds,
}
/// Physical symbol information, separate from drawing primitives and unit suffixes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Component {
    /// Pin number to its electrical drawing primitive, for PCB cross-selection.
    #[serde(default)]
    pub pins: BTreeMap<String, usize>,
    #[serde(default)]
    pub primitives: Vec<usize>,
    #[serde(default)]
    pub at: Point,
    pub uuid: String,
    pub lib_id: String,
    pub reference: String,
    pub references: BTreeMap<String, String>,
    pub properties: BTreeMap<String, String>,
    pub in_bom: bool,
    pub dnp: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Schematic {
    #[serde(default)]
    pub nets: Vec<crate::nets::Net>,
    #[serde(skip)]
    connections: Vec<crate::nets::Connection>,
    pub version: String,
    pub uuid: String,
    pub title: String,
    pub paper: Point,
    pub primitives: Vec<Primitive>,
    pub sheets: Vec<SheetRef>,
    pub bounds: Bounds,
    pub warnings: Vec<String>,
    pub symbol_count: usize,
    #[serde(default)]
    pub components: Vec<Component>,
}
fn number(e: &Expr, tag: &str, index: usize, default: f64) -> f64 {
    e.child(tag)
        .and_then(|c| c.number(index))
        .unwrap_or(default)
}
fn value<'a>(e: &'a Expr, tag: &'a str) -> &'a str {
    e.child(tag).and_then(|c| c.value(1)).unwrap_or("")
}
fn xy(e: &Expr) -> Point {
    Point::new(e.number(1).unwrap_or(0.), e.number(2).unwrap_or(0.))
}
fn at(e: &Expr) -> Point {
    e.child("at").map(xy).unwrap_or_default()
}
fn hidden(e: &Expr) -> bool {
    e.has_atom("hide")
        || e.child("hide").is_some_and(|n| n.value(1) != Some("no"))
        || e.child("effects").is_some_and(hidden)
}
fn width(e: &Expr) -> f64 {
    e.child("stroke")
        .map(|s| number(s, "width", 1, 0.15))
        .unwrap_or(0.15)
        .max(0.1)
}
#[derive(Clone, Copy)]
struct Transform {
    origin: Point,
    angle: f64,
    library: bool,
    mirror_x: bool,
    mirror_y: bool,
}
impl Transform {
    fn sheet() -> Self {
        Self {
            origin: Point::default(),
            angle: 0.,
            library: false,
            mirror_x: false,
            mirror_y: false,
        }
    }
    fn point(self, p: Point) -> Point {
        if !self.library {
            return p.plus(self.origin);
        }
        let (sin, cos) = self.angle.to_radians().sin_cos();
        // KiCad mirrors in the placed symbol's axes, after rotation.
        let x = cos * p.x - sin * p.y;
        let y = -(sin * p.x + cos * p.y);
        self.origin.plus(Point::new(
            if self.mirror_y { -x } else { x },
            if self.mirror_x { -y } else { y },
        ))
    }
}
impl Schematic {
    pub fn parse(source: &str) -> Result<Self> {
        let root = syntax::parse(source)?;
        if root.tag() != Some("kicad_sch") {
            return Err(Error::new(
                "Expected a modern .kicad_sch file (KiCad 6 or newer)",
            ));
        }
        let version = value(&root, "version").to_owned();
        if version.is_empty() {
            return Err(Error::new("Schematic version is missing"));
        }
        let mut scene = Self {
            nets: vec![],
            connections: vec![],
            version,
            uuid: value(&root, "uuid").into(),
            title: root
                .child("title_block")
                .map(|b| value(b, "title").into())
                .unwrap_or_default(),
            paper: paper(&root),
            primitives: vec![],
            sheets: vec![],
            bounds: Bounds::default(),
            warnings: vec![],
            symbol_count: 0,
            components: vec![],
        };
        let definitions: BTreeMap<_, _> = root
            .child("lib_symbols")
            .map(|lib| {
                lib.children("symbol")
                    .filter_map(|s| s.value(1).map(|name| (name, s)))
                    .collect()
            })
            .unwrap_or_default();
        for node in root.items().iter().skip(1) {
            let first = scene.primitives.len();
            match node.tag().unwrap_or("") {
                "wire" | "bus" | "polyline" | "rectangle" | "circle" | "arc" | "bezier" => scene
                    .graphic(
                        node,
                        Transform::sheet(),
                        if node.tag() == Some("wire") {
                            Ink::Wire
                        } else if node.tag() == Some("bus") {
                            Ink::Bus
                        } else {
                            Ink::Symbol
                        },
                    ),
                "junction" => scene.primitives.push(Primitive::Dot {
                    center: at(node),
                    radius: number(node, "diameter", 1, 0.9).max(0.9) / 2.,
                    ink: Ink::Wire,
                }),
                "no_connect" => scene.cross(at(node), 0.6, Ink::Pin),
                "bus_entry" => {
                    let start = at(node);
                    let size = node.child("size").map(xy).unwrap_or_default();
                    scene.path(vec![start, start.plus(size)], 0.25, Ink::Bus, false);
                }
                "text_box" => scene.text_box(node),
                "text" => scene.text(
                    node,
                    node.value(1).unwrap_or(""),
                    Transform::sheet(),
                    Ink::Note,
                    BTreeMap::new(),
                ),
                "label" | "global_label" | "hierarchical_label" => {
                    if node.tag() == Some("label") {
                        scene.text(
                            node,
                            node.value(1).unwrap_or(""),
                            Transform::sheet(),
                            Ink::Label,
                            BTreeMap::new(),
                        );
                    } else {
                        scene.global_label(node);
                    }
                }
                "symbol" => {
                    scene.symbol_count += 1;
                    scene.symbol(node, &definitions);
                }
                "sheet" => scene.sheet(node),
                "image" => scene.bitmap(node),
                "version" | "generator" | "generator_version" | "uuid" | "paper"
                | "title_block" | "lib_symbols" | "sheet_instances" | "symbol_instances"
                | "embedded_fonts" | "embedded_files" | "bus_alias" => {}
                "" => {}
                other => scene.warn(&format!("Unsupported schematic graphic: {other}")),
            }
            let tag = node.tag().unwrap_or("");
            if matches!(
                tag,
                "wire" | "junction" | "label" | "global_label" | "hierarchical_label"
            ) {
                let points = if tag == "wire" {
                    node.child("pts")
                        .map(|p| p.children("xy").map(xy).collect())
                        .unwrap_or_default()
                } else {
                    vec![at(node)]
                };
                let name = matches!(tag, "label" | "global_label" | "hierarchical_label")
                    .then(|| node.value(1).unwrap_or("").to_owned());
                scene.connections.push(crate::nets::Connection {
                    points,
                    port: match tag {
                        "global_label" => name.as_ref().map(|n| format!("g:{n}")),
                        "hierarchical_label" => name.as_ref().map(|n| format!("h:{n}")),
                        _ => None,
                    },
                    name,
                    primitives: (first..scene.primitives.len()).collect(),
                });
            }
        }
        // Electrical anchors, rather than visual intersections, define connectivity.
        scene.nets = crate::nets::build(&scene.connections);
        scene.bounds = scene.content_bounds();
        Ok(scene)
    }
    fn warn(&mut self, message: &str) {
        if !self.warnings.iter().any(|w| w == message) {
            self.warnings.push(message.into());
        }
    }
    fn path(&mut self, points: Vec<Point>, width: f64, ink: Ink, fill: bool) {
        if points.len() >= 2 {
            self.primitives.push(Primitive::Path {
                points,
                width,
                ink,
                fill,
                solid: false,
                fill_color: None,
                stroke_color: None,
            });
        }
    }
    fn cross(&mut self, p: Point, r: f64, ink: Ink) {
        self.path(
            vec![p.plus(Point::new(-r, -r)), p.plus(Point::new(r, r))],
            0.15,
            ink,
            false,
        );
        self.path(
            vec![p.plus(Point::new(-r, r)), p.plus(Point::new(r, -r))],
            0.15,
            ink,
            false,
        );
    }
    fn graphic(&mut self, e: &Expr, t: Transform, ink: Ink) {
        let fill = e
            .child("fill")
            .is_some_and(|f| matches!(value(f, "type"), "background" | "outline" | "color"));
        let points = match e.tag().unwrap_or("") {
            "wire" | "bus" | "polyline" => e
                .child("pts")
                .map(|pts| pts.children("xy").map(xy).collect())
                .unwrap_or_default(),
            "rectangle" => {
                let a = e.child("start").map(xy).unwrap_or_default();
                let b = e.child("end").map(xy).unwrap_or_default();
                vec![a, Point::new(b.x, a.y), b, Point::new(a.x, b.y), a]
            }
            "circle" => {
                let center = e.child("center").map(xy).unwrap_or_default();
                let radius = number(e, "radius", 1, 0.);
                (0..=64)
                    .map(|i| {
                        let angle = i as f64 * std::f64::consts::TAU / 64.;
                        center.plus(Point::new(radius * angle.cos(), radius * angle.sin()))
                    })
                    .collect()
            }
            "arc" => {
                let a = e.child("start").map(xy).unwrap_or_default();
                let b = e.child("mid").map(xy).unwrap_or_default();
                let c = e.child("end").map(xy).unwrap_or_default();
                arc_points(a, b, c)
            }
            "bezier" => {
                let p: Vec<_> = e
                    .child("pts")
                    .map(|pts| pts.children("xy").map(xy).collect())
                    .unwrap_or_default();
                if p.len() == 4 {
                    (0..=48)
                        .map(|i| {
                            let a = i as f64 / 48.;
                            let b = 1. - a;
                            Point::new(
                                b.powi(3) * p[0].x
                                    + 3. * b * b * a * p[1].x
                                    + 3. * b * a * a * p[2].x
                                    + a.powi(3) * p[3].x,
                                b.powi(3) * p[0].y
                                    + 3. * b * b * a * p[1].y
                                    + 3. * b * a * a * p[2].y
                                    + a.powi(3) * p[3].y,
                            )
                        })
                        .collect()
                } else {
                    self.warn("Invalid Bezier point count");
                    vec![]
                }
            }
            _ => return,
        };
        self.path(
            points.into_iter().map(|p| t.point(p)).collect(),
            if ink == Ink::Bus {
                width(e).max(0.3)
            } else {
                width(e)
            },
            ink,
            fill,
        );
        if let Some(Primitive::Path {
            solid,
            fill_color,
            stroke_color,
            ..
        }) = self.primitives.last_mut()
        {
            let custom = |node: Option<&Expr>| {
                let c = node?.child("color")?;
                let alpha = c.number(4).unwrap_or(1.).clamp(0., 1.);
                (alpha > 0.).then(|| {
                    [
                        c.number(1).unwrap_or(0.).clamp(0., 255.) as u8,
                        c.number(2).unwrap_or(0.).clamp(0., 255.) as u8,
                        c.number(3).unwrap_or(0.).clamp(0., 255.) as u8,
                        (alpha * 255.).round() as u8,
                    ]
                })
            };
            *stroke_color = custom(e.child("stroke"));
            if let Some(f) = e.child("fill") {
                *solid = fill && matches!(value(f, "type"), "outline" | "color");
                *fill_color = match value(f, "type") {
                    "color" => custom(Some(f)),
                    "outline" => *stroke_color,
                    "background" if ink == Ink::Symbol => Some([255, 255, 194, 255]),
                    _ => None,
                };
            }
        }
    }
    fn text(
        &mut self,
        e: &Expr,
        text: &str,
        t: Transform,
        ink: Ink,
        instances: BTreeMap<String, String>,
    ) {
        if text.is_empty() || hidden(e) {
            return;
        }
        let effects = e.child("effects");
        let font = effects.and_then(|e| e.child("font"));
        let size = font
            .map(|f| number(f, "size", 1, 1.27))
            .unwrap_or(1.27)
            .clamp(0.1, 1000.);
        let justify = effects.and_then(|e| e.child("justify"));
        let mut align = [
            if justify.is_some_and(|e| e.has_atom("left")) {
                -1
            } else if justify.is_some_and(|e| e.has_atom("right")) {
                1
            } else {
                0
            },
            if justify.is_some_and(|e| e.has_atom("top")) {
                -1
            } else if justify.is_some_and(|e| e.has_atom("bottom")) {
                1
            } else {
                0
            },
        ];
        let mut angle = number(e, "at", 3, 0.);
        if t.library {
            angle = t.angle + angle / 10.;
        }
        // KiCad keeps labels readable when their electrical direction is 180°/270°.
        let normalized = angle.rem_euclid(360.);
        if normalized > 90. && normalized <= 270. {
            angle = (angle + 180.).rem_euclid(360.);
            if ink != Ink::Label {
                align[0] = -align[0];
                align[1] = -align[1];
            }
        }
        self.primitives.push(Primitive::Text {
            at: t.point(at(e)),
            text: text
                .replace("{slash}", "/")
                .replace("{backslash}", "\\")
                .replace("{colon}", ":"),
            size,
            angle,
            align,
            ink,
            bold: font.is_some_and(|f| f.has_atom("bold") || value(f, "bold") == "yes"),
            instances,
        });
    }
    fn text_box(&mut self, e: &Expr) {
        if hidden(e) {
            return;
        }
        let effects = e.child("effects");
        let font = effects.and_then(|e| e.child("font"));
        let font_size = font
            .map(|f| number(f, "size", 1, 1.27))
            .unwrap_or(1.27)
            .clamp(0.1, 1000.);
        let justify = effects.and_then(|e| e.child("justify"));
        let align = [
            if justify.is_some_and(|e| e.has_atom("left")) {
                -1
            } else if justify.is_some_and(|e| e.has_atom("right")) {
                1
            } else {
                0
            },
            if justify.is_some_and(|e| e.has_atom("top")) {
                -1
            } else if justify.is_some_and(|e| e.has_atom("bottom")) {
                1
            } else {
                0
            },
        ];
        let stroke = e.child("stroke");
        let width = stroke.map(|s| number(s, "width", 1, 0.)).unwrap_or(0.);
        let enabled = width >= 0. && stroke.is_none_or(|s| value(s, "type") != "none");
        let legacy_margin = width.max(0.) / 2. + font_size * 0.75;
        let mut margins = [legacy_margin; 4];
        if let Some(m) = e.child("margins") {
            for (i, margin) in margins.iter_mut().enumerate() {
                *margin = m.number(i + 1).unwrap_or(legacy_margin).max(0.);
            }
        }
        let color = |node: Option<&Expr>, default: [u8; 4]| -> [u8; 4] {
            let Some(c) = node.and_then(|s| s.child("color")) else {
                return default;
            };
            let alpha = c.number(4).unwrap_or(1.).clamp(0., 1.);
            if alpha == 0. {
                return default;
            }
            [
                c.number(1).unwrap_or(0.).clamp(0., 255.) as u8,
                c.number(2).unwrap_or(0.).clamp(0., 255.) as u8,
                c.number(3).unwrap_or(0.).clamp(0., 255.) as u8,
                (alpha * 255.).round() as u8,
            ]
        };
        let fill = e.child("fill");
        let fill_color = match fill.map(|f| value(f, "type")).unwrap_or("none") {
            "color" => Some(color(fill, [0, 0, 255, 255])),
            "outline" => Some([0, 0, 255, 255]),
            "background" => Some([255, 255, 245, 255]),
            _ => None,
        };
        let size = e.child("size").map(xy).unwrap_or_default();
        if size.x <= 0. || size.y <= 0. {
            self.warn("Invalid text-box size");
            return;
        }
        self.primitives.push(Primitive::TextBox {
            at: at(e),
            size,
            angle: number(e, "at", 3, 0.),
            text: e.value(1).unwrap_or("").to_owned(),
            font_size,
            align,
            bold: font.is_some_and(|f| f.has_atom("bold") || value(f, "bold") == "yes"),
            italic: font.is_some_and(|f| f.has_atom("italic") || value(f, "italic") == "yes"),
            margins,
            stroke_width: if enabled {
                if width == 0. { 0.15 } else { width }
            } else {
                0.
            },
            stroke_color: color(stroke, [0, 0, 255, 255]),
            fill_color,
        });
    }
    fn bitmap(&mut self, e: &Expr) {
        use base64::Engine;
        let result = (|| -> std::result::Result<Primitive, String> {
            let data = e.child("data").ok_or("Image data is missing")?;
            let encoded: String = data.items().iter().skip(1).filter_map(Expr::atom).collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| e.to_string())?;
            let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(64 * 1024 * 1024);
            reader.limits(limits);
            let image = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
            let (width, height) = image.dimensions();
            let existing: usize = self
                .primitives
                .iter()
                .filter_map(|p| {
                    if let Primitive::Bitmap { rgba, .. } = p {
                        Some(rgba.len())
                    } else {
                        None
                    }
                })
                .sum();
            if existing.saturating_add(image.len()) > 128 * 1024 * 1024 {
                return Err("Decoded schematic images exceed 128 MB".into());
            }

            let mut dpi = 300.;
            if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                let mut offset = 8;
                while offset + 12 <= bytes.len() {
                    let len =
                        u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                    if len > bytes.len() - offset - 12 {
                        break;
                    }
                    if &bytes[offset + 4..offset + 8] == b"pHYs"
                        && len == 9
                        && bytes[offset + 16] == 1
                    {
                        dpi = u32::from_be_bytes(bytes[offset + 8..offset + 12].try_into().unwrap())
                            as f64
                            * 0.0254;
                    }
                    offset += len + 12;
                }
            }
            let mm =
                25.4 / dpi.clamp(1., 10000.) * number(e, "scale", 1, 1.).abs().clamp(0.001, 1000.);
            Ok(Primitive::Bitmap {
                center: at(e),
                size: Point::new(width as f64 * mm, height as f64 * mm),
                width,
                height,
                rgba: image.into_raw(),
            })
        })();
        match result {
            Ok(p) => self.primitives.push(p),
            Err(e) => self.warn(&format!("Could not decode schematic image: {e}")),
        }
    }
    fn global_label(&mut self, e: &Expr) {
        let size = e
            .child("effects")
            .and_then(|e| e.child("font"))
            .map(|f| number(f, "size", 1, 1.27))
            .unwrap_or(1.27);
        let angle = number(e, "at", 3, 0.).rem_euclid(360.);
        let radians = -(angle + 180.).to_radians();
        let origin = at(e);
        let transform = |p: Point| {
            origin.plus(Point::new(
                p.x * radians.cos() - p.y * radians.sin(),
                p.x * radians.sin() + p.y * radians.cos(),
            ))
        };
        let height = size * 0.8;
        let len = e.value(1).unwrap_or("").chars().count() as f64 * size * 0.8 + size;
        let points = match value(e, "shape") {
            "output" => vec![
                Point::new(0., -height),
                Point::new(-len, -height),
                Point::new(-len - height, 0.),
                Point::new(-len, height),
                Point::new(0., height),
                Point::new(0., -height),
            ],
            "bidirectional" | "tri_state" => vec![
                Point::new(0., 0.),
                Point::new(-height, -height),
                Point::new(-len, -height),
                Point::new(-len - height, 0.),
                Point::new(-len, height),
                Point::new(-height, height),
                Point::new(0., 0.),
            ],
            _ => vec![
                Point::new(0., 0.),
                Point::new(-height, -height),
                Point::new(-len, -height),
                Point::new(-len, height),
                Point::new(-height, height),
                Point::new(0., 0.),
            ],
        };
        self.path(
            points.into_iter().map(transform).collect(),
            0.15,
            Ink::Symbol,
            false,
        );
        self.text(
            e,
            e.value(1).unwrap_or(""),
            Transform::sheet(),
            Ink::Label,
            BTreeMap::new(),
        );
        if let Some(Primitive::Text {
            at,
            angle: a,
            align,
            ..
        }) = self.primitives.last_mut()
        {
            *at = transform(Point::new(-size, 0.));
            if angle == 0. || angle == 180. {
                *a = 0.;
                *align = [if angle == 0. { -1 } else { 1 }, 0];
            } else {
                *a = 90.;
                *align = [if angle == 90. { -1 } else { 1 }, 0];
            }
        }
    }
    fn symbol(&mut self, e: &Expr, definitions: &BTreeMap<&str, &Expr>) {
        let lib = value(e, "lib_id");
        let properties: BTreeMap<String, String> = e
            .children("property")
            .filter_map(|p| Some((p.value(1)?.into(), p.value(2)?.into())))
            .collect();
        self.components.push(Component {
            pins: BTreeMap::new(),
            primitives: vec![],
            at: at(e),
            uuid: value(e, "uuid").into(),
            lib_id: lib.into(),
            reference: properties.get("Reference").cloned().unwrap_or_default(),
            references: reference_instances(e),
            properties,
            in_bom: value(e, "in_bom") != "no",
            dnp: value(e, "dnp") == "yes",
        });
        let symbol_first = self.primitives.len();
        let connection_first = self.connections.len();
        let unit = number(e, "unit", 1, 1.) as u32;
        let style = number(e, "convert", 1, number(e, "body_style", 1, 1.)) as u32;
        let mirror = value(e, "mirror");
        let t = Transform {
            origin: at(e),
            angle: number(e, "at", 3, 0.),
            library: true,
            mirror_x: mirror == "x",
            mirror_y: mirror == "y",
        };
        // lib_name explicitly selects an embedded definition when KiCad has
        // renamed conflicting symbols while preserving their original lib_id.
        let definition = definitions
            .get(value(e, "lib_name"))
            .or_else(|| definitions.get(lib));
        if let Some(def) = definition {
            let mut chain = vec![];
            let mut current = *def;
            for _ in 0..32 {
                chain.push(current);
                let parent = value(current, "extends");
                if parent.is_empty() {
                    break;
                }
                if let Some(next) = definitions.get(parent).or_else(|| {
                    definitions
                        .iter()
                        .find(|(name, _)| name.rsplit(':').next() == Some(parent))
                        .map(|(_, e)| e)
                }) {
                    if chain.contains(next) {
                        self.warn("Cyclic library-symbol inheritance");
                        break;
                    }
                    current = next;
                } else {
                    self.warn(&format!("Missing inherited symbol: {parent}"));
                    break;
                }
            }
            let background_start = self.primitives.len();
            for def in chain.into_iter().rev() {
                let names_hidden = def.child("pin_names").is_some_and(hidden);
                let numbers_hidden = def.child("pin_numbers").is_some_and(hidden);
                for graphics in def.children("symbol") {
                    let name = graphics.value(1).unwrap_or("");
                    let mut fields = name.rsplit('_');
                    let draw_style = fields
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(1);
                    let draw_unit = fields
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    if draw_unit != 0 && draw_unit != unit || draw_style != 0 && draw_style != style
                    {
                        continue;
                    }
                    for shape in graphics.items().iter().skip(2) {
                        match shape.tag().unwrap_or("") {
                            "pin" => self.pin(
                                shape,
                                t,
                                names_hidden,
                                numbers_hidden,
                                def.child("pin_names")
                                    .map(|p| number(p, "offset", 1, 0.508))
                                    .unwrap_or(0.508),
                            ),
                            "text" => self.text(
                                shape,
                                shape.value(1).unwrap_or(""),
                                t,
                                Ink::Text,
                                BTreeMap::new(),
                            ),
                            "polyline" | "rectangle" | "circle" | "arc" | "bezier" => {
                                self.graphic(shape, t, Ink::Symbol)
                            }
                            other => self.warn(&format!("Unsupported symbol graphic: {other}")),
                        }
                    }
                }
            }
            // KiCad fills symbol backgrounds before any of the symbol's foreground strokes.
            let mut order: Vec<_> = (background_start..self.primitives.len()).collect();
            order.sort_by_key(|&i| {
                !matches!(
                    &self.primitives[i],
                    Primitive::Path {
                        fill: true,
                        solid: false,
                        ..
                    }
                )
            });
            let sorted: Vec<_> = order.iter().map(|&i| self.primitives[i].clone()).collect();
            let remap: BTreeMap<_, _> = order
                .into_iter()
                .enumerate()
                .map(|(i, old)| (old, background_start + i))
                .collect();
            self.primitives[background_start..].clone_from_slice(&sorted);
            if let Some(component) = self.components.last_mut() {
                for index in component.pins.values_mut() {
                    if let Some(new) = remap.get(index) {
                        *index = *new;
                    }
                }
            }
            for connection in &mut self.connections[connection_first..] {
                for i in &mut connection.primitives {
                    if let Some(new) = remap.get(i) {
                        *i = *new;
                    }
                }
            }
        } else {
            self.warn(&format!("Missing embedded symbol: {lib}"));
            self.path(
                vec![
                    t.origin.plus(Point::new(-3., -3.)),
                    t.origin.plus(Point::new(3., -3.)),
                    t.origin.plus(Point::new(3., 3.)),
                    t.origin.plus(Point::new(-3., 3.)),
                    t.origin.plus(Point::new(-3., -3.)),
                ],
                0.3,
                Ink::Symbol,
                false,
            );
        }
        let references = reference_instances(e);
        let multi_unit = definition.is_some_and(|def| {
            def.children("symbol").any(|g| {
                g.value(1)
                    .unwrap_or("")
                    .rsplit('_')
                    .nth(1)
                    .and_then(|n| n.parse::<u32>().ok())
                    .is_some_and(|n| n > 1)
            })
        });
        for property in e.children("property") {
            self.text(
                property,
                property.value(2).unwrap_or(""),
                Transform::sheet(),
                Ink::Text,
                if property.value(1) == Some("Reference") {
                    references.clone()
                } else {
                    BTreeMap::new()
                },
            );
            if !hidden(property)
                && let Some(Primitive::Text { angle, align, .. }) = self.primitives.last_mut()
            {
                let field_angle = number(property, "at", 3, 0.).to_radians();
                let origin = t.point(Point::default());
                let direction = t.point(Point::new(field_angle.cos(), field_angle.sin()));
                let dx = direction.x - origin.x;
                let dy = direction.y - origin.y;
                *angle = if dy.abs() > dx.abs() { 90. } else { 0. };
                let sign = if *angle == 90. { -dy } else { dx };
                if sign < 0. {
                    align[0] = -align[0];
                }
            }
            if multi_unit
                && property.value(1) == Some("Reference")
                && !hidden(property)
                && let Some(Primitive::Text {
                    text, instances, ..
                }) = self.primitives.last_mut()
            {
                let suffix = char::from_u32('A' as u32 + unit.saturating_sub(1)).unwrap_or('?');
                text.push(suffix);
                for reference in instances.values_mut() {
                    reference.push(suffix);
                }
            }
        }
        if definition.is_some_and(|d| d.child("power").is_some()) || lib.starts_with("power:") {
            let name = e
                .children("property")
                .find(|p| p.value(1) == Some("Value"))
                .and_then(|p| p.value(2))
                .unwrap_or("")
                .to_owned();
            self.connections.push(crate::nets::Connection {
                points: vec![t.origin],
                port: Some(format!("g:{name}")),
                name: Some(name),
                primitives: (symbol_first..self.primitives.len()).collect(),
            });
        }
        if value(e, "dnp") == "yes" {
            self.cross(t.origin, 3., Ink::Text);
        }
        if let Some(component) = self.components.last_mut() {
            component.primitives = (symbol_first..self.primitives.len()).collect();
        }
    }
    fn pin(
        &mut self,
        e: &Expr,
        t: Transform,
        names_hidden: bool,
        numbers_hidden: bool,
        name_offset: f64,
    ) {
        if hidden(e) {
            // Hidden power-input pins connect implicitly by pin name in KiCad.
            let name = (e.value(1) == Some("power_in"))
                .then(|| value(e, "name").to_owned())
                .filter(|n| !n.is_empty() && n != "~");
            self.connections.push(crate::nets::Connection {
                points: vec![t.point(at(e))],
                port: name.as_ref().map(|n| format!("g:{n}")),
                name,
                primitives: vec![],
            });
            return;
        }
        let first = self.primitives.len();
        let start = at(e);
        let angle = number(e, "at", 3, 0.).to_radians();
        let direction = Point::new(angle.cos(), angle.sin());
        let length = number(e, "length", 1, 2.54);
        let end = start.plus(Point::new(direction.x * length, direction.y * length));
        self.path(vec![t.point(start), t.point(end)], 0.15, Ink::Pin, false);
        if let Some(component) = self.components.last_mut() {
            component.pins.insert(value(e, "number").into(), first);
        }
        if matches!(e.value(2), Some("inverted" | "inverted_clock")) {
            let circle = (0..=24)
                .map(|i| {
                    let a = i as f64 * std::f64::consts::TAU / 24.;
                    t.point(end.plus(Point::new(a.cos() * 0.35, a.sin() * 0.35)))
                })
                .collect();
            self.path(circle, 0.15, Ink::Symbol, true);
        }
        if matches!(e.value(2), Some("clock" | "inverted_clock" | "clock_low")) {
            let normal = Point::new(-direction.y * 0.6, direction.x * 0.6);
            self.path(
                vec![
                    t.point(end.plus(normal)),
                    t.point(end.plus(Point::new(direction.x * 0.9, direction.y * 0.9))),
                    t.point(end.plus(Point::new(-normal.x, -normal.y))),
                ],
                0.15,
                Ink::Symbol,
                false,
            );
        }
        for (tag, show, position) in [
            (
                "number",
                !numbers_hidden,
                Point::new(
                    (start.x + end.x) / 2. - direction.y * 0.65,
                    (start.y + end.y) / 2. + direction.x * 0.65,
                ),
            ),
            (
                "name",
                !names_hidden,
                end.plus(Point::new(
                    direction.x * name_offset,
                    direction.y * name_offset,
                )),
            ),
        ] {
            if let Some(label) = e.child(tag)
                && show
                && label.value(1) != Some("~")
            {
                self.primitives.push(Primitive::Text {
                    // Pin numbers stay above horizontal pins and left of vertical pins,
                    // regardless of the electrical direction or symbol rotation/mirroring.
                    at: if tag == "number" {
                        let a = t.point(start);
                        let b = t.point(end);
                        let midpoint = Point::new((a.x + b.x) / 2., (a.y + b.y) / 2.);
                        midpoint.plus(if (a.y - b.y).abs() > (a.x - b.x).abs() {
                            Point::new(-0.65, 0.)
                        } else {
                            Point::new(0., -0.65)
                        })
                    } else {
                        t.point(position)
                    },
                    text: label.value(1).unwrap_or("").into(),
                    size: label
                        .child("effects")
                        .and_then(|e| e.child("font"))
                        .map(|f| number(f, "size", 1, 1.))
                        .unwrap_or(1.),
                    angle: {
                        let a = t.point(end);
                        let b = t.point(start);
                        if (a.y - b.y).abs() > (a.x - b.x).abs() {
                            90.
                        } else {
                            0.
                        }
                    },
                    align: if tag == "number" {
                        [0, 1]
                    } else {
                        let a = t.point(end);
                        let b = t.point(start);
                        [
                            if (a.y - b.y).abs() > (a.x - b.x).abs() {
                                if a.y < b.y { -1 } else { 1 }
                            } else if a.x > b.x {
                                -1
                            } else {
                                1
                            },
                            0,
                        ]
                    },
                    ink: if tag == "name" { Ink::Text } else { Ink::Pin },
                    bold: false,
                    instances: BTreeMap::new(),
                });
            }
        }
        self.connections.push(crate::nets::Connection {
            points: vec![t.point(start)],
            name: None,
            port: None,
            primitives: (first..self.primitives.len()).collect(),
        });
    }
    fn sheet(&mut self, e: &Expr) {
        let origin = at(e);
        let size = e.child("size").map(xy).unwrap_or(Point::new(20., 15.));
        let end = origin.plus(size);
        let name = e
            .children("property")
            .find(|p| p.value(1) == Some("Sheetname"))
            .and_then(|p| p.value(2))
            .unwrap_or("Sheet");
        let file = e
            .children("property")
            .find(|p| p.value(1) == Some("Sheetfile"))
            .and_then(|p| p.value(2))
            .unwrap_or("");
        self.sheets.push(SheetRef {
            excluded_from_bom: value(e, "in_bom") == "no",
            uuid: value(e, "uuid").into(),
            name: name.into(),
            file: file.into(),
            bounds: Bounds {
                min: origin,
                max: end,
            },
        });
        self.path(
            vec![
                origin,
                Point::new(end.x, origin.y),
                end,
                Point::new(origin.x, end.y),
                origin,
            ],
            width(e),
            Ink::Sheet,
            false,
        );
        for property in e.children("property") {
            self.text(
                property,
                property.value(2).unwrap_or(""),
                Transform::sheet(),
                Ink::Sheet,
                BTreeMap::new(),
            );
        }
        for pin in e.children("pin") {
            let first = self.primitives.len();
            self.text(
                pin,
                pin.value(1).unwrap_or(""),
                Transform::sheet(),
                Ink::Label,
                BTreeMap::new(),
            );
            self.primitives.push(Primitive::Dot {
                center: at(pin),
                radius: 0.35,
                ink: Ink::Sheet,
            });
            self.connections.push(crate::nets::Connection {
                points: vec![at(pin)],
                name: None,
                port: Some(format!(
                    "s:{}:{}",
                    value(e, "uuid"),
                    pin.value(1).unwrap_or("")
                )),
                primitives: (first..self.primitives.len()).collect(),
            });
        }
    }
    fn content_bounds(&self) -> Bounds {
        let mut points = Vec::new();
        for primitive in &self.primitives {
            match primitive {
                Primitive::TextBox {
                    at, size, angle, ..
                } => {
                    let (sin, cos) = angle.to_radians().sin_cos();
                    for (x, y) in [(0., 0.), (size.x, 0.), (size.x, size.y), (0., size.y)] {
                        points.push(at.plus(Point::new(x * cos + y * sin, -x * sin + y * cos)));
                    }
                }
                Primitive::Bitmap { center, size, .. } => {
                    points.push(center.plus(Point::new(-size.x / 2., -size.y / 2.)));
                    points.push(center.plus(Point::new(size.x / 2., size.y / 2.)));
                }
                Primitive::Path { points: p, .. } => points.extend(p.iter().copied()),
                Primitive::Dot { center, radius, .. } => {
                    points.push(center.plus(Point::new(-radius, -radius)));
                    points.push(center.plus(Point::new(*radius, *radius)));
                }
                Primitive::Text { at, text, size, .. } => {
                    let width = text.lines().map(|l| l.chars().count()).max().unwrap_or(1) as f64
                        * size
                        * 0.65;
                    points.push(at.plus(Point::new(-width / 2., -size)));
                    points.push(at.plus(Point::new(
                        width / 2.,
                        size * text.lines().count().max(1) as f64,
                    )));
                }
            }
        }
        if points.is_empty() {
            return Bounds {
                min: Point::default(),
                max: self.paper,
            };
        }
        let mut min = points[0];
        let mut max = min;
        for p in points {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        Bounds {
            min: min.plus(Point::new(-5., -5.)),
            max: max.plus(Point::new(5., 5.)),
        }
    }
}
fn reference_instances(e: &Expr) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    if let Some(instances) = e.child("instances") {
        for project in instances.children("project") {
            for path in project.children("path") {
                if let Some(reference) = path.child("reference").and_then(|n| n.value(1)) {
                    result.insert(path.value(1).unwrap_or("").into(), reference.into());
                }
            }
        }
    }
    result
}
fn paper(root: &Expr) -> Point {
    let Some(p) = root.child("paper") else {
        return Point::new(297., 210.);
    };
    let mut size = match p.value(1).unwrap_or("A4") {
        "A0" => Point::new(1189., 841.),
        "A1" => Point::new(841., 594.),
        "A2" => Point::new(594., 420.),
        "A3" => Point::new(420., 297.),
        "A5" => Point::new(210., 148.),
        "A" => Point::new(279.4, 215.9),
        "B" => Point::new(431.8, 279.4),
        "C" => Point::new(558.8, 431.8),
        "D" => Point::new(863.6, 558.8),
        "E" => Point::new(1117.6, 863.6),
        "User" => Point::new(p.number(2).unwrap_or(297.), p.number(3).unwrap_or(210.)),
        _ => Point::new(297., 210.),
    };
    if p.has_atom("portrait") {
        std::mem::swap(&mut size.x, &mut size.y);
    }
    size
}
/// Circular arc through start/mid/end; the sweep always passes through mid.
pub fn arc_points(a: Point, b: Point, c: Point) -> Vec<Point> {
    let d = 2. * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-10 {
        return vec![a, b, c];
    }
    let aa = a.x * a.x + a.y * a.y;
    let bb = b.x * b.x + b.y * b.y;
    let cc = c.x * c.x + c.y * c.y;
    let center = Point::new(
        (aa * (b.y - c.y) + bb * (c.y - a.y) + cc * (a.y - b.y)) / d,
        (aa * (c.x - b.x) + bb * (a.x - c.x) + cc * (b.x - a.x)) / d,
    );
    let start = (a.y - center.y).atan2(a.x - center.x);
    let mid = (b.y - center.y).atan2(b.x - center.x);
    let end = (c.y - center.y).atan2(c.x - center.x);
    let tau = std::f64::consts::TAU;
    let ccw = (end - start).rem_euclid(tau);
    let sweep = if (mid - start).rem_euclid(tau) <= ccw {
        ccw
    } else {
        ccw - tau
    };
    let radius = (a.x - center.x).hypot(a.y - center.y);
    let steps = ((radius * sweep.abs() / 0.1).ceil() as usize).clamp(8, 256);
    (0..=steps)
        .map(|i| {
            let angle = start + sweep * i as f64 / steps as f64;
            center.plus(Point::new(radius * angle.cos(), radius * angle.sin()))
        })
        .collect()
}
