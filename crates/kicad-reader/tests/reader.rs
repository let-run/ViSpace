use inventory_kicad::*;
use std::collections::BTreeMap;
fn files() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "demo.kicad_pro".into(),
            include_str!("fixtures/demo.kicad_pro").into(),
        ),
        (
            "demo.kicad_sch".into(),
            include_str!("fixtures/demo.kicad_sch").into(),
        ),
        (
            "power.kicad_sch".into(),
            include_str!("fixtures/power.kicad_sch").into(),
        ),
    ])
}
#[test]
fn handwritten_json_handles_unicode_numbers_and_rejects_bad_input() {
    let parsed =
        json::parse(r#"{"name":"\u03bc \ud83e\uddf0","n":-1.25e2,"array":[true,false,null]}"#)
            .unwrap();
    assert_eq!(parsed.get("name").unwrap().string(), Some("μ 🧰"));
    for invalid in [
        r#"{"x":1,"x":2}"#,
        "[1,]",
        "01",
        "1.",
        r#""\ud800""#,
        r#""\q""#,
    ] {
        assert!(json::parse(invalid).is_err(), "{invalid}");
    }
}
#[test]
fn sexpr_quotes_comments_errors_and_limits() {
    let root =
        syntax::parse("; comment\n(root (text \"μ \\\"quoted\\\"\\nnext\") (future 1 2))").unwrap();
    assert_eq!(
        root.child("text").unwrap().value(1),
        Some("μ \"quoted\"\nnext")
    );
    assert!(root.child("future").is_some());
    assert_eq!(syntax::parse("(root\n (bad").unwrap_err().line, 2);
    assert!(syntax::parse(&format!("{}x{}", "(".repeat(140), ")".repeat(140))).is_err());
}
#[test]
fn graphics_symbol_units_rotation_mirror_and_hidden_text() {
    let scene = Schematic::parse(include_str!("fixtures/demo.kicad_sch")).unwrap();
    assert_eq!(scene.symbol_count, 1);
    assert_eq!(scene.sheets.len(), 3);
    let rectangle = scene
        .primitives
        .iter()
        .find_map(|p| match p {
            Primitive::Path {
                points, fill: true, ..
            } => Some(points),
            _ => None,
        })
        .unwrap();
    assert!((rectangle[0].x - 52.).abs() < 1e-8 && (rectangle[0].y - 39.).abs() < 1e-8);
    let texts: Vec<_> = scene
        .primitives
        .iter()
        .filter_map(|p| match p {
            Primitive::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!texts.contains(&"SHOULD_NOT_RENDER"));
    assert!(!texts.contains(&"Hidden"));
    assert!(texts.contains(&"R1A"));
    assert!(
        scene.bounds.max.x < 200.,
        "Unused symbol units must not affect drawing bounds"
    );
}
#[test]
fn hierarchy_shared_instances_and_missing_child_are_distinct() {
    let sources = files();
    let bundle = load_project("demo.kicad_pro", |p| {
        sources
            .get(p)
            .cloned()
            .ok_or_else(|| Error::new("Missing file"))
    })
    .unwrap();
    assert_eq!(bundle.schematics.len(), 2);
    assert_eq!(bundle.root.children.len(), 3);
    assert_eq!(bundle.root.children[0].instance_path, "/root/sheet-a");
    assert_eq!(bundle.root.children[1].instance_path, "/root/sheet-b");
    assert!(bundle.root.children[2].error.is_some());
    let references = bundle.schematics["power.kicad_sch"]
        .primitives
        .iter()
        .find_map(|p| match p {
            Primitive::Text { instances, .. } if !instances.is_empty() => Some(instances),
            _ => None,
        })
        .unwrap();
    assert_eq!(references["/root/sheet-a"], "C101");
    assert_eq!(references["/root/sheet-b"], "C201");
}
#[test]
fn hierarchy_cycles_do_not_recurse_forever() {
    let mut sources = files();
    sources.insert("power.kicad_sch".into(),"(kicad_sch(version 20250114)(uuid power)(sheet(uuid loop)(property \"Sheetname\" \"Loop\")(property \"Sheetfile\" \"demo.kicad_sch\")))".into());
    let bundle = load_project("demo.kicad_pro", |p| {
        sources
            .get(p)
            .cloned()
            .ok_or_else(|| Error::new("Missing file"))
    })
    .unwrap();
    assert!(
        bundle.root.children[0].children[0]
            .error
            .as_ref()
            .unwrap()
            .contains("Circular")
    );
}
#[test]
fn path_resolution_and_arcs() {
    assert_eq!(
        resolve_sheet("sheets/root.kicad_sch", "../power.kicad_sch"),
        "power.kicad_sch"
    );
    assert_eq!(
        resolve_sheet("root.kicad_sch", r"sub\power.kicad_sch"),
        "sub/power.kicad_sch"
    );
    assert_eq!(
        resolve_sheet("root.kicad_sch", "/shared/power.kicad_sch"),
        "/shared/power.kicad_sch"
    );
    let points = arc_points(Point::new(1., 0.), Point::new(0., 1.), Point::new(-1., 0.));
    assert!(
        points
            .iter()
            .all(|p| p.y >= -1e-6 && (p.x * p.x + p.y * p.y - 1.).abs() < 1e-6)
    );
}

#[test]
fn project_directory_variable_resolves_from_nested_sheet() {
    let mut sources = files();
    let nested = sources
        .remove("demo.kicad_sch")
        .unwrap()
        .replace("power.kicad_sch", "${KIPRJMOD}/power.kicad_sch");
    sources.insert("design/demo.kicad_sch".into(), nested);
    sources.insert("design/demo.kicad_pro".into(), "{}".into());
    let child = sources.remove("power.kicad_sch").unwrap();
    sources.insert("design/power.kicad_sch".into(), child);
    let bundle = load_project("design/demo.kicad_pro", |name| {
        sources.get(name).cloned().ok_or_else(|| Error::new(name))
    })
    .unwrap();
    assert_eq!(bundle.root.children[0].file, "design/power.kicad_sch");
    assert!(bundle.root.children[0].error.is_none());
}

#[test]
fn reversed_labels_are_readable_and_kicad_filename_escapes_are_decoded() {
    let scene = Schematic::parse(
        r#"(kicad_sch (version 20250114) (uuid root)
       (label "D13{slash}SCK" (at 10 20 180) (effects (font (size 1 1)) (justify left bottom))))"#,
    )
    .unwrap();
    assert!(scene.primitives.iter().any(|p| matches!(p,
        Primitive::Text { text, angle, align, .. }
        if text == "D13/SCK" && *angle == 0. && *align == [-1, 1])));
}

#[test]
fn symbol_backgrounds_precede_internal_strokes_and_pin_names_follow_direction() {
    let scene = Schematic::parse(r#"(kicad_sch (version 20250114) (uuid root)
      (lib_symbols (symbol "Test:MOS" (pin_names (offset 0.5))
       (symbol "MOS_1_1"
        (polyline (pts (xy -1 1) (xy -1 -1)) (fill (type none)))
        (rectangle (start -2 2) (end 2 -2) (fill (type background)))
        (pin input line (at -5 0 0) (length 3) (name "G" (effects (font (size 1 1)))) (number "1" (effects (font (size 1 1)))))
        (pin passive line (at 0 -5 90) (length 3) (name "S" (effects (font (size 1 1)))) (number "2" (effects (font (size 1 1))))))))
      (symbol (lib_id "Test:MOS") (at 10 10 0) (unit 1)))"#).unwrap();
    assert!(matches!(
        scene.primitives[0],
        Primitive::Path {
            fill: true,
            solid: false,
            ..
        }
    ));
    assert!(scene.primitives.iter().any(|p| matches!(p,Primitive::Text{text,angle,align,..} if text=="G" && *angle==0. && *align==[-1,0])));
    assert!(
        scene
            .primitives
            .iter()
            .any(|p| matches!(p,Primitive::Text{text,angle,..} if text=="S" && *angle==90.))
    );
}

#[test]
fn global_labels_share_text_and_outline_direction() {
    let scene = Schematic::parse(r#"(kicad_sch (version 20250114) (uuid root)
       (global_label "I2C_SDA" (shape input) (at 10 20 180) (effects (font (size 1.27 1.27)) (justify right)))
       (global_label "USB2_N" (shape input) (at 30 20 0) (effects (font (size 1.27 1.27)) (justify left))))"#).unwrap();
    for (label, side, alignment) in [("I2C_SDA", -1., 1), ("USB2_N", 1., -1)] {
        let anchor = if label == "I2C_SDA" { 10. } else { 30. };
        let text = scene
            .primitives
            .iter()
            .find(|p| matches!(p,Primitive::Text{text,..} if text==label))
            .unwrap();
        assert!(
            matches!(text,Primitive::Text{at,angle,align,..} if (at.x-anchor)*side>0. && *angle==0. && align[0]==alignment)
        );
    }
}

#[test]
fn embedded_images_decode_with_physical_scale_and_bad_images_are_warnings() {
    let source = r#"(kicad_sch (version 20250114) (uuid root)
       (image (at 10 20) (scale 2) (data "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACXBIWXMAAC4jAAAuIwF4pT92AAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="))
       (image (at 0 0) (data "invalid!")))"#;
    let scene = Schematic::parse(source).unwrap();
    assert_eq!(scene.warnings.len(), 1);
    assert!(scene.primitives.iter().any(|p| matches!(p,Primitive::Bitmap{center,size,width,height,rgba} if *center==Point::new(10.,20.) && *width==1 && *height==1 && rgba==&[255,0,0,255] && (size.x-0.169333).abs()<0.00001)));
}

