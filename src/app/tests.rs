use super::help::styled_help_lines;
use super::input::single_edit;
use super::shell::shell_insertion_point;
use super::view::*;
use super::*;
use crate::editor::{AdapterMode, TextEdit};
use crate::references::activation::detect_activation;
use crate::references::model::{ContextCost, QueryScope, ReferenceCandidate, TextRange};
use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use std::fs;
use std::path::Path;
use std::thread;
use std::time::Instant;

mod editing;
mod providers;
mod view;

fn app_for(root: &Path, config: &Config, text: &str) -> App {
    App::new(
        Repository::discover(root).unwrap(),
        config,
        Document::from_text(text),
        None,
    )
    .unwrap()
}

fn activate_text(app: &mut App, text: &str) {
    let activation = detect_activation(
        text,
        text.chars().count(),
        &app.leaders,
        &app.repo.search_root,
    )
    .unwrap()
    .expect("test text must activate a leader");
    app.reference_session
        .activate(activation, app.search_limit)
        .unwrap();
}

fn wait_for(app: &mut App, ready: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        app.drain_reference_events();
        app.drain_shell_results();
        if ready(app) {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for app state; status={}", app.status);
}

fn accept_and_lower(app: &mut App) -> String {
    wait_for(app, |app| !app.reference_session.candidates().is_empty());
    app.accept_selected();
    wait_for(app, |app| !app.document.references().is_empty());
    app.dispatch_command(":copy".into());
    wait_for(app, |app| app.pending_clipboard.is_some());
    app.pending_clipboard.take().unwrap()
}

#[cfg(unix)]
fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}
