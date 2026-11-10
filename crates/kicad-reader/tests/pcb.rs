use inventory_kicad::{Pcb, PcbShape, Point, load_project};

const BOARD: &str = r#"(kicad_pcb (version 20260206)
(layers (0 "F.Cu" signal) (4 "In1.Cu" power) (2 "B.Cu" signal) (5 "F.SilkS" user) (25 "Edge.Cuts" user) (1 "F.Mask" user) (3 "B.Mask" user))
(gr_rect (start 0 0) (end 40 30) (stroke (width 0.05)) (fill none) (layer "Edge.Cuts"))
(segment (start 2 2) (end 8 2) (width 0.25) (layer "F.Cu"))
(arc (start 8 2) (mid 10 4) (end 8 6) (width 0.25) (layer "F.Cu"))
(via (at 8 6) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu"))
(footprint "Demo" (layer "F.Cu") (at 10 10 90)
 (property "Reference" "U1" (at 0 0 90) (layer "F.SilkS") (effects (font (size 1 1))))
 (fp_line (start 0 0) (end 4 0) (stroke (width 0.1)) (layer "F.SilkS"))
 (pad "1" thru_hole oval (at 2 0 90) (size 4 2) (drill oval 2 1) (layers "*.Cu" "*.Mask"))
 (pad "2" smd roundrect (at 0 2 90) (size 2 1) (layers "F.Cu") (roundrect_rratio 0.25)))
(zone (net 1) (layer "F.Cu") (filled_polygon (layer "F.Cu") (pts (xy 1 1) (xy 6 1) (xy 6 5) (xy 3 3) (xy 1 5)))))"#;

#[test]
fn board_reads_layers_tracks_arcs_footprints_drills_and_filled_zones() {
    let board = Pcb::parse(BOARD).unwrap();
    assert_eq!(board.layers.len(), 7);
    assert_eq!(board.footprints, 1);
    assert_eq!(board.footprint_list[0].reference, "U1");
    assert_eq!(board.footprint_list[0].library, "Demo");
    assert_eq!(board.footprint_list[0].properties["Reference"], "U1");
    assert_eq!(board.footprint_list[0].layer, "F.Cu");
    assert!(
        board.footprint_list[0]
            .bounds
            .contains(Point::new(10., 10.))
    );
    assert!(board.items.iter().any(|item| item.reference == "U1"));
    assert_eq!(board.holes.len(), 1);
    assert_eq!(board.vias.len(), 1);
    assert_eq!(board.vias[0].drill, 0.4);
    assert_eq!(board.vias[0].layers.len(), 3);
    assert_eq!(board.bounds.min, Point::new(0., 0.));
    assert_eq!(board.bounds.max, Point::new(40., 30.));
    assert!(board.warnings.is_empty(), "{:?}", board.warnings);
    assert!(board.items.iter().any(|item| item.zone && item.net == "1"));
    assert!(board.items.iter().any(|item| item.layers.len() == 5));
    let pin = board
        .items
        .iter()
        .find(|item| item.layers.len() == 5)
        .unwrap();
    let PcbShape::Path {
        points, triangles, ..
    } = &pin.shape
    else {
        panic!("pad path")
    };
    assert!(!triangles.is_empty());
    let (min_y, max_y) = points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
            (a.min(p.y), b.max(p.y))
        });
    assert!(
        (min_y - 6.).abs() < 1e-6 && (max_y - 10.).abs() < 1e-6,
        "{min_y} {max_y}"
    );
    assert!(board.items.iter().any(
        |item| matches!(&item.shape,PcbShape::Path{points,filled:false,..} if points.len()>10)
    ));
    assert!(
        board
            .items
            .iter()
            .any(|item| matches!(&item.shape,PcbShape::Text{text,..} if text=="U1"))
    );
}

#[test]
fn project_loads_optional_board_without_breaking_schematic_only_projects() {
    let load = |board: Option<&str>| {
        load_project("demo.kicad_pro", |path| match path {
            "demo.kicad_pro" => Ok("{}".into()),
            "demo.kicad_sch" => Ok("(kicad_sch (version 20250114) (uuid root))".into()),
            "demo.kicad_pcb" => board
                .map(str::to_owned)
                .ok_or_else(|| inventory_kicad::Error::new("missing")),
            _ => Err(inventory_kicad::Error::new("missing")),
        })
        .unwrap()
    };
    assert!(load(None).pcb.is_none());
    assert!(load(Some(BOARD)).pcb.is_some());
    assert!(load(Some("(bad)")).pcb_error.is_some());
}

