//! Portable note-document LaTeX: text formatting, paragraphs, math and managed images.
//! This is a bounded document dialect, not a TeX engine or shell/file execution environment.
use crate::{
    model::id,
    notes::{Block, Content, Document, Style, Text, math_svg},
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeMap;

pub struct Source {
    pub text: String,
    pub images: BTreeMap<String, String>,
}
pub struct Parsed {
    pub document: Document,
    pub images: BTreeMap<String, String>,
}
pub fn encode(document: &Document) -> Source {
    let mut source = Source {
        text: String::new(),
        images: BTreeMap::new(),
    };
    for block in &document.blocks {
        match &block.content {
            Content::Text(text) => {
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
                    let mut value = escape(&chars[start..end].iter().collect::<String>());
                    if style.underline {
                        value = format!("\\underline{{{value}}}");
                    }
                    if style.italic {
                        value = format!("\\textit{{{value}}}");
                    }
                    if style.bold {
                        value = format!("\\textbf{{{value}}}");
                    }
                    source.text.push_str(&value);
                    start = end;
                }
            }
            Content::Math {
                source: math,
                display,
            } => source.text.push_str(&if *display {
                format!("\\[{math}\\]")
            } else {
                format!("\\({math}\\)")
            }),
            Content::Image { path, width } => {
                let extension = std::path::Path::new(path)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("png");
                let key = format!("{}.{}", block.id, extension);
                source.images.insert(key.clone(), path.clone());
                source
                    .text
                    .push_str(&format!("\\includegraphics[width={width}px]{{{key}}}"));
            }
        }
    }
    source
}
fn escape(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '\\' => "\\textbackslash{}".into(),
            '^' => "\\textasciicircum{}".into(),
            '~' => "\\textasciitilde{}".into(),
            '{' | '}' | '$' | '%' | '_' | '&' | '#' => format!("\\{c}"),
            _ => c.to_string(),
        })
        .collect()
}
pub fn parse(source: &str, images: &BTreeMap<String, String>) -> Result<Parsed> {
    ensure!(source.len() <= 2_000_000, "Notes source is too large");
    let mut parser = Parser {
        source,
        index: 0,
        document: Document::default(),
        images,
        used: BTreeMap::new(),
        depth: 0,
    };
    parser.segment(Style::default(), false)?;
    if parser.document.blocks.is_empty() {
        parser.document = Document::plain("");
    }
    parser.document.ensure_inline_text();
    Ok(Parsed {
        document: parser.document,
        images: parser.used,
    })
}
struct Parser<'a> {
    source: &'a str,
    index: usize,
    document: Document,
    images: &'a BTreeMap<String, String>,
    used: BTreeMap<String, String>,
    depth: usize,
}
impl Parser<'_> {
    fn text(&mut self, value: &str, style: Style) {
        if value.is_empty() {
            return;
        }
        if let Some(Block {
            content: Content::Text(text),
            ..
        }) = self.document.blocks.last_mut()
        {
            text.value.push_str(value);
            text.styles
                .extend(std::iter::repeat_n(style, value.chars().count()));
        } else {
            self.document
                .blocks
                .push(Block::new(Content::Text(Text::new(value.into(), style))));
        }
    }
    fn skip_space(&mut self) {
        while self.source[self.index..].starts_with([' ', '\t']) {
            self.index += 1;
        }
    }
    fn group(&mut self) -> Result<String> {
        self.skip_space();
        ensure!(
            self.source[self.index..].starts_with('{'),
            "Expected {{ at character {}",
            self.index + 1
        );
        self.index += 1;
        let start = self.index;
        let mut depth = 1;
        while self.index < self.source.len() {
            let c = self.source[self.index..].chars().next().unwrap();
            self.index += c.len_utf8();
            if c == '\\' {
                if let Some(c) = self.source[self.index..].chars().next() {
                    self.index += c.len_utf8();
                }
                continue;
            }
            if c == '{' {
                depth += 1;
            }
            if c == '}' {
                depth -= 1;
                if depth == 0 {
                    return Ok(self.source[start..self.index - 1].into());
                }
            }
        }
        bail!("Unclosed {{ group")
    }
    fn math(&mut self, close: &str, display: bool) -> Result<()> {
        let start = self.index;
        let end = self.source[start..]
            .find(close)
            .context("Unclosed equation delimiter")?
            + start;
        let math = self.source[start..end].trim();
        ensure!(!math.is_empty(), "Equation is empty");
        math_svg(math, display).context("Invalid equation")?;
        self.document.blocks.push(Block::new(Content::Math {
            source: math.into(),
            display,
        }));
        self.index = end + close.len();
        Ok(())
    }
    fn segment(&mut self, style: Style, group: bool) -> Result<()> {
        self.depth += 1;
        ensure!(self.depth <= 64, "Formatting is nested too deeply");
        while self.index < self.source.len() {
            let c = self.source[self.index..].chars().next().unwrap();
            self.index += c.len_utf8();
            match c {
                '}' => {
                    ensure!(group, "Unexpected }}; use \\}} for literal braces");
                    self.depth -= 1;
                    return Ok(());
                }
                '{' => self.segment(style, true)?,
                '%' => {
                    while self.index < self.source.len()
                        && !self.source[self.index..].starts_with('\n')
                    {
                        self.index += self.source[self.index..].chars().next().unwrap().len_utf8();
                    }
                }
                '$' => {
                    let display = self.source[self.index..].starts_with('$');
                    if display {
                        self.index += 1;
                    }
                    self.math(if display { "$$" } else { "$" }, display)?;
                }
                '\\' => {
                    let rest = &self.source[self.index..];
                    let next = rest.chars().next().context("Incomplete command")?;
                    if next == '(' || next == '[' {
                        self.index += 1;
                        self.math(if next == '[' { "\\]" } else { "\\)" }, next == '[')?;
                        continue;
                    }
                    if !next.is_ascii_alphabetic() {
                        self.index += next.len_utf8();
                        match next {
                            '\\' => self.text("\n", style),
                            '{' | '}' | '$' | '%' | '_' | '&' | '#' | ' ' => {
                                self.text(&next.to_string(), style)
                            }
                            _ => bail!("Unsupported command \\{next}"),
                        }
                        continue;
                    }
                    let start = self.index;
                    while self.index < self.source.len()
                        && self.source.as_bytes()[self.index].is_ascii_alphabetic()
                    {
                        self.index += 1;
                    }
                    let command = &self.source[start..self.index];
                    match command {
                        "textbf" | "textit" | "emph" | "underline" | "textnormal" => {
                            let mut nested = style;
                            match command {
                                "textbf" => nested.bold = true,
                                "textit" | "emph" => nested.italic = true,
                                "underline" => nested.underline = true,
                                _ => nested = Style::default(),
                            }
                            self.skip_space();
                            ensure!(
                                self.source[self.index..].starts_with('{'),
                                "Expected {{ after \\{command}"
                            );
                            self.index += 1;
                            self.segment(nested, true)?;
                        }
                        "par" => {
                            self.text("\n\n", style);
                            self.skip_space();
                        }
                        "newline" => {
                            self.text("\n", style);
                            self.skip_space();
                        }
                        "textbackslash" | "textasciicircum" | "textasciitilde" => {
                            let _ = self.group()?;
                            self.text(
                                match command {
                                    "textbackslash" => "\\",
                                    "textasciicircum" => "^",
                                    _ => "~",
                                },
                                style,
                            );
                        }
                        "section" | "subsection" => {
                            self.text("\n\n", style);
                            self.skip_space();
                            ensure!(
                                self.source[self.index..].starts_with('{'),
                                "Expected section title"
                            );
                            self.index += 1;
                            self.segment(
                                Style {
                                    bold: true,
                                    ..style
                                },
                                true,
                            )?;
                            self.text("\n\n", style);
                        }
                        "includegraphics" => {
                            self.skip_space();
                            let mut width = 480.;
                            if self.source[self.index..].starts_with('[') {
                                self.index += 1;
                                let end = self.source[self.index..]
                                    .find(']')
                                    .context("Unclosed image options")?
                                    + self.index;
                                let options = &self.source[self.index..end];
                                width = image_width(options)?;
                                self.index = end + 1;
                            }
                            let key = self.group()?;
                            let path=self.images.get(&key).with_context(||format!("Image {key} is not attached; add it with the image or clipboard button"))?;
                            self.used.insert(key.clone(), path.clone());
                            self.document.blocks.push(Block {
                                id: id(),
                                content: Content::Image {
                                    path: path.clone(),
                                    width,
                                },
                            });
                        }
                        "begin" | "end" => {
                            ensure!(
                                self.group()? == "document",
                                "Only the document environment is supported in notes"
                            );
                        }
                        "documentclass" => {
                            ensure!(
                                self.group()? == "article",
                                "Notes support the article document class"
                            );
                        }
                        "usepackage" => {
                            let packages = self.group()?;
                            ensure!(
                                packages.split(',').all(|p| matches!(
                                    p.trim(),
                                    "graphicx" | "amsmath" | "amssymb"
                                )),
                                "Unsupported notes package"
                            );
                        }
                        _ => bail!(
                            "Unsupported command \\{command}. Put mathematical commands inside $…$ or \\(…\\)."
                        ),
                    }
                }
                _ => self.text(&c.to_string(), style),
            }
        }
        ensure!(!group, "Unclosed formatting group");
        self.depth -= 1;
        Ok(())
    }
}
fn image_width(options: &str) -> Result<f32> {
    let raw = options
        .trim()
        .strip_prefix("width=")
        .context("Image options use width=480px or width=0.5\\textwidth")?
        .trim();
    let (number, factor) = if let Some(n) = raw.strip_suffix("\\textwidth") {
        (n, 900.)
    } else if let Some(n) = raw.strip_suffix("px") {
        (n, 1.)
    } else if let Some(n) = raw.strip_suffix("pt") {
        (n, 96. / 72.)
    } else if let Some(n) = raw.strip_suffix("mm") {
        (n, 96. / 25.4)
    } else if let Some(n) = raw.strip_suffix("cm") {
        (n, 96. / 2.54)
    } else {
        (raw, 1.)
    };
    let width = number
        .trim()
        .parse::<f32>()
        .context("Invalid image width")?
        * factor;
    ensure!(
        width.is_finite() && (24. ..=1400.).contains(&width),
        "Image width must be between 24 and 1400 pixels"
    );
    Ok(width)
}
