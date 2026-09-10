use super::TokenState;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub use crate::references::model::FileVersion as ContextFileVersion;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextIdentity {
    File(PathBuf),
    Range {
        canonical_path: PathBuf,
        start_byte: usize,
        end_byte: usize,
        file_version: ContextFileVersion,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceContext {
    pub identity: Option<ContextIdentity>,
    pub state: TokenState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextTotal {
    pub ready_tokens: usize,
    pub pending: usize,
    pub unavailable: usize,
}

pub fn context_total<'a>(contexts: impl IntoIterator<Item = &'a ReferenceContext>) -> ContextTotal {
    let contexts: Vec<_> = contexts.into_iter().collect();
    let files: HashSet<_> = contexts
        .iter()
        .filter_map(|context| match &context.identity {
            Some(ContextIdentity::File(path)) => Some(path),
            _ => None,
        })
        .collect();
    let mut unique: HashMap<&ContextIdentity, TokenState> = HashMap::new();
    for context in &contexts {
        let Some(identity) = &context.identity else {
            continue;
        };
        if matches!(identity, ContextIdentity::Range { canonical_path, .. } if files.contains(canonical_path))
        {
            continue;
        }
        unique
            .entry(identity)
            .and_modify(|state| *state = preferred_state(state, &context.state))
            .or_insert_with(|| context.state.clone());
    }
    unique
        .values()
        .fold(ContextTotal::default(), |mut total, state| {
            match state {
                TokenState::Ready(tokens) => total.ready_tokens += tokens,
                TokenState::Pending => total.pending += 1,
                TokenState::Bytes(_) | TokenState::Unavailable => total.unavailable += 1,
            }
            total
        })
}

fn preferred_state(left: &TokenState, right: &TokenState) -> TokenState {
    match (left, right) {
        (TokenState::Ready(left), TokenState::Ready(right)) => {
            TokenState::Ready((*left).max(*right))
        }
        (TokenState::Ready(_), _) => left.clone(),
        (_, TokenState::Ready(_)) => right.clone(),
        (TokenState::Pending, _) => left.clone(),
        (_, TokenState::Pending) => right.clone(),
        (TokenState::Bytes(left), TokenState::Bytes(right)) => {
            TokenState::Bytes((*left).max(*right))
        }
        (TokenState::Bytes(_), TokenState::Unavailable) => left.clone(),
        _ => right.clone(),
    }
}

pub fn format_tokens(tokens: usize) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    let tenths = tokens.saturating_add(50) / 100;
    format!("{}.{:01}k", tenths / 10, tenths % 10)
}
