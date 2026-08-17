# Configuration and Minimal UI Design

## 1. Purpose

This document specifies configuration discovery, schema principles, safe
layering, key bindings owned by `tg`, and the visual behavior of the editor and
completion overlay.

## 2. Configuration Locations and Precedence

Configuration is merged from lowest to highest precedence:

1. compiled defaults;
2. `$XDG_CONFIG_HOME/tg/config.toml`, or `~/.config/tg/config.toml` when
   `XDG_CONFIG_HOME` is unset;
3. `.tg.toml` at the discovered repository root;
4. explicitly supported `TG_*` environment overrides; and
5. CLI flags.

`--config <PATH>` replaces the normal user-config path but does not suppress
repository configuration unless `--no-project-config` is also present.
`TG_CONFIG` provides the same path override for editor environment setup.

Unknown keys are errors by default. Diagnostics name the file and full key
path. A bad project configuration must not destroy or rewrite the edited file;
the application reports the error before entering the terminal UI.

## 3. Project Configuration Safety

Repository `.tg.toml` is data owned by the checked-out project and may be
untrusted. It may override:

- leaders;
- UI appearance;
- non-executable search limits and exclusions;
- Jira key prefix; and
- tokenizer selection from the built-in tokenizer registry.

It may not override:

- provider executable paths;
- arbitrary command arguments;
- skill roots outside the repository;
- configuration paths;
- update repositories; or
- any future command-execution setting.

Executable paths and external skill roots are accepted only from user config,
environment, or CLI flags. This avoids running a repository-supplied binary
merely because the user typed `#`, `!`, or `&`.

## 4. Proposed TOML Schema

```toml
version = 1

[editor]
line_numbers = "relative"       # relative | absolute | none
current_line_absolute = true
tab_width = 4
wrap = true
copy_command = ":copy"

[ui]
preview = "manual"              # manual | automatic | disabled
preview_toggle = "ctrl-p"
completion_height = 12
completion_width_percent = 80
status_timeout_ms = 2500
color = "auto"                  # auto | always | never

[leaders]
files = "@"
broad_files = "%"
symbols = "::"
skills = "$"
github_issues = "#"
github_pull_requests = "!"
jira_issues = "&"

[search]
limit = 100
debounce_ms = 35
broad_excludes = [".git"]

[tokens]
tokenizer = "gpt-4o"
decimals = 1
show_file = true
show_symbol = true
show_total = true

[skills]
profile = "codex-local"
mention = "${leader}${name}"
read_codex_disable_rules = true

[[skills.roots]]
path = "~/company-agent/skills"
scope = "user"
discovery = "recursive"         # recursive | direct-children
metadata = "SKILL.md"

[providers.github]
enabled = true
command = "gh"
limit = 50
timeout_ms = 5000

[providers.jira]
enabled = true
command = "jira"
key_prefix = "G5"
limit = 50
timeout_ms = 5000
```

The schema is deserialized into typed structures with defaults. `figment` is a
good fit for layered TOML and environment providers, but the implementation
must still enforce the per-source safety restrictions above rather than blindly
merging arbitrary values.

## 5. Leader Validation

Leaders are nonempty printable strings without whitespace. They may contain
multiple characters. Validation rejects:

- duplicate leaders;
- an ambiguous prefix relation such as `#` and `##`;
- backslash, which is reserved for escaping;
- line breaks or terminal control characters; and
- leaders that collide with an application-level key notation.

Changing the input leader does not necessarily change the lowered syntax. A
skill selected through a customized leader still uses the configured skill
mention template, which defaults to the coding agent's native `$name` form.

## 6. Environment and CLI Overrides

The initial environment surface should remain small:

```text
TG_CONFIG
TG_ROOT
TG_TOKENIZER
TG_GH_COMMAND
TG_JIRA_COMMAND
TG_NO_PROJECT_CONFIG
```

