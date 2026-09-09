#!/bin/sh
set -eu

repository="${TG_REPOSITORY:-wkaisertexas/tg}"
install_dir="${TG_INSTALL_DIR:-$HOME/.local/bin}"
requested_install_dir="$install_dir"

fail() {
  printf 'tg: %s\n' "$1" >&2
  exit 1
}

missing=""
for dependency in curl uname awk mktemp chmod mv mkdir rm sed; do
  command -v "$dependency" >/dev/null 2>&1 || missing="$missing $dependency"
done
if command -v sha256sum >/dev/null 2>&1; then
  checksum_tool=sha256sum
elif command -v shasum >/dev/null 2>&1; then
  checksum_tool=shasum
else
  missing="$missing sha256sum-or-shasum"
fi
[ -z "$missing" ] || fail "missing required commands:$missing. Install them with your system package manager and rerun the installer; SHA-256 verification requires sha256sum or shasum."

system="$(uname -s)" || fail "could not detect the operating system with uname -s."
machine="$(uname -m)" || fail "could not detect the architecture with uname -m."
case "$system-$machine" in
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-gnu" ;;
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  *) fail "unsupported platform $system-$machine. Build tg from source with Cargo instead." ;;
esac

case "$install_dir" in
  /*) ;;
  *) install_dir="$(pwd -P)/$install_dir" ;;
esac
mkdir -p "$install_dir" || fail "cannot create install directory $install_dir. Choose a writable directory with TG_INSTALL_DIR (for example, TG_INSTALL_DIR=\"\$HOME/.local/bin\" on the sh command)."
[ -d "$install_dir" ] && [ -w "$install_dir" ] && [ -x "$install_dir" ] || fail "install directory $install_dir is not writable/searchable. Fix its permissions or set TG_INSTALL_DIR to a directory you own."
install_dir="$(CDPATH= cd "$install_dir" && pwd -P)" || fail "cannot access install directory $install_dir. Check its permissions or choose another TG_INSTALL_DIR."
installed_tg="$install_dir/tg"
[ ! -d "$installed_tg" ] || fail "$installed_tg is a directory. Choose another TG_INSTALL_DIR or move that directory before installing."
temporary="$(mktemp -d "$install_dir/.tg-install.XXXXXX")" || fail "cannot create a staging directory in $install_dir. Check permissions and free space, or choose another TG_INSTALL_DIR."
trap 'rm -rf "$temporary"' 0
trap 'exit 1' HUP INT TERM

asset="tg-$target"
base="https://github.com/$repository/releases/latest/download"
curl -fsSL "$base/$asset" -o "$temporary/$asset" || fail "could not download $asset from $base. Check connectivity and TG_REPOSITORY; the installed tg was not changed."
curl -fsSL "$base/$asset.sha256" -o "$temporary/$asset.sha256" || fail "could not download the SHA-256 checksum. The installed tg was not changed; retry when the release checksum is available."
expected="$(awk '{print $1}' "$temporary/$asset.sha256")" || fail "could not read the release checksum; the installed tg was not changed."
if [ "$checksum_tool" = sha256sum ]; then
  checksum_output="$(sha256sum "$temporary/$asset")" || fail "sha256sum failed; the installed tg was not changed."
else
  checksum_output="$(shasum -a 256 "$temporary/$asset")" || fail "shasum failed; the installed tg was not changed."
fi
actual="$(printf '%s\n' "$checksum_output" | awk '{print $1}')" || fail "could not read the calculated checksum; the installed tg was not changed."
[ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum mismatch; the installed tg was not changed. Retry the download or report the release; do not bypass verification."

chmod 755 "$temporary/$asset" || fail "cannot make the downloaded tg executable; the installed tg was not changed."
if ! version_output="$("$temporary/$asset" --version </dev/null 2>&1)"; then
  printf '%s\n' "$version_output" >&2
  fail "downloaded tg failed its --version check on $system-$machine; the installed tg was not changed. Check platform/runtime compatibility and whether $install_dir allows execution, or build from source."
fi
mv -f "$temporary/$asset" "$installed_tg" || fail "cannot replace $installed_tg. Check destination permissions and free space, then retry."
printf 'Installed tg to %s\nVersion: %s\nSHA-256 verified; candidate --version check passed.\n' "$installed_tg" "$version_output"

quote_path() {
  case "${SHELL:-}" in
    */fish|fish) escaped_path="$(printf '%s' "$1" | sed 's/[\\$\"]/\\&/g')" ;;
    *) escaped_path="$(printf '%s' "$1" | sed 's/[\\$\"`]/\\&/g')" ;;
  esac
  printf '"%s"' "$escaped_path"
}

