use super::CancellationFlag;
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(crate) struct ProcessRequest {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub output_limit: usize,
    /// Additional process-local settings. The rest of the environment is inherited.
    pub env: Vec<(OsString, OsString)>,
}

#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub stdout: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProcessError {
    #[error("provider executable `{}` was not found; install it or configure its path", .0.display())]
    MissingExecutable(PathBuf),
    #[error("could not run `{}`: {source}", .executable.display())]
    Spawn {
        executable: PathBuf,
        source: io::Error,
    },
    #[error("could not wait for provider command: {0}")]
    Wait(#[source] io::Error),
    #[error("could not read provider output: {0}")]
    Read(#[source] io::Error),
    #[error("provider command timed out after {} ms", .0.as_millis())]
    TimedOut(Duration),
    #[error("provider command was cancelled")]
    Cancelled,
    #[error("provider output exceeded the {0}-byte safety limit")]
    OutputLimitExceeded(usize),
    #[error("provider command exited with {status}{separator}{stderr}", separator = if .stderr.is_empty() { "" } else { ": " })]
    Failed { status: ExitStatus, stderr: String },
}

pub(crate) fn run(
    request: ProcessRequest,
    cancellation: &CancellationFlag,
) -> Result<ProcessOutput, ProcessError> {
    run_bounded(request, cancellation, true)
}

pub(crate) fn run_per_stream(
    request: ProcessRequest,
    cancellation: &CancellationFlag,
) -> Result<ProcessOutput, ProcessError> {
    run_bounded(request, cancellation, false)
}

fn run_bounded(
    request: ProcessRequest,
    cancellation: &CancellationFlag,
    combined_limit: bool,
) -> Result<ProcessOutput, ProcessError> {
    let mut command = Command::new(&request.executable);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &request.env {
        command.env(key, value);
    }

    let mut child = command.spawn().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProcessError::MissingExecutable(request.executable.clone())
        } else {
            ProcessError::Spawn {
                executable: request.executable.clone(),
                source,
            }
        }
    })?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let bytes_seen = Arc::new(AtomicUsize::new(0));
    let stdout_reader = spawn_reader(
        child.stdout.take().expect("piped stdout is available"),
        request.output_limit,
        Arc::clone(&exceeded),
        Arc::clone(&bytes_seen),
    );
    let stderr_reader = spawn_reader(
        child.stderr.take().expect("piped stderr is available"),
        request.output_limit,
        Arc::clone(&exceeded),
        if combined_limit {
            bytes_seen
        } else {
            Arc::default()
        },
    );

    let started = Instant::now();
    let outcome = loop {
        if cancellation.is_cancelled() {
            terminate(&mut child);
            break Err(ProcessError::Cancelled);
        }
        if exceeded.load(Ordering::Acquire) {
            terminate(&mut child);
            break Err(ProcessError::OutputLimitExceeded(request.output_limit));
        }
        if started.elapsed() >= request.timeout {
            terminate(&mut child);
            break Err(ProcessError::TimedOut(request.timeout));
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(error) => {
                terminate(&mut child);
                break Err(ProcessError::Wait(error));
            }
        }
    };

    let status = outcome?;
    let stdout = receive_output(stdout_reader, cancellation, started, request.timeout)?;
    let stderr = receive_output(stderr_reader, cancellation, started, request.timeout)?;
    if exceeded.load(Ordering::Acquire) {
        return Err(ProcessError::OutputLimitExceeded(request.output_limit));
    }
    if !status.success() {
        return Err(ProcessError::Failed {
            status,
            stderr: redact_stderr(&String::from_utf8_lossy(&stderr)),
        });
    }
    Ok(ProcessOutput { stdout })
}

fn spawn_reader(
    mut reader: impl Read + Send + 'static,
    limit: usize,
    exceeded: Arc<AtomicBool>,
    bytes_seen: Arc<AtomicUsize>,
) -> Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let mut kept = Vec::with_capacity(limit.min(8 * 1024));
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    return Ok(kept);
                }
                let previous = bytes_seen.fetch_add(count, Ordering::AcqRel);
                let keep = count.min(limit.saturating_sub(previous));
                kept.extend_from_slice(&buffer[..keep]);
                if previous.saturating_add(count) > limit {
                    exceeded.store(true, Ordering::Release);
                    return Ok(kept);
                }
            }
        })();
        let _ = sender.send(result);
    });
    receiver
}

