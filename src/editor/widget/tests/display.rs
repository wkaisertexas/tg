use super::*;
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    widgets::Widget,
};

#[test]
fn application_commands_and_completion_preview_are_routed_before_widget() {
    for command in ["w", "q", "q!", "wq", "copy"] {
        let mut session = EditorSession::new("unchanged");
        assert_eq!(
            type_command(&mut session, command),
            EditorInput::CommandSubmitted(command.into())
        );
        assert_eq!(session.text(), "unchanged");
        assert_eq!(session.mode(), AdapterMode::Normal);
    }
    let mut session = EditorSession::new("text");
    assert_eq!(
        session.handle_event(ctrl('p'), true),
        EditorInput::TogglePreview
    );
    assert_eq!(
        session.handle_event(ctrl('p'), false),
        EditorInput::Delegated {
            text_changed: false
        }
    );
    assert_eq!(session.text(), "text");
    assert_eq!(
        session.handle_event(ctrl('v'), false),
        EditorInput::Delegated {
            text_changed: false
        }
    );
    assert_eq!(session.mode(), AdapterMode::VisualBlock);
    session.handle_event(key(KeyCode::Esc), false);
    assert_eq!(
        session.handle_event(key(KeyCode::Char('"')), false),
        EditorInput::Ignored
    );
    assert_eq!(
        session.handle_event(key(KeyCode::Char('a')), false),
        EditorInput::Ignored
    );
    assert_eq!(
        session.handle_event(key(KeyCode::F(1)), false),
        EditorInput::Ignored
    );
    keys(&mut session, "i:");
    assert_eq!(session.text(), ":text");
    assert_eq!(session.mode(), AdapterMode::Insert);
}

#[test]
fn editor_render_and_preview_key_options_are_configurable() {
    let mut session = EditorSession::new("one\ttwo\nthree");
    let config = EditorConfig {
        line_numbers: ConfigLineNumbers::None,
        current_line_absolute: false,
        tab_width: 8,
        wrap: false,
        ..EditorConfig::default()
    };
    session.configure(&config, "ctrl-x").unwrap();
    assert_eq!(session.line_numbers, LineNumbers::None);
    assert!(!session.current_line_absolute);
    assert_eq!(session.tab_width, 8);
    assert!(!session.wrap);
    assert_eq!(
        session.handle_event(ctrl('x'), true),
        EditorInput::TogglePreview
    );
    assert_ne!(
        session.handle_event(ctrl('p'), true),
        EditorInput::TogglePreview
    );
}

#[test]
fn global_character_ranges_map_to_multiline_unicode_highlights() {
    let mut session = EditorSession::new("α\nβ\u{1f980}z");
    session
        .set_reference_ranges([TextRange { start: 2, end: 4 }])
        .unwrap();
    assert_eq!(session.state().highlights.len(), 1);
    assert_eq!(session.state().highlights[0].start, Index2::new(1, 0));
    assert_eq!(session.state().highlights[0].end, Index2::new(1, 1));
    assert_eq!(session.text(), "α\nβ\u{1f980}z");
    assert!(
        session
            .set_reference_ranges([TextRange { start: 0, end: 99 }])
            .is_err()
    );
}

#[test]
fn relative_gutter_uses_absolute_number_on_the_cursor_row() {
    let text = (1..=12)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut session = EditorSession::new(text);
    session.state.cursor = Index2::new(8, 0);
    let area = Rect::new(0, 0, 16, 12);
    let mut buffer = Buffer::empty(area);
    session.view().render(area, &mut buffer);
    let row = |y| (0..3).map(|x| buffer[(x, y)].symbol()).collect::<String>();
    assert_eq!(row(7), " 1 ");
    assert_eq!(row(8), "9  ");
    assert_eq!(row(9), " 1 ");
}

#[test]
fn virtual_text_positions_follow_gutters_tabs_and_wrapping() {
    let area = Rect::new(2, 3, 20, 8);
    let mut session = EditorSession::new("abc\nx\tz");
    session.line_numbers = LineNumbers::None;
    session.wrap = false;
    session.tab_width = 4;
    assert_eq!(
        session.virtual_text_position(3, area),
        Some(Position::new(5, 3))
    );
    assert_eq!(
        session.virtual_text_position(7, area),
        Some(Position::new(7, 4))
    );
    let mut wrapped = EditorSession::new("abcdef\nz");
    wrapped.line_numbers = LineNumbers::None;
    wrapped.wrap = true;
    assert_eq!(
        wrapped.virtual_text_position(8, Rect::new(0, 0, 5, 5)),
        Some(Position::new(1, 2))
    );
}
