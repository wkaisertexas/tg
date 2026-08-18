#!/usr/bin/env bash
# 🦀 UTF-8 before declarations
GLOBAL="日本語"
export EXPORTED=2
readonly READONLY=3

posix_fn() { printf '%s\n' "$GLOBAL"; }
function bash_fn { :; }
redirected() { :; } >"$TMPDIR/out"

outer() {
  local LOCAL=1
  FOO=bar command argument
  inner() { :; }
}

A=1 B=2 command
ARRAY[0]=value
for item in one two; do
  LOOP=$item
done

one() { child() { :; } }
two() { child() { :; } }

render() { :; }
render() { printf duplicate; }
