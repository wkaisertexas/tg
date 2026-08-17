#[cfg(unix)]
#[test]
fn terminal_binary_wq_writes_for_external_editor_parent_and_restores_the_screen() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};

    let temp = tempfile::tempdir().unwrap();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_tg"));
    let edited = temp.path().join("prompt.md");
    command.arg(&edited);
    command.cwd(temp.path());
    command.env("TERM", "xterm-256color");
    command.env("HOME", temp.path());
    for name in [
        "XDG_CONFIG_HOME",
        "TG_CONFIG",
        "TG_ROOT",
        "TG_TOKENIZER",
        "TG_GH_COMMAND",
        "TG_JIRA_COMMAND",
        "TG_NO_PROJECT_CONFIG",
        "NO_COLOR",
    ] {
        command.env_remove(name);
    }
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        output
    });
    let mut writer = pair.master.take_writer().unwrap();
    writer.write_all(b"iphase seven prompt").unwrap();
    writer.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    writer.write_all(b"\x1b").unwrap();
    writer.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    writer.write_all(b":wq\r").unwrap();
    writer.flush().unwrap();

    let status = child.wait().unwrap();
    drop(writer);
    drop(pair.master);
    let output = reader_thread.join().unwrap();
    let output = String::from_utf8_lossy(&output);

    assert!(status.success(), "terminal child failed: {output}");
    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        "phase seven prompt"
    );
    assert!(
        output.contains("\u{1b}[?1049h"),
        "alternate screen was not entered"
    );
    assert!(
        output.contains("\u{1b}[?1049l"),
        "alternate screen was not restored"
    );
}
