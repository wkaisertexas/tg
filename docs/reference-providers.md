# Reference and Provider Design

## 1. Purpose

This document defines the structured reference engine shared by file, symbol,
skill, GitHub, and Jira completion. It also defines provider execution,
lowering, validation, previewing, and token accounting.

## 2. Common Model

Every provider exposes the same conceptual operations:

```rust
trait ReferenceProvider {
    fn kind(&self) -> ReferenceKind;
    fn leader(&self) -> &str;
    fn begin_query(&self, request: QueryRequest, sink: ResultSink);
    fn resolve(&self, candidate: CandidateId) -> Result<ReferenceTarget>;
    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget>;
    fn lower(&self, target: &ValidatedTarget) -> Result<String>;
    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>>;
    fn context_cost(&self, target: &ReferenceTarget) -> Result<ContextCost>;
}
```

This is an internal Rust trait, not a dynamic plugin ABI. Only built-in
providers ship in version one. Agent-specific skill discovery is configured by
data in TOML rather than loaded executable code.

Queries and result batches carry monotonically increasing generation IDs.
The editor discards results from a generation older than the current query.
Slow filesystem walks, Tree-sitter parsing, tokenization, and subprocesses
never run on the render thread.

## 3. Leaders and Token Boundaries

Default leaders are:

```text
@   Git-aware file
%   broad file
::  repository symbol
$   skill
#   GitHub issue
!   GitHub pull request
&   Jira issue
```

A leader activates only at the start of the document or after whitespace or a
configured opening delimiter. This avoids interpreting email addresses,
Markdown fragments, shell expansions, and ordinary punctuation as references.

Multi-character leaders are matched before single-character leaders. The
configuration validator rejects duplicate leaders and rejects cases where one
configured leader is an ambiguous prefix of another unless the provider parser
has an explicit disambiguation rule.

Escaping a leader with `\` leaves it as ordinary text and removes the escape
character when the lowered snapshot is rendered.

## 4. File Providers

### Git-aware files: `@`

The existing `ignore`-based repository walk is preserved. It includes tracked
files and non-ignored untracked files, respects Git and conventional ignore
files, includes hidden files when not ignored, and never enters `.git`.

### Broad files: `%`

The broad walk disables ignore processing, excludes `.git`, and preserves the
existing symlink containment and cycle protections. It may enumerate generated,
cached, ignored, or sensitive local files, so the completion header clearly
labels it `BROAD`.

### Candidate presentation

Each file candidate contains:

- search-root-relative path;
- fuzzy-match indices;
- whole-file GPT-4o token count;
- ordinary or broad origin; and
- a file-version key for caching.

Example:

```text
src/language/mod.rs                               18.7k
```

Binary and invalid UTF-8 files remain selectable as paths but display a byte
size instead of a token count and do not offer symbol completion.

### Lowering

The accepted friendly form retains its leader. Lowering removes the leader and
emits a `/`-separated path relative to the search root.

## 5. Tree-sitter Symbol Provider

The current language registry, extraction rules, leaf-name behavior, qualified
ranking, Markdown heading support, and repository-wide lazy index are the
required semantic-search behavior. The core release tier adds Bash/shell, C#,
Go, Java, JavaScript/JSX, Ruby, and TypeScript/TSX to the existing C, C++, Rust,
Python, and Markdown adapters. The complete core and extended target lists are
normative in [`spec.md`](spec.md#81-language-coverage).

Every supported language adapter defines:

- recognized extensions and conventional filenames;
- grammar version and parser selection;
- declaration or structural-node categories;
- name, parent, range, and qualification extraction;
- local or noisy nodes that must be excluded; and
- malformed-source and representative-language fixtures.

Adapters may only be built on maintained upstream `tree-sitter-<language>`
packages available to Rust. The provider layer does not own grammar development
or carry project-maintained grammar forks. A file remains available for
whole-file search and token counting when no eligible grammar package exists.

Extended data, build, and component formats use the same common symbol model
for named structural units. They must not manufacture callable/type semantics
that the source format does not possess.

Typing `::` after an accepted file searches that file. Typing standalone `::`
starts the repository-wide index. Typing `.` after a resolved parent continues
to prefer direct children.

Candidates show:

- leaf and best-effort qualified name;
- symbol kind;
- one-based line and column;
- token count of `start_byte..end_byte`; and
- token count of the whole file.

Example:

```text
App::submit  function · 347:5       symbol 1.2k · file 18.7k
```

Code symbols lower to:

```text
path::line:column SymbolName
```

Markdown headings retain the existing compact `path.md#anchor` lowering.

