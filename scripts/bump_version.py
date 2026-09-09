#!/usr/bin/env python3

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import stat
import sys
import tempfile
import tomllib

SEMVER = re.compile(r"(\d+)\.(\d+)\.(\d+)")
PACKAGE_HEADER = re.compile(r"(?m)^\[package\]\s*$")
SECTION_HEADER = re.compile(r"(?m)^\[")
VERSION_LINE = re.compile(r'(?m)^(\s*version\s*=\s*")([^"]+)(".*)$')


def next_patch(version: str) -> str:
    match = SEMVER.fullmatch(version)
    if match is None:
        raise ValueError(f"package version is not major.minor.patch: {version}")
    major, minor, patch = map(int, match.groups())
    return f"{major}.{minor}.{patch + 1}"


def update_manifest(text: str) -> tuple[str, str]:
    package_version = tomllib.loads(text).get("package", {}).get("version")
    if not isinstance(package_version, str):
        raise ValueError("Cargo.toml does not define package.version")

    package = PACKAGE_HEADER.search(text)
    if package is None:
        raise ValueError("Cargo.toml does not contain a [package] section")
    body_start = package.end()
    following = SECTION_HEADER.search(text, body_start)
    body_end = following.start() if following else len(text)
    body = text[body_start:body_end]
    declarations = list(VERSION_LINE.finditer(body))
    if len(declarations) != 1:
        raise ValueError("[package] must contain exactly one version declaration")
    declaration = declarations[0]
    if declaration.group(2) != package_version:
        raise ValueError("parsed package version does not match its declaration")

    version = next_patch(package_version)
    replacement = f'{declaration.group(1)}{version}{declaration.group(3)}'
    updated_body = body[: declaration.start()] + replacement + body[declaration.end() :]
    return text[:body_start] + updated_body + text[body_end:], version


def write_atomic(path: Path, content: str) -> None:
    mode = stat.S_IMODE(path.stat().st_mode)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary_path = Path(temporary)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary_path, mode)
        os.replace(temporary_path, path)
    finally:
        temporary_path.unlink(missing_ok=True)


def bump_manifest(path: Path) -> str:
    updated, version = update_manifest(path.read_text(encoding="utf-8"))
    write_atomic(path, updated)
    return version


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", nargs="?", type=Path, default=Path("Cargo.toml"))
    args = parser.parse_args()
    try:
        version = bump_manifest(args.manifest)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    print(version)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
