use super::{Document, DocumentPath, DocumentSnapshot};
pub use crate::references::session::LowerPurpose;
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExCommand {
    Write,
    Quit,
    ForceQuit,
    WriteAndQuit,
    Copy,
    ReadShell(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionEffect {
    Stay,
    Quit,
}

#[derive(Debug, Clone)]
pub struct LowerRequest {
    pub purpose: LowerPurpose,
    pub snapshot: DocumentSnapshot,
}

impl LowerRequest {
    /// The shell may quit only after a write-and-quit request succeeds.
    pub fn success_effect(&self) -> CompletionEffect {
        match self.purpose {
            LowerPurpose::WriteAndQuit => CompletionEffect::Quit,
            LowerPurpose::Write | LowerPurpose::Copy => CompletionEffect::Stay,
        }
    }

    pub fn failure_effect(&self) -> CompletionEffect {
        CompletionEffect::Stay
    }
}

#[derive(Debug, Clone)]
pub enum CommandEffect {
    Quit,
    Lower(LowerRequest),
    ReadShell(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    InvalidCopyCommand,
    Empty,
    Unknown(String),
    ArgumentsNotSupported(String),
    EmptyShellCommand,
    NoWriteSinceLastChange,
    NoFileName,
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCopyCommand => formatter.write_str(
                "copy command must be colon-prefixed and must not replace a built-in command",
            ),
            Self::Empty => formatter.write_str("empty command"),
            Self::Unknown(command) => write!(formatter, "unknown command: {command}"),
            Self::ArgumentsNotSupported(command) => {
                write!(formatter, "command does not accept arguments: {command}")
            }
            Self::EmptyShellCommand => formatter.write_str("read command requires text after `!`"),
            Self::NoWriteSinceLastChange => formatter.write_str("No write since last change"),
            Self::NoFileName => formatter.write_str("No file name"),
        }
    }
}

impl Error for CommandError {}

#[derive(Debug, Clone)]
pub struct CommandDispatcher {
    copy_command: String,
}

impl CommandDispatcher {
    pub fn new(copy_command: impl Into<String>) -> Result<Self, CommandError> {
        let copy_command = copy_command.into();
        if !copy_command.starts_with(':')
            || copy_command.len() == 1
            || copy_command.trim() != copy_command
            || matches!(copy_command.as_str(), ":w" | ":q" | ":q!" | ":wq")
            || copy_command.starts_with(":r !")
            || copy_command.starts_with(":read !")
        {
            return Err(CommandError::InvalidCopyCommand);
        }
        Ok(Self { copy_command })
    }

    /// Parses a complete command line. Only surrounding whitespace is ignored;
    /// command abbreviations, suffixes, and arguments are deliberately unsupported.
    pub fn parse(&self, line: &str) -> Result<ExCommand, CommandError> {
        let command = line.trim();
        if command.is_empty() {
            return Err(CommandError::Empty);
        }
        if command == self.copy_command {
            return Ok(ExCommand::Copy);
        }
        if let Some(shell) = [":r !", ":read !"]
            .into_iter()
            .find_map(|prefix| command.strip_prefix(prefix))
        {
            let shell = shell.trim();
            if shell.is_empty() {
                return Err(CommandError::EmptyShellCommand);
            }
            return Ok(ExCommand::ReadShell(shell.into()));
        }
        match command {
            ":w" => Ok(ExCommand::Write),
            ":q" => Ok(ExCommand::Quit),
            ":q!" => Ok(ExCommand::ForceQuit),
            ":wq" => Ok(ExCommand::WriteAndQuit),
            _ if has_arguments(command, &self.copy_command)
                || [":w", ":q", ":q!", ":wq"]
                    .iter()
                    .any(|known| has_arguments(command, known)) =>
            {
                Err(CommandError::ArgumentsNotSupported(command.into()))
            }
            _ => Err(CommandError::Unknown(command.into())),
        }
    }

    pub fn dispatch(&self, line: &str, document: &Document) -> Result<CommandEffect, CommandError> {
        match self.parse(line)? {
            ExCommand::Quit if document.is_dirty() => Err(CommandError::NoWriteSinceLastChange),
            ExCommand::Quit | ExCommand::ForceQuit => Ok(CommandEffect::Quit),
            ExCommand::Write => self.lower(document, LowerPurpose::Write),
            ExCommand::WriteAndQuit => self.lower(document, LowerPurpose::WriteAndQuit),
            ExCommand::Copy => Ok(CommandEffect::Lower(LowerRequest {
                purpose: LowerPurpose::Copy,
                snapshot: document.snapshot(),
            })),
            ExCommand::ReadShell(command) => Ok(CommandEffect::ReadShell(command)),
        }
    }

    fn lower(
        &self,
        document: &Document,
        purpose: LowerPurpose,
    ) -> Result<CommandEffect, CommandError> {
        if matches!(document.path(), DocumentPath::Unnamed) {
            return Err(CommandError::NoFileName);
        }
        Ok(CommandEffect::Lower(LowerRequest {
            purpose,
            snapshot: document.snapshot(),
        }))
    }
}

fn has_arguments(command: &str, known: &str) -> bool {
    command
        .strip_prefix(known)
        .is_some_and(|suffix| suffix.chars().next().is_some_and(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::TextEdit;
    use crate::references::model::TextRange;
    use std::fs;

    fn dispatcher() -> CommandDispatcher {
        CommandDispatcher::new(":copy").unwrap()
    }

    #[test]
    fn parses_only_exact_commands_with_surrounding_whitespace() {
        let parser = dispatcher();
        for (input, expected) in [
            (":w", ExCommand::Write),
            ("  :q\t", ExCommand::Quit),
            (":q!", ExCommand::ForceQuit),
            ("\n:wq ", ExCommand::WriteAndQuit),
            (":copy", ExCommand::Copy),
        ] {
            assert_eq!(parser.parse(input).unwrap(), expected);
        }
        assert_eq!(parser.parse("  ").unwrap_err(), CommandError::Empty);
        assert!(
            matches!(parser.parse(":x"), Err(CommandError::Unknown(command)) if command == ":x")
        );
        assert!(matches!(parser.parse("w"), Err(CommandError::Unknown(command)) if command == "w"));
        assert!(
            matches!(parser.parse(":write"), Err(CommandError::Unknown(command)) if command == ":write")
        );
    }

    #[test]
    fn read_shell_commands_require_explicit_bang_syntax() {
        let parser = dispatcher();
        assert_eq!(
            parser.parse(":r !printf 'hello'").unwrap(),
            ExCommand::ReadShell("printf 'hello'".into())
        );
        assert_eq!(
            parser.parse(":read !printf 'hello world'").unwrap(),
            ExCommand::ReadShell("printf 'hello world'".into())
        );
        assert_eq!(
            parser.parse(":r !   ").unwrap_err(),
            CommandError::EmptyShellCommand
        );
        assert!(matches!(
            parser.parse(":r printf x"),
            Err(CommandError::Unknown(_))
        ));
        let CommandEffect::ReadShell(command) = parser
            .dispatch(":r !printf ok", &Document::unnamed())
            .unwrap()
        else {
            panic!("read did not request shell execution")
        };
        assert_eq!(command, "printf ok");
    }

    #[test]
    fn arguments_are_rejected_instead_of_becoming_paths_or_force_variants() {
        let parser = dispatcher();
        for input in [
            ":w other.md",
            ":q now",
            ":q! later",
            ":wq file",
            ":copy all",
        ] {
            assert!(matches!(
                parser.parse(input),
                Err(CommandError::ArgumentsNotSupported(command)) if command == input
            ));
        }
        assert!(matches!(parser.parse(":w!"), Err(CommandError::Unknown(_))));
        assert!(matches!(
            parser.parse(":q!!"),
            Err(CommandError::Unknown(_))
        ));
    }

    #[test]
    fn configurable_copy_spelling_is_exact_and_cannot_shadow_builtins() {
        let parser = CommandDispatcher::new(":clip prompt").unwrap();
        assert_eq!(parser.parse(" :clip prompt ").unwrap(), ExCommand::Copy);
        assert!(matches!(
            parser.parse(":copy"),
            Err(CommandError::Unknown(_))
        ));
        assert!(matches!(
            parser.parse(":clip prompt extra"),
            Err(CommandError::ArgumentsNotSupported(_))
        ));
        for invalid in [
            "copy",
            ":",
            " :copy",
            ":copy ",
            ":w",
            ":q",
            ":q!",
            ":wq",
            ":r !echo shadow",
            ":read !echo shadow",
        ] {
            assert_eq!(
                CommandDispatcher::new(invalid).unwrap_err(),
                CommandError::InvalidCopyCommand
            );
        }
    }

    #[test]
    fn quit_obeys_dirty_rules_and_force_quit_never_requests_lowering() {
        let parser = dispatcher();
        let mut document = Document::unnamed();
        assert!(matches!(
            parser.dispatch(":q", &document),
            Ok(CommandEffect::Quit)
        ));
        document
            .apply(&[TextEdit::new(TextRange { start: 0, end: 0 }, "x")])
            .unwrap();
        assert_eq!(
            parser.dispatch(":q", &document).unwrap_err(),
            CommandError::NoWriteSinceLastChange
        );
        assert!(matches!(
            parser.dispatch(":q!", &document),
            Ok(CommandEffect::Quit)
        ));
    }

    #[test]
    fn write_requires_a_path_but_copy_supports_an_unnamed_buffer() {
        let parser = dispatcher();
        let document = Document::from_text("prompt");
        assert_eq!(
            parser.dispatch(":w", &document).unwrap_err(),
            CommandError::NoFileName
        );
        assert_eq!(
            parser.dispatch(":wq", &document).unwrap_err(),
            CommandError::NoFileName
        );
        let CommandEffect::Lower(request) = parser.dispatch(":copy", &document).unwrap() else {
            panic!("copy did not request lowering")
        };
        assert_eq!(request.purpose, LowerPurpose::Copy);
        assert_eq!(request.snapshot.text(), "prompt");
    }

    #[test]
    fn dispatch_is_read_only_and_write_and_copy_capture_the_same_revision() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prompt.md");
        fs::write(&path, "héllo").unwrap();
        let mut document = Document::open(&path).unwrap();
        document
            .apply(&[TextEdit::new(TextRange { start: 5, end: 5 }, "日")])
            .unwrap();
        let before = (
            document.text().to_owned(),
            document.revision(),
            document.is_dirty(),
        );

        let CommandEffect::Lower(write) = dispatcher().dispatch(":w", &document).unwrap() else {
            panic!("write did not request lowering")
        };
        let CommandEffect::Lower(copy) = dispatcher().dispatch(":copy", &document).unwrap() else {
            panic!("copy did not request lowering")
        };
        assert_eq!(write.purpose, LowerPurpose::Write);
        assert_eq!(copy.purpose, LowerPurpose::Copy);
        assert_eq!(write.snapshot.revision(), copy.snapshot.revision());
        assert_eq!(write.snapshot.text(), copy.snapshot.text());
        assert_eq!(
            write.snapshot.lower(|_| unreachable!()).unwrap(),
            copy.snapshot.lower(|_| unreachable!()).unwrap()
        );
        assert_eq!(
            (
                document.text().to_owned(),
                document.revision(),
                document.is_dirty()
            ),
            before
        );
    }

    #[test]
    fn write_and_quit_exits_only_after_success() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prompt.md");
        fs::write(&path, "prompt").unwrap();
        let document = Document::open(path).unwrap();
        let CommandEffect::Lower(request) = dispatcher().dispatch(":wq", &document).unwrap() else {
            panic!("wq did not request lowering")
        };
        assert_eq!(request.purpose, LowerPurpose::WriteAndQuit);
        assert_eq!(request.success_effect(), CompletionEffect::Quit);
        assert_eq!(request.failure_effect(), CompletionEffect::Stay);

        let CommandEffect::Lower(write) = dispatcher().dispatch(":w", &document).unwrap() else {
            unreachable!()
        };
        let CommandEffect::Lower(copy) = dispatcher().dispatch(":copy", &document).unwrap() else {
            unreachable!()
        };
        assert_eq!(write.success_effect(), CompletionEffect::Stay);
        assert_eq!(copy.success_effect(), CompletionEffect::Stay);
    }
}
