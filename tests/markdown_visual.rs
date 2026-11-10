use vispace::markdown_visual::{Object, Visual};
#[test]
fn visual_typing_preserves_markdown_and_formulas() {
    let source = "# Features\n\nThe **resistance** is $R=\\frac{V}{I}$ ohms.\n\n## Applications\n\nCircuits.\n";
    let visual = Visual::new(source);
    assert!(
        visual
            .text
            .starts_with("Features\n\nThe resistance is \u{fffc} ohms.")
    );
    assert!(!visual.text.contains("**"));
    let edited = visual.text.replace("resistance", "new resistance");
    let result = visual.edit(source, &edited);
    assert!(result.contains("**new resistance**"));
    assert!(result.contains("$R=\\frac{V}{I}$"));
    assert!(result.ends_with("## Applications\n\nCircuits.\n"));
    let index = visual
        .objects
        .iter()
        .find(|(_, o)| matches!(o, Object::Math(..)))
        .unwrap()
        .0;
    let offset = visual.characters[index].source.start;
    assert_eq!(&source[offset..offset + 1], "$");
}
#[test]
fn visual_newlines_and_object_deletion_roundtrip() {
    let source = "Text $x^2$ after.\n\n![Circuit](a.png \"width=320\")\n";
    let visual = Visual::new(source);
    let edited = visual.text.replacen("Text ", "Text\nNext ", 1);
    let result = visual.edit(source, &edited);
    assert!(Visual::new(&result).text.contains("Text\nNext"));
    let edited = visual.text.replacen('\u{fffc}', "", 1);
    let result = visual.edit(source, &edited);
    assert!(!result.contains("$x^2$"));
    assert!(result.contains("![Circuit]"));
}
#[test]
fn unicode_escaped_text_and_entities_map_to_valid_source_boundaries() {
    let source = "# Résumé\n\nVoltage \\*literal\\* &amp; current Ω.\n";
    let visual = Visual::new(source);
    assert!(visual.text.contains("*literal* & current Ω."));
    for c in &visual.characters {
        assert!(source.is_char_boundary(c.source.start));
        assert!(source.is_char_boundary(c.source.end));
    }
    let result = visual.edit(source, &visual.text.replace("current", "measured current"));
    assert!(result.contains("\\*literal\\* &amp; measured current Ω."));
}
#[test]
fn empty_visual_document_accepts_text_and_literal_markdown() {
    assert_eq!(
        Visual::new("").edit("", "hello *world*"),
        "hello \\*world\\*"
    );
}

#[test]
fn repeated_enter_at_end_preserves_blank_paragraphs_and_caret() {
    let mut source = "First paragraph".to_owned();
    for count in 1..=4 {
        let visual = Visual::new(&source);
        let (edited, caret) = visual.edit_with_caret(&source, &format!("{}\n", visual.text));
        assert!(edited.ends_with(&"\n".repeat(count)), "{edited:?}");
        let next = Visual::new(&edited);
        assert_eq!(next.text, format!("First paragraph{}", "\n".repeat(count)));
        assert_eq!(caret, edited.len());
        assert_eq!(next.point(next.characters.len(), &edited), edited.len());
        source = edited;
    }
    let visual = Visual::new(&source);
    source = visual.edit(&source, &format!("{}Second paragraph", visual.text));
    assert_eq!(
        Visual::new(&source).text,
        "First paragraph\n\n\n\nSecond paragraph"
    );
}
#[test]
fn enter_in_middle_creates_one_visual_line_and_preserves_neighbors() {
    let source = "Before after $x^2$.";
    let visual = Visual::new(source);
    let (source, caret) =
        visual.edit_with_caret(source, &visual.text.replace("Before ", "Before\n"));
    assert_eq!(source, "Before\nafter $x^2$.");
    assert_eq!(&source[caret..], "after $x^2$.");
    assert_eq!(Visual::new(&source).text, "Before\nafter \u{fffc}.");
}

#[test]
fn character_by_character_typing_keeps_spaces_and_blank_paragraphs() {
    let mut source = String::new();
    for c in "First paragraph with spaces.\n\nNext paragraph.".chars() {
        let visual = Visual::new(&source);
        let edited = format!("{}{c}", visual.text);
        source = visual.edit(&source, &edited);
        if c == ' ' {
            assert!(Visual::new(&source).text.ends_with(' '));
        }
    }
    assert_eq!(source, "First paragraph with spaces.\n\nNext paragraph.");
    assert_eq!(Visual::new(&source).text, source);
    assert_eq!(Visual::new("**bold** ").text, "bold ");
}

#[test]
fn enter_before_blank_lines_does_not_split_following_heading() {
    let source = "# aaaaa\n\ntest1\n\n\n## sub-content 1\n\ntest 3";
    let visual = Visual::new(source);
    let cursor = visual.text.find("test1").unwrap() + "test1".len();
    let mut edited = visual.text.clone();
    edited.insert(cursor, '\n');
    let (result, _) = visual.edit_at_cursor(source, &edited, Some(cursor + 1));
    assert_eq!(result, source.replacen("test1", "test1\n", 1));
    assert_eq!(Visual::new(&result).text, edited);
}

#[test]
fn enter_at_heading_start_keeps_marker_with_heading_text() {
    for source in [
        "## sub-content 1\n\n\n\ntest 3",
        "Intro\n\n## sub-content 1\n\ntext",
    ] {
        let visual = Visual::new(source);
        let at = visual.text.find("sub-content").unwrap();
        let mut edited = visual.text.clone();
        edited.insert(at, '\n');
        let (result, _) = visual.edit_at_cursor(source, &edited, Some(at + 1));
        assert_eq!(
            result,
            source.replacen("## sub-content", "\n## sub-content", 1)
        );
        assert_eq!(Visual::new(&result).text, edited);
    }
}