Before saving, a symbol target is reparsed if its file-version key changed. A
unique identity match updates its location. A missing or ambiguous match blocks
the operation rather than emitting a known-stale line number.

## 6. Token Counter

The token service is independent of providers:

```rust
trait TokenCounter {
    fn name(&self) -> &str;
    fn count(&self, text: &str) -> Result<usize>;
}
```

Version one implements `gpt-4o` using `tiktoken-rs` and the `o200k_base`
singleton. The public configuration uses a tokenizer name rather than exposing
the crate type so other encodings can be added later.

Counts are cached by:

```text
canonical path + byte size + modification time + tokenizer name
```

Symbol range counts reuse parsed source already held in the parse cache. Files
are tokenized off the UI thread. Before a count arrives, candidates display an
ellipsis rather than a byte-derived estimate.

The live reference total uses normalized context identities:

- `File(canonical_path)` for whole files;
- `Range(canonical_path, start_byte, end_byte, file_version)` for symbols;
- no source-context contribution for skills, GitHub URLs, or Jira URLs;
- duplicate identities count once; and
- a `File` identity suppresses every `Range` identity for the same file.

This total is an awareness aid, not a promise that an agent will load precisely
that number of tokens.

## 7. Skill Provider

### Codex-compatible roots

Without calling the Codex app server, the built-in profile discovers local
skills from:

1. `.agents/skills` in each directory from the working directory through the
   discovered repository root;
2. `~/.agents/skills`;
3. `$CODEX_HOME/skills`, defaulting to `~/.codex/skills`, including bundled
   `.system` skills;
4. `/etc/codex/skills`; and
5. skill roots declared by locally installed Codex plugin manifests under
   `$CODEX_HOME/plugins`.

The loader follows valid symlinked skill directories for repo, user, and admin
scopes, avoids cycles, and does not follow a skill outside an explicitly
configured root when that root is marked contained.

It parses `SKILL.md` YAML frontmatter for at least `name` and `description`.
Optional `agents/openai.yaml` interface metadata may provide display name and
short description. Invalid skills are skipped with a diagnostic.

The loader reads applicable `[[skills.config]]` path rules from Codex TOML and
omits disabled skills. Because no app server is used, remote environment skills
and session-only skill roots cannot be guaranteed; the UI identifies its list
as local discovery.

### Generic agent configuration

Additional TOML-defined skill roots specify:

- absolute, home-relative, or repository-relative path;
- whether to scan direct children or recursively;
- whether to walk matching roots through repository ancestors;
- scope label;
- metadata filename and frontmatter keys; and
- emitted mention template.

The default emitted template is `${leader}${name}`. The accepted Codex skill
therefore remains `$gh-fix-ci`; no instructions or file paths are inserted.

Duplicate names remain separate completion candidates with scope and source
path shown. Since the emitted mention may be identical, accepting a duplicate
name warns that the target agent will apply its own precedence rules.

## 8. GitHub Provider

The GitHub provider invokes the configured `gh` executable directly with
`std::process::Command`; it never constructs a shell command.

Issue search uses the current repository inferred by `gh`, conceptually:

```sh
gh issue list --search QUERY --limit LIMIT \
  --json number,title,url,state,labels,updatedAt
```

Pull-request search uses:

```sh
gh pr list --search QUERY --limit LIMIT \
  --json number,title,url,state,isDraft,updatedAt
```

