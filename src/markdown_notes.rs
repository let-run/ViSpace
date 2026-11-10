//! Markdown notes with math and managed image attachments. No HTML or script execution.
use crate::notes::{Block, Content, Document, Style, Text};
use anyhow::{Result, ensure};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::{collections::BTreeMap, ops::Range};

#[derive(Clone, Debug)]
pub enum Inline {
    Text(String, Style, bool),
    Math(String, bool),
    Image {
        key: String,
        width: f32,
        percent: bool,
    },
    Link(String, String),
}
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub range: Range<usize>,
    pub heading: u8,
    pub code: bool,
    pub rule: bool,
    pub indent: usize,
    pub spans: Vec<Inline>,
}
impl Row {
    pub fn title(&self) -> String {
        self.spans
            .iter()
            .map(|s| match s {
                Inline::Text(s, ..) | Inline::Math(s, _) | Inline::Link(s, _) => s.as_str(),
                Inline::Image { .. } => "Image",
            })
            .collect()
    }
}
pub fn parse(source: &str) -> Result<Vec<Row>> {
    ensure!(source.len() <= 2_000_000, "Notes source exceeds 2 MB");
    let mut rows = Vec::new();
    let mut row = Row::default();
    let mut style = Style::default();
    let mut depth = 0usize;
    let mut link = None;
    let mut image = false;
    let options = Options::ENABLE_MATH
        | Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph
                | Tag::Heading { .. }
                | Tag::CodeBlock(_)
                | Tag::TableRow
                | Tag::TableHead => {
                    if !row.spans.is_empty()
                        && !matches!(row.spans.as_slice(), [Inline::Text(s, ..)] if s == "• ")
                    {
                        rows.push(std::mem::take(&mut row));
                    }
                    row.range = range.clone();
                    row.indent = depth;
                    if let Tag::Heading { level, .. } = tag {
                        row.heading = level as u8;
                    }
                    row.code = matches!(tag, Tag::CodeBlock(_));
                }
                Tag::List(_) | Tag::BlockQuote(_) => depth += 1,
                Tag::Item => {
                    row.range = range.clone();
                    row.indent = depth;
                    row.spans.push(Inline::Text("• ".into(), style, false));
                }
                Tag::Strong => style.bold = true,
                Tag::Emphasis => style.italic = true,
                Tag::Link { dest_url, .. } => link = Some(dest_url.to_string()),
                Tag::Image {
                    dest_url, title, ..
                } => {
                    let spec = title.strip_prefix("width=").unwrap_or("480");
                    let percent = spec.ends_with('%');
                    let width = spec
                        .trim_end_matches('%')
                        .trim_end_matches("px")
                        .parse::<f32>()
                        .map_err(|_| anyhow::anyhow!("Invalid Markdown image width"))?;
                    ensure!(
                        width.is_finite() && width > 0. && width <= 4000.,
                        "Invalid Markdown image width"
                    );
                    row.spans.push(Inline::Image {
                        key: dest_url.into_string(),
                        width,
                        percent,
                    });
                    image = true;
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::TableRow
                | TagEnd::TableHead => {
                    row.range.end = range.end;
                    rows.push(std::mem::take(&mut row));
                }
                TagEnd::Item => {
                    if !row.spans.is_empty() {
                        row.range.end = range.end;
                        rows.push(std::mem::take(&mut row));
                    }
                }
                TagEnd::List(_) | TagEnd::BlockQuote(_) => depth = depth.saturating_sub(1),
                TagEnd::Strong => style.bold = false,
                TagEnd::Emphasis => style.italic = false,
                TagEnd::Link => link = None,
                TagEnd::Image => image = false,
                TagEnd::TableCell => row
                    .spans
                    .push(Inline::Text("    │    ".into(), style, false)),
                _ => {}
            },
            Event::Text(text) if !image => {
                if let Some(url) = &link {
                    row.spans
                        .push(Inline::Link(text.into_string(), url.clone()));
                } else {
                    row.spans
                        .push(Inline::Text(text.into_string(), style, row.code));
                }
            }
            Event::Code(text) => row
                .spans
                .push(Inline::Text(text.into_string(), style, true)),
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                let display = matches!(source.get(range.clone()), Some(s) if s.starts_with("$$"));
                row.spans.push(Inline::Math(math.into_string(), display));
                row.range.end = row.range.end.max(range.end);
            }
            Event::SoftBreak => row.spans.push(Inline::Text(" ".into(), style, false)),
            Event::HardBreak => row.spans.push(Inline::Text("\n".into(), style, false)),
            Event::TaskListMarker(done) => row.spans.push(Inline::Text(
                if done { "☑ " } else { "☐ " }.into(),
                style,
                false,
            )),
            Event::Rule => rows.push(Row {
                range,
                rule: true,
                ..Default::default()
            }),
            Event::Html(text) | Event::InlineHtml(text) => {
                row.spans
                    .push(Inline::Text(text.into_string(), style, true))
            }
            _ => {}
        }
    }
    if !row.spans.is_empty() {
        rows.push(row);
    }
    Ok(rows)
}
pub fn document(source: &str, images: &BTreeMap<String, String>) -> Result<Document> {
    let mut document = Document::default();
    for row in parse(source)? {
        for span in row.spans {
            document.blocks.push(Block::new(match span {
                Inline::Text(value, style, _) => Content::Text(Text::new(value, style)),
                Inline::Link(value, _) => Content::Text(Text::new(value, Style::default())),
                Inline::Math(source, display) => Content::Math { source, display },
                Inline::Image {
                    key,
                    width,
                    percent,
                } => {
                    let path = images.get(&key).ok_or_else(|| {
                        anyhow::anyhow!(
                            "Image {key} is not attached; insert it with the image button"
                        )
                    })?;
                    Content::Image {
                        path: path.clone(),
                        width: if percent { width * 8. } else { width },
                    }
                }
            }));
        }
        document.blocks.push(Block::new(Content::Text(Text::new(
            "\n\n".into(),
            Style::default(),
        ))));
    }
    Ok(document)
}
/// Migrate rich/Swift/previous LaTeX notes into Markdown without losing attachments.
pub fn encode(document: &Document) -> (String, BTreeMap<String, String>) {
    let mut source = String::new();
    let mut images = BTreeMap::new();
    for block in &document.blocks {
        match &block.content {
            Content::Text(text) => {
                let chars: Vec<_> = text.value.chars().collect();
                let mut i = 0;
                while i < chars.len() {
                    let style = text.styles.get(i).copied().unwrap_or_default();
                    let mut end = i + 1;
                    while end < chars.len()
                        && text.styles.get(end).copied().unwrap_or_default() == style
                    {
                        end += 1;
                    }
                    let mut value = String::new();
                    for c in &chars[i..end] {
                        if "\\`*{}_[]<>#$!|".contains(*c) {
                            value.push('\\');
                        }
                        value.push(*c);
                    }
                    if style.italic {
                        value = format!("*{value}*");
                    }
                    if style.bold {
                        value = format!("**{value}**");
                    }
                    source.push_str(&value);
                    i = end;
                }
            }
            Content::Math {
                source: math,
                display,
            } => source.push_str(&if *display {
                format!("\n\n$${math}$$\n\n")
            } else {
                format!("${math}$")
            }),
            Content::Image { path, width } => {
                let key = format!("{}.png", block.id);
                images.insert(key.clone(), path.clone());
                source.push_str(&format!("\n\n![Image]({key} \"width={width}\")\n\n"));
            }
        }
    }
    (source, images)
}
