//! Portable paginated notes export. Raster pages embed bundled-font text, math and images.
use crate::markdown_notes::{self, Inline};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use anyhow::{Context, Result, ensure};
use image::{Rgba, RgbaImage};
use std::{collections::BTreeMap, io::Write};
const W: u32 = 1240;
const H: u32 = 1754;
const M: f32 = 85.;
struct Line {
    items: Vec<(RgbaImage, f32)>,
    width: f32,
    height: f32,
}
impl Default for Line {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            width: 0.,
            height: 30.,
        }
    }
}
struct Pages {
    pages: Vec<RgbaImage>,
    page: RgbaImage,
    y: f32,
    line: Line,
}
impl Pages {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            page: RgbaImage::from_pixel(W, H, Rgba([255, 255, 255, 255])),
            y: M,
            line: Line::default(),
        }
    }
    fn flush(&mut self) -> Result<()> {
        let line = std::mem::take(&mut self.line);
        if line.items.is_empty() {
            return Ok(());
        }
        if self.y + line.height > H as f32 - M {
            self.next()?;
        }
        for (image, x) in line.items {
            let y = self.y + (line.height - image.height() as f32) / 2.;
            image::imageops::overlay(&mut self.page, &image, (M + x) as i64, y as i64);
        }
        self.y += line.height + 5.;
        Ok(())
    }
    fn next(&mut self) -> Result<()> {
        ensure!(
            self.pages.len() < 100,
            "Notes exceed the 100-page PDF limit"
        );
        self.pages.push(std::mem::replace(
            &mut self.page,
            RgbaImage::from_pixel(W, H, Rgba([255, 255, 255, 255])),
        ));
        self.y = M;
        Ok(())
    }
    fn item(&mut self, mut image: RgbaImage) -> Result<()> {
        if image.width() as f32 > W as f32 - 2. * M || image.height() as f32 > H as f32 - 2. * M {
            let factor = ((W as f32 - 2. * M) / image.width() as f32)
                .min((H as f32 - 2. * M) / image.height() as f32)
                .min(1.);
            image = image::imageops::resize(
                &image,
                (image.width() as f32 * factor).max(1.) as u32,
                (image.height() as f32 * factor).max(1.) as u32,
                image::imageops::FilterType::Lanczos3,
            );
        }
        if self.line.width + image.width() as f32 > W as f32 - 2. * M {
            self.flush()?;
        }
        self.line.height = self.line.height.max(image.height() as f32);
        let x = self.line.width;
        self.line.width += image.width() as f32;
        self.line.items.push((image, x));
        Ok(())
    }
}
fn text_image(text: &str, size: f32, bold: bool, italic: bool, code: bool) -> RgbaImage {
    let font = FontRef::try_from_slice(if code {
        epaint_default_fonts::HACK_REGULAR
    } else {
        epaint_default_fonts::UBUNTU_LIGHT
    })
    .expect("Bundled font");
    let scaled = font.as_scaled(PxScale::from(size));
    let width = text
        .chars()
        .map(|c| scaled.h_advance(font.glyph_id(c)))
        .sum::<f32>();
    let mut image = RgbaImage::new(
        (width + if italic { size * 0.2 } else { 0. } + 1.)
            .ceil()
            .max(1.) as u32,
        (size * 1.35).ceil() as u32,
    );
    let mut x = 0.;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(outline) =
            font.outline_glyph(id.with_scale_and_position(size, point(x, scaled.ascent() + 2.)))
        {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, a| {
                let slant = if italic {
                    (scaled.ascent() - bounds.min.y - gy as f32) * 0.18
                } else {
                    0.
                };
                for dx in 0..=u32::from(bold) {
                    let px = bounds.min.x as i32 + gx as i32 + dx as i32 + slant as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    if px >= 0 && py >= 0 && px < image.width() as i32 && py < image.height() as i32
                    {
                        image.put_pixel(px as u32, py as u32, Rgba([25, 25, 30, (a * 255.) as u8]));
                    }
                }
            });
        }
        x += scaled.h_advance(id);
    }
    image
}
fn math_image(source: &str, display: bool) -> Result<RgbaImage> {
    let svg = crate::notes::math_svg(source, display)?
        .replace("#ebebef", "#19191e")
        .replace("rgb(235,235,239)", "rgb(25,25,30)");
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default())?;
    let size = tree.size();
    let factor = 2.;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(
        (size.width() * factor).ceil().max(1.) as u32,
        (size.height() * factor).ceil().max(1.) as u32,
    )
    .context("Equation too large")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(factor, factor),
        &mut pixmap.as_mut(),
    );
    let mut bytes = pixmap.data().to_vec();
    for p in bytes.chunks_exact_mut(4) {
        if p[3] > 0 {
            for c in 0..3 {
                p[c] = (u32::from(p[c]) * 255 / u32::from(p[3])).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(pixmap.width(), pixmap.height(), bytes).context("Invalid equation image")
}
fn attachment(path: &str) -> Result<RgbaImage> {
    use base64::Engine;
    let bytes = if path.starts_with("data:image/") {
        base64::engine::general_purpose::STANDARD
            .decode(path.split_once(',').context("Invalid image")?.1)?
    } else {
        crate::platform::fs::read(path)?
    };
    Ok(image::load_from_memory(&bytes)?.into_rgba8())
}
#[derive(Clone)]
struct Heading {
    title: String,
    level: u8,
    page: usize,
    top: f32,
}
const CONTENTS_ROWS: usize = 36;
pub fn render(source: &str, images: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    let mut pages = Pages::new();
    let mut headings = Vec::new();
    for row in markdown_notes::parse(source)? {
        let size = if row.heading > 0 {
            (46. - f32::from(row.heading - 1) * 4.).max(28.)
        } else {
            26.
        };
        if row.heading > 0 {
            if pages.y + size * 1.35 > H as f32 - M {
                pages.next()?;
            }
            headings.push(Heading {
                title: row.title(),
                level: row.heading,
                page: pages.pages.len(),
                top: pages.y,
            });
        }
        if row.rule {
            pages.flush()?;
            for x in M as u32..W - M as u32 {
                if pages.y < H as f32 - M {
                    pages
                        .page
                        .put_pixel(x, pages.y as u32, Rgba([180, 180, 185, 255]));
                }
            }
            pages.y += 15.;
            continue;
        }
        for span in row.spans {
            match span {
                Inline::Text(text, style, code) => {
                    for part in text.split_inclusive([' ', '\n']) {
                        let newline = part.ends_with('\n');
                        let part = part.trim_end_matches('\n');
                        // Character fallback wraps long URLs and code without clipping.
                        if part.chars().count() > 45 {
                            for c in part.chars() {
                                pages.item(text_image(
                                    &c.to_string(),
                                    size,
                                    style.bold || row.heading > 0,
                                    style.italic,
                                    code,
                                ))?;
                            }
                        } else if !part.is_empty() {
                            pages.item(text_image(
                                part,
                                size,
                                style.bold || row.heading > 0,
                                style.italic,
                                code,
                            ))?;
                        }
                        if newline {
                            pages.flush()?;
                        }
                    }
                }
                Inline::Link(text, _) => {
                    for part in text.split_inclusive(' ') {
                        pages.item(text_image(part, size, false, false, false))?;
                    }
                }
                Inline::Math(math, display) => {
                    if display {
                        pages.flush()?;
                    }
                    pages.item(math_image(&math, display)?)?;
                    if display {
                        pages.flush()?;
                    }
                }
                Inline::Image {
                    key,
                    width,
                    percent,
                } => {
                    pages.flush()?;
                    let path = images
                        .get(&key)
                        .with_context(|| format!("Missing attached image: {key}"))?;
                    let image = attachment(path)?;
                    let width = if percent {
                        (W as f32 - 2. * M) * width / 100.
                    } else {
                        width * 1.6
                    }
                    .min(W as f32 - 2. * M)
                    .max(1.);
                    let height = width * image.height() as f32 / image.width().max(1) as f32;
                    pages.item(image::imageops::resize(
                        &image,
                        width as u32,
                        height.max(1.) as u32,
                        image::imageops::FilterType::Lanczos3,
                    ))?;
                    pages.flush()?;
                }
            }
        }
        pages.flush()?;
        pages.y += if row.heading > 0 { 18. } else { 12. };
    }
    pages.flush()?;
    pages.pages.push(pages.page);
    let contents_count = headings.len().div_ceil(CONTENTS_ROWS);
    ensure!(
        pages.pages.len() + contents_count <= 100,
        "Notes exceed the 100-page PDF limit"
    );
    let mut contents = Vec::new();
    for chunk in headings.chunks(CONTENTS_ROWS) {
        let mut page = RgbaImage::from_pixel(W, H, Rgba([255, 255, 255, 255]));
        image::imageops::overlay(
            &mut page,
            &text_image("Contents", 46., true, false, false),
            M as i64,
            M as i64,
        );
        for (index, heading) in chunk.iter().enumerate() {
            let y = M + 85. + index as f32 * 38.;
            let indent = f32::from(heading.level.saturating_sub(1)) * 22.;
            let title: String = heading
                .title
                .chars()
                .take(60usize.saturating_sub(usize::from(heading.level) * 2))
                .collect();
            let title = if title != heading.title {
                format!("{title}…")
            } else {
                title
            };
            image::imageops::overlay(
                &mut page,
                &text_image(&title, 26., heading.level == 1, false, false),
                (M + indent) as i64,
                y as i64,
            );
            image::imageops::overlay(
                &mut page,
                &text_image(
                    &(heading.page + contents_count + 1).to_string(),
                    26.,
                    false,
                    false,
                    false,
                ),
                (W as f32 - M - 55.) as i64,
                y as i64,
            );
        }
        contents.push(page);
    }
    contents.extend(pages.pages);
    pdf(contents, &headings, contents_count)
}
fn stream(data: &[u8], dictionary: &str) -> Vec<u8> {
    let mut out = format!("<< {dictionary} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend(data);
    out.extend(b"\nendstream");
    out
}
fn pdf(pages: Vec<RgbaImage>, headings: &[Heading], contents_count: usize) -> Result<Vec<u8>> {
    let count = pages.len();
    let kids = (0..count)
        .map(|i| format!("{} 0 R", 3 + i * 3))
        .collect::<Vec<_>>()
        .join(" ");
    let root = 3 + count * 3;
    let outline = if headings.is_empty() {
        String::new()
    } else {
        format!(" /Outlines {root} 0 R /PageMode /UseOutlines")
    };
    let mut objects = vec![
        format!("<< /Type /Catalog /Pages 2 0 R{outline} >>").into_bytes(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {count} >>").into_bytes(),
    ];
    for (i, page) in pages.into_iter().enumerate() {
        let number = 3 + i * 3;
        let links = if i < contents_count {
            let ids = (i * CONTENTS_ROWS..((i + 1) * CONTENTS_ROWS).min(headings.len()))
                .map(|j| format!("{} 0 R", root + 2 + headings.len() + j))
                .collect::<Vec<_>>()
                .join(" ");
            format!(" /Annots [{ids}]")
        } else {
            String::new()
        };
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Resources << /XObject << /PageImage {} 0 R >> >> /Contents {} 0 R{links} >>",number+1,number+2).into_bytes());
        let rgb = image::DynamicImage::ImageRgba8(page).into_rgb8();
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(rgb.as_raw())?;
        let compressed = encoder.finish()?;
        objects.push(stream(&compressed,&format!("/Type /XObject /Subtype /Image /Width {W} /Height {H} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode")));
        objects.push(stream(b"q 595.28 0 0 841.89 0 0 cm /PageImage Do Q\n", ""));
    }
    if !headings.is_empty() {
        // Construct PDF outlines with real parent/sibling relationships and Unicode titles.
        let mut parents = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for (i, h) in headings.iter().enumerate() {
            while stack.last().is_some_and(|p| headings[*p].level >= h.level) {
                stack.pop();
            }
            parents.push(stack.last().copied());
            stack.push(i);
        }
        let roots: Vec<_> = parents
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_none())
            .map(|(i, _)| i)
            .collect();
        objects.push(
            format!(
                "<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {} >>",
                root + 1,
                root + 2 + roots[roots.len() - 1],
                headings.len() + 1
            )
            .into_bytes(),
        );
        // Preview treats a sole top-level item as the document root, hiding its
        // label. A real Contents destination also keeps the first heading visible.
        objects.push(format!("<< /Title (Contents) /Parent {root} 0 R /Next {} 0 R /Count 0 /A << /S /GoTo /D [3 0 R /Fit] >> >>",root+2+roots[0]).into_bytes());
        for (i, h) in headings.iter().enumerate() {
            let siblings: Vec<_> = parents
                .iter()
                .enumerate()
                .filter(|(_, p)| **p == parents[i])
                .map(|(j, _)| j)
                .collect();
            let position = siblings.iter().position(|p| *p == i).unwrap();
            let mut relations = String::new();
            if position == 0 && parents[i].is_none() {
                relations.push_str(&format!(" /Prev {} 0 R", root + 1));
            }
            if position > 0 {
                relations.push_str(&format!(" /Prev {} 0 R", root + 2 + siblings[position - 1]));
            }
            if position + 1 < siblings.len() {
                relations.push_str(&format!(" /Next {} 0 R", root + 2 + siblings[position + 1]));
            }
            let children: Vec<_> = parents
                .iter()
                .enumerate()
                .filter(|(_, p)| **p == Some(i))
                .map(|(j, _)| j)
                .collect();
            if let (Some(first), Some(last)) = (children.first(), children.last()) {
                let descendants = headings[i + 1..]
                    .iter()
                    .take_while(|child| child.level > h.level)
                    .count();
                relations.push_str(&format!(
                    " /First {} 0 R /Last {} 0 R /Count {descendants}",
                    root + 2 + first,
                    root + 2 + last
                ));
            }
            if children.is_empty() {
                relations.push_str(" /Count 0");
            }
            let title = std::iter::once(0xfeffu16)
                .chain(h.title.encode_utf16())
                .map(|c| format!("{c:04X}"))
                .collect::<String>();
            let parent = parents[i].map_or(root, |p| root + 2 + p);
            let page = 3 + (h.page + contents_count) * 3;
            let top = 841.89 - h.top / H as f32 * 841.89;
            objects.push(format!("<< /Title <{title}> /Parent {parent} 0 R /A << /S /GoTo /D [{page} 0 R /XYZ 0 {top:.2} 0] >>{relations} >>").into_bytes());
        }
        for (i, h) in headings.iter().enumerate() {
            let y = M + 85. + (i % CONTENTS_ROWS) as f32 * 38.;
            let bottom = 841.89 - (y + 36.) / H as f32 * 841.89;
            let top = 841.89 - y / H as f32 * 841.89;
            let page = 3 + (h.page + contents_count) * 3;
            let destination = 841.89 - h.top / H as f32 * 841.89;
            objects.push(format!("<< /Type /Annot /Subtype /Link /Rect [40 {bottom:.2} 555 {top:.2}] /Border [0 0 0] /Dest [{page} 0 R /XYZ 0 {destination:.2} 0] >>").into_bytes());
        }
    }
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend(object);
        out.extend(b"\nendobj\n");
    }
    let start = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in &offsets[1..] {
        out.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    Ok(out)
}