fn receive_output(
    receiver: Receiver<io::Result<Vec<u8>>>,
    cancellation: &CancellationFlag,
    started: Instant,
    timeout: Duration,
) -> Result<Vec<u8>, ProcessError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(ProcessError::Cancelled);
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(ProcessError::TimedOut(timeout));
        }
        match receiver.recv_timeout(remaining.min(POLL_INTERVAL)) {
            Ok(result) => return result.map_err(ProcessError::Read),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(ProcessError::Read(io::Error::other(
                    "output reader stopped",
                )));
            }
        }
    }
}

fn terminate(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn redact_stderr(stderr: &str) -> String {
    let mut redacted = stderr.trim().to_owned();
    for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"] {
        while let Some(start) = redacted.to_ascii_lowercase().find(prefix) {
            let end = redacted[start..]
                .find(char::is_whitespace)
                .map_or(redacted.len(), |offset| start + offset);
            redacted.replace_range(start..end, "[REDACTED]");
        }
    }
    for marker in ["Authorization:", "authorization:"] {
        let mut search_from = 0;
        while let Some(relative_start) = redacted[search_from..].find(marker) {
            let start = search_from + relative_start;
            let value_start = start + marker.len();
            let end = redacted[value_start..]
                .find(['\r', '\n'])
                .map_or(redacted.len(), |offset| value_start + offset);
            redacted.replace_range(value_start..end, "[REDACTED]");
            search_from = value_start + "[REDACTED]".len();
        }
    }
    for marker in ["Bearer ", "bearer "] {
        let mut search_from = 0;
        while let Some(relative_start) = redacted[search_from..].find(marker) {
            let value_start = search_from + relative_start + marker.len();
            let end = redacted[value_start..]
                .find(char::is_whitespace)
                .map_or(redacted.len(), |offset| value_start + offset);
            redacted.replace_range(value_start..end, "[REDACTED]");
            search_from = value_start + "[REDACTED]".len();
        }
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::redact_stderr;

    #[cfg(unix)]
    #[test]
    fn inherited_output_pipes_cannot_extend_the_deadline() {
        use super::{ProcessError, ProcessRequest, run};
        use crate::references::CancellationFlag;
        use std::time::{Duration, Instant};

        for script in ["/bin/sleep 1 & wait", "/bin/sleep 1 & exit 0"] {
            let started = Instant::now();
            let result = run(
                ProcessRequest {
                    executable: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into()],
                    cwd: std::env::current_dir().unwrap(),
                    timeout: Duration::from_millis(50),
                    output_limit: 1024,
                    env: Vec::new(),
                },
                &CancellationFlag::default(),
            );
            assert!(
                matches!(result, Err(ProcessError::TimedOut(_))),
                "{result:?}"
            );
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "a descendant kept the output pipe open"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn separate_stream_limits_preserve_jira_budget_without_weakening_other_callers() {
        use super::{ProcessError, ProcessRequest, run, run_per_stream};
        use crate::references::CancellationFlag;
        use std::time::Duration;

        let request = || ProcessRequest {
            executable: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 12345678; printf 12345678 >&2".into()],
            cwd: std::env::current_dir().unwrap(),
            timeout: Duration::from_secs(2),
            output_limit: 8,
            env: Vec::new(),
        };
        let cancellation = CancellationFlag::default();
        assert!(matches!(
            run(request(), &cancellation),
            Err(ProcessError::OutputLimitExceeded(8))
        ));
        assert_eq!(
            run_per_stream(request(), &cancellation).unwrap().stdout,
            b"12345678"
        );
        let mut oversized = request();
        oversized.output_limit = 7;
        assert!(matches!(
            run_per_stream(oversized, &cancellation),
            Err(ProcessError::OutputLimitExceeded(7))
        ));
    }

    #[test]
    fn redacts_token_shaped_and_authorization_values() {
        let input = "Authorization:Bearer secret\nghp_abcdefghijklmnopqrstuvwxyz123456";
        let output = redact_stderr(input);
        assert!(!output.contains("secret"));
        assert!(!output.contains("ghp_"));
        assert!(output.contains("[REDACTED]"));
    }
}