quoted_tg="$(quote_path "$installed_tg")"
quoted_install_dir="$(quote_path "$install_dir")"
case ":${PATH:-}:" in
  *":$install_dir:"*|*":$requested_install_dir:"*) ;;
  *)
    printf '\nThe install directory is not on PATH: %s\n' "$install_dir"
    case "${SHELL:-}" in
      */bash|bash)
        printf 'For this session, run:\n  export PATH=%s:"$PATH"\n' "$quoted_install_dir"
        printf 'To persist, add that line to %s/.bashrc (login shells must source it).\n' "$HOME"
        ;;
      */zsh|zsh)
        printf 'For this session, run:\n  export PATH=%s:"$PATH"\n' "$quoted_install_dir"
        printf 'To persist, add that line to %s/.zshrc.\n' "${ZDOTDIR:-$HOME}"
        ;;
      */fish|fish)
        printf 'For this session, run:\n  set -gx PATH %s $PATH\n' "$quoted_install_dir"
        printf 'To persist, add that line to %s/fish/config.fish.\n' "${XDG_CONFIG_HOME:-$HOME/.config}"
        ;;
      */sh|sh|*/dash|dash|*/ksh|ksh)
        printf 'For this session, run:\n  export PATH=%s:"$PATH"\n' "$quoted_install_dir"
        printf 'To persist for login shells, add that line to %s/.profile.\n' "$HOME"
        ;;
      *)
        printf 'Unrecognized shell %s: add %s using your shell\047s PATH configuration.\n' "${SHELL:-unknown}" "$quoted_install_dir"
        printf 'The commands below use sh quoting; run them in sh if your shell uses different syntax.\n'
        ;;
    esac
    printf 'The installer has not changed PATH or added PATH entries to shell startup files.\n'
    ;;
esac
printf '\nNext steps (these absolute paths work without changing PATH):\n  %s setup\n  %s doctor\n' "$quoted_tg" "$quoted_tg"
printf 'setup shows onboarding and effective reference leaders; doctor reports local readiness without network requests.\n'
printf 'Remote checks are opt-in: run %s doctor --check jira (or --check all).\n' "$quoted_tg"
printf 'GitHub and Jira authentication stays in gh and jira; tg does not manage credentials.\n'
printf 'In the editor, use :providers or Normal-mode Space then p for provider status.\n'