#[test]
fn rejects_other_documents_and_supports_user_board_smoke_fixture() {
    assert!(Pcb::parse("(kicad_sch)").is_err());
    if let Ok(path) = std::env::var("KICAD_PCB_FIXTURE") {
        let board = Pcb::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(board.footprints > 0 && !board.items.is_empty());
        eprintln!(
            "{} footprints, {} items, {} layers; warnings: {:?}",
            board.footprints,
            board.items.len(),
            board.layers.len(),
            board.warnings
        );
    }
}

#[test]
fn vias_preserve_modern_and_legacy_net_names_and_blind_layer_spans() {
    for net in ["(net 1)", "(net \"GND\")"] {
        let source = format!(
            r#"(kicad_pcb (layers (0 "F.Cu" signal) (2 "In1.Cu" signal) (4 "In2.Cu" signal) (31 "B.Cu" signal)) (net 1 "GND") (via blind (at 5 8) (size 0.6) (drill 0.3) (layers "F.Cu" "In1.Cu") {net}))"#
        );
        let board = Pcb::parse(&source).unwrap();
        assert_eq!(board.vias[0].net, "GND");
        assert_eq!(board.vias[0].layers, vec!["F.Cu", "In1.Cu"]);
    }
}

#[test]
fn clearance_precedence_and_project_netclass_patterns() {
    let source = r#"(kicad_pcb (version 20260206) (layers (0 "F.Cu" signal))
    (net 1 "USB_D+")
    (segment (start 0 0) (end 10 0) (width 0.2) (layer "F.Cu") (net 1))
    (footprint "Test" (at 10 20 90) (clearance 0.3)
      (pad "1" smd rect (at 0 0 90) (size 2 1) (layers "F.Cu") (net 1) (clearance 0.4))
      (pad "2" smd rect (at 3 0 90) (size 2 1) (layers "F.Cu") (net 1))
      (pad "3" smd rect (at 6 0 90) (size 2 1) (layers "F.Cu") (net 1) (clearance 0)))
    (footprint "Test" (at 0 0)
      (pad "4" smd rect (at 0 0) (size 2 1) (layers "F.Cu") (net 1))
      (pad "5" smd rect (at 3 0) (size 2 1) (layers "F.Cu") (net "GND"))))"#;
    let mut board = Pcb::parse(source).unwrap();
    board.apply_project_settings(r#"{"net_settings":{"classes":[{"name":"Default","clearance":0.1},{"name":"USB","priority":0,"clearance":0.25}],"netclass_patterns":[{"pattern":"USB*","netclass":"USB"}]}}"#).unwrap();
    assert_eq!(
        board.pads.iter().map(|p| p.clearance).collect::<Vec<_>>(),
        vec![0.4, 0.3, 0., 0.25, 0.1]
    );
    assert_eq!(board.pads[3].clearance_source, "Net class: USB");
    assert_eq!(board.pads[0].net, "USB_D+");
    assert!(board.items.iter().any(|item| item.net == "USB_D+"));
    // Rotated 2 x 1 mm pad at (10,20), expanded by 0.4 mm on each side.
    let contour = &board.pads[0].clearance_contour;
    let min_x = contour.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let max_y = contour
        .iter()
        .map(|p| p.y)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!((min_x - 9.1).abs() < 1e-6);
    assert!((max_y - 21.4).abs() < 1e-6);
    let old = Pcb::parse(&source.replace("20260206", "20240108")).unwrap();
    assert_eq!(old.pads[2].clearance, 0.3); // pre-9 zero inherits footprint
}