Exact numeric queries rank the matching issue or pull request first. Text
queries use the CLI's search semantics. The provider does not add `--repo` or
`--hostname` by default, allowing `gh` to infer the repository and enterprise
host from the worktree and its standard configuration.

The child inherits `GH_HOST`, `GH_REPO`, `GH_TOKEN`, `GITHUB_TOKEN`,
`GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN`, and normal `gh` credential
storage. `TG` never reads or logs credential values. Onboarding may inspect
non-secret host/repository metadata to identify the target of an explicit check.
It sets noninteractive and no-color behavior where supported and captures
stdout/stderr separately.

Candidates show number, title, state, and update age. Accepted issues and pull
requests lower to the exact full `url` returned by `gh`.

## 9. Jira Provider

The Jira provider invokes the configured `jira` executable directly and uses
raw JSON output. It inherits the CLI's environment and configuration, including
`JIRA_API_TOKEN`, `JIRA_AUTH_TYPE`, `JIRA_CONFIG_FILE`, Cloud/local server
selection, project selection, keychain or netrc use, and mTLS files.

Configuration may define:

```toml
[providers.jira]
key_prefix = "G5"
```

Query normalization is:

- digits only: prepend `G5-` when a prefix is configured;
- a complete `LETTERS-NUMBER` key: preserve it exactly;
- other text: search within the Jira CLI's configured project using JQL; and
- an empty query: list recent issues from the configured project.

The prefix is normalized without a trailing hyphen in configuration. A value
outside Jira's project-key character rules is rejected during configuration
loading.

Exact keys are resolved directly. Text search requests JSON with enough fields
to present key, summary, status, assignee, and update time. The browser URL is
the configured Jira base URL joined with `/browse/<KEY>`. API `self` URLs may
be used to derive the base when the CLI's output does not expose it directly.

Accepted issues always lower to a full URL, never only to the key.

Onboarding reads allowlisted server/project/auth-mode metadata from the selected
Jira CLI configuration so an authentication failure can still name its target.
An explicit connection check performs a bounded issue search. A successful
search is shown as `Search works`, not as verified authentication: the supported
Jira CLI's `me` command only prints the locally configured login. Public or empty
results do not prove that an API token is valid. Authentication rejection,
permission denial, connectivity failure, and malformed output remain distinct
from successful search access; raw service output and credentials are not shown.

## 10. Subprocess Policy

Provider subprocesses:

- run only after their leader activates or the user explicitly requests a connection check;
- never use a shell;
- receive query text as a distinct argument;
- inherit the environment without enumerating it in logs;
- have configurable timeouts;
- are cancelled or ignored when their generation becomes stale;
- cap captured output; and
- redact stderr before presenting it if it appears to contain authorization
  headers or token-shaped values.

A missing executable disables only that provider. Authentication failures show
the CLI's concise remediation, such as running `gh auth login` or `jira init`,
without blocking local reference providers.

## 11. Preview Policy

`Ctrl-P` toggles preview while completion is open.

- Files show numbered source beginning near the top.
- Symbols center on the declaration and highlight its range.
- Skills show name, description, scope, and source path, not the full skill.
- GitHub and Jira items show their metadata and a bounded plain-text body when
  it is already available without a second request.

Preview never adds content to the reference context total and never performs a
write or browser launch.

## 12. Verification

Provider contract tests use fixtures and fake executables. They must cover:

- token-boundary and leader escaping behavior;
- unchanged existing file and symbol search results;
- GPT-4o counts for known text fixtures;
- range/file total deduplication and whole-file subsumption;
- skill root precedence, symlinks, invalid frontmatter, disabled paths, and
  generic mention templates;
- GitHub issue/PR command arguments and enterprise URLs;
- Jira exact keys, `G5` numeric expansion, text JQL, Cloud and on-premises URLs;
- missing executables, timeouts, malformed JSON, oversized output, and
  authentication errors; and
- cancellation and stale-generation rejection; and
- a fixture matrix for every core language and each implemented extended
  language, including nested, duplicate, malformed, and excluded-local cases.
