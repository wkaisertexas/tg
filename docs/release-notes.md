# Prompt Editor Release Notes

`tg` is now a focused Vim-like prompt editor instead of a selector REPL. There
is no persistent transcript or multi-submission session. Open the prompt file
that an agent supplies through `VISUAL` or `EDITOR`, or name a file directly:

```sh
tg prompts/task.md
tg --root /path/to/project prompts/task.md
VISUAL=tg EDITOR=tg codex
```

The old `tg .` invocation no longer opens the selector; directories are not
editable documents. To search a project while editing an unnamed buffer, use
`tg --root .`. To edit a file while searching a different project, use the
two-argument form shown above.

Accepted file, symbol, skill, GitHub, and Jira references stay friendly in the
buffer and lower on `:w`, `:wq`, or `:copy`. Existing checksum-verified release
binaries, `TG_INSTALL_DIR`, `TG_REPOSITORY`, and `tg update` remain supported.
The installer can optionally add the absolute `tg` path as `EDITOR`, `VISUAL`,
or both; pressing Enter makes no shell changes, and
`TG_NO_EDITOR_PROMPT=1` disables that prompt for automation.
