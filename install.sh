#!/bin/sh
set -eu

repository="${TG_REPOSITORY:-wkaisertexas/tg}"
install_dir="${TG_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-gnu" ;;
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  *) echo "tg: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

asset="tg-$target"
base="https://github.com/$repository/releases/latest/download"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

curl -fsSL "$base/$asset" -o "$temporary/$asset"
curl -fsSL "$base/$asset.sha256" -o "$temporary/$asset.sha256"
expected="$(awk '{print $1}' "$temporary/$asset.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$temporary/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$temporary/$asset" | awk '{print $1}')"
fi
[ "$expected" = "$actual" ] || { echo "tg: checksum mismatch" >&2; exit 1; }

mkdir -p "$install_dir"
chmod 755 "$temporary/$asset"
mv "$temporary/$asset" "$install_dir/tg"
echo "Installed tg to $install_dir/tg"
case ":$PATH:" in *":$install_dir:"*) ;; *) echo "Add $install_dir to PATH." ;; esac

editor_setup() {
  [ "${TG_NO_EDITOR_PROMPT:-0}" != "1" ] || return 0

  installed_tg="$(cd "$install_dir" && pwd -P)/tg"
  escaped_tg="$(printf '%s' "$installed_tg" | sed 's/[\\$\"`]/\\&/g')"
  editor_line="export EDITOR=\"$escaped_tg\""
  visual_line="export VISUAL=\"$escaped_tg\""

  case "${SHELL:-}" in
    */bash) rc_file="$HOME/.bashrc" ;;
    */zsh) rc_file="$HOME/.zshrc" ;;
    *)
      echo "tg: automatic editor setup skipped: unrecognized shell ${SHELL:-unknown}."
      echo "To configure manually, add the desired lines to your shell startup file:"
      [ "${EDITOR+x}" = x ] || echo "  $editor_line"
      [ "${VISUAL+x}" = x ] || echo "  $visual_line"
      return 0
      ;;
  esac

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
