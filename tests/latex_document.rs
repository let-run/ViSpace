use vispace::{
    attachments::AttachmentDraft,
    latex_document::{encode, parse},
    model::{Notes, NotesTarget},
    notes::{Block, Content, Document, Style, Text},
    store::Store,
};
use std::collections::BTreeMap;

#[test]
fn complete_document_supports_nested_styles_paragraphs_and_both_math_modes() {
    let parsed=parse(r"\documentclass{article}\usepackage{graphicx,amsmath}\begin{document}\section{Ohm law}The \textbf{ohm \textit{law}}: $R=\frac{V}{I}$ looks nice.\par Next paragraph.\[V=IR\]\end{document}",&BTreeMap::new()).unwrap();
    assert!(parsed.document.blocks.iter().any(
        |b| matches!(&b.content,Content::Math{display:false,source} if source.contains("frac"))
    ));
    assert!(
        parsed
            .document
            .blocks
            .iter()
            .any(|b| matches!(&b.content, Content::Math { display: true, .. }))
    );
    let text: Vec<_> = parsed
        .document
        .blocks
        .iter()
        .filter_map(|b| {
            if let Content::Text(t) = &b.content {
                Some(t)
            } else {
                None
            }
        })
        .collect();
    assert!(
        text.iter()
            .any(|t| t.styles.iter().any(|s| s.bold && s.italic))
    );
    assert!(text.iter().any(|t| t.value.contains("\n\nNext paragraph")));
}
#[test]
fn source_roundtrip_preserves_literal_characters_unicode_and_formatting() {
    let document = Document {
        blocks: vec![Block::new(Content::Text(Text::new(
            "Ω {literal} $5 10% A_B & C# \\ ^ ~\nnext".into(),
            Style {
                bold: true,
                italic: true,
                underline: true,
            },
        )))],
    };
    let source = encode(&document);
    let parsed = parse(&source.text, &source.images).unwrap();
    assert_eq!(
        parsed.document.save(&Notes::default()).text,
        document.save(&Notes::default()).text
    );
    let Content::Text(text) = &parsed.document.blocks[0].content else {
        panic!()
    };
    assert!(
        text.styles
            .iter()
            .all(|s| s.bold && s.italic && s.underline)
    );
}
#[test]
fn image_positions_widths_and_repeated_images_are_preserved() {
    let map = BTreeMap::from([("photo.png".into(), "/managed/photo.png".into())]);
    let source = "Before\\includegraphics[width=320px]{photo.png}Between\\includegraphics[width=0.5\\textwidth]{photo.png}After";
    let parsed = parse(source, &map).unwrap();
    assert_eq!(parsed.images, map);
    let images: Vec<_> = parsed
        .document
        .blocks
        .iter()
        .filter_map(|b| {
            if let Content::Image { width, .. } = b.content {
                Some((b.id.clone(), width))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(images[0].1, 320.);
    assert_eq!(images[1].1, 450.);
    assert_ne!(images[0].0, images[1].0);
    let exported = encode(&parsed.document);
    assert_eq!(exported.images.len(), 2);
    assert!(exported.text.starts_with("Before\\includegraphics"));
    assert!(exported.text.contains("Between\\includegraphics"));
    assert!(exported.text.ends_with("After"));
}
#[test]
fn invalid_or_unsupported_source_is_rejected_instead_of_silently_dropped() {
    for source in [
        r"\textbf{unfinished",
        r"$x",
        r"\input{/etc/passwd}",
        r"\includegraphics{missing.png}",
        r"\includegraphics[width=NaNpx]{missing.png}",
        r"\begin{unknown}",
        r"\unknown{data}",
    ] {
        assert!(parse(source, &BTreeMap::new()).is_err(), "{source}");
    }
}
#[test]
fn source_is_persisted_for_parts_and_projects_and_reloads_from_managed_images() {
    let temp = tempfile::TempDir::new().unwrap();
    let mut store = Store::open(temp.path().join("inventory.sqlite")).unwrap();
    let draft = AttachmentDraft::new().unwrap();
    let image = draft.rgba(2, 1, vec![255; 8]).unwrap();
    let path = image["originalPath"].as_str().unwrap();
    let source =
        "% exact source comment\n\\textbf{A note} $x^2$\\includegraphics[width=240px]{photo.png}";
    let parsed = parse(source, &BTreeMap::from([("photo.png".into(), path.into())])).unwrap();
    let mut notes = parsed.document.save(&Notes::default());
    notes.images.extend(parsed.images);
    notes.extra.insert("latexSource".into(), source.into());
    store
        .change(|d| {
            d.ensure_project("Board")?;
            d.save_notes(&NotesTarget::Project("Board".into()), notes.clone())
        })
        .unwrap();
    let path = store.path.clone();
    drop(store);
    let reopened = Store::open(&path).unwrap();
    let saved = &reopened.data.projects[0].notes;
    assert_eq!(saved.extra["latexSource"], source);
    let document = Document::load(saved, &draft).unwrap();
    assert!(
        document
            .blocks
            .iter()
            .any(|b| matches!(b.content,Content::Image{width,..} if width==240.))
    );
}

#[test]
fn visual_document_save_does_not_keep_a_stale_source_override() {
    let original = Notes {
        extra: BTreeMap::from([("latexSource".into(), "Old source".into())]),
        ..Default::default()
    };
    let saved = Document::plain("Updated visually").save(&original);
    assert!(!saved.extra.contains_key("latexSource"));
    let draft = AttachmentDraft::new().unwrap();
    let loaded = Document::load(&saved, &draft).unwrap();
    assert_eq!(loaded.save(&Notes::default()).text, "Updated visually");
}
