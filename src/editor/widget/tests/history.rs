use super::*;

#[test]
fn snapshots_restore_widget_and_reference_state_for_project_history() {
    let mut session = EditorSession::new("a\u{1f980}b\n");
    session.state.cursor = Index2::new(0, 1);
    session
        .set_reference_ranges([TextRange { start: 1, end: 2 }])
        .unwrap();
    let snapshot = session.snapshot();
    keys(&mut session, "i");
    session.handle_event(Event::Paste("changed".into()), false);
    assert_ne!(session.text(), snapshot.text());
    assert!(session.state().highlights.is_empty());
    session.restore(&snapshot).unwrap();
    assert_eq!(session.text(), "a\u{1f980}b\n");
    assert_eq!(session.state().cursor, Index2::new(0, 1));
    assert_eq!(session.state().highlights.len(), 1);
    assert_eq!(session.snapshot().text(), snapshot.text());
    assert_eq!(session.snapshot().cursor(), snapshot.cursor());
}

#[test]
fn cursor_focus_replacement_and_external_history_routes_are_public() {
    let mut session = EditorSession::new("a\u{1f980}\nβ");
    session.set_cursor_char_offset(2).unwrap();
    assert_eq!(session.cursor_char_offset(), 2);
    assert_eq!(session.state().cursor, Index2::new(0, 2));
    assert!(session.set_cursor_char_offset(99).is_err());
    assert_eq!(
        session.handle_event(key(KeyCode::Char('u')), false),
        EditorInput::Undo
    );
    assert_eq!(session.handle_event(ctrl('r'), false), EditorInput::Redo);
    session.blur();
    assert!(!session.is_focused());
    assert_eq!(
        session.handle_event(key(KeyCode::Char('i')), false),
        EditorInput::Ignored
    );
    session.focus();
    session
        .replace_text_and_ranges("x\u{1f980}y", [TextRange { start: 1, end: 2 }])
        .unwrap();
    assert_eq!(session.text(), "x\u{1f980}y");
    assert_eq!(session.state().highlights.len(), 1);
}

#[test]
fn accepted_reference_replacement_preserves_insert_mode() {
    let mut session = EditorSession::new("@src");
    session.set_cursor_char_offset(4).unwrap();
    keys(&mut session, "i");
    session
        .replace_text_and_ranges("@src/app.rs", [TextRange { start: 0, end: 11 }])
        .unwrap();
    session.set_cursor_char_offset(11).unwrap();
    assert_eq!(session.mode(), AdapterMode::Insert);
    assert_eq!(
        session.handle_event(key(KeyCode::Char(':')), false),
        EditorInput::Delegated { text_changed: true }
    );
    assert_eq!(session.text(), "@src/app.rs:");
    assert_eq!(session.mode(), AdapterMode::Insert);
    assert_eq!(session.command_line(), None);
}
