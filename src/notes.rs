//! Portable rich-note documents: styled text, editable LaTeX and managed images.
use crate::{
    attachments::AttachmentDraft,
    model::{Notes, id},
};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub value: String,
    pub styles: Vec<Style>,
}
impl Text {
    pub fn new(value: String, style: Style) -> Self {
        let styles = vec![style; value.chars().count()];
        Self { value, styles }
    }
    pub fn replace(&mut self, value: String, insert_style: Style) {
        let old: Vec<_> = self.value.chars().collect();
        let new: Vec<_> = value.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        self.styles.resize(old.len(), Style::default());
        self.styles.splice(
            prefix..old.len() - suffix,
            std::iter::repeat_n(insert_style, new.len() - prefix - suffix),
        );
        self.value = value;
    }
    pub fn toggle(&mut self, range: std::ops::Range<usize>, attribute: usize) {
        let range = range.start.min(self.styles.len())..range.end.min(self.styles.len());
        let get = |s: Style| match attribute {
            0 => s.bold,
            1 => s.italic,
            _ => s.underline,
        };
        let value = !self.styles[range.clone()].iter().all(|s| get(*s));
        for style in &mut self.styles[range] {
            match attribute {
                0 => style.bold = value,
                1 => style.italic = value,
                _ => style.underline = value,
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(Text),
    Math { source: String, display: bool },
    Image { path: String, width: f32 },
}
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub id: String,
    pub content: Content,
}
impl Block {
    pub fn new(content: Content) -> Self {
        Self { id: id(), content }
    }
}
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
}
/// One editable text flow. Equations occupy a single object character, so wrapping,
/// selections and newlines are handled by the text engine rather than separate fields.
#[derive(Clone, Debug)]
pub struct InlineRun {
    pub text: Text,
    pub equations: std::collections::BTreeMap<usize, Block>,
}
impl InlineRun {
    pub fn from_blocks(blocks: &[Block]) -> Self {
        let mut run = Self {
            text: Text::new(String::new(), Style::default()),
            equations: Default::default(),
        };
        for block in blocks {
            match &block.content {
                Content::Text(text) => {
                    run.text.value.push_str(&text.value);
                    run.text.styles.extend_from_slice(&text.styles);
                }
                Content::Math { display: false, .. } => {
                    run.equations.insert(run.text.styles.len(), block.clone());
                    run.text.value.push('\u{fffc}');
                    run.text.styles.push(Style::default());
                }
                _ => {}
            }
        }
        run
    }
    pub fn replace(&mut self, value: String, style: Style) {
        let old: Vec<_> = self.text.value.chars().collect();
        let new: Vec<_> = value.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let removed_end = old.len() - suffix;
        let added_end = new.len() - suffix;
        self.equations = self
            .equations
            .iter()
            .filter_map(|(&index, block)| {
                let shifted = if index < prefix {
                    index
                } else if index >= removed_end {
                    added_end + index - removed_end
                } else {
                    return None;
                };
                Some((shifted, block.clone()))
            })
            .collect();
        self.text.replace(value, style);
    }
    pub fn insert_equation(
        &mut self,
        range: std::ops::Range<usize>,
        source: String,
        style: Style,
    ) -> usize {
        let mut chars: Vec<_> = self.text.value.chars().collect();
        let start = range.start.min(chars.len());
        let end = range.end.max(start).min(chars.len());
        chars.splice(start..end, ['\u{fffc}']);
        self.replace(chars.into_iter().collect(), style);
        self.equations.insert(
            start,
            Block::new(Content::Math {
                source,
                display: false,
            }),
        );
        start + 1
    }
    pub fn into_blocks(self, first_id: String) -> Vec<Block> {
        let mut blocks = Vec::new();
        let chars: Vec<_> = self.text.value.chars().collect();
        let mut start = 0;
        for (&index, equation) in &self.equations {
            blocks.push(Block::new(Content::Text(Text {
                value: chars[start..index].iter().collect(),
                styles: self.text.styles[start..index].to_vec(),
            })));
            blocks.push(equation.clone());
            start = index + 1;
        }
        blocks.push(Block::new(Content::Text(Text {
            value: chars[start..].iter().collect(),
            styles: self.text.styles[start..].to_vec(),
        })));
        blocks[0].id = first_id;
        blocks
    }
}

impl Document {
    pub fn plain(text: &str) -> Self {
        Self {
            blocks: vec![Block::new(Content::Text(Text::new(
                text.into(),
                Style::default(),
            )))],
        }
    }
    pub fn load(notes: &Notes, draft: &AttachmentDraft) -> Result<Self> {
        if let Some(source) = notes.extra.get("markdownSource").and_then(Value::as_str) {
            return crate::markdown_notes::document(source, &notes.images);
        }
        if let Some(source) = notes.extra.get("latexSource").and_then(Value::as_str) {
            return Ok(crate::latex_document::parse(source, &notes.images)?.document);
        }
        let mut doc = Self::default();
        if let Some(tokens) = notes.extra.get("visualTokens").and_then(Value::as_array) {
            for token in tokens {
                let value = token["value"].as_str().unwrap_or_default();
                let style = Style {
                    bold: token["bold"].as_bool().unwrap_or(false),
                    italic: token["italic"].as_bool().unwrap_or(false),
                    underline: token["underline"].as_bool().unwrap_or(false),
                };
                match token["kind"].as_str() {
                    Some("text") => doc.push_text(value, style),
                    Some("math") => doc.blocks.push(Block::new(Content::Math {
                        source: value.into(),
                        display: token["display"].as_bool().unwrap_or(false),
                    })),
                    Some("image") => {
                        let path = if let Some(path) = notes.images.get(value) {
                            path.clone()
                        } else if value.starts_with("data:image/") {
                            let (_, encoded) = value
                                .split_once(',')
                                .context("Invalid embedded note image")?;
                            let bytes =
                                base64::engine::general_purpose::STANDARD.decode(encoded)?;
                            ensure!(bytes.len() <= 30_000_000, "Note image exceeds 30 MB");
                            draft.image_bytes(&bytes)?["originalPath"]
                                .as_str()
                                .context("Image path missing")?
                                .into()
                        } else {
                            anyhow::bail!("Note image is missing: {value}")
                        };
                        doc.blocks.push(Block::new(Content::Image {
                            path,
                            width: token["width"].as_f64().unwrap_or(480.).clamp(24., 1400.) as f32,
                        }));
                    }
                    _ => {}
                }
            }
        } else if !notes.rtf.is_empty() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&notes.rtf)
                .context("Invalid imported RTF encoding")?;
            ensure!(
                bytes.starts_with(b"{\\rtf"),
                "Unsupported imported RTF document"
            );
            doc = read_rtf(&bytes, notes)?;
            doc.convert_all_math();
        } else {
            doc.push_text(&notes.text, Style::default());
            for path in notes.images.values() {
                doc.blocks.push(Block::new(Content::Image {
                    path: path.clone(),
                    width: 480.,
                }));
            }
            doc.convert_all_math();
        }
        if doc.blocks.is_empty() {
            doc = Self::plain("");
        }
        Ok(doc)
    }
    fn push_text(&mut self, value: &str, style: Style) {
        if value.is_empty() {
            return;
        }
        if let Some(Block {
            content: Content::Text(text),
            ..
        }) = self.blocks.last_mut()
        {
            text.value.push_str(value);
            text.styles
                .extend(std::iter::repeat_n(style, value.chars().count()));
        } else {
            self.blocks
                .push(Block::new(Content::Text(Text::new(value.into(), style))));
        }
    }
    pub fn save(&self, original: &Notes) -> Notes {
        let mut notes = original.clone();
        notes.text.clear();
        notes.rtf.clear();
        notes.images.clear();
        notes.extra.remove("visualHTML");
        notes.extra.remove("latexSource");
        let mut tokens = Vec::new();
        for block in &self.blocks {
            match &block.content {
                Content::Text(text) => {
                    notes.text.push_str(&text.value);
                    let chars: Vec<_> = text.value.chars().collect();
                    let mut start = 0;
                    while start < chars.len() {
                        let style = text.styles.get(start).copied().unwrap_or_default();
                        let mut end = start + 1;
                        while end < chars.len()
                            && text.styles.get(end).copied().unwrap_or_default() == style
                        {
                            end += 1;
                        }
                        tokens.push(json!({"kind":"text","value":chars[start..end].iter().collect::<String>(),"bold":style.bold,"italic":style.italic,"underline":style.underline}));
                        start = end;
                    }
                }
                Content::Math { source, display } => {
                    notes.text.push_str(&if *display {
                        format!("\\[{source}\\]")
                    } else {
                        format!("\\({source}\\)")
                    });
                    tokens.push(json!({"kind":"math","value":source,"display":display}));
                }
                Content::Image { path, width } => {
                    notes.text.push('\u{fffc}');
                    let extension = std::path::Path::new(path)
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("png");
                    let name = format!("{}.{}", block.id, extension);
                    notes.images.insert(name.clone(), path.clone());
                    tokens.push(json!({"kind":"image","value":name,"width":width}));
                }
            }
        }
        notes
            .extra
            .insert("visualTokens".into(), Value::Array(tokens));
        notes.extra.insert("editorVersion".into(), json!(1));
        notes
    }
    /// Empty text slots keep both sides of an inline object editable after reopening.
    pub fn ensure_inline_text(&mut self) {
        let mut index = 0;
        while index < self.blocks.len() {
            if matches!(
                self.blocks[index].content,
                Content::Math { display: false, .. }
            ) {
                if index == 0 || !matches!(self.blocks[index - 1].content, Content::Text(_)) {
                    self.blocks.insert(
                        index,
                        Block::new(Content::Text(Text::new(String::new(), Style::default()))),
                    );
                    index += 1;
                }
                if index + 1 == self.blocks.len()
                    || !matches!(self.blocks[index + 1].content, Content::Text(_))
                {
                    self.blocks.insert(
                        index + 1,
                        Block::new(Content::Text(Text::new(String::new(), Style::default()))),
                    );
                }
            }
            index += 1;
        }
    }
    /// Insert an inline object at the selected character range, preserving styled text on both sides.
    pub fn insert_inline_math(
        &mut self,
        block_id: &str,
        range: std::ops::Range<usize>,
        source: String,
    ) -> Option<String> {
        let index = self.blocks.iter().position(|b| b.id == block_id)?;
        let Content::Text(text) = &self.blocks[index].content else {
            return None;
        };
        let chars: Vec<_> = text.value.chars().collect();
        let start = range.start.min(chars.len());
        let end = range.end.max(start).min(chars.len());
        let before = Block::new(Content::Text(Text {
            value: chars[..start].iter().collect(),
            styles: text.styles[..start].to_vec(),
        }));
        let after = Block::new(Content::Text(Text {
            value: chars[end..].iter().collect(),
            styles: text.styles[end..].to_vec(),
        }));
        let tail = after.id.clone();
        self.blocks.splice(
            index..=index,
            [
                before,
                Block::new(Content::Math {
                    source,
                    display: false,
                }),
                after,
            ],
        );
        Some(tail)
    }
    pub fn convert_all_math(&mut self) {
        let mut i = 0;
        while i < self.blocks.len() {
            let added = self.convert_math(i);
            i += added.max(1);
        }
    }
    /// Replace completed delimiters with editable equation objects, keeping surrounding styles.
    pub fn convert_math(&mut self, index: usize) -> usize {
        let Some(Block {
            content: Content::Text(text),
            ..
        }) = self.blocks.get(index)
        else {
            return 0;
        };
        let regex =
            regex::Regex::new(r"(?s)\$\$(.+?)\$\$|\\\[(.+?)\\\]|\\\((.+?)\\\)|\$([^$\n]+?)\$")
                .unwrap();
        let mut blocks = Vec::new();
        let mut last = 0;
        for captures in regex.captures_iter(&text.value) {
            let found = captures.get(0).unwrap();
            if captures.get(4).is_some()
                && (text.value[..found.start()].ends_with('$')
                    || text.value[found.end()..].starts_with('$'))
            {
                continue;
            }
            let source = captures.iter().skip(1).flatten().next().unwrap().as_str();
            let display = captures.get(1).is_some() || captures.get(2).is_some();
            if math_svg(source, display).is_err() {
                continue;
            }
            let before = &text.value[last..found.start()];
            if !before.is_empty() {
                let start = text.value[..last].chars().count();
                let end = start + before.chars().count();
                blocks.push(Block::new(Content::Text(Text {
                    value: before.into(),
                    styles: text.styles[start..end].to_vec(),
                })));
            }
            blocks.push(Block::new(Content::Math {
                source: source.into(),
                display,
            }));
            last = found.end();
        }
        if last == 0 {
            return 0;
        }
        let after = &text.value[last..];
        if !after.is_empty() {
            let start = text.value[..last].chars().count();
            blocks.push(Block::new(Content::Text(Text {
                value: after.into(),
                styles: text.styles[start..].to_vec(),
            })));
        }
        let count = blocks.len();
        self.blocks.splice(index..=index, blocks);
        count
    }
}
/// Mathematical glyphs are self-contained outlines; no system fonts or browser are used.
pub fn math_svg(source: &str, display: bool) -> Result<String> {
    use std::sync::OnceLock;
    static FONT: OnceLock<latex_rust::MathFont> = OnceLock::new();
    let font =
        FONT.get_or_init(|| latex_rust::MathFont::stix_two_math().expect("Bundled math font"));
    ensure!(source.len() < 32_768, "Equation is too long");
    let options = latex_rust::SvgOptions {
        font_size_pt: latex_rust::Dim::from_i64(16),
        color: latex_rust::Color::rgb(235, 235, 239),
        display,
    };
    latex_rust::latex_to_svg(source, font, &options).map_err(|e| anyhow::anyhow!("{e}"))
}
#[derive(Clone, Copy, Default)]
struct RtfState {
    style: Style,
    skip: bool,
    uc: usize,
}
fn read_rtf(bytes: &[u8], notes: &Notes) -> Result<Document> {
    let mut doc = Document::default();
    let mut state = RtfState {
        uc: 1,
        ..Default::default()
    };
    let mut stack = Vec::new();
    let mut i = 0;
    let mut fallback = 0;
    let mut high_surrogate = None;
    let image_width = regex::Regex::new(r"\\width(\d+)").unwrap();
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        match b {
            b'{' => stack.push(state),
            b'}' => state = stack.pop().unwrap_or_default(),
            b'\\' => {
                if i >= bytes.len() {
                    break;
                }
                let b = bytes[i];
                i += 1;
                if matches!(b, b'\\' | b'{' | b'}') {
                    if !state.skip {
                        if fallback > 0 {
                            fallback -= 1;
                        } else {
                            doc.push_text(&(b as char).to_string(), state.style);
                        }
                    }
                    continue;
                }
                if b == b'\'' && i + 2 <= bytes.len() {
                    let hex = std::str::from_utf8(&bytes[i..i + 2])?;
                    let value = u8::from_str_radix(hex, 16)?;
                    i += 2;
                    if !state.skip {
                        if fallback > 0 {
                            fallback -= 1;
                        } else {
                            doc.push_text(&cp1252(value).to_string(), state.style);
                        }
                    }
                    continue;
                }
                if b == b'*' {
                    state.skip = true;
                    continue;
                }
                if !b.is_ascii_alphabetic() {
                    if !state.skip && (b == b'\n' || b == b'\r') {
                        doc.push_text("\n", state.style);
                    }
                    if !state.skip && b == b'~' {
                        doc.push_text("\u{a0}", state.style);
                    }
                    continue;
                }
                let start = i - 1;
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = std::str::from_utf8(&bytes[start..i])?;
                let number_start = i;
                if i < bytes.len() && bytes[i] == b'-' {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let number = std::str::from_utf8(&bytes[number_start..i])?
                    .parse::<i32>()
                    .ok();
                if i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                if [
                    "fonttbl",
                    "colortbl",
                    "expandedcolortbl",
                    "stylesheet",
                    "info",
                    "pict",
                    "object",
                    "header",
                    "footer",
                ]
                .contains(&word)
                {
                    state.skip = true;
                    continue;
                }
                if state.skip {
                    continue;
                }
                match word {
                    "b" => state.style.bold = number != Some(0),
                    "i" => state.style.italic = number != Some(0),
                    "ul" => state.style.underline = number != Some(0),
                    "ulnone" => state.style.underline = false,
                    "plain" => state.style = Style::default(),
                    "par" | "line" => doc.push_text("\n", state.style),
                    "tab" => doc.push_text("\t", state.style),
                    "uc" => state.uc = number.unwrap_or(1).max(0) as usize,
                    "u" => {
                        let value = number.unwrap_or(0) as u16;
                        if (0xd800..=0xdbff).contains(&value) {
                            high_surrogate = Some(value);
                        } else {
                            let value = if (0xdc00..=0xdfff).contains(&value) {
                                high_surrogate
                                    .take()
                                    .map(|high| {
                                        0x10000
                                            + ((u32::from(high) - 0xd800) << 10)
                                            + (u32::from(value) - 0xdc00)
                                    })
                                    .unwrap_or(0xfffd)
                            } else {
                                u32::from(value)
                            };
                            doc.push_text(
                                &char::from_u32(value).unwrap_or('\u{fffd}').to_string(),
                                state.style,
                            );
                        }
                        fallback = state.uc;
                    }
                    "NeXTGraphic" => {
                        let end = bytes[i..]
                            .iter()
                            .position(|&b| b == b'}')
                            .map(|end| i + end)
                            .context("Invalid RTF attachment")?;
                        let payload = String::from_utf8_lossy(&bytes[i..end]);
                        let name = payload.split_whitespace().next().unwrap_or_default();
                        if let Some(path) = notes.images.get(name) {
                            let width = image_width
                                .captures(&payload)
                                .and_then(|c| c[1].parse::<f32>().ok())
                                .map(|n| n / 20.)
                                .unwrap_or(480.);
                            doc.blocks.push(Block::new(Content::Image {
                                path: path.clone(),
                                width: width.clamp(24., 1400.),
                            }));
                        } else {
                            anyhow::bail!("Missing imported note image: {name}");
                        }
                        i = end;
                    }
                    _ => {}
                }
            }
            b'\r' | b'\n' => {}
            0xac => {}
            _ => {
                if !state.skip {
                    if fallback > 0 {
                        fallback -= 1;
                    } else {
                        doc.push_text(&cp1252(b).to_string(), state.style);
                    }
                }
            }
        }
    }
    Ok(doc)
}
fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    if (0x80..0xa0).contains(&b) {
        HIGH[(b - 0x80) as usize]
    } else {
        char::from(b)
    }
}
