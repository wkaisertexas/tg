use super::*;
use crate::references::model::GenerationId;
use std::fs;

mod index;
mod targets;

fn request(generation: u64, query: &str, scope: QueryScope) -> QueryRequest {
    QueryRequest {
        generation: GenerationId(generation),
        query: query.into(),
        scope,
        limit: 100,
        typed_leader: "@".into(),
    }
}

fn query_file(provider: &SymbolProvider, path: &Path, query: &str) -> Vec<ReferenceCandidate> {
    provider
        .query(
            request(
                1,
                query,
                QueryScope::File {
                    path: path.to_path_buf(),
                    origin: FileOrigin::GitAware,
                },
            ),
            &CancellationFlag::default(),
        )
        .unwrap()
}

fn resolved(provider: &SymbolProvider, candidate: &ReferenceCandidate) -> SymbolTarget {
    let ReferenceTarget::Symbol(target) = provider.resolve(&candidate.id).unwrap() else {
        panic!("expected symbol")
    };
    target
}
