use super::{MAX_MODEL_BYTES, Mesh};
use std::collections::BTreeMap;
#[derive(Clone, Default)]
struct Node {
    kind: String,
    fields: BTreeMap<String, Value>,
}
#[derive(Clone)]
enum Value {
    Numbers(Vec<f32>),
    Text(String),
    Nodes(Vec<Node>),
    Node(Box<Node>),
}
struct Parser {
    tokens: Vec<String>,
    index: usize,
    defs: BTreeMap<String, Node>,
}
impl Parser {
    fn peek(&self) -> &str {
        self.tokens
            .get(self.index)
            .map(String::as_str)
            .unwrap_or("")
    }
    fn take(&mut self) -> String {
        let t = self.peek().to_owned();
        self.index += 1;
        t
    }
    fn node(&mut self, depth: usize) -> Result<Node, String> {
        if depth > 64 {
            return Err("VRML nesting exceeds 64 levels".into());
        }
        let mut name = None;
        if self.peek() == "DEF" {
            self.take();
            name = Some(self.take());
        }
        if self.peek() == "USE" {
            self.take();
            let name = self.take();
            return self
                .defs
                .get(&name)
                .cloned()
                .ok_or_else(|| format!("VRML missing DEF {name}"));
        }
        let kind = self.take();
        if self.take() != "{" {
            return Err(format!("VRML expected node after {kind}"));
        }
        let mut node = Node {
            kind,
            ..Default::default()
        };
        while !matches!(self.peek(), "}" | "") {
            let field = self.take();
            let value = if self.peek() == "[" {
                self.take();
                if self.peek().parse::<f32>().is_ok() || self.peek() == "]" {
                    let mut numbers = vec![];
                    while !matches!(self.peek(), "]" | "") {
                        numbers.push(
                            self.take()
                                .parse::<f32>()
                                .map_err(|_| "Invalid VRML number")?,
                        );
                    }
                    Value::Numbers(numbers)
                } else if self.peek() == "DEF"
                    || self.peek() == "USE"
                    || self.tokens.get(self.index + 1).is_some_and(|t| t == "{")
                {
                    let mut nodes = vec![];
                    while !matches!(self.peek(), "]" | "") {
                        nodes.push(self.node(depth + 1)?);
                    }
                    Value::Nodes(nodes)
                } else {
                    let mut text = String::new();
                    while !matches!(self.peek(), "]" | "") {
                        text.push_str(&self.take());
                    }
                    Value::Text(text)
                }
            } else if self.peek().parse::<f32>().is_ok() {
                let mut numbers = vec![];
                while let Ok(number) = self.peek().parse::<f32>() {
                    self.take();
                    numbers.push(number);
                }
                Value::Numbers(numbers)
            } else if self.peek() == "DEF"
                || self.peek() == "USE"
                || self.tokens.get(self.index + 1).is_some_and(|t| t == "{")
            {
                Value::Node(Box::new(self.node(depth + 1)?))
            } else {
                Value::Text(self.take())
            };
            if self.peek() == "]" {
                self.take();
            }
            node.fields.insert(field, value);
        }
        if self.take() != "}" {
            return Err("Unclosed VRML node".into());
        }
        if let Some(name) = name {
            self.defs.insert(name, node.clone());
        }
        Ok(node)
    }
}
impl Node {
    fn nums(&self, key: &str) -> &[f32] {
        match self.fields.get(key) {
            Some(Value::Numbers(v)) => v,
            _ => &[],
        }
    }
    fn child(&self, key: &str) -> Option<&Node> {
        match self.fields.get(key) {
            Some(Value::Node(n)) => Some(n),
            _ => None,
        }
    }
    fn vector(&self, key: &str, default: [f32; 3]) -> [f32; 3] {
        self.nums(key)
            .get(..3)
            .and_then(|v| v.try_into().ok())
            .unwrap_or(default)
    }
}
type Matrix = [[f32; 4]; 4];
fn identity() -> Matrix {
    [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ]
}
fn multiply(a: Matrix, b: Matrix) -> Matrix {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..4).map(|k| a[i][k] * b[k][j]).sum()))
}
fn point(m: Matrix, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3])
}
fn traverse(node: &Node, parent: Matrix, mesh: &mut Mesh) -> Result<(), String> {
    let mut transform = parent;
    if node.kind == "Transform" {
        let t = node.vector("translation", [0.; 3]);
        let s = node.vector("scale", [1.; 3]);
        let r = node.nums("rotation");
        let c = node.vector("center", [0.; 3]);
        let mut local = identity();
        if r.len() >= 4 {
            let len = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt().max(1e-8);
            let [x, y, z] = [r[0] / len, r[1] / len, r[2] / len];
            let (sn, cs) = r[3].sin_cos();
            let d = 1. - cs;
            local[0][..3].copy_from_slice(&[
                cs + x * x * d,
                x * y * d - z * sn,
                x * z * d + y * sn,
            ]);
            local[1][..3].copy_from_slice(&[
                y * x * d + z * sn,
                cs + y * y * d,
                y * z * d - x * sn,
            ]);
            local[2][..3].copy_from_slice(&[
                z * x * d - y * sn,
                z * y * d + x * sn,
                cs + z * z * d,
            ]);
        }
        for i in 0..3 {
            for j in 0..3 {
                local[i][j] *= s[j];
            }
            local[i][3] = t[i] + c[i] - (0..3).map(|j| local[i][j] * c[j]).sum::<f32>();
        }
        transform = multiply(parent, local);
    }
    if node.kind == "Shape" {
        let color = node
            .child("appearance")
            .and_then(|a| a.child("material"))
            .map(|m| m.vector("diffuseColor", [0.35; 3]))
            .unwrap_or([0.35; 3]);
        if let Some(geometry) = node.child("geometry")
            && matches!(
                geometry.kind.as_str(),
                "IndexedFaceSet" | "IndexedTriangleSet"
            )
        {
            let coords = geometry
                .child("coord")
                .map(|c| c.nums("point"))
                .unwrap_or(&[]);
            let points: Vec<_> = coords
                .chunks_exact(3)
                .map(|p| point(transform, [p[0], p[1], p[2]]))
                .collect();
            let mut face = Vec::new();
            let ccw = !matches!(geometry.fields.get("ccw"),Some(Value::Text(t)) if t=="FALSE");
            let mut emit = |face: &[usize]| -> Result<(), String> {
                for i in 1..face.len().saturating_sub(1) {
                    let mut p = [
                        *points
                            .get(face[0])
                            .ok_or("VRML coordinate index out of range")?,
                        *points
                            .get(face[i])
                            .ok_or("VRML coordinate index out of range")?,
                        *points
                            .get(face[i + 1])
                            .ok_or("VRML coordinate index out of range")?,
                    ];
                    if !ccw {
                        p.swap(1, 2);
                    }
                    mesh.triangle(p, color)?;
                }
                Ok(())
            };
            for &index in geometry.nums(if geometry.kind == "IndexedTriangleSet" {
                "index"
            } else {
                "coordIndex"
            }) {
                if index < 0. {
                    emit(&face)?;
                    face.clear();
                } else {
                    face.push(index as usize);
                    if geometry.kind == "IndexedTriangleSet" && face.len() == 3 {
                        emit(&face)?;
                        face.clear();
                    }
                }
            }
            emit(&face)?;
        }
    }
    for value in node.fields.values() {
        match value {
            Value::Nodes(nodes) => {
                for child in nodes {
                    traverse(child, transform, mesh)?
                }
            }
            Value::Node(child) if node.kind != "Shape" => traverse(child, transform, mesh)?,
            _ => {}
        }
    }
    Ok(())
}
pub(super) fn read(source: &str) -> Result<Mesh, String> {
    if source.len() > MAX_MODEL_BYTES {
        return Err("VRML model too large".into());
    }
    let mut tokens = vec![];
    let mut word = String::new();
    let mut quoted = false;
    for line in source
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
    {
        for c in line.chars() {
            if c == '"' {
                quoted = !quoted;
                word.push(c);
            } else if !quoted && (c.is_whitespace() || c == ',' || "{}[]".contains(c)) {
                if !word.is_empty() {
                    tokens.push(std::mem::take(&mut word));
                }
                if "{}[]".contains(c) {
                    tokens.push(c.to_string());
                }
            } else if c == '#' && !quoted {
                break;
            } else {
                word.push(c);
            }
        }
        if !word.is_empty() {
            tokens.push(std::mem::take(&mut word));
        }
    }
    let mut parser = Parser {
        tokens,
        index: 0,
        defs: BTreeMap::new(),
    };
    let mut mesh = Mesh::default();
    while !parser.peek().is_empty() {
        let node = parser.node(0)?;
        traverse(&node, identity(), &mut mesh)?;
    }
    Ok(mesh)
}