#[test]
fn symbol_field_angles_and_justification_include_parent_rotation() {
    let scene = Schematic::parse(r#"(kicad_sch (version 20250114) (uuid root)
       (lib_symbols (symbol "Test:C" (symbol "C_1_1")))
       (symbol (lib_id "Test:C") (at 20 30 90) (unit 1)
        (property "Reference" "C12" (at 20 24 90) (effects (font (size 1.27 1.27)))))
       (symbol (lib_id "Test:C") (at 50 30 180) (unit 1)
        (property "Reference" "R14" (at 53 29 0) (effects (font (size 1.27 1.27)) (justify right)))))"#).unwrap();
    assert!(scene.primitives.iter().any(|p| matches!(p,Primitive::Text{text,at,angle,..} if text=="C12" && *at==Point::new(20.,24.) && *angle==0.)));
    assert!(scene.primitives.iter().any(|p| matches!(p,Primitive::Text{text,angle,align,..} if text=="R14" && *angle==0. && align[0]==-1)));
}

#[test]
fn pin_numbers_use_same_side_for_opposite_directions_after_symbol_transforms() {
    for rotation in [0, 90, 180, 270] {
        for mirror in ["", "(mirror x)", "(mirror y)"] {
            let source = format!(
                r#"(kicad_sch (version 20250114) (uuid root)
              (lib_symbols (symbol "Test:Four" (pin_names (hide yes)) (symbol "Four_1_1"
                (pin passive line (at -5 0 0) (length 3) (number "L"))
                (pin passive line (at 5 0 180) (length 3) (number "R"))
                (pin passive line (at 0 5 270) (length 3) (number "T"))
                (pin passive line (at 0 -5 90) (length 3) (number "B")))))
              (symbol (lib_id "Test:Four") (at 10 20 {rotation}) (unit 1) {mirror}))"#
            );
            let scene = Schematic::parse(&source).unwrap();
            let mut pins = scene.primitives.chunks_exact(2);
            for pair in &mut pins {
                let Primitive::Path { points, .. } = &pair[0] else {
                    panic!("pin line missing")
                };
                let Primitive::Text { at, angle, .. } = &pair[1] else {
                    panic!("pin number missing")
                };
                let midpoint = Point::new(
                    (points[0].x + points[1].x) / 2.,
                    (points[0].y + points[1].y) / 2.,
                );
                let expected = if *angle == 0. {
                    Point::new(midpoint.x, midpoint.y - 0.65)
                } else {
                    Point::new(midpoint.x - 0.65, midpoint.y)
                };
                assert!(
                    (at.x - expected.x).abs() < 1e-9 && (at.y - expected.y).abs() < 1e-9,
                    "rotation {rotation}, {mirror}: {at:?} != {expected:?}"
                );
            }
            assert!(pins.remainder().is_empty());
        }
    }
}

