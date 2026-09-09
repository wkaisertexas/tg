use super::*;

#[test]
fn exact_text_unicode_tabs_and_terminal_newline_round_trip() {
    let source = "first\tcolumn\n日本語 \u{1f980}\n";
    let session = EditorSession::new(source);
    assert_eq!(session.text(), source);
    assert_eq!(session.snapshot().text(), source);
}

#[test]
fn unicode_edit_and_bracketed_paste_are_character_safe() {
    let mut session = EditorSession::new("a\u{1f980}b\t\n");
    keys(&mut session, "liX");
    session.handle_event(key(KeyCode::Esc), false);
    assert_eq!(session.text(), "aX\u{1f980}b\t\n");
    let mut pasted = EditorSession::new("");
    keys(&mut pasted, "i");
    let result = pasted.handle_event(Event::Paste("a\t\u{1f980}\nβ\n".into()), false);
    assert_eq!(result, EditorInput::Delegated { text_changed: true });
    assert_eq!(pasted.text(), "a\t\u{1f980}\nβ\n");
}

#[test]
fn visual_character_and_line_edits_delegate_to_edtui() {
    let mut character = EditorSession::new("abcd");
    keys(&mut character, "v");
    assert_eq!(character.mode(), AdapterMode::VisualCharacter);
    keys(&mut character, "ld");
    assert_eq!(character.text(), "cd");
    assert_eq!(character.mode(), AdapterMode::Normal);
    let mut line = EditorSession::new("a\nb\nc\n");
    line.handle_event(shift('V'), false);
    assert_eq!(line.mode(), AdapterMode::VisualLine);
    keys(&mut line, "jd");
    assert_eq!(line.text(), "c\n");
    assert_eq!(line.mode(), AdapterMode::Normal);
}

#[test]
fn counted_motions_and_operator_composition_follow_vim_counts() {
    let mut motion = EditorSession::new("one two three four");
    keys(&mut motion, "2w");
    assert_eq!(motion.state().cursor, Index2::new(0, 8));
    let mut operator = EditorSession::new("one two three four five");
    keys(&mut operator, "2d2w");
    assert_eq!(operator.text(), "five");
    assert_eq!(
        operator.register_text('"').as_deref(),
        Some("one two three four ")
    );
    let mut lines = EditorSession::new("one\ntwo\nthree\nfour\n");
    keys(&mut lines, "2dd");
    assert_eq!(lines.text(), "three\nfour\n");
}

#[test]
fn word_text_objects_cover_inner_and_around_forms() {
    let mut inner = EditorSession::new("one two");
    keys(&mut inner, "ldiw");
    assert_eq!(inner.text(), " two");
    let mut around = EditorSession::new("one two");
    keys(&mut around, "ldaw");
    assert_eq!(around.text(), "two");
}

#[test]
fn quote_and_bracket_text_objects_cover_inner_and_around_forms() {
    for (source, cursor, object, expected) in [
        ("say \"hello world\" now", 6, "i\"", "say \"\" now"),
        ("call(alpha + beta) now", 7, "a(", "call now"),
        ("map[key + value] tail", 5, "i]", "map[] tail"),
        ("map{key + value} tail", 5, "a}", "map tail"),
        ("say 'hello' now", 6, "a'", "say  now"),
    ] {
        let mut session = EditorSession::new(source);
        session.set_cursor_char_offset(cursor).unwrap();
        keys(&mut session, "d");
        keys(&mut session, object);
        assert_eq!(session.text(), expected, "object {object}");
    }
}

#[test]
fn change_and_yank_compose_with_motions_and_text_objects() {
    let mut change = EditorSession::new("one two three");
    keys(&mut change, "2cwX");
    change.handle_event(key(KeyCode::Esc), false);
    assert_eq!(change.text(), "Xthree");
    assert_eq!(change.register_text('"').as_deref(), Some("one two "));
    let mut quoted = EditorSession::new("say \"hello world\" now");
    quoted.set_cursor_char_offset(6).unwrap();
    keys(&mut quoted, "\"aci\"X");
    quoted.handle_event(key(KeyCode::Esc), false);
    assert_eq!(quoted.text(), "say \"X\" now");
    assert_eq!(quoted.register_text('a').as_deref(), Some("hello world"));
    assert_eq!(quoted.register_text('"').as_deref(), Some("hello world"));
    let mut yank = EditorSession::new("call(alpha + beta) now");
    yank.set_cursor_char_offset(7).unwrap();
    keys(&mut yank, "\"bya(");
    assert_eq!(yank.text(), "call(alpha + beta) now");
    assert_eq!(yank.register_text('b').as_deref(), Some("(alpha + beta)"));
    assert_eq!(yank.register_text('"').as_deref(), Some("(alpha + beta)"));
    let mut motion = EditorSession::new("one two three");
    keys(&mut motion, "2yw");
    assert_eq!(motion.text(), "one two three");
    assert_eq!(motion.register_text('"').as_deref(), Some("one two "));
    let mut line_change = EditorSession::new("one\ntwo\n");
    keys(&mut line_change, "ccX");
    line_change.handle_event(key(KeyCode::Esc), false);
    assert_eq!(line_change.text(), "X\ntwo\n");
    let mut line_yank = EditorSession::new("one\ntwo\n");
    keys(&mut line_yank, "yy");
    assert_eq!(line_yank.text(), "one\ntwo\n");
    assert_eq!(line_yank.register_text('"').as_deref(), Some("one\n"));
}

