use super::*;
use crate::editor::TextEdit;
use crate::references::model::TextRange;
use crate::references::process::{ProcessRequest, run as run_process};
use crate::references::{BackgroundExecutor, CancellationFlag};
use std::sync::mpsc::TryRecvError;

const SHELL_TIMEOUT: Duration = Duration::from_secs(30);
const SHELL_OUTPUT_LIMIT: usize = 1024 * 1024;

pub(super) struct PendingShell {
    id: u64,
    cancellation: CancellationFlag,
}

pub(super) struct ShellResult {
    id: u64,
    revision: DocumentRevision,
    offset: usize,
    prefix_newline: bool,
    output: Result<Vec<u8>, String>,
}

impl App {
    pub(super) fn begin_shell_read(&mut self, command: String) {
        if let Some(pending) = self.pending_shell.take() {
            pending.cancellation.cancel();
        }
        self.next_shell_id = self.next_shell_id.wrapping_add(1);
        let id = self.next_shell_id;
        let revision = self.revision();
        let (offset, prefix_newline) =
            shell_insertion_point(self.document.text(), self.editor.cursor_char_offset());
        let cancellation = CancellationFlag::default();
        let worker_cancellation = cancellation.clone();
        let sender = self.shell_sender.clone();
        let cwd = self.repo.invocation_root.clone();
        ThreadExecutor.spawn(Box::new(move || {
            let output = run_process(
                ProcessRequest {
                    executable: "bash".into(),
                    args: vec!["-c".into(), command.into()],
                    cwd,
                    timeout: SHELL_TIMEOUT,
                    output_limit: SHELL_OUTPUT_LIMIT,
                    env: Vec::new(),
                },
                &worker_cancellation,
            )
            .map(|output| output.stdout)
            .map_err(|error| error.to_string());
            let _ = sender.send(ShellResult {
                id,
                revision,
                offset,
                prefix_newline,
                output,
            });
        }));
        self.pending_shell = Some(PendingShell { id, cancellation });
        self.status = "Running shell command…".into();
    }

    pub(super) fn drain_shell_results(&mut self) {
        loop {
            let result = match self.shell_receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            };
            if !self
                .pending_shell
                .as_ref()
                .is_some_and(|pending| pending.id == result.id)
            {
                continue;
            }
            self.pending_shell = None;
            if result.revision != self.revision() {
                self.status = "Command output discarded because the document changed".into();
                continue;
            }
            let bytes = match result.output {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.status = format!("Command failed: {error}");
                    continue;
                }
            };
            if bytes.is_empty() {
                self.status = "Command produced no output".into();
                continue;
            }
            let output = match String::from_utf8(bytes) {
                Ok(output) => output,
                Err(_) => {
                    self.status = "Command failed: output is not UTF-8".into();
                    continue;
                }
            };
            let mut insertion = String::new();
            if result.prefix_newline {
                insertion.push('\n');
            }
            insertion.push_str(&output);
            if !insertion.ends_with('\n') {
                insertion.push('\n');
            }
            let inserted_bytes = insertion.len();
            let cursor = result.offset + usize::from(result.prefix_newline);
            let edit = TextEdit::new(
                TextRange {
                    start: result.offset,
                    end: result.offset,
                },
                insertion,
            );
            let update = self
                .document
                .apply(&[edit])
                .and_then(|_| self.replace_widget_from_document())
                .and_then(|_| self.editor.set_cursor_char_offset(cursor))
                .and_then(|_| self.sync_reference_state());
            match update {
                Ok(()) => self.status = format!("Read {inserted_bytes} bytes from shell"),
                Err(error) => {
                    self.status = format!("Command output could not be inserted: {error}")
                }
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(pending) = self.pending_shell.take() {
            pending.cancellation.cancel();
        }
    }
}

pub(super) fn shell_insertion_point(text: &str, cursor: usize) -> (usize, bool) {
    let characters: Vec<_> = text.chars().collect();
    let cursor = cursor.min(characters.len());
    if let Some(line_end) = characters[cursor..]
        .iter()
        .position(|character| *character == '\n')
    {
        (cursor + line_end + 1, false)
    } else {
        (characters.len(), !characters.is_empty())
    }
}