#[test]
fn exact_netclass_assignments_and_class_priority_are_resolved() {
    let mut board = Pcb::parse(BOARD).unwrap();
    for pad in &mut board.pads {
        pad.net = "DATA1".into();
    }
    board.apply_project_settings(r#"{"net_settings":{"classes":[{"name":"Default","clearance":0.1},{"name":"Low","priority":5,"clearance":0.2},{"name":"High","priority":0,"clearance":0.35}],"netclass_assignments":{"DATA1":["High"]},"netclass_patterns":[{"pattern":"DATA?","netclass":"Low"}]}}"#).unwrap();
    assert!(board.pads.iter().all(|p| p.clearance == 0.35));
}

#[test]
fn schematic_loading_defers_board_parsing_and_board_loading_never_reads_sheets() {
    use inventory_kicad::{Error, load_board, load_schematics};
    let mut reads = Vec::new();
    let bundle = load_schematics("demo.kicad_pro", |path| {
        reads.push(path.to_owned());
        match path {
            "demo.kicad_pro" => Ok("{}".into()),
            "demo.kicad_sch" => Ok("(kicad_sch (version 20250114) (uuid root))".into()),
            // Deliberately invalid: discovery must not parse this file.
            "demo.kicad_pcb" => Ok("not parsed yet".into()),
            _ => Err(Error::new("Missing")),
        }
    })
    .unwrap();
    assert!(bundle.pcb_available);
    assert!(bundle.pcb.is_none());
    assert!(bundle.pcb_error.is_none());
    assert!(!reads.iter().any(|p| p.ends_with(".kicad_dru")));
    let board = load_board("demo.kicad_pro", |path| match path {
        "demo.kicad_pro" => Ok("{}".into()),
        "demo.kicad_pcb" => Ok(BOARD.into()),
        "demo.kicad_dru" => Err(Error::new("No custom rules")),
        _ => panic!("Board load must not reread schematics: {path}"),
    })
    .unwrap();
    assert_eq!(board.footprints, 1);
    assert!(!board.items.is_empty());
}

#[test]
fn omitted_pad_angle_is_absolute_zero_and_does_not_inherit_footprint_rotation() {
    let board = Pcb::parse(r#"(kicad_pcb (layers (0 "F.Cu" signal))
        (footprint "Rotated" (layer "F.Cu") (at 10 20 90) (property "Reference" "U5")
            (pad "1" thru_hole oval (at 1 0) (size 0.8 0.3) (layers "F.Cu") (drill oval 0.2 0.1 (offset 0.1 0)))
            (pad "2" smd rect (at 1 1 90) (size 0.8 0.3) (layers "F.Cu"))
            (pad "3" smd roundrect (at 1 2 0) (size 0.8 0.3) (layers "F.Cu"))
            (pad "4" smd rect (at 1 3 -90) (size 0.8 0.3) (layers "F.Cu"))))"#).unwrap();
    for (i, pad) in board.pads.iter().enumerate() {
        let center_x = if i == 0 { 10.1 } else { 10. + i as f64 };
        assert!((pad.at.x - center_x).abs() < 1e-6);
        assert!((pad.at.y - 19.).abs() < 1e-6);
        let min_x = pad
            .contour
            .iter()
            .map(|p| p.x)
            .fold(f64::INFINITY, f64::min);
        let max_x = pad
            .contour
            .iter()
            .map(|p| p.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = pad
            .contour
            .iter()
            .map(|p| p.y)
            .fold(f64::INFINITY, f64::min);
        let max_y = pad
            .contour
            .iter()
            .map(|p| p.y)
            .fold(f64::NEG_INFINITY, f64::max);
        let expected = if i % 2 == 0 { (0.8, 0.3) } else { (0.3, 0.8) };
        assert!(((min_x + max_x) / 2. - center_x).abs() < 1e-6);
        assert!((max_x - min_x - expected.0).abs() < 1e-6);
        assert!((max_y - min_y - expected.1).abs() < 1e-6);
    }
    assert_eq!(board.pads[0].angle, 0.);
    let hole = &board.holes[0];
    let min_x = hole.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let max_x = hole.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
    assert!(
        ((min_x + max_x) / 2. - 10.).abs() < 1e-6,
        "the drill stays at the pad anchor"
    );
}

#[test]
fn reads_3d_models_placement_visibility_thickness_and_embedded_payloads() {
    let board=Pcb::parse(r#"(kicad_pcb (general (thickness 2.4)) (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (footprint "Demo" (layer "B.Cu") (at 5 6 90) (property "Reference" "U1")
            (model "${KICAD10_3DMODEL_DIR}/U.step" (offset (xyz 1 2 3)) (scale (xyz 2 3 4)) (rotate (xyz 10 20 30)))
            (model "hidden.step" (hide yes)))
        (embedded_files (file (name "part.step") (data "QUJD" "REVG"))))"#).unwrap();
    assert_eq!(board.thickness, 2.4);
    assert_eq!(board.footprint_list[0].angle, 90.);
    let models = &board.footprint_list[0].models;
    assert_eq!(models[0].offset, [1., 2., 3.]);
    assert_eq!(models[0].scale, [2., 3., 4.]);
    assert_eq!(models[0].rotate, [10., 20., 30.]);
    assert!(!models[0].hidden);
    assert!(models[1].hidden);
    assert_eq!(models[1].scale, [1.; 3]);
    assert_eq!(board.embedded_files["part.step"], "QUJDREVG");
}

#[test]
fn outline_font_cache_uses_exact_board_coordinates_and_avoids_duplicate_text() {
    let board=Pcb::parse(r#"(kicad_pcb
        (layers (0 "F.Cu" signal) (5 "F.SilkS" user) (6 "B.SilkS" user))
        (footprint "Demo" (layer "F.Cu") (at 100 200 90)
            (property "Reference" "U1" (at 1 2 90) (layer "F.SilkS")
                (effects (font (face "Century Gothic") (size 1 1)) (justify right))
                (render_cache "U1" 90 (polygon (pts (xy 98 199) (xy 99 199) (xy 99 200) (xy 98 200))))))
        (gr_text "HIDDEN" (at 0 0) (layer "B.SilkS") (effects (font (size 1 1)) (hide yes))
            (render_cache "HIDDEN" 0 (polygon (pts (xy 0 0) (xy 1 0) (xy 0 1)))))
        (gr_text "changed" (at 5 5) (layer "F.SilkS") (effects (font (size 1 1)))
            (render_cache "old" 0 (polygon (pts (xy 0 0) (xy 1 0) (xy 0 1))))))"#).unwrap();
    let outlines: Vec<_> = board
        .items
        .iter()
        .filter_map(|item| match &item.shape {
            PcbShape::Path {
                points,
                triangles,
                filled: true,
                ..
            } => Some((item, points, triangles)),
            _ => None,
        })
        .collect();
    assert_eq!(outlines.len(), 1);
    assert_eq!(outlines[0].0.reference, "U1");
    assert_eq!(outlines[0].1[0], Point::new(98., 199.));
    assert_eq!(outlines[0].2.len(), 2);
    let texts: Vec<_> = board
        .items
        .iter()
        .filter_map(|item| match &item.shape {
            PcbShape::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["changed"]);
}

#[test]
fn footprint_library_table_preserves_model_search_directories() {
    let libraries=inventory_kicad::footprint_model_libraries(r#"(fp_lib_table (version 7)
        (lib (name "TPS25751DREFR") (type "KiCad") (uri "${KIPRJMOD}/Lib/TPS25751DREFR") (options "") (descr ""))
        (lib (name "Space library") (uri "${KIPRJMOD}/some folder/parts.pretty")))"#).unwrap();
    assert_eq!(libraries["TPS25751DREFR"], "${KIPRJMOD}/Lib/TPS25751DREFR");
    assert_eq!(
        libraries["Space library"],
        "${KIPRJMOD}/some folder/parts.pretty"
    );
    assert!(inventory_kicad::footprint_model_libraries("(kicad_pcb)").is_err());
}


#[test]
fn rotated_smd_pad_offset_keeps_adjacent_trace_clear() {
    let board = Pcb::parse(r#"(kicad_pcb (version 20260206) (layers (0 "F.Cu" signal))
        (footprint "T-Micro32" (layer "F.Cu") (at 76.5 135 180) (property "Reference" "U2")
            (pad "1" smd rect (at 0 0 180) (size 2.1 0.7) (drill (offset -0.3 0)) (layers "F.Cu"))
            (pad "2" smd rect (at 0 1 180) (size 2.1 0.7) (drill (offset -0.3 0)) (layers "F.Cu"))))"#).unwrap();
    let trace_right_edge = 75.474999 + 0.25 / 2.;
    for pad in &board.pads {
        assert!((pad.at.x - 76.8).abs() < 1e-6);
        assert!(pad.contour.iter().all(|point| point.x > trace_right_edge));
    }
    assert!(board.holes.is_empty());
}