#[test]
fn cancelled_and_unsupported_operator_sequences_do_not_leak_state() {
    let mut cancelled = EditorSession::new("one two");
    keys(&mut cancelled, "\"a2d");
    cancelled.handle_event(key(KeyCode::Esc), false);
    keys(&mut cancelled, "yiw");
    assert_eq!(cancelled.text(), "one two");
    assert_eq!(cancelled.register_text('a'), None);
    assert_eq!(cancelled.register_text('"').as_deref(), Some("one"));
    let mut unsupported = EditorSession::new("one two");
    keys(&mut unsupported, "di");
    assert_eq!(
        unsupported.handle_event(key(KeyCode::Char('z')), false),
        EditorInput::Ignored
    );
    keys(&mut unsupported, "w");
    assert_eq!(unsupported.text(), "one two");
    assert_eq!(unsupported.state().cursor, Index2::new(0, 4));
}

#[test]
fn dot_repeats_the_last_delegated_change() {
    let mut delete = EditorSession::new("one two three");
    keys(&mut delete, "dw.");
    assert_eq!(delete.text(), "three");
}

#[test]
fn replace_operation_replace_mode_and_join_are_available() {
    let mut operation = EditorSession::new("abc");
    keys(&mut operation, "rX");
    assert_eq!(operation.text(), "Xbc");
    assert_eq!(operation.mode(), AdapterMode::Normal);
    let mut mode = EditorSession::new("abc");
    mode.handle_event(shift('R'), false);
    keys(&mut mode, "XY");
    mode.handle_event(key(KeyCode::Esc), false);
    assert_eq!(mode.text(), "XYc");
    assert_eq!(mode.mode(), AdapterMode::Normal);
    let mut join = EditorSession::new("one \ntwo\nthree");
    join.handle_event(shift('J'), false);
    assert_eq!(join.text(), "one two\nthree");
}

#[test]
fn visual_block_registers_delete_change_insert_append_and_paste() {
    let mut yank = EditorSession::new("αβγ\nδεζ\n");
    yank.set_cursor_char_offset(1).unwrap();
    keys(&mut yank, "\"a");
    yank.handle_event(ctrl('v'), false);
    keys(&mut yank, "jly");
    assert_eq!(yank.register_text('a').as_deref(), Some("βγ\nεζ"));
    assert_eq!(yank.register_text('"').as_deref(), Some("βγ\nεζ"));
    yank.set_cursor_char_offset(0).unwrap();
    yank.handle_event(ctrl('v'), false);
    keys(&mut yank, "jp");
    assert_eq!(yank.text(), "βγβγ\nεζεζ\n");
    for (operation, expected) in [('I', "aXbc\ndXef\n"), ('A', "abXc\ndeXf\n")] {
        let mut session = EditorSession::new("abc\ndef\n");
        session.set_cursor_char_offset(1).unwrap();
        session.handle_event(ctrl('v'), false);
        keys(&mut session, "j");
        session.handle_event(shift(operation), false);
        keys(&mut session, "X");
        session.handle_event(key(KeyCode::Esc), false);
        assert_eq!(session.text(), expected);
    }
    let mut change = EditorSession::new("abc\ndef\n");
    change.set_cursor_char_offset(1).unwrap();
    change.handle_event(ctrl('v'), false);
    keys(&mut change, "jcZ");
    change.handle_event(key(KeyCode::Esc), false);
    assert_eq!(change.text(), "aZc\ndZf\n");
}

#[test]
fn project_text_operators_are_unicode_safe_transactions() {
    let mut session = EditorSession::new("  éAb\n\t日z\n");
    assert!(
        session
            .apply_operator(TextRange { start: 2, end: 5 }, TextOperator::Uppercase)
            .unwrap()
    );
    assert_eq!(session.text(), "  ÉAB\n\t日z\n");
    assert!(
        session
            .apply_operator(TextRange { start: 2, end: 5 }, TextOperator::Lowercase)
            .unwrap()
    );
    assert!(
        session
            .apply_operator(TextRange { start: 0, end: 0 }, TextOperator::Indent)
            .unwrap()
    );
    assert!(
        session
            .apply_operator(TextRange { start: 0, end: 0 }, TextOperator::Dedent)
            .unwrap()
    );
    assert!(
        session
            .apply_operator(TextRange { start: 2, end: 5 }, TextOperator::ToggleCase)
            .unwrap()
    );
    assert_eq!(session.text(), "  ÉAB\n\t日z\n");
}
