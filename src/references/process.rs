use super::CancellationFlag;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

#[derive(Debug)]
pub(crate) enum ProcessError {
    MissingExecutable(PathBuf),
    Spawn {
        executable: PathBuf,
        source: io::Error,
    },
    Wait(io::Error),
    Read(io::Error),
    TimedOut(Duration),
    Cancelled,
    OutputLimitExceeded(usize),
    Failed {
        status: ExitStatus,
        stderr: String,
    },
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingExecutable(path) => write!(
                formatter,
                "provider executable `{}` was not found; install it or configure its path",
                path.display()
            ),
            Self::Spawn { executable, source } => {
                write!(
                    formatter,
                    "could not run `{}`: {source}",
                    executable.display()
                )
            }
            Self::Wait(source) => {
                write!(formatter, "could not wait for provider command: {source}")
            }
            Self::Read(source) => write!(formatter, "could not read provider output: {source}"),
            Self::TimedOut(timeout) => {
                write!(
                    formatter,
                    "provider command timed out after {} ms",
                    timeout.as_millis()
                )
            }
            Self::Cancelled => formatter.write_str("provider command was cancelled"),
            Self::OutputLimitExceeded(limit) => write!(
                formatter,
                "provider output exceeded the {limit}-byte safety limit"
            ),
            Self::Failed { status, stderr } if stderr.is_empty() => {
                write!(formatter, "provider command exited with {status}")
            }
            Self::Failed { status, stderr } => {
                write!(formatter, "provider command exited with {status}: {stderr}")
            }
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::Wait(source) | Self::Read(source) => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn run(
    request: ProcessRequest,
    cancellation: &CancellationFlag,
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
        Arc::clone(&bytes_seen),
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

    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    let status = outcome?;
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
) -> std::thread::JoinHandle<io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
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
            }
        }
    })
}

fn join_reader(
    reader: std::thread::JoinHandle<io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, ProcessError> {
    reader
        .join()
        .map_err(|_| ProcessError::Read(io::Error::other("output reader panicked")))?
        .map_err(ProcessError::Read)
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

    #[test]
    fn redacts_token_shaped_and_authorization_values() {
        let input = "Authorization:Bearer secret\nghp_abcdefghijklmnopqrstuvwxyz123456";
        let output = redact_stderr(input);
        assert!(!output.contains("secret"));
        assert!(!output.contains("ghp_"));
        assert!(output.contains("[REDACTED]"));
    }
}
