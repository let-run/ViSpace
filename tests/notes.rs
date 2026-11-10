use base64::Engine;
use vispace::{
    attachments::AttachmentDraft,
    model::Notes,
    notes::{Content, Document, Style, Text, math_svg},
};
#[test]
fn rich_notes_and_image_sizes_roundtrip_without_platform_apis() {
    let draft = AttachmentDraft::new().unwrap();
    let image = draft
        .rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255])
        .unwrap();
    let notes = Notes {
        extra: std::collections::BTreeMap::from([(
            "visualTokens".into(),
            serde_json::json!([
                {"kind":"text","value":"Bold Ω ","bold":true,"underline":true},
                {"kind":"math","value":"V=IR","display":true},
                {"kind":"image","value":"picture.png","width":598}
            ]),
        )]),
        images: std::collections::BTreeMap::from([(
            "picture.png".into(),
            image["originalPath"].as_str().unwrap().into(),
        )]),
        ..Default::default()
    };
    let document = Document::load(&notes, &draft).unwrap();
    let saved = document.save(&notes);
    let reloaded = Document::load(&saved, &draft).unwrap();
    let Content::Text(text) = &reloaded.blocks[0].content else {
        panic!()
    };
    assert_eq!(text.value, "Bold Ω ");
    assert!(text.styles.iter().all(|s| s.bold && s.underline));
    assert!(
        matches!(&reloaded.blocks[1].content,Content::Math{source,display:true}if source=="V=IR")
    );
    assert!(
        matches!(&reloaded.blocks[2].content,Content::Image{path,width}if *width==598.&&std::path::Path::new(path).is_file())
    );
    assert!(saved.rtf.is_empty());
    assert!(saved.text.contains("\\[V=IR\\]"));
    assert!(!saved.extra.contains_key("visualHTML"));
}
#[test]
fn imported_rtf_keeps_styles_unicode_equations_and_attachments() {
    let draft = AttachmentDraft::new().unwrap();
    let rtf = r"{\rtf1\ansi\uc1{\fonttbl{\f0 Helvetica;}}\b Bold\b0  \i italic\i0  \ul under\ulnone  \u937? \\(V=IR\\)\par {\NeXTGraphic image.tiff \width11960 \height5980} }";
    let notes = Notes {
        rtf: base64::engine::general_purpose::STANDARD.encode(rtf),
        images: std::collections::BTreeMap::from([(
            "image.tiff".into(),
            "/existing/image.tiff".into(),
        )]),
        ..Default::default()
    };
    let doc = Document::load(&notes, &draft).unwrap();
    let saved = doc.save(&notes);
    assert!(saved.text.starts_with("Bold italic under Ω"));
    assert!(saved.text.contains("\\(V=IR\\)"));
    assert!(
        saved.extra["visualTokens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["bold"] == true)
    );
    assert!(
        saved.extra["visualTokens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["italic"] == true)
    );
    assert!(
        matches!(&doc.blocks.iter().find(|b|matches!(b.content,Content::Image{..})).unwrap().content,Content::Image{width,..}if *width==598.)
    );
}
#[test]
fn unicode_editing_preserves_formats_around_insertions_and_deletions() {
    let mut text = Text::new(
        "αβγ".into(),
        Style {
            bold: true,
            ..Default::default()
        },
    );
    text.replace(
        "αXβγ".into(),
        Style {
            italic: true,
            ..Default::default()
        },
    );
    assert_eq!(text.styles.len(), 4);
    assert!(text.styles[0].bold && text.styles[1].italic && text.styles[2].bold);
    text.toggle(1..3, 2);
    assert!(text.styles[1].underline && text.styles[2].underline);
    text.replace("αγ".into(), Style::default());
    assert_eq!(text.styles.len(), 2);
    assert!(text.styles.iter().all(|s| s.bold));
}
#[test]
fn offline_math_renders_fractions_roots_and_display_equations() {
    for source in [
        "V=IR",
        r"\frac{1}{2}",
        r"\sqrt{x^2+y^2}",
        r"\sum_{n=0}^{\infty} x_n",
    ] {
        let svg = math_svg(source, true).unwrap();
        assert!(svg.contains("<svg") && svg.contains("<path"));
        assert!(!svg.contains("<script"));
    }
    let mut doc = Document::plain("Ohm law: $V=IR$ and $$P=I^2R$$");
    doc.convert_all_math();
    assert_eq!(
        doc.blocks
            .iter()
            .filter(|b| matches!(b.content, Content::Math { .. }))
            .count(),
        2
    );
}
#[test]
fn broken_legacy_images_do_not_silently_disappear() {
    let draft = AttachmentDraft::new().unwrap();
    let notes = Notes {
        extra: std::collections::BTreeMap::from([(
            "visualTokens".into(),
            serde_json::json!([{"kind":"image","value":"missing.png"}]),
        )]),
        ..Default::default()
    };
    assert!(Document::load(&notes, &draft).is_err());
}

#[test]
fn incomplete_display_delimiters_remain_editable_text() {
    let mut partial = Document::plain("$$V=IR$");
    partial.convert_all_math();
    assert!(
        partial
            .blocks
            .iter()
            .all(|b| matches!(b.content, Content::Text(_)))
    );
    let mut full = Document::plain("$$V=IR$$");
    full.convert_all_math();
    assert!(matches!(
        &full.blocks[0].content,
        Content::Math { display: true, .. }
    ));
}

#[test]
fn inline_equation_inserts_at_unicode_selection_and_roundtrips_with_surrounding_text() {
    let mut document = Document::plain("Ω before SELECT after");
    let id = document.blocks[0].id.clone();
    let Content::Text(text) = &mut document.blocks[0].content else {
        panic!()
    };
    text.toggle(0..8, 0);
    let tail = document
        .insert_inline_math(&id, 9..15, "V=IR".into())
        .unwrap();
    assert_eq!(document.blocks[2].id, tail);
    assert!(
        matches!(&document.blocks[0].content, Content::Text(t) if t.value == "Ω before " && t.styles[0].bold)
    );
    assert!(
        matches!(&document.blocks[1].content, Content::Math{source,display:false} if source=="V=IR")
    );
    assert!(matches!(&document.blocks[2].content, Content::Text(t) if t.value==" after"));
    let saved = document.save(&Notes::default());
    assert_eq!(saved.text, "Ω before \\(V=IR\\) after");
    let draft = AttachmentDraft::new().unwrap();
    let reloaded = Document::load(&saved, &draft).unwrap();
    assert!(matches!(
        &reloaded.blocks[1].content,
        Content::Math { display: false, .. }
    ));
    assert!(matches!(&reloaded.blocks[2].content,Content::Text(t) if t.value==" after"));
}

#[test]
fn inline_and_display_delimiters_keep_distinct_layout_intent() {
    let mut document = Document::plain("Before $x^2$ after $$y=2$$ tail");
    document.convert_all_math();
    let modes: Vec<_> = document
        .blocks
        .iter()
        .filter_map(|b| match b.content {
            Content::Math { display, .. } => Some(display),
            _ => None,
        })
        .collect();
    assert_eq!(modes, [false, true]);
    assert_eq!(
        document.save(&Notes::default()).text,
        "Before \\(x^2\\) after \\[y=2\\] tail"
    );
}

#[test]
fn inline_equations_keep_editable_text_slots_after_reload() {
    let draft = AttachmentDraft::new().unwrap();
    let mut doc = Document::plain("$x$ before $y$");
    doc.convert_all_math();
    let saved = doc.save(&Notes::default());
    let mut loaded = Document::load(&saved, &draft).unwrap();
    loaded.ensure_inline_text();
    assert!(
        matches!(&loaded.blocks.first().unwrap().content,Content::Text(t) if t.value.is_empty())
    );
    assert!(
        matches!(&loaded.blocks.last().unwrap().content,Content::Text(t) if t.value.is_empty())
    );
    let original_length = loaded.blocks.len();
    loaded.ensure_inline_text();
    assert_eq!(loaded.blocks.len(), original_length);
    assert_eq!(loaded.save(&Notes::default()).text, saved.text);
}

#[test]
fn flow_editing_newlines_moves_equations_and_keeps_following_text() {
    use vispace::notes::InlineRun;
    let mut doc = Document::plain("The ohm law: $R=\\frac{V}{I}$ looks nice\nNext line");
    doc.convert_all_math();
    let mut run = InlineRun::from_blocks(&doc.blocks);
    assert_eq!(
        run.text.value,
        "The ohm law: \u{fffc} looks nice\nNext line"
    );
    let equation = run.equations.values().next().unwrap().id.clone();
    run.replace(format!("Intro\n{}", run.text.value), Style::default());
    assert_eq!(run.equations[&19].id, equation);
    let blocks = run.into_blocks(doc.blocks[0].id.clone());
    let saved = Document { blocks }.save(&Notes::default());
    assert_eq!(
        saved.text,
        "Intro\nThe ohm law: \\(R=\\frac{V}{I}\\) looks nice\nNext line"
    );
}

#[test]
fn unified_flow_selection_can_delete_equations_and_insert_at_caret() {
    use vispace::notes::InlineRun;
    let mut doc = Document::plain("Before $x$ after");
    doc.convert_all_math();
    let mut run = InlineRun::from_blocks(&doc.blocks);
    run.replace("Before  after".into(), Style::default());
    assert!(run.equations.is_empty());
    let caret = run.insert_equation(7..8, "R=\\frac{V}{I}".into(), Style::default());
    assert_eq!(caret, 8);
    assert_eq!(run.text.value, "Before \u{fffc}after");
    assert!(
        matches!(&run.equations[&7].content,Content::Math{source,..} if source=="R=\\frac{V}{I}")
    );
}
