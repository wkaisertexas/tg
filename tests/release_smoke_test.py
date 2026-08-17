#!/usr/bin/env python3
"""Smoke-test a built tg artifact through its public process interfaces."""

from __future__ import annotations

import argparse
import errno
import fcntl
import os
from pathlib import Path
import pty
import re
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time


ROOT = Path(__file__).resolve().parents[1]
TIMEOUT_SECONDS = 10


class SmokeFailure(RuntimeError):
    pass


def isolated_environment(home: Path) -> dict[str, str]:
    env = os.environ.copy()
    for name in (
        "TG_CONFIG",
        "TG_GH_COMMAND",
        "TG_JIRA_COMMAND",
        "TG_ROOT",
        "TG_TOKENIZER",
        "XDG_CONFIG_HOME",
    ):
        env.pop(name, None)
    env.update(
        {
            "HOME": str(home),
            "TERM": "xterm-256color",
            "TG_NO_PROJECT_CONFIG": "1",
        }
    )
    return env


def run(
    argv: list[str], *, cwd: Path, env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        cwd=cwd,
        env=env,
        check=False,
        capture_output=True,
        text=True,
        timeout=TIMEOUT_SECONDS,
    )


def require_success(result: subprocess.CompletedProcess[str], context: str) -> None:
    if result.returncode != 0:
        raise SmokeFailure(
            f"{context} exited {result.returncode}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )


def run_pty(
    argv: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    input_chunks: list[bytes],
) -> tuple[int, bytes]:
    master_fd, slave_fd = pty.openpty()
    fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
    process = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        start_new_session=True,
    )
    os.close(slave_fd)

    output = bytearray()

    def read_terminal() -> None:
        while True:
            try:
                chunk = os.read(master_fd, 4096)
            except OSError as error:
                if error.errno in (errno.EIO, errno.EBADF):
                    return
                raise
            if not chunk:
                return
            output.extend(chunk)

    reader = threading.Thread(target=read_terminal, daemon=True)
    reader.start()

    try:
        # Let the terminal enter raw mode before sending editor keystrokes.
        time.sleep(0.3)
        for chunk in input_chunks:
            os.write(master_fd, chunk)
            time.sleep(0.15)
        return_code = process.wait(timeout=TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as error:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
        raise SmokeFailure(f"PTY command timed out: {' '.join(argv)}") from error
    finally:
        reader.join(timeout=1)
        try:
            os.close(master_fd)
        except OSError:
            pass

    return return_code, bytes(output)


def assert_editor_result(
    *, return_code: int, output: bytes, prompt: Path, expected: str, context: str
) -> None:
    rendered = output.decode("utf-8", errors="replace")
    if return_code != 0:
        raise SmokeFailure(f"{context} exited {return_code}\nterminal output:\n{rendered}")
    actual = prompt.read_text(encoding="utf-8")
    if actual != expected:
        raise SmokeFailure(
            f"{context} wrote {actual!r}, expected {expected!r}\n"
            f"terminal output:\n{rendered}"
        )
    if b"\x1b[?1049h" not in output or b"\x1b[?1049l" not in output:
        raise SmokeFailure(f"{context} did not restore the alternate screen")


def test_version(binary: Path) -> None:
    cargo_toml = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml, re.MULTILINE)
    if match is None:
        raise SmokeFailure("could not read the package version from Cargo.toml")
    result = run([str(binary), "--version"], cwd=ROOT, env=os.environ.copy())
    require_success(result, "version check")
    expected = f"tscodeselection {match.group(1)}"
    if result.stdout.strip() != expected:
        raise SmokeFailure(f"version output was {result.stdout.strip()!r}, expected {expected!r}")


def test_help(binary: Path) -> None:
    result = run([str(binary), "--help"], cwd=ROOT, env=os.environ.copy())
    require_success(result, "help check")
    for expected in ("Usage: tg", "--resolve", "update"):
        if expected not in result.stdout:
            raise SmokeFailure(f"help output did not contain {expected!r}")


def test_headless_resolution(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="tg-smoke-resolve-") as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        (root / "selected.txt").write_text("selected\n", encoding="utf-8")
        result = run(
            [str(binary), "--root", str(root), "--resolve", "Read @selected.txt"],
            cwd=root,
            env=isolated_environment(home),
        )
        require_success(result, "headless resolution")
        if result.stdout != "Read selected.txt\n":
            raise SmokeFailure(f"headless resolution returned {result.stdout!r}")


def test_direct_editor(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="tg-smoke-editor-") as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        prompt = root / "prompt.md"
        return_code, output = run_pty(
            [str(binary), str(prompt)],
            cwd=root,
            env=isolated_environment(home),
            input_chunks=[b"iDirect smoke", b"\x1b", b":wq\r"],
        )
        assert_editor_result(
            return_code=return_code,
            output=output,
            prompt=prompt,
            expected="Direct smoke",
            context="direct editor",
        )


def test_external_editor_parent(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="tg-smoke-parent-") as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        prompt = root / "prompt.md"
        env = isolated_environment(home)
        env.update({"VISUAL": str(binary), "EDITOR": str(binary)})
        parent = (
            "import os, subprocess, sys; "
            "editor = os.environ.get('VISUAL') or os.environ['EDITOR']; "
            "raise SystemExit(subprocess.call([editor, sys.argv[1]]))"
        )
        return_code, output = run_pty(
            [sys.executable, "-c", parent, str(prompt)],
            cwd=root,
            env=env,
            input_chunks=[b"iParent smoke", b"\x1b", b":wq\r"],
        )
        assert_editor_result(
            return_code=return_code,
            output=output,
            prompt=prompt,
            expected="Parent smoke",
            context="external-editor parent",
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="path to the built tg executable")
    args = parser.parse_args()
    binary = args.binary.expanduser().resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"not an executable file: {binary}")

    checks = (
        ("version", test_version),
        ("help", test_help),
        ("headless resolution", test_headless_resolution),
        ("direct PTY editing", test_direct_editor),
        ("external-editor parent invocation", test_external_editor_parent),
    )
    try:
        for label, check in checks:
            check(binary)
            print(f"ok: {label}")
    except (OSError, SmokeFailure, subprocess.SubprocessError) as error:
        print(f"FAILED: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