#[test]
fn mirrored_rotated_power_symbols_use_sheet_axes() {
    for (angle, mirror, expected) in [
        (90, "(mirror x)", Point::new(6., 20.)),
        (270, "", Point::new(14., 20.)),
        (0, "(mirror x)", Point::new(10., 24.)),
    ] {
        let source = format!(
            r#"(kicad_sch (version 20250114) (uuid root)
    (lib_symbols (symbol "power:+3.3V" (symbol "+3.3V_0_1" (polyline (pts (xy 0 0) (xy 0 4)) (fill (type none))))))
    (symbol (lib_id "power:+3.3V") (at 10 20 {angle}) {mirror} (unit 1)))"#
        );
        let scene = Schematic::parse(&source).unwrap();
        let Primitive::Path { points, .. } = &scene.primitives[0] else {
            panic!("missing power arrow")
        };
        assert!(
            (points[1].x - expected.x).abs() < 1e-9 && (points[1].y - expected.y).abs() < 1e-9,
            "{angle} {mirror}: {:?}",
            points[1]
        );
    }
}

#[test]
fn renamed_embedded_symbol_uses_lib_name_without_changing_bom_identity() {
    let source = r#"(kicad_sch (version 20250114)
      (lib_symbols
        (symbol "MCU" (symbol "MCU_1_1" (rectangle (start -1 -1) (end 1 1))))
        (symbol "MCU_1"
          (symbol "MCU_1_0_0" (pin passive line (at -10 0 0) (length 3) (name "IO") (number "2")))
          (symbol "MCU_1_1_0" (pin power_in line (at 0 10 270) (length 3) (name "VDD") (number "1")))
          (symbol "MCU_1_1_1" (rectangle (start -7 7) (end 7 -7) (fill (type background))))))
      (symbol (lib_name "MCU_1") (lib_id "MCU") (at 30 30 0) (unit 1)
        (property "Reference" "IC1" (at 20 20 0)) (property "Value" "MCU" (at 30 40 0))))"#;
    let scene = Schematic::parse(source).unwrap();
    assert_eq!(scene.components[0].lib_id, "MCU");
    let graphics = &scene.components[0].primitives;
    assert!(!graphics.is_empty());
    assert!(graphics.iter().all(|&i| i < scene.primitives.len()));
    assert!(graphics.iter().any(|&i| matches!(
        scene.primitives[i],
        Primitive::Path {
            ink: Ink::Symbol,
            ..
        }
    )));
    assert!(
        graphics
            .iter()
            .any(|&i| matches!(scene.primitives[i], Primitive::Path { ink: Ink::Pin, .. }))
    );
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    assert!(
        scene
            .primitives
            .iter()
            .any(|p| matches!(p,Primitive::Text{text,..} if text=="VDD"))
    );
    assert!(
        scene
            .primitives
            .iter()
            .any(|p| matches!(p,Primitive::Text{text,..} if text=="IO"))
    );
    assert!(scene.primitives.iter().any(|p|matches!(p,Primitive::Path{points,fill:true,..} if points.contains(&Point::new(23.,23.)) && points.contains(&Point::new(37.,37.)))));
}
#[test]
fn text_boxes_preserve_size_fill_alpha_alignment_and_legacy_margins() {
    let source = r#"(kicad_sch (version 20250114)
      (text_box "A long paragraph to wrap inside its box.\nExplicit second line."
        (at 10 20 0) (size 64.77 6.35) (stroke (width 0) (type default))
        (fill (type color) (color 35 187 255 0.5))
        (effects (font (size 1.27 1.27))))
      (text_box "Top left" (at 100 20 90) (size 10 20)
        (stroke (width -1)) (fill (type none)) (margins 1 2 3 4)
        (effects (font (size 2 2) bold italic) (justify left top))))"#;
    let scene = Schematic::parse(source).unwrap();
    let Primitive::TextBox {
        at,
        size,
        text,
        fill_color,
        margins,
        align,
        stroke_width,
        ..
    } = &scene.primitives[0]
    else {
        panic!("Expected a text box")
    };
    assert_eq!(*at, Point::new(10., 20.));
    assert_eq!(*size, Point::new(64.77, 6.35));
    assert!(text.contains('\n'));
    assert_eq!(*fill_color, Some([35, 187, 255, 128]));
    assert_eq!(*align, [0, 0]);
    assert!((*stroke_width - 0.15).abs() < 1e-9);
    assert!(margins.iter().all(|m| (*m - 0.9525).abs() < 1e-9));
    assert!(
        matches!(&scene.primitives[1],Primitive::TextBox{angle,align,bold:true,italic:true,margins,stroke_width,fill_color:None,..} if *angle==90. && *align==[-1,-1] && *margins==[1.,2.,3.,4.] && *stroke_width==0.)
    );
    // Fit-to-content includes the entire rectangle, not the unwrapped text width.
    assert!((scene.bounds.max.x - 125.).abs() < 1e-9);
}

