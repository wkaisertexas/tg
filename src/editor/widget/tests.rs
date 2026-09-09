use super::*;

mod display;
mod editing;
mod history;

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn ctrl(character: char) -> Event {
    Event::Key(KeyEvent::new(
        KeyCode::Char(character),
        KeyModifiers::CONTROL,
    ))
}
fn shift(character: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::SHIFT))
}

fn keys(session: &mut EditorSession, text: &str) {
    for character in text.chars() {
        session.handle_event(key(KeyCode::Char(character)), false);
    }
}

fn type_command(session: &mut EditorSession, command: &str) -> EditorInput {
    assert_eq!(
        session.handle_event(key(KeyCode::Char(':')), false),
        EditorInput::CommandStarted
    );
    for character in command.chars() {
        assert_eq!(
            session.handle_event(key(KeyCode::Char(character)), false),
            EditorInput::CommandUpdated
        );
    }
    session.handle_event(key(KeyCode::Enter), false)
}
