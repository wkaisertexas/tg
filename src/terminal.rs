//! Transactional terminal setup and best-effort restoration.
//!
//! The guard records each successfully acquired terminal feature separately,
//! so a partially failed setup restores only the state it actually changed.

use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::fmt;
use std::io::{self, Stderr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TerminalOperation {
    EnableRawMode,
    EnterAlternateScreen,
    EnableBracketedPaste,
    HideCursor,
    ShowCursor,
    DisableBracketedPaste,
    LeaveAlternateScreen,
    DisableRawMode,
}

impl fmt::Display for TerminalOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EnableRawMode => "enable raw mode",
            Self::EnterAlternateScreen => "enter alternate screen",
            Self::EnableBracketedPaste => "enable bracketed paste",
            Self::HideCursor => "hide cursor",
            Self::ShowCursor => "show cursor",
            Self::DisableBracketedPaste => "disable bracketed paste",
            Self::LeaveAlternateScreen => "leave alternate screen",
            Self::DisableRawMode => "disable raw mode",
        })
    }
}

pub trait TerminalBackend {
    fn perform(&mut self, operation: TerminalOperation) -> io::Result<()>;
}

/// Production terminal operations directed to stderr.
pub struct StderrTerminal {
    stderr: Stderr,
}

impl Default for StderrTerminal {
    fn default() -> Self {
        Self {
            stderr: io::stderr(),
        }
    }
}

impl StderrTerminal {
    pub fn writer_mut(&mut self) -> &mut Stderr {
        &mut self.stderr
    }
}

