//! Portable 50 × 30 mm labels. The PNG and PDF share one Rust raster layout.
use crate::model::Part;
use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use anyhow::{Context, Result};
use image::{Rgb, RgbImage};
use std::io::Write;
const WIDTH: u32 = 591;
const HEIGHT: u32 = 354;
const SCALE: f32 = 300. / 72.;
const PAGE_WIDTH: f32 = 50. * 72. / 25.4;
const PAGE_HEIGHT: f32 = 30. * 72. / 25.4;
fn font() -> FontRef<'static> {
    FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).expect("Bundled Ubuntu font")
}
fn text_width(text: &str, size: f32) -> f32 {
    let f = font();
    let s = f.as_scaled(PxScale::from(size * SCALE));
    let mut width = 0.;
    let mut last = None;
    for ch in text.chars() {
        let glyph = f.glyph_id(ch);
        if let Some(last) = last {
            width += s.kern(last, glyph);
        }
        width += s.h_advance(glyph);
        last = Some(glyph);
    }
    width
}
fn wrap(text: &str, size: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for paragraph in text.split('\n') {
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() {
                word.into()
            } else {
                format!("{current} {word}")
            };
            if text_width(&candidate, size) > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
            if text_width(&current, size) > width {
                let mut line = String::new();
                for ch in std::mem::take(&mut current).chars() {
                    let candidate = format!("{line}{ch}");
                    if text_width(&candidate, size) > width && !line.is_empty() {
                        lines.push(std::mem::take(&mut line));
                    }
                    line.push(ch);
                }
                current = line;
            }
        }
        lines.push(std::mem::take(&mut current));
    }
    lines
}
fn draw(image: &mut RgbImage, text: &str, region: [f32; 4], size: f32, bold: bool, single: bool) {
    let [x, y, width, height] = region;
    let (sx, sy) = if image.dimensions() == (WIDTH, HEIGHT) {
        (SCALE, SCALE)
    } else {
        (
            image.width() as f32 / PAGE_WIDTH,
            image.height() as f32 / PAGE_HEIGHT,
        )
    };
    let mut size = size;
    let normalized = text.replace(['\r', '\n'], " ");
    let lines = loop {
        let lines = if single {
            vec![normalized.clone()]
        } else {
            wrap(text, size, width * SCALE)
        };
        if size <= 0.5
            || (lines.len() as f32 * size * SCALE * 1.18 <= height * SCALE
                && lines.iter().all(|s| text_width(s, size) <= width * SCALE))
        {
            break lines;
        }
        size -= 0.25;
    };
    let f = font();
    let scaled = f.as_scaled(PxScale {
        x: size * sx,
        y: size * sy,
    });
    let ascent = scaled.ascent();
    for (line_index, line) in lines.iter().enumerate() {
        let mut cursor = x * sx;
        let baseline = y * sy + ascent + line_index as f32 * size * sy * 1.18;
        let mut previous = None;
        for ch in line.chars() {
            let id = f.glyph_id(ch);
            if let Some(last) = previous {
                cursor += scaled.kern(last, id);
            }
            let glyph = id.with_scale_and_position(scaled.scale(), point(cursor, baseline));
            if let Some(outline) = f.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    for dx in 0..=u32::from(bold) {
                        let px = bounds.min.x as i32 + gx as i32 + dx as i32;
                        let py = bounds.min.y as i32 + gy as i32;
                        if px >= 0
                            && py >= 0
                            && px < image.width() as i32
                            && py < image.height() as i32
                        {
                            let pixel = image.get_pixel_mut(px as u32, py as u32);
                            let value = (f32::from(pixel[0]) * (1. - coverage)).round() as u8;
                            *pixel = Rgb([value; 3]);
                        }
                    }
                });
            }
            cursor += scaled.h_advance(id);
            previous = Some(id);
        }
    }
}
pub fn raster(part: &Part) -> Result<RgbImage> {
    raster_at_size(part, WIDTH, HEIGHT)
}
fn raster_at_size(part: &Part, width: u32, height: u32) -> Result<RgbImage> {
    let sx = if width == WIDTH {
        SCALE
    } else {
        width as f32 / PAGE_WIDTH
    };
    let sy = if height == HEIGHT {
        SCALE
    } else {
        height as f32 / PAGE_HEIGHT
    };
    let mut image = RgbImage::from_pixel(width, height, Rgb([255; 3]));
    draw(
        &mut image,
        &part.number,
        [4., 3., PAGE_WIDTH - 8., 13.],
        10.,
        true,
        true,
    );
    let qr =
        qrcode::QrCode::with_error_correction_level(part.barcode.as_bytes(), qrcode::EcLevel::M)?;
    let quiet = 2;
    let modules = qr.width() + quiet * 2;
    let pixels = (42. * sx / modules as f32).floor().max(1.) as u32;
    let left = (5. * sx).round() as u32;
    let top = ((PAGE_HEIGHT - 60.) * sy).round() as u32;
    for y in 0..qr.width() {
        for x in 0..qr.width() {
            if qr[(x, y)] == qrcode::Color::Dark {
                for dy in 0..pixels {
                    for dx in 0..pixels {
                        let px = left + (x + quiet) as u32 * pixels + dx;
                        let py = top + (y + quiet) as u32 * pixels + dy;
                        if px < width && py < height {
                            image.put_pixel(px, py, Rgb([0; 3]));
                        }
                    }
                }
            }
        }
    }
    let w = PAGE_WIDTH - 57.;
    draw(
        &mut image,
        &part.manufacturer,
        [53., PAGE_HEIGHT - 66., w, 12.],
        7.5,
        false,
        false,
    );
    let specs = [
        part.value.as_str(),
        part.tolerance.as_str(),
        part.package.as_str(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" • ");
    draw(
        &mut image,
        &specs,
        [53., PAGE_HEIGHT - 53., w, 11.],
        7.,
        true,
        false,
    );
    draw(
        &mut image,
        &part.description,
        [53., PAGE_HEIGHT - 41., w, 28.],
        7.,
        false,
        false,
    );
    draw(
        &mut image,
        &format!("Qty: {}", part.stock),
        [53., PAGE_HEIGHT - 13., w, 11.],
        8.,
        true,
        true,
    );
    Ok(image)
}
/// Render directly at the B1 print-head resolution, preserving whole QR modules.
pub fn printer_raster(part: &Part) -> Result<inventory_niimbot::Raster> {
    let rgba = image::DynamicImage::ImageRgb8(raster_at_size(part, 384, 240)?).into_rgba8();
    inventory_niimbot::Raster::packed_rgba(384, 240, rgba.as_raw(), 4)
}
pub fn render(part: &Part, pdf: bool) -> Result<Vec<u8>> {
    let image = raster(part)?;
    if pdf {
        return pdf_data(image.as_raw());
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, WIDTH, HEIGHT);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_pixel_dims(Some(png::PixelDimensions {
            xppu: 11811,
            yppu: 11811,
            unit: png::Unit::Meter,
        }));
        encoder.write_header()?.write_image_data(image.as_raw())?;
    }
    Ok(bytes)
}
fn pdf_data(rgb: &[u8]) -> Result<Vec<u8>> {
    let mut compressor =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    compressor.write_all(rgb)?;
    let compressed = compressor.finish()?;
    let commands = format!("q {PAGE_WIDTH:.5} 0 0 {PAGE_HEIGHT:.5} 0 0 cm /Label Do Q\n");
    let mut image=format!("<< /Type /XObject /Subtype /Image /Width {WIDTH} /Height {HEIGHT} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",compressed.len()).into_bytes();
    image.extend(compressed);
    image.extend(b"\nendstream");
    let objects=[b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_WIDTH:.5} {PAGE_HEIGHT:.5}] /Resources << /XObject << /Label 4 0 R >> >> /Contents 5 0 R >>").into_bytes(),image,format!("<< /Length {} >>\nstream\n{}endstream",commands.len(),commands).into_bytes()];
    let mut pdf = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.into_iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend(object);
        pdf.extend(b"\nendobj\n");
    }
    let start = pdf.len();
    pdf.extend(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n").as_bytes(),
    );
    Ok(pdf)
}
pub fn copy(part: &Part, clipboard: &mut crate::platform::clipboard::Clipboard) -> Result<()> {
    let image = image::DynamicImage::ImageRgb8(raster(part)?).into_rgba8();
    clipboard
        .set_image(crate::platform::clipboard::ImageData {
            width: WIDTH as usize,
            height: HEIGHT as usize,
            bytes: std::borrow::Cow::Owned(image.into_raw()),
        })
        .context("Could not copy label image")
}