#[test]
fn nets_include_pin_graphics_after_symbol_background_sorting() {
    let scene = Schematic::parse(
        r##"(kicad_sch (version 20250114)
      (lib_symbols (symbol "power:PWR" (power)
        (symbol "PWR_1_1"
          (pin power_in line (at 0 0 90) (length 2.54)
            (name "VCC") (number "1"))
          (rectangle (start -1 -1) (end 1 1) (fill (type background))) )))
      (symbol (lib_id "power:PWR") (at 10 10 0) (unit 1)
        (property "Reference" "#PWR1" (at 10 10 0) (effects (hide yes)))
        (property "Value" "+3V3" (at 10 7 0)))
      (wire (pts (xy 10 10) (xy 20 10)))
      (global_label "+3V3" (at 40 40 0))
      (label "OTHER" (at 60 60 0)))"##,
    )
    .unwrap();
    let net = scene
        .nets
        .iter()
        .find(|n| n.aliases.iter().any(|n| n == "+3V3"))
        .unwrap();
    assert!(net.primitives.iter().all(|&i| i < scene.primitives.len()));
    for label in ["VCC", "1", "+3V3"] {
        assert!(
            net.primitives
                .iter()
                .any(|&i| matches!(&scene.primitives[i],Primitive::Text{text,..} if text==label)),
            "missing {label}"
        );
    }
    assert!(
        net.primitives
            .iter()
            .any(|&i| matches!(&scene.primitives[i], Primitive::Path { ink: Ink::Wire, .. }))
    );
    assert!(
        !net.primitives
            .iter()
            .any(|&i| matches!(&scene.primitives[i],Primitive::Text{text,..} if text=="OTHER"))
    );
}