editor_setup() {
  [ "${TG_NO_EDITOR_PROMPT:-0}" != "1" ] || return 0

  editor_line="export EDITOR=$quoted_tg"
  visual_line="export VISUAL=$quoted_tg"

  case "${SHELL:-}" in
    */bash|bash) rc_file="$HOME/.bashrc" ;;
    */zsh|zsh) rc_file="${ZDOTDIR:-$HOME}/.zshrc" ;;
    */fish|fish)
      printf 'tg: automatic editor setup skipped for fish.\n'
      printf 'To configure manually, add only unset variables to %s/fish/config.fish; keep any existing assignments:\n' "${XDG_CONFIG_HOME:-$HOME/.config}"
      [ "${EDITOR+x}" = x ] || printf '  set -gx EDITOR %s\n' "$quoted_tg"
      [ "${VISUAL+x}" = x ] || printf '  set -gx VISUAL %s\n' "$quoted_tg"
      return 0
      ;;
    *)
      echo "tg: automatic editor setup skipped: unrecognized shell ${SHELL:-unknown}."
      echo "To configure manually, use your shell's equivalents of these POSIX assignments; keep any existing assignments:"
      [ "${EDITOR+x}" = x ] || printf '  %s\n' "$editor_line"
      [ "${VISUAL+x}" = x ] || printf '  %s\n' "$visual_line"
      return 0
      ;;
  esac

  for dependency in grep tail od tr; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
      printf 'tg: automatic editor setup skipped: missing %s. Install it or inspect %s and configure manually; no editor settings were changed.\n' "$dependency" "$rc_file"
      return 0
    fi
  done
  if [ -e "$rc_file" ] && { [ ! -f "$rc_file" ] || [ ! -r "$rc_file" ]; }; then
    printf 'tg: automatic editor setup skipped: cannot safely inspect %s. Keep existing EDITOR/VISUAL settings when configuring manually.\n' "$rc_file"
    return 0
  fi

  offer_editor=1
  offer_visual=1
  [ "${EDITOR+x}" = x ] && offer_editor=0
  [ "${VISUAL+x}" = x ] && offer_visual=0
  if [ -f "$rc_file" ]; then
    grep -Eq '^[[:space:]]*(export[[:space:]]+)?EDITOR[[:space:]]*=' "$rc_file" && offer_editor=0
    grep -Eq '^[[:space:]]*(export[[:space:]]+)?VISUAL[[:space:]]*=' "$rc_file" && offer_visual=0
  fi
  [ "$offer_editor" -eq 1 ] || [ "$offer_visual" -eq 1 ] || return 0

  echo "Optional editor setup destination: $rc_file"
  [ "$offer_editor" -eq 0 ] || echo "  $editor_line"
  [ "$offer_visual" -eq 0 ] || echo "  $visual_line"

  if ! [ -t 0 ] || ! [ -t 1 ]; then
    echo "tg: no controlling terminal; editor setup skipped. Add the lines above manually if desired."
    return 0
  fi

  if [ "$offer_editor" -eq 1 ] && [ "$offer_visual" -eq 1 ]; then
    printf 'Configure tg as [e] EDITOR, [v] VISUAL, [b] both, or [Enter] no change? '
  elif [ "$offer_editor" -eq 1 ]; then
    printf 'Configure tg as [e] EDITOR, or [Enter] no change? '
  else
    printf 'Configure tg as [v] VISUAL, or [Enter] no change? '
  fi
  IFS= read -r choice || choice=""

  append_editor=0
  append_visual=0
  case "$choice" in
    e|E) [ "$offer_editor" -eq 1 ] && append_editor=1 ;;
    v|V) [ "$offer_visual" -eq 1 ] && append_visual=1 ;;
    b|B)
      if [ "$offer_editor" -eq 1 ] && [ "$offer_visual" -eq 1 ]; then
        append_editor=1
        append_visual=1
      fi
      ;;
    "") return 0 ;;
    *)
      echo "tg: editor setup declined; no changes made."
      return 0
      ;;
  esac
  [ "$append_editor" -eq 1 ] || [ "$append_visual" -eq 1 ] || return 0

  if [ -s "$rc_file" ]; then
    last_byte="$(tail -c 1 "$rc_file" | od -An -t u1 | tr -d '[:space:]')"
    [ "$last_byte" = 10 ] || printf '\n' >> "$rc_file"
  fi
  [ "$append_editor" -eq 0 ] || printf '%s\n' "$editor_line" >> "$rc_file"
  [ "$append_visual" -eq 0 ] || printf '%s\n' "$visual_line" >> "$rc_file"
  echo "Updated $rc_file."
}

editor_setup
