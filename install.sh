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