#[test]
fn project_nets_resolve_hierarchy_globals_and_repeated_sheet_local_scope() {
    let root = r#"(kicad_sch (version 20250114) (uuid root)
      (label "INPUT" (at 10 10 0))
      (sheet (at 10 10) (size 10 10) (uuid a)
        (property "Sheetname" "First") (property "Sheetfile" "child.kicad_sch")
        (pin "IN" input (at 10 10 0)))
      (sheet (at 50 50) (size 10 10) (uuid b)
        (property "Sheetname" "Second") (property "Sheetfile" "child.kicad_sch")
        (pin "IN" input (at 50 50 0)))
      (global_label "+3V3" (at 90 90 0)))"#;
    let child = r#"(kicad_sch (version 20250114) (uuid child)
      (hierarchical_label "IN" (at 10 10 0))
      (global_label "+3V3" (at 20 20 0))
      (label "LOCAL" (at 30 30 0)))"#;
    let bundle = load_project("test.kicad_pro", |file| {
        Ok(match file {
            "test.kicad_pro" => "{}",
            "test.kicad_sch" => root,
            "child.kicad_sch" => child,
            "test.kicad_pcb" => return Err(Error::new("No board in this fixture")),
            _ => panic!("{file}"),
        }
        .into())
    })
    .unwrap();
    assert_eq!(
        bundle
            .nets
            .iter()
            .find(|n| n.name == "+3V3")
            .unwrap()
            .members
            .len(),
        3
    );
    let input = bundle.nets.iter().find(|n| n.name == "INPUT").unwrap();
    assert_eq!(input.members.len(), 2);
    assert!(input.members.iter().any(|m| m.instance == "/root/a"));
    assert!(!input.members.iter().any(|m| m.instance == "/root/b"));
    assert_eq!(
        bundle
            .nets
            .iter()
            .filter(|n| n.name.contains("LOCAL"))
            .count(),
        2
    );
}

#[test]
fn imported_symbol_preserves_custom_white_fill_and_outline_color() {
    let mut sources = files();
    let source = sources.get_mut("demo.kicad_sch").unwrap();
    *source = source
        .replace(
            "(fill (type background))",
            "(fill (type color) (color 255 255 255 1))",
        )
        .replace(
            "(stroke (width 0.2) (type default))",
            "(stroke (width 0.2) (type default) (color 128 0 0 1))",
        );
    let bundle = load_project("demo.kicad_pro", |p| {
        sources
            .get(p)
            .cloned()
            .ok_or_else(|| Error::new("Missing file"))
    })
    .unwrap();
    assert!(
        bundle.schematics["demo.kicad_sch"]
            .primitives
            .iter()
            .any(|p| matches!(
                p,
                Primitive::Path {
                    fill: true,
                    fill_color: Some([255, 255, 255, 255]),
                    stroke_color: Some([128, 0, 0, 255]),
                    ..
                }
            ))
    );
}