Provider authentication variables are inherited and interpreted only by their
own CLIs. `tg` does not introduce aliases for GitHub or Jira tokens.

Useful CLI options are:

```text
--root <DIRECTORY>
--config <FILE>
--no-project-config
--tokenizer <NAME>
--no-preview
--no-color
```

Existing `tg update` remains a subcommand. Headless `--resolve` may remain for
tests and scripts, updated to use the new provider/lowering engine.

## 7. Persistent Editor UI

The normal screen dedicates all but one row to the buffer. There is no product
logo, header, transcript, persistent help, match list, or preview pane.

The gutter uses a dark or otherwise subdued style. The current line may have a
slightly stronger number, but the full line is not highlighted by default.
Structured references receive one subtle configurable style.

The status line is one row:

```text
 INSERT  refactor-auth.md [+]                refs 31.2k  18:7
```

It contains, as space permits:

1. mode;
2. filename or `[No Name]`;
3. dirty marker;
4. transient error or success message;
5. deduplicated reference token total; and
6. cursor line and column.

Fields disappear from lowest importance when the terminal narrows. The editor
must remain usable at 40 columns by 8 rows.

## 8. Completion Overlay

The overlay does not exist until a leader activates. It is anchored near the
cursor where possible and constrained to the terminal bounds.

Without preview:

```text
┌ FILES ───────────────────────────────────────────────────┐
│ src/app.rs                                      12.4k    │
│ src/composer/mod.rs                              1.1k    │
│ src/language/mod.rs                             18.7k    │
└──────────────────────────────────────────────────────────┘
```

With preview toggled by `Ctrl-P`:

```text
┌ FILES ───────────────────┬ PREVIEW ──────────────────────┐
│ src/app.rs       12.4k   │  341  fn submit(...) {       │
│ src/composer...   1.1k   │  342      ...                │
│ src/language...  18.7k   │                              │
└──────────────────────────┴───────────────────────────────┘
```

The selected row uses a single strong accent. Nonselected rows, borders, match
highlights, metadata, and preview line numbers use lower visual contrast.

The overlay header names the provider and may show an unobtrusive spinner or
result count. It does not repeat keyboard instructions. On-demand help can
show the active bindings.

## 9. Preview Behavior

The default is `manual`: preview begins hidden for each new completion session.
`Ctrl-P` toggles it for the current session. The preference does not consume
vertical space after completion closes.

`automatic` opens preview after the selection remains unchanged for the search
debounce interval. `disabled` causes `Ctrl-P` to show a brief status message
rather than opening it.

Long source lines are clipped by default. Horizontal preview scrolling is
deferred; the result must clearly indicate clipping.

## 10. Color and Accessibility

The default theme follows terminal colors and avoids large filled regions.
Color is not the only signal for mode, dirty state, provider type, errors, or
selection. `NO_COLOR` is honored, and `[ui].color = "never"` has the same
effect.

All essential metadata remains legible in 16-color terminals. True color may
improve themes but is not required.

## 11. Errors and Diagnostics

Transient nonfatal messages replace the middle of the status line and expire
after `status_timeout_ms`. Errors that block saving remain until acknowledged
or corrected.

Provider stderr is summarized, not streamed into the buffer. Detailed logs are
opt-in and written outside stdout. Secrets and full inherited environments are
never logged.

Examples:

```text
GitHub unavailable: run `gh auth login`
Jira unavailable: run `jira init`
Reference moved; select the symbol again
Copied 4.8k tokens with OSC 52
```

## 12. Verification

Configuration tests must cover defaults, every precedence layer, unknown keys,
leader collisions, XDG paths, home expansion, source-restricted settings, and
invalid Jira prefixes.

Snapshot or buffer-level UI tests must cover normal editing, narrow terminals,
completion with and without preview, long paths, Unicode, no-color mode,
provider errors, and status-field elision. Visual tests should assert that no
search panes or header are rendered while completion is inactive.
