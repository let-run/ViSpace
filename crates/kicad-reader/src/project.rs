use crate::{Error, MAX_FILE_BYTES, Result, Schematic, json};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub net_classes: crate::NetClasses,
    pub file: String,
    pub root_schematic: String,
    pub text_variables: BTreeMap<String, String>,
    pub sheet_names: BTreeMap<String, String>,
}
impl Project {
    pub fn parse(file: &str, source: &str) -> Result<Self> {
        if !file.ends_with(".kicad_pro") {
            return Err(Error::new("Choose a .kicad_pro project (KiCad 6 or newer)"));
        }
        let value = json::parse(source)?;
        if value.object().is_none() {
            return Err(Error::new("KiCad project must be a JSON object"));
        }
        let mut variables = BTreeMap::new();
        if let Some(object) = value.get("text_variables").and_then(json::Value::object) {
            for (key, value) in object {
                if let Some(value) = value.string() {
                    variables.insert(key.clone(), value.into());
                }
            }
        }
        let mut sheets = BTreeMap::new();
        if let Some(list) = value.get("sheets") {
            for sheet in list.array() {
                let fields = sheet.array();
                if let (Some(id), Some(name)) = (
                    fields.first().and_then(json::Value::string),
                    fields.get(1).and_then(json::Value::string),
                ) {
                    sheets.insert(id.into(), name.into());
                }
            }
        }
        let file = normalize(file);
        let root_schematic = format!("{}.kicad_sch", file.strip_suffix(".kicad_pro").unwrap());
        Ok(Self {
            net_classes: crate::NetClasses::parse(source)?,
            file,
            root_schematic,
            text_variables: variables,
            sheet_names: sheets,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SheetNode {
    #[serde(default)]
    pub excluded_from_bom: bool,
    pub name: String,
    pub file: String,
    pub instance_path: String,
    pub sheet_uuid: String,
    pub children: Vec<SheetNode>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    #[serde(default)]
    pub pcb: Option<Arc<crate::Pcb>>,
    #[serde(default)]
    pub pcb_file: String,
    #[serde(default)]
    pub pcb_available: bool,
    #[serde(default)]
    pub pcb_error: Option<String>,
    #[serde(default)]
    pub nets: Vec<crate::nets::ProjectNet>,
    pub project: Project,
    pub root: SheetNode,
    pub schematics: BTreeMap<String, Arc<Schematic>>,
}
/// The caller controls file access; this crate never touches the filesystem/network.
/// Missing child sheets and cycles become visible tree errors, not lost siblings.
pub fn load_project(file: &str, read: impl FnMut(&str) -> Result<String>) -> Result<Bundle> {
    load_project_inner(file, read, true)
}
/// Load the schematic hierarchy and nets without parsing board geometry.
/// PCB availability is detected, but its scene is loaded separately on demand.
pub fn load_schematics(file: &str, read: impl FnMut(&str) -> Result<String>) -> Result<Bundle> {
    load_project_inner(file, read, false)
}
fn load_project_inner(
    file: &str,
    mut read: impl FnMut(&str) -> Result<String>,
    include_pcb: bool,
) -> Result<Bundle> {
    let project_source = read(file)?;
    let project = Project::parse(file, &project_source)?;
    let root_source = read(&project.root_schematic)?;
    let root_scene = Schematic::parse(&root_source)
        .map_err(|e| Error::new(format!("{}: {e}", project.root_schematic)))?;
    let uuid = root_scene.uuid.clone();
    let root_file = project.root_schematic.clone();
    let mut loader = Loader {
        read: &mut read,
        project: &project,
        schematics: BTreeMap::from([(root_file.clone(), Arc::new(root_scene))]),
        nodes: 0,
        total_bytes: root_source.len(),
    };
    let root = loader.walk(&root_file, "Root", &format!("/{uuid}"), "", &mut vec![], 0);
    let schematics = std::mem::take(&mut loader.schematics);
    drop(loader);
    let pcb_file = format!("{}.kicad_pcb", project.file.trim_end_matches(".kicad_pro"));
    let source = read(&pcb_file).ok();
    let pcb_available = source.is_some();
    let (pcb, pcb_error) = if include_pcb {
        match source
            .map(|source| parse_board(&project.file, &project_source, &source, &mut read))
            .transpose()
        {
            Ok(board) => (board.map(Arc::new), None),
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (None, None)
    };
    let mut bundle = Bundle {
        pcb,
        pcb_file,
        pcb_available,
        pcb_error,
        nets: vec![],
        root,
        schematics,
        project,
    };
    bundle.nets = crate::nets::project_nets(&bundle);
    Ok(bundle)
}
/// Read only the board and its project clearance settings; schematics are untouched.
pub fn load_board(file: &str, mut read: impl FnMut(&str) -> Result<String>) -> Result<crate::Pcb> {
    let project_source = read(file)?;
    let project = Project::parse(file, &project_source)?;
    let source = read(&format!(
        "{}.kicad_pcb",
        project.file.trim_end_matches(".kicad_pro")
    ))?;
    parse_board(&project.file, &project_source, &source, &mut read)
}
fn parse_board(
    file: &str,
    project_source: &str,
    source: &str,
    read: &mut impl FnMut(&str) -> Result<String>,
) -> Result<crate::Pcb> {
    let mut board = crate::Pcb::parse(source)?;
    board.apply_project_settings(project_source)?;
    let rules_file = format!("{}.kicad_dru", file.trim_end_matches(".kicad_pro"));
    if read(&rules_file).is_ok_and(|s| !s.trim().is_empty()) {
        board
            .warnings
            .push("Custom .kicad_dru rules are not evaluated for clearance outlines".into());
    }
    Ok(board)
}
struct Loader<'a, F> {
    read: &'a mut F,
    project: &'a Project,
    schematics: BTreeMap<String, Arc<Schematic>>,
    nodes: usize,
    total_bytes: usize,
}
impl<F: FnMut(&str) -> Result<String>> Loader<'_, F> {
    fn walk(
        &mut self,
        file: &str,
        name: &str,
        instance: &str,
        uuid: &str,
        ancestors: &mut Vec<String>,
        depth: usize,
    ) -> SheetNode {
        let mut node = SheetNode {
            excluded_from_bom: false,
            name: name.into(),
            file: file.into(),
            instance_path: instance.into(),
            sheet_uuid: uuid.into(),
            children: vec![],
            error: None,
        };
        self.nodes += 1;
        let result = (|| -> Result<()> {
            if depth > 32 || self.nodes > 512 {
                return Err(Error::new(
                    "Project hierarchy exceeds 32 levels or 512 sheet instances",
                ));
            }
            if ancestors.iter().any(|p| p == file) {
                return Err(Error::new("Circular schematic sheet reference"));
            }
            if !file.ends_with(".kicad_sch") {
                return Err(Error::new("Only modern .kicad_sch sheets are supported"));
            }
            if !self.schematics.contains_key(file) {
                let source = (self.read)(file)?;
                self.total_bytes += source.len();
                if source.len() > MAX_FILE_BYTES || self.total_bytes > 64 * 1024 * 1024 {
                    return Err(Error::new("Project schematic files exceed the size limit"));
                }
                self.schematics
                    .insert(file.into(), Arc::new(Schematic::parse(&source)?));
            }
            let children = self.schematics[file].sheets.clone();
            ancestors.push(file.into());
            for sheet in children {
                if self.nodes >= 512 {
                    node.error = Some("Project exceeds 512 sheet instances".into());
                    break;
                }
                let expanded = self.expand(&sheet.file);
                let target = if expanded.contains("${KIPRJMOD}") {
                    let base = self
                        .project
                        .file
                        .rsplit_once('/')
                        .map(|(p, _)| p)
                        .unwrap_or(".");
                    normalize(&expanded.replace("${KIPRJMOD}", base))
                } else {
                    resolve_sheet(file, &expanded)
                };
                let mut child = self.walk(
                    &target,
                    &sheet.name,
                    &format!("{instance}/{}", sheet.uuid),
                    &sheet.uuid,
                    ancestors,
                    depth + 1,
                );
                child.excluded_from_bom = sheet.excluded_from_bom;
                node.children.push(child);
            }
            ancestors.pop();
            Ok(())
        })();
        if let Err(error) = result {
            node.error = Some(error.to_string());
        }
        node
    }
    fn expand(&self, value: &str) -> String {
        let mut result = value.to_owned();
        for _ in 0..16 {
            let old = result.clone();
            for (key, value) in &self.project.text_variables {
                result = result.replace(&format!("${{{key}}}"), value);
            }
            if result == old {
                break;
            }
        }
        result
    }
}
pub fn resolve_sheet(parent: &str, child: &str) -> String {
    let child = child.replace('\\', "/");
    if child.starts_with('/') || child.as_bytes().get(1) == Some(&b':') {
        return normalize(&child);
    }
    let folder = parent.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    if folder.is_empty() {
        normalize(&child)
    } else {
        normalize(&format!("{folder}/{child}"))
    }
}
pub fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    let prefix = if path.starts_with("//") {
        "//"
    } else if path.starts_with('/') {
        "/"
    } else {
        ""
    };
    let mut stack = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if stack.last().is_some_and(|p| *p != "..") {
                    stack.pop();
                } else {
                    stack.push(part);
                }
            }
            _ => stack.push(part),
        }
    }
    format!("{prefix}{}", stack.join("/"))
}
impl Bundle {
    pub fn nodes(&self) -> Vec<&SheetNode> {
        fn walk<'a>(n: &'a SheetNode, out: &mut Vec<&'a SheetNode>) {
            out.push(n);
            for child in &n.children {
                walk(child, out);
            }
        }
        let mut out = vec![];
        walk(&self.root, &mut out);
        out
    }
}
