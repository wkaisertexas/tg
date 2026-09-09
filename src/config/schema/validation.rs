use super::{CONFIG_VERSION, ValidationError};
use std::fmt;
use std::path::{Component, Path};

pub(super) fn validate_version(version: u32) -> Result<(), ValidationError> {
    if version != CONFIG_VERSION {
        return Err(invalid(
            "version",
            format!("unsupported configuration version `{version}`; expected `{CONFIG_VERSION}`"),
        ));
    }
    Ok(())
}

pub(super) fn validate_command(path: &'static str, command: &Path) -> Result<(), ValidationError> {
    if command.as_os_str().is_empty() {
        return Err(invalid(path, "must not be empty"));
    }
    if command.to_string_lossy().chars().any(char::is_control) {
        return Err(invalid(
            path,
            "must not contain terminal control characters",
        ));
    }
    Ok(())
}

pub(super) fn validate_metadata_filename(path: String, value: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.chars().any(char::is_control)
        || Path::new(value).components().count() != 1
        || !matches!(
            Path::new(value).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(invalid(path, "must be a printable filename, not a path"));
    }
    Ok(())
}

pub(super) fn validate_mention(
    path: impl Into<String>,
    value: &str,
) -> Result<(), ValidationError> {
    let path = path.into();
    if value.chars().any(char::is_control) {
        return Err(invalid(path, "must be a single printable line"));
    }
    if !value.contains("${name}") {
        return Err(invalid(path, "must contain the `${name}` placeholder"));
    }
    let remainder = value.replace("${name}", "").replace("${leader}", "");
    if remainder.contains("${") {
        return Err(invalid(
            path,
            "supports only `${leader}` and `${name}` placeholders",
        ));
    }
    Ok(())
}

pub(super) fn validate_printable(
    path: impl Into<String>,
    value: &str,
) -> Result<(), ValidationError> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(invalid(path, "must be nonempty and printable"));
    }
    Ok(())
}

pub(super) fn is_key_notation(value: &str) -> bool {
    let Some((modifier, key)) = value.split_once('-') else {
        return false;
    };
    matches!(
        modifier.to_ascii_lowercase().as_str(),
        "ctrl" | "alt" | "shift" | "meta"
    ) && !key.is_empty()
        && !key.contains('-')
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

pub(super) fn validate_range<T: Copy + Ord + fmt::Display>(
    path: &'static str,
    value: T,
    minimum: T,
    maximum: T,
) -> Result<(), ValidationError> {
    if value < minimum || value > maximum {
        return Err(invalid(
            path,
            format!("must be between {minimum} and {maximum}, got {value}"),
        ));
    }
    Ok(())
}

pub(super) fn validate_minimum<T: Copy + Ord + fmt::Display>(
    path: &'static str,
    value: T,
    minimum: T,
) -> Result<(), ValidationError> {
    if value < minimum {
        return Err(invalid(
            path,
            format!("must be at least {minimum}, got {value}"),
        ));
    }
    Ok(())
}

pub(super) fn invalid(path: impl Into<String>, message: impl Into<String>) -> ValidationError {
    ValidationError {
        path: path.into(),
        message: message.into(),
    }
}
