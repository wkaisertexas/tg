use super::*;

#[test]
fn diff_is_unicode_character_based_and_minimal() {
    assert_eq!(
        single_edit("a\u{1f980}b", "a日本b"),
        TextEdit::new(TextRange { start: 1, end: 2 }, "日本")
    );
}

#[test]
fn shell_insertion_points_follow_the_current_line() {
    assert_eq!(shell_insertion_point("", 0), (0, false));
    assert_eq!(shell_insertion_point("one", 1), (3, true));
    assert_eq!(shell_insertion_point("one\ntwo", 1), (4, false));
    assert_eq!(shell_insertion_point("one\ntwo", 5), (7, true));
}

#[cfg(unix)]
#[test]
fn read_shell_command_inserts_bounded_output_as_one_undoable_edit() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), &Config::default(), "first\nlast");
    app.editor.set_cursor_char_offset(1).unwrap();
    app.dispatch_command(":r !printf 'alpha\\nbeta'".into());
    assert!(app.pending_shell.is_some());
    assert_eq!(app.document.text(), "first\nlast");
    wait_for(&mut app, |app| app.pending_shell.is_none());
    assert_eq!(app.document.text(), "first\nalpha\nbeta\nlast");
    assert_eq!(app.editor.text(), app.document.text());
    assert!(app.status.starts_with("Read "));
    app.handle_event(Event::Key(event::KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::NONE,
    )));
    assert_eq!(app.document.text(), "first\nlast");
    app.dispatch_command(
        ":r !printf 'Authorization: Bearer super-secret-token' >&2; exit 7".into(),
    );
    wait_for(&mut app, |app| app.pending_shell.is_none());
    assert!(app.status.starts_with("Command failed:"));
    assert!(!app.status.contains("super-secret-token"));
    assert_eq!(app.document.text(), "first\nlast");
}

#[test]
fn accepted_file_completion_stays_inserted_for_symbol_chaining() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/app.rs"), "pub fn submit() {}\n").unwrap();
    let mut app = app_for(temp.path(), &Config::default(), "@src/app.rs");
    app.editor.set_cursor_char_offset(11).unwrap();
    app.handle_event(Event::Key(event::KeyEvent::new(
        KeyCode::Char('i'),
        KeyModifiers::NONE,
    )));
    activate_text(&mut app, "@src/app.rs");
    wait_for(&mut app, |app| {
        !app.reference_session.candidates().is_empty()
    });
    app.handle_event(Event::Key(event::KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::NONE,
    )));
    wait_for(&mut app, |app| !app.document.references().is_empty());
    assert_eq!(app.editor.mode(), AdapterMode::Insert);
    for _ in 0..2 {
        app.handle_event(Event::Key(event::KeyEvent::new(
            KeyCode::Char(':'),
            KeyModifiers::SHIFT,
        )));
    }
    assert_eq!(app.document.text(), "@src/app.rs::");
    assert_eq!(app.editor.command_line(), None);
    assert_eq!(app.editor.mode(), AdapterMode::Insert);
    assert_eq!(
        app.reference_session.active_kind(),
        Some(ReferenceKind::Symbol)
    );
}

#[test]
fn replace_mode_edits_form_one_history_group() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), &Config::default(), "abc");
    for event in [
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('R'),
            KeyModifiers::SHIFT,
        )),
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('X'),
            KeyModifiers::NONE,
        )),
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('Y'),
            KeyModifiers::NONE,
        )),
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )),
    ] {
        app.handle_event(event);
    }
    assert_eq!(app.document.text(), "XYc");
    assert!(!app.edit_group_active);
    assert!(!app.completion_active());
    app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::NONE,
    )));
    assert_eq!(app.document.text(), "abc");
    assert_eq!(app.editor.text(), "abc");
}
