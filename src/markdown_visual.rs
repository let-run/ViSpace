//! Source-mapped visual editing: rendered text changes patch only their Markdown range.
use crate::notes::Style;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;
#[derive(Clone, Debug)]
pub struct Character {
    pub source: Range<usize>,
    pub style: Style,
    pub size: f32,
    pub code: bool,
}
#[derive(Clone, Debug)]
pub enum Object {
    Math(String, bool),
    Image(String, f32, bool),
}
#[derive(Clone, Debug, Default)]
pub struct Visual {
    pub text: String,
    pub characters: Vec<Character>,
    pub objects: Vec<(usize, Object)>,
}
impl Visual {
    pub fn new(source: &str) -> Self {
        let mut visual = Self::default();
        // Markdown ignores leading blank lines, but they remain editable space.
        let leading = source.bytes().take_while(|b| *b == b'\n').count();
        if leading > 0 {
            visual.push(&source[..leading], 0..leading, Style::default(), 15., false);
        }
        let mut style = Style::default();
        let mut size = 15.;
        let mut code = false;
        let mut image = false;
        for (event, range) in Parser::new_ext(
            source,
            Options::ENABLE_MATH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS,
        )
        .into_offset_iter()
        {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    size = (28. - (level as u8 - 1) as f32 * 3.).max(16.);
                    style.bold = true;
                }
                Event::Start(Tag::Item) => {
                    visual.push("• ", range.start..range.start, style, size, false)
                }
                Event::End(TagEnd::TableCell) => {
                    visual.push("    │    ", range.end..range.end, style, size, false)
                }
                Event::Start(Tag::Strong) => style.bold = true,
                Event::Start(Tag::Emphasis) => style.italic = true,
                Event::Start(Tag::CodeBlock(_)) => code = true,
                Event::Start(Tag::Image {
                    dest_url, title, ..
                }) => {
                    image = true;
                    let spec = title.strip_prefix("width=").unwrap_or("480");
                    visual.objects.push((
                        visual.characters.len(),
                        Object::Image(
                            dest_url.to_string(),
                            spec.trim_end_matches('%')
                                .trim_end_matches("px")
                                .parse()
                                .unwrap_or(480.),
                            spec.ends_with('%'),
                        ),
                    ));
                    visual.push("\u{fffc}", range, style, size, false);
                }
                Event::End(TagEnd::Image) => image = false,
                Event::End(TagEnd::Strong) => style.bold = false,
                Event::End(TagEnd::Emphasis) => style.italic = false,
                Event::End(TagEnd::Heading(_)) => {
                    style = Style::default();
                    size = 15.;
                    visual.break_line(source, range.end, 2);
                }
                Event::End(TagEnd::Paragraph) | Event::End(TagEnd::CodeBlock) => {
                    code = false;
                    visual.break_line(source, range.end, 2);
                }
                Event::End(TagEnd::Item)
                | Event::End(TagEnd::TableRow)
                | Event::End(TagEnd::TableHead) => visual.break_line(source, range.end, 1),
                Event::Text(text) if !image => {
                    // Parser offsets include escape/entity syntax, while the editor shows decoded text.
                    let mut at = range.start;
                    for c in text.chars() {
                        let start = at;
                        if source[at..range.end].starts_with('\\') {
                            at += 1;
                        }
                        if source[at..range.end].starts_with('&')
                            && let Some(end) = source[at..range.end].find(';')
                        {
                            at += end + 1;
                        } else {
                            at = (at + c.len_utf8()).min(range.end);
                        }
                        visual.push(&c.to_string(), start..at, style, size, code);
                    }
                }
                Event::Code(text) => {
                    let n = source[range.clone()]
                        .chars()
                        .take_while(|c| *c == '`')
                        .count();
                    let start = range.start + n;
                    visual.push(&text, start..range.end.saturating_sub(n), style, size, true);
                }
                Event::InlineMath(math) | Event::DisplayMath(math) => {
                    let display = source[range.clone()].starts_with("$$");
                    visual.objects.push((
                        visual.characters.len(),
                        Object::Math(math.to_string(), display),
                    ));
                    visual.push("\u{fffc}", range, style, size, false);
                }
                Event::SoftBreak | Event::HardBreak => visual.push("\n", range, style, size, code),
                Event::TaskListMarker(done) => {
                    visual.push(if done { "☑ " } else { "☐ " }, range, style, size, false)
                }
                Event::Rule => visual.break_line(source, range.end, 2),
                Event::Html(text) | Event::InlineHtml(text) => {
                    visual.push(&text, range, style, size, true)
                }
                _ => {}
            }
        }
        // Remove only synthetic paragraph spacing. Real trailing blank paragraphs
        // are editable content and must survive every frame and reopening.
        while visual.text.ends_with('\n')
            && visual
                .characters
                .last()
                .is_some_and(|c| c.source.is_empty())
        {
            visual.text.pop();
            visual.characters.pop();
        }
        // CommonMark omits unfinished trailing whitespace from text events.
        // Keep it in an editor, otherwise typing a space then a word joins them.
        let trailing = source.trim_end_matches([' ', '\t', '\r', '\n']).len();
        let mapped = visual
            .characters
            .iter()
            .map(|c| c.source.end)
            .max()
            .unwrap_or(0);
        let start = trailing.max(mapped);
        if start < source.len() {
            visual.push(
                &source[start..],
                start..source.len(),
                Style::default(),
                15.,
                false,
            );
        }
        visual
    }
    fn push(&mut self, text: &str, range: Range<usize>, style: Style, size: f32, code: bool) {
        let exact = range.len() == text.len();
        let mut at = range.start;
        for c in text.chars() {
            let mapped = if exact {
                let end = at + c.len_utf8();
                let r = at..end;
                at = end;
                r
            } else {
                range.clone()
            };
            self.characters.push(Character {
                source: mapped,
                style,
                size,
                code,
            });
        }
        self.text.push_str(text);
        debug_assert_eq!(self.text.chars().count(), self.characters.len());
    }
    fn break_line(&mut self, source: &str, end: usize, count: usize) {
        let before = usize::from(source[..end].ends_with('\n'));
        let after = source[end..].chars().take_while(|c| *c == '\n').count();
        let count = count.max(before + after);
        let existing = self.text.chars().rev().take_while(|c| *c == '\n').count();
        for n in existing..count {
            let at = end.saturating_sub(1) + n;
            let range = if source.as_bytes().get(at) == Some(&b'\n') {
                at..at + 1
            } else {
                end..end
            };
            self.push("\n", range, Style::default(), 15., false);
        }
    }
    pub fn point(&self, index: usize, source: &str) -> usize {
        self.characters.get(index).map_or_else(
            || {
                self.characters
                    .last()
                    .map_or(source.len(), |c| c.source.end)
            },
            |c| c.source.start,
        )
    }
    pub fn selection(&self, range: Range<usize>, source: &str) -> Range<usize> {
        if range.is_empty() {
            let at = self.point(range.start, source);
            return at..at;
        }
        let start = self
            .characters
            .get(range.start)
            .map_or(source.len(), |c| c.source.start);
        let end = self
            .characters
            .get(range.end.saturating_sub(1))
            .map_or(source.len(), |c| c.source.end);
        start..end.max(start)
    }
    pub fn edit(&self, source: &str, edited: &str) -> String {
        self.edit_with_caret(source, edited).0
    }
    pub fn edit_with_caret(&self, source: &str, edited: &str) -> (String, usize) {
        self.edit_at_cursor(source, edited, None)
    }
    pub fn edit_at_cursor(
        &self,
        source: &str,
        edited: &str,
        cursor: Option<usize>,
    ) -> (String, usize) {
        let old: Vec<_> = self.text.chars().collect();
        let new: Vec<_> = edited.chars().collect();
        let mut prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        // Equal blank lines make a text diff ambiguous. Anchor insertions to the
        // actual editor caret, before hidden Markdown syntax in the next block.
        if new.len() > old.len()
            && let Some(cursor) = cursor
        {
            prefix = prefix.min(cursor.saturating_sub(new.len() - old.len()));
        }
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let mut range = self.selection(prefix..old.len() - suffix, source);
        let code = self
            .characters
            .get(prefix)
            .or_else(|| self.characters.last())
            .is_some_and(|c| c.code);
        let mut inserted = String::new();
        for c in &new[prefix..new.len() - suffix] {
            if *c == '\u{fffc}' {
                continue;
            }
            if !code && "\\`*{}_[]<>#$!|".contains(*c) {
                inserted.push('\\');
            }
            inserted.push(*c);
        }
        // At the start of an ATX heading, its marker is hidden in WYSIWYG.
        // Insert a blank line before the entire heading rather than splitting
        // the marker from its text.
        if range.is_empty() && !inserted.is_empty() && inserted.chars().all(|c| c == '\n') {
            let line_start = source[..range.start].rfind('\n').map_or(0, |at| at + 1);
            let hidden = &source[line_start..range.start];
            let hashes = hidden.bytes().take_while(|b| *b == b'#').count();
            if (1..=6).contains(&hashes)
                && hidden[hashes..].starts_with([' ', '\t'])
                && hidden[hashes..].trim().is_empty()
            {
                range = line_start..line_start;
            }
        }
        let mut result = source.to_owned();
        let caret = range.start + inserted.len();
        result.replace_range(range, &inserted);
        (result, caret)
    }
}
