use vispace::{
    attachments::AttachmentDraft,
    markdown_notes::{self, Inline},
    model::Notes,
    notes::{Content, Document},
};
use std::collections::BTreeMap;

#[test]
fn markdown_math_and_outline_respect_code_and_newlines() {
    let source = "# Features\n\nOhm: $R = \\frac{V}{I}$ looks **nice**.\n\n## Applications\n\n```rust\n# not a heading\nlet cost = \"$5\";\n```\n\n$$P = I^2R$$\n";
    let rows = markdown_notes::parse(source).unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r.heading > 0)
            .map(|r| (r.heading, r.title()))
            .collect::<Vec<_>>(),
        [(1, "Features".into()), (2, "Applications".into())]
    );
    let ohm = rows.iter().find(|r| r.title().starts_with("Ohm:")).unwrap();
    assert!(
        ohm.spans
            .iter()
            .any(|s| matches!(s,Inline::Math(s,false) if s.contains("frac")))
    );
    assert!(
        ohm.spans
            .iter()
            .any(|s| matches!(s,Inline::Text(s,style,_) if s=="nice" && style.bold))
    );
    let code = rows.iter().find(|r| r.code).unwrap();
    assert!(!code.spans.iter().any(|s| matches!(s, Inline::Math(..))));
    assert!(
        rows.iter()
            .any(|r| r.spans.iter().any(|s| matches!(s, Inline::Math(_, true))))
    );
    assert!(rows.iter().all(|r| source.is_char_boundary(r.range.start) && source.is_char_boundary(r.range.end)));
}
#[test]
fn images_have_managed_keys_and_markdown_widths() {
    let source = "![Circuit](circuit.png \"width=75%\")\n\n![Detail](detail.png \"width=320\")";
    let rows = markdown_notes::parse(source).unwrap();
    assert!(
        matches!(&rows[0].spans[0],Inline::Image {key,width,percent:true} if key=="circuit.png" && *width==75.)
    );
    assert!(markdown_notes::document(source, &BTreeMap::new()).is_err());
    let images = BTreeMap::from([
        ("circuit.png".into(), "/tmp/circuit.png".into()),
        ("detail.png".into(), "/tmp/detail.png".into()),
    ]);
    let doc = markdown_notes::document(source, &images).unwrap();
    assert!(
        doc.blocks
            .iter()
            .any(|b| matches!(&b.content,Content::Image{width,..} if *width==320.))
    );
    assert!(markdown_notes::parse("![Bad](x \"width=NaN\")").is_err());
}
#[test]
fn legacy_notes_migrate_and_markdown_reopens_without_losing_math() {
    let draft = AttachmentDraft::new().unwrap();
    let mut notes = Notes::default();
    notes.extra.insert(
        "latexSource".into(),
        serde_json::json!("The resistance is $R=\\frac{V}{I}$, \\textbf{in ohms}."),
    );
    let legacy = Document::load(&notes, &draft).unwrap();
    let (source, images) = markdown_notes::encode(&legacy);
    assert!(source.contains("**in ohms**"));
    let document = markdown_notes::document(&source, &images).unwrap();
    let mut saved = document.save(&notes);
    saved.extra.remove("latexSource");
    saved
        .extra
        .insert("markdownSource".into(), serde_json::json!(source));
    saved.images.extend(images);
    let reopened = Document::load(&saved, &draft).unwrap();
    assert!(
        reopened
            .blocks
            .iter()
            .any(|b| matches!(&b.content,Content::Math{source,..} if source.contains("frac")))
    );
}
#[test]
fn pdf_paginates_and_includes_formula_and_image() {
    let temp = std::env::temp_dir().join(format!("notes-export-{}.png", uuid::Uuid::new_v4()));
    image::RgbaImage::from_pixel(300, 120, image::Rgba([30, 160, 180, 255]))
        .save(&temp)
        .unwrap();
    let images = BTreeMap::from([("circuit.png".into(), temp.to_string_lossy().into_owned())]);
    let mut source="# Ohm’s law\n\nThe resistance is $R=\\frac{V}{I}$ measured in **ohms**.\n\n![Circuit](circuit.png \"width=320\")\n\n## Measurements\n\n".to_owned();
    source.push_str(
        &"A useful measurement with several words and an inline formula $P=I^2R$.\n\n".repeat(55),
    );
    let pdf = vispace::notes_pdf::render(&source, &images).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"));
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/Type /Outlines"));
    assert!(text.contains("/PageMode /UseOutlines"));
    assert!(text.contains("/Subtype /Link"));
    assert!(text.contains("/Title <FEFF004F0068006D201900730020006C00610077>"));
    assert!(
        text.contains("/Count 3") || text.contains("/Count 4") || text.contains("/Count 5"),
        "expected multiple pages"
    );
    if let Ok(path) = std::env::var("BOM_NOTES_PDF_PREVIEW") {
        std::fs::write(path, &pdf).unwrap();
    }
    std::fs::remove_file(temp).unwrap();
}

#[test]
fn tight_and_loose_list_items_stay_separate() {
    for source in ["- First\n- Second\n", "- First\n\n- Second\n"] {
        let rows = markdown_notes::parse(source).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].title().contains("First"));
        assert!(!rows[0].title().contains("Second"));
        assert!(rows[1].title().contains("Second"));
    }
}

#[test]
fn pdf_single_heading_has_a_real_contents_bookmark_and_explicit_destinations() {
    let bytes =
        vispace::notes_pdf::render("# Only heading\n\nBody.", &BTreeMap::new()).unwrap();
    let pdf = String::from_utf8_lossy(&bytes);
    assert!(pdf.contains("/Title (Contents)"));
    assert!(pdf.contains("/A << /S /GoTo /D ["));
    assert!(pdf.contains("/Type /Outlines /First 10 0 R /Last 11 0 R /Count 2"));
    assert!(pdf.contains("/Parent 9 0 R /Next 11 0 R"));
    assert!(pdf.contains("/Prev 10 0 R"));
    assert!(!pdf.contains("/XYZ null"));
}