impl TerminalBackend for StderrTerminal {
    fn perform(&mut self, operation: TerminalOperation) -> io::Result<()> {
        match operation {
            TerminalOperation::EnableRawMode => enable_raw_mode(),
            TerminalOperation::EnterAlternateScreen => {
                execute!(self.stderr, EnterAlternateScreen).map(|_| ())
            }
            TerminalOperation::EnableBracketedPaste => {
                execute!(self.stderr, EnableBracketedPaste).map(|_| ())
            }
            TerminalOperation::HideCursor => execute!(self.stderr, Hide).map(|_| ()),
            TerminalOperation::ShowCursor => execute!(self.stderr, Show).map(|_| ()),
            TerminalOperation::DisableBracketedPaste => {
                execute!(self.stderr, DisableBracketedPaste).map(|_| ())
            }
            TerminalOperation::LeaveAlternateScreen => {
                execute!(self.stderr, LeaveAlternateScreen).map(|_| ())
            }
            TerminalOperation::DisableRawMode => disable_raw_mode(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("could not {operation}: {source}")]
pub struct TerminalError {
    pub operation: TerminalOperation,
    source: io::Error,
}

impl TerminalError {
    fn new(operation: TerminalOperation, source: io::Error) -> Self {
        Self { operation, source }
    }
}

/// Owns all terminal modes acquired for one interactive session.
pub struct TerminalGuard<B: TerminalBackend> {
    backend: B,
    raw_mode: bool,
    alternate_screen: bool,
    bracketed_paste: bool,
    cursor_hidden: bool,
}

impl TerminalGuard<StderrTerminal> {
    pub fn stderr() -> Result<Self, TerminalError> {
        Self::acquire(StderrTerminal::default())
    }
}

impl<B: TerminalBackend> TerminalGuard<B> {
    pub fn acquire(backend: B) -> Result<Self, TerminalError> {
        let mut guard = Self {
            backend,
            raw_mode: false,
            alternate_screen: false,
            bracketed_paste: false,
            cursor_hidden: false,
        };

        guard.acquire_step(TerminalOperation::EnableRawMode)?;
        guard.raw_mode = true;
        guard.acquire_step(TerminalOperation::EnterAlternateScreen)?;
        guard.alternate_screen = true;
        guard.acquire_step(TerminalOperation::EnableBracketedPaste)?;
        guard.bracketed_paste = true;
        guard.acquire_step(TerminalOperation::HideCursor)?;
        guard.cursor_hidden = true;
        Ok(guard)
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Restores every acquired state in reverse order.
    ///
    /// All restoration steps are attempted even when one fails. A failed flag
    /// remains armed so `Drop` can make one final best-effort retry.
    pub fn restore(&mut self) -> Result<(), TerminalError> {
        let mut first_error = None;
        self.restore_step(
            self.cursor_hidden,
            TerminalOperation::ShowCursor,
            |guard| guard.cursor_hidden = false,
            &mut first_error,
        );
        self.restore_step(
            self.bracketed_paste,
            TerminalOperation::DisableBracketedPaste,
            |guard| guard.bracketed_paste = false,
            &mut first_error,
        );
        self.restore_step(
            self.alternate_screen,
            TerminalOperation::LeaveAlternateScreen,
            |guard| guard.alternate_screen = false,
            &mut first_error,
        );
        self.restore_step(
            self.raw_mode,
            TerminalOperation::DisableRawMode,
            |guard| guard.raw_mode = false,
            &mut first_error,
        );
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn acquire_step(&mut self, operation: TerminalOperation) -> Result<(), TerminalError> {
        self.backend
            .perform(operation)
            .map_err(|error| TerminalError::new(operation, error))
    }

    fn restore_step(
        &mut self,
        acquired: bool,
        operation: TerminalOperation,
        clear: impl FnOnce(&mut Self),
        first_error: &mut Option<TerminalError>,
    ) {
        if !acquired {
            return;
        }
        match self.backend.perform(operation) {
            Ok(()) => clear(self),
            Err(error) if first_error.is_none() => {
                *first_error = Some(TerminalError::new(operation, error));
            }
            Err(_) => {}
        }
    }
}

impl<B: TerminalBackend> Drop for TerminalGuard<B> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct MockBackend {
        calls: Arc<Mutex<Vec<TerminalOperation>>>,
        fail_once: Arc<Mutex<BTreeSet<TerminalOperation>>>,
    }

    impl MockBackend {
        fn new(fail_once: impl IntoIterator<Item = TerminalOperation>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail_once: Arc::new(Mutex::new(fail_once.into_iter().collect())),
            }
        }

        fn calls(&self) -> Vec<TerminalOperation> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl TerminalBackend for MockBackend {
        fn perform(&mut self, operation: TerminalOperation) -> io::Result<()> {
            self.calls.lock().unwrap().push(operation);
            if self.fail_once.lock().unwrap().remove(&operation) {
                Err(io::Error::other("injected terminal failure"))
            } else {
                Ok(())
            }
        }
    }

    const ACQUIRE: [TerminalOperation; 4] = [
        TerminalOperation::EnableRawMode,
        TerminalOperation::EnterAlternateScreen,
        TerminalOperation::EnableBracketedPaste,
        TerminalOperation::HideCursor,
    ];
    const RESTORE: [TerminalOperation; 4] = [
        TerminalOperation::ShowCursor,
        TerminalOperation::DisableBracketedPaste,
        TerminalOperation::LeaveAlternateScreen,
        TerminalOperation::DisableRawMode,
    ];

    #[test]
    fn normal_drop_and_explicit_restore_use_reverse_order_once() {
        let backend = MockBackend::new([]);
        let observer = backend.clone();
        {
            let _guard = TerminalGuard::acquire(backend).unwrap();
        }
        assert_eq!(
            observer.calls(),
            [ACQUIRE.as_slice(), RESTORE.as_slice()].concat()
        );

        let backend = MockBackend::new([]);
        let observer = backend.clone();
        {
            let mut guard = TerminalGuard::acquire(backend).unwrap();
            guard.restore().unwrap();
        }
        assert_eq!(
            observer.calls(),
            [ACQUIRE.as_slice(), RESTORE.as_slice()].concat()
        );
    }

    #[test]
    fn early_error_restores_every_acquired_state() {
        fn fail_after_acquisition(backend: MockBackend) -> Result<(), &'static str> {
            let _guard = TerminalGuard::acquire(backend).unwrap();
            Err("application error")
        }

        let backend = MockBackend::new([]);
        let observer = backend.clone();
        assert_eq!(fail_after_acquisition(backend), Err("application error"));
        assert_eq!(
            observer.calls(),
            [ACQUIRE.as_slice(), RESTORE.as_slice()].concat()
        );
    }

    #[test]
    fn every_acquisition_failure_restores_only_completed_steps() {
        let expected = [
            vec![TerminalOperation::EnableRawMode],
            vec![
                TerminalOperation::EnableRawMode,
                TerminalOperation::EnterAlternateScreen,
                TerminalOperation::DisableRawMode,
            ],
            vec![
                TerminalOperation::EnableRawMode,
                TerminalOperation::EnterAlternateScreen,
                TerminalOperation::EnableBracketedPaste,
                TerminalOperation::LeaveAlternateScreen,
                TerminalOperation::DisableRawMode,
            ],
            vec![
                TerminalOperation::EnableRawMode,
                TerminalOperation::EnterAlternateScreen,
                TerminalOperation::EnableBracketedPaste,
                TerminalOperation::HideCursor,
                TerminalOperation::DisableBracketedPaste,
                TerminalOperation::LeaveAlternateScreen,
                TerminalOperation::DisableRawMode,
            ],
        ];
        for (failed, expected) in ACQUIRE.into_iter().zip(expected) {
            let backend = MockBackend::new([failed]);
            let observer = backend.clone();
            let error = match TerminalGuard::acquire(backend) {
                Ok(_) => panic!("{failed:?} unexpectedly succeeded"),
                Err(error) => error,
            };
            assert_eq!(error.operation, failed);
            assert_eq!(observer.calls(), expected, "{failed:?}");
        }
    }

    #[test]
    fn unwind_drops_the_guard_and_restores_terminal_state() {
        let backend = MockBackend::new([]);
        let observer = backend.clone();
        let result = catch_unwind(AssertUnwindSafe(move || {
            let _guard = TerminalGuard::acquire(backend).unwrap();
            panic!("injected panic");
        }));
        assert!(result.is_err());
        assert_eq!(
            observer.calls(),
            [ACQUIRE.as_slice(), RESTORE.as_slice()].concat()
        );
    }

    #[test]
    fn restoration_attempts_every_step_and_drop_retries_a_failed_one() {
        let backend = MockBackend::new([TerminalOperation::ShowCursor]);
        let observer = backend.clone();
        {
            let mut guard = TerminalGuard::acquire(backend).unwrap();
            let error = guard.restore().unwrap_err();
            assert_eq!(error.operation, TerminalOperation::ShowCursor);
        }
        let mut expected = [ACQUIRE.as_slice(), RESTORE.as_slice()].concat();
        expected.push(TerminalOperation::ShowCursor);
        assert_eq!(observer.calls(), expected);
    }
}
