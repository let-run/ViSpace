use vispace::{label, model::Part};
#[test]
fn portable_label_has_print_size_readable_qr_and_similar_layout() {
    let part = Part {
        barcode: "427337E1-9D5A-4668-87F7-2C4B60118FAA".into(),
        number: "0603YD105KAT2A".into(),
        manufacturer: "Kyocera AVX".into(),
        value: "1uF".into(),
        description: "CAP CER 1UF 16V X5R 0603".into(),
        stock: 100,
        ..Default::default()
    };
    let png = label::render(&part, false).unwrap();
    let decoded = image::load_from_memory(&png).unwrap();
    let pixels = decoded.to_rgb8();
    assert_eq!(pixels.dimensions(), (591, 354));
    let dark = |x1: u32, y1: u32, x2: u32, y2: u32| {
        (x1..x2)
            .flat_map(|x| (y1..y2).map(move |y| (x, y)))
            .filter(|&(x, y)| pixels.get_pixel(x, y)[0] < 80)
            .count()
    };
    assert!(dark(10, 5, 575, 65) > 500, "Number belongs across the top");
    assert!(dark(20, 105, 200, 290) > 2000, "QR belongs at lower left");
    assert!(
        dark(215, 75, 570, 125) > 150,
        "Manufacturer belongs to right"
    );
    assert!(
        dark(215, 295, 570, 350) > 150,
        "Quantity belongs at bottom right"
    );
    assert_eq!(dark(10, 295, 195, 350), 0);
    let mut prepared = rqrr::PreparedImage::prepare(decoded.to_luma8());
    let grids = prepared.detect_grids();
    assert!(!grids.is_empty());
    assert_eq!(grids[0].decode().unwrap().1, part.barcode);
    let decoder = png::Decoder::new(std::io::Cursor::new(&png));
    let reader = decoder.read_info().unwrap();
    let dims = reader.info().pixel_dims.unwrap();
    assert_eq!((dims.xppu, dims.yppu), (11811, 11811));
    let pdf = label::render(&part, true).unwrap();
    let content = String::from_utf8_lossy(&pdf);
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(content.contains("/MediaBox [0 0 141.73228 85.03937]"));
    assert!(content.contains("/Count 1"));
    assert!(content.contains("/Subtype /Image"));
    assert!(content.ends_with("%%EOF\n"));
}
#[test]
fn long_descriptions_fit_and_label_is_deterministic() {
    let part = Part {
        number: "超長 0603YD105KAT2A full title".into(),
        description: "A long description with specifications and Unicode Ω µ ± ".repeat(20),
        stock: 999,
        ..Default::default()
    };
    assert_eq!(
        label::render(&part, false).unwrap(),
        label::render(&part, false).unwrap()
    );
    assert!(!label::render(&part, true).unwrap().is_empty());
}

#[test]
fn b1_native_raster_has_readable_qr_after_threshold_and_offset() {
    let part = Part {
        barcode: "427337E1-9D5A-4668-87F7-2C4B60118FAA".into(),
        number: "RC0402FR-07200KL".into(),
        manufacturer: "YAGEO".into(),
        stock: 100,
        description: "200kΩ 62.5mW 50V Thick Film Resistor ±1% 0402 Chip Resistor".into(),
        ..Default::default()
    };
    let raster = label::printer_raster(&part).unwrap();
    assert_eq!((raster.width, raster.height), (384, 240));
    assert_eq!(raster.bytes.len(), 48 * 240);
    assert!(raster.bytes[..48 * 4].iter().all(|b| *b == 0));
    let image = image::GrayImage::from_fn(384, 240, |x, y| {
        image::Luma([
            if raster.bytes[y as usize * 48 + x as usize / 8] & (0x80 >> (x % 8)) != 0 {
                0
            } else {
                255
            },
        ])
    });
    let mut prepared = rqrr::PreparedImage::prepare(image);
    let grids = prepared.detect_grids();
    assert!(
        !grids.is_empty(),
        "B1's packed pixels must retain a scannable QR code"
    );
    assert_eq!(grids[0].decode().unwrap().1, part.barcode);
    let mut decoder = inventory_niimbot::Decoder::default();
    for frame in raster.rows().unwrap() {
        assert_eq!(decoder.append(&frame.encoded().unwrap()), vec![frame]);
    }
}
