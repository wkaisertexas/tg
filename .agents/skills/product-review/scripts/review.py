import argparse
import errno
import fcntl
import hashlib
import json
import os
import pty
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time
from datetime import datetime, timezone
from pathlib import Path

from screen import Capture, font_path, render_png


SKILL = Path(__file__).resolve().parents[1]
REPOSITORY = SKILL.parents[2]
KEYS = {
    "Escape": b"\x1b", "Enter": b"\r", "Tab": b"\t", "Backspace": b"\x7f",
    "Up": b"\x1b[A", "Down": b"\x1b[B", "Right": b"\x1b[C", "Left": b"\x1b[D",
    "Home": b"\x1b[H", "End": b"\x1b[F", "PageUp": b"\x1b[5~", "PageDown": b"\x1b[6~",
    "Space": b" ",
}


def local_only(environment=None):
    environment = os.environ if environment is None else environment
    for name in ("CI", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE", "TF_BUILD"):
        if environment.get(name, "").lower() not in ("", "0", "false", "no"):
            raise ValueError("Product review is local-only; do not run it in CI")


def dimensions(cols, rows):
    if not 30 <= cols <= 240 or not 8 <= rows <= 80:
        raise ValueError("Terminal size must be 30..240 columns and 8..80 rows")


def encode_key(key):
    if key in KEYS:
        return KEYS[key]
    if len(key) == 6 and key.startswith("Ctrl-") and key[-1].isascii() and key[-1].isalpha():
        return bytes([ord(key[-1].upper()) - 64])
    if len(key) == 1 and key.isprintable():
        return key.encode()
    raise ValueError(f"Unsupported key: {key}")


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def isolated_environment(run):
    return {
        "HOME": str(run / "home"), "XDG_CONFIG_HOME": str(run / "home/config"),
        "XDG_CACHE_HOME": str(run / "home/cache"), "TMPDIR": str(run / "tmp"),
        "PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "LANG": "en_US.UTF-8",
        "GIT_CONFIG_NOSYSTEM": "1", "TG_NO_PROJECT_CONFIG": "1",
    }


def create_run(binary, output, cols, rows, no_color=False, provider_error=False, font=None):
    local_only()
    dimensions(cols, rows)
    binary = binary.resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError("--binary must name an executable tg build")
    font = font_path(font)
    output.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix=datetime.now().strftime("%Y%m%d-%H%M%S-"), dir=output)).resolve()
    os.chmod(run, 0o700)
    for directory in ("home", "home/config", "home/cache", "tmp", "frames"):
        (run / directory).mkdir(parents=True, exist_ok=True)
    shutil.copytree(SKILL / "assets/repository", run / "repository")
    with (run / "repository/.gitignore").open("a", encoding="utf-8") as rules:
        rules.write("generated/\n")
    env = isolated_environment(run)
    git = shutil.which("git")
    if not git:
        raise ValueError("git is required to prepare the fixture repository")
    for args in (["init", "--quiet"], ["add", "."]):
        subprocess.run([git, *args], cwd=run / "repository", env=env, check=True, capture_output=True)
    (run / "prompt.md").write_text("", encoding="utf-8")
    config = '[skills]\nprofile = "review-fixtures"\nread_codex_disable_rules = false\n'
    config += f'[providers.github]\nenabled = {str(provider_error).lower()}\ncommand = "/usr/bin/false"\n'
    config += '[providers.jira]\nenabled = false\n'
    (run / "config.toml").write_text(config, encoding="utf-8")
    with binary.open("rb") as executable:
        binary_hash = hashlib.file_digest(executable, "sha256").hexdigest()
    socket_dir = Path(tempfile.mkdtemp(prefix="tg-review-", dir="/tmp"))
    manifest = {
        "schema_version": 1, "created_at": datetime.now(timezone.utc).isoformat(),
        "binary": str(binary), "binary_sha256": binary_hash,
        "cols": cols, "rows": rows, "no_color": no_color, "provider_error": provider_error,
        "font": font, "font_sha256": hashlib.sha256(Path(font).read_bytes()).hexdigest(),
        "socket": str(socket_dir / "control.sock"),
        "fixture_sha256": hashlib.sha256(b"".join(
            str(path.relative_to(run / "repository")).encode() + path.read_bytes()
            for path in sorted((run / "repository").rglob("*"))
            if path.is_file() and ".git" not in path.relative_to(run / "repository").parts
        )).hexdigest(),
        "renderer": "pyte-0.8.2/pillow-11.3.0; fixed dark palette; synthesized bold/italic",
        "isolation": "Fixture cwd and clean environment, not an OS security sandbox",
    }
    write_json(run / "run.json", manifest)
    return run


class Session:
    def __init__(self, run):
        self.run = run
        self.manifest = json.loads((run / "run.json").read_text())
        self.frame = 0
        self.received = 0
        self.output = (run / "terminal.ansi").open("wb")
        self.master, slave = pty.openpty()
        self.capture = Capture(self.manifest["cols"], self.manifest["rows"], self.send)
        self.set_size(self.manifest["cols"], self.manifest["rows"])
        command = [self.manifest["binary"], "--root", str(run / "repository"),
                   "--config", str(run / "config.toml"), "--no-project-config", "../prompt.md"]
        if self.manifest["no_color"]:
            command.append("--no-color")
        try:
            self.process = subprocess.Popen(
                command, cwd=run / "repository", env=isolated_environment(run),
                stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
            )
        except BaseException:
            os.close(self.master)
            self.output.close()
            raise
        finally:
            os.close(slave)
        self.eof = False

    def send(self, data):
        view = memoryview(data)
        while view:
            sent = os.write(self.master, view)
            view = view[sent:]

    def set_size(self, cols, rows):
        dimensions(cols, rows)
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.capture.screen.resize(lines=rows, columns=cols)
        if hasattr(self, "process") and self.process.poll() is None:
            self.process.send_signal(signal.SIGWINCH)

    def pump(self, timeout=0.05):
        if self.eof:
            time.sleep(min(timeout, 0.02))
            return False
        if not select.select([self.master], [], [], timeout)[0]:
            return False
        try:
            data = os.read(self.master, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            data = b""
        if not data:
            self.eof = True
            return False
        self.received += len(data)
        if self.received > 20_000_000:
            raise ValueError("Terminal output exceeded 20 MB session limit")
        self.output.write(data)
        self.output.flush()
        self.capture.feed(data)
        return True

    def settle(self, contains=None, timeout=3):
        start = changed = time.monotonic()
        previous = None
        while time.monotonic() - start < timeout:
            self.pump()
            state = self.capture.snapshot()
            if state != previous:
                previous, changed = state, time.monotonic()
            matched = contains is None or contains in "\n".join(state["text"])
            if matched and time.monotonic() - changed >= 0.2 and time.monotonic() - start >= 0.25:
                return True
        return False

    def execute(self, request):
        op = request["op"]
        started = time.monotonic()
        if self.frame >= 200 and op != "stop":
            raise ValueError("Session action budget exhausted; stop and start a new run")
        if op in ("type", "press", "resize") and self.process.poll() is not None:
            raise ValueError("tg has exited; start a fresh session")
        if op == "type":
            text = request["text"]
            if len(text) > 4000 or any(not char.isprintable() for char in text):
                raise ValueError("type accepts up to 4000 printable characters; use press for control keys")
            for char in text:
                self.send(char.encode())
                self.pump(0.003)
        elif op == "press":
            keys = request["keys"]
            repeat = request.get("repeat", 1)
            if not 1 <= repeat <= 40 or not 1 <= len(keys) <= 40:
                raise ValueError("Use 1..40 keys and repetitions")
            encoded = [encode_key(key) for key in keys]
            for _ in range(repeat):
                for data in encoded:
                    self.send(data)
                    self.pump(0.03)
        elif op == "resize":
            self.set_size(request["cols"], request["rows"])
        elif op not in ("observe", "stop"):
            raise ValueError(f"Unknown operation: {op}")
        settled = self.settle(request.get("contains"), min(10, max(0.25, request.get("timeout", 3))))
        self.frame += 1
        snapshot = self.capture.snapshot()
        snapshot.update({"frame": self.frame, "request": request, "settled": settled,
                         "exit_code": self.process.poll(), "elapsed_ms": round((time.monotonic() - started) * 1000)})
        stem = self.run / "frames" / f"{self.frame:04d}"
        write_json(stem.with_suffix(".json"), snapshot)
        stem.with_suffix(".txt").write_text("\n".join(snapshot["text"]) + "\n", encoding="utf-8")
        render_png(snapshot, stem.with_suffix(".png"), self.manifest["font"])
        if self.capture.clipboard:
            write_json(self.run / "clipboard.json", self.capture.clipboard)
        event = {key: value for key, value in snapshot.items() if key not in ("cells", "text")}
        with (self.run / "actions.jsonl").open("a", encoding="utf-8") as log:
            log.write(json.dumps(event) + "\n")
        return {**event, "session": str(self.run), "screenshot": str(stem.with_suffix(".png")),
                "cells": str(stem.with_suffix(".json")), "text": "\n".join(snapshot["text"])}

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=2)
        os.close(self.master)
        self.output.close()
        write_json(self.run / "stopped.json", {"exit_code": self.process.returncode,
                                               "frames": self.frame, "output_bytes": self.received})


def rpc(run, request):
    manifest = json.loads((run / "run.json").read_text())
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(20)
        client.connect(manifest["socket"])
        client.sendall(json.dumps(request).encode() + b"\n")
        with client.makefile("rb") as stream:
            response = json.loads(stream.readline(2_000_000))
    if "error" in response:
        raise ValueError(response["error"])
    return response


def serve(run):
    session = Session(run)
    address = Path(session.manifest["socket"])
    started = last_request = time.monotonic()
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
            server.bind(str(address))
            os.chmod(address, 0o600)
            server.listen(1)
            while time.monotonic() - started < 1800 and time.monotonic() - last_request < 900:
                session.pump(0)
                if not select.select([server], [], [], 0.05)[0]:
                    continue
                connection, _ = server.accept()
                last_request = time.monotonic()
                request = {}
                with connection:
                    connection.settimeout(5)
                    try:
                        with connection.makefile("rb") as reader:
                            request = json.loads(reader.readline(65536))
                        response = session.execute(request)
                    except (OSError, ValueError, KeyError, TypeError) as error:
                        response = {"error": str(error)}
                    try:
                        connection.sendall(json.dumps(response).encode() + b"\n")
                    except OSError:
                        pass
                if request.get("op") == "stop":
                    break
    finally:
        session.close()
        address.unlink(missing_ok=True)
        address.parent.rmdir()


def start(args):
    run = create_run(args.binary, args.output, args.cols, args.rows, args.no_color, args.provider_error, args.font)
    with (run / "driver.log").open("wb") as log:
        server = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), "serve", str(run)],
            stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True,
        )
    address = Path(json.loads((run / "run.json").read_text())["socket"])
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if server.poll() is not None:
            raise ValueError(f"Driver failed; inspect {run / 'driver.log'}")
        if address.exists():
            return rpc(run, {"op": "observe"})
        time.sleep(0.05)
    server.terminate()
    server.wait(timeout=3)
    raise ValueError(f"Driver startup timed out; inspect {run / 'driver.log'}")


def validate_report(path):
    report = json.loads(path.read_text())
    if report.get("schema_version") != 1 or report.get("mode") not in ("calibration", "exploratory"):
        raise ValueError("Report requires schema_version 1 and calibration/exploratory mode")
    if not isinstance(report.get("summary"), str) or not report["summary"].strip():
        raise ValueError("Report requires a nonempty summary")
    for field in ("tasks", "matrix", "limitations", "findings"):
        if not isinstance(report.get(field), list):
            raise ValueError(f"Report requires a {field} list")
    if len(report["findings"]) > 5:
        raise ValueError("Report at most five substantive findings")
    seen = set()
    for finding in report["findings"]:
        for field in ("id", "title", "scope", "observed", "impact_hypothesis", "proposal"):
            if not isinstance(finding.get(field), str) or not finding[field].strip():
                raise ValueError(f"Finding requires a nonempty {field}")
        if finding["id"] in seen:
            raise ValueError("Duplicate finding ID")
        seen.add(finding["id"])
        for field, choices in {
            "category": ("correctness", "usability"),
            "severity": ("low", "medium", "high"),
            "confidence": ("low", "medium", "high"),
            "decision": ("unreviewed", "accepted", "rejected", "deferred"),
        }.items():
            if finding.get(field) not in choices:
                raise ValueError(f"Invalid finding {field}")
        for field in ("reproduction", "counterexamples", "acceptance_criteria", "evidence"):
            if not isinstance(finding.get(field), list) or not finding[field]:
                raise ValueError(f"Finding requires a nonempty {field} list")
        if finding["decision"] != "unreviewed" and not finding.get("decision_rationale"):
            raise ValueError("Reviewed decisions require the user's rationale")
        for evidence in finding["evidence"]:
            session = Path(evidence["session"])
            frame = evidence["frame"]
            if not session.is_absolute() or type(frame) is not int or frame < 1:
                raise ValueError("Evidence requires an absolute session path and positive frame ID")
            json.loads((session / "run.json").read_text())
            recorded = json.loads((session / "frames" / f"{frame:04d}.json").read_text())
            if recorded["frame"] != frame or not (session / "frames" / f"{frame:04d}.png").is_file():
                raise ValueError("Evidence does not match a captured frame and screenshot")
    return {"valid": True, "findings": len(seen), "report": str(path.resolve())}


def replay(args):
    original = json.loads((args.session / "run.json").read_text())
    options = argparse.Namespace(
        binary=args.binary or Path(original["binary"]), output=args.session.resolve().parent,
        cols=original["cols"], rows=original["rows"], no_color=original["no_color"],
        provider_error=original["provider_error"], font=original["font"],
    )
    result = start(options)
    run = Path(result["session"])
    records = []
    try:
        for line in (args.session / "actions.jsonl").read_text().splitlines():
            event = json.loads(line)
            if event["request"]["op"] == "stop":
                break
            result = rpc(run, event["request"])
            before = json.loads((args.session / "frames" / f"{event['frame']:04d}.json").read_text())
            after = json.loads(Path(result["cells"]).read_text())
            records.append({"original_frame": event["frame"], "replay_frame": result["frame"],
                            "settled": result["settled"], "same_cells": before["cells"] == after["cells"]})
            if not result["settled"]:
                break
    finally:
        rpc(run, {"op": "stop"})
    current = json.loads((run / "run.json").read_text())
    comparison = {"original_session": str(args.session.resolve()), "session": str(run), "frames": records,
                  "settled": all(record["settled"] for record in records),
                  "same_fixture": original["fixture_sha256"] == current["fixture_sha256"],
                  "same_saved_output": (args.session / "prompt.md").read_bytes() == (run / "prompt.md").read_bytes(),
                  "note": "Action replay is deterministic input, not a new agent usability judgment; inspect differences"}
    write_json(run / "replay.json", comparison)
    return comparison


def terminate(signum, frame):
    raise SystemExit(0)


def parser():
    result = argparse.ArgumentParser(description="Local-only, fixture-backed tg terminal review")
    commands = result.add_subparsers(dest="op", required=True)
    command = commands.add_parser("start")
    command.add_argument("--binary", type=Path, default=REPOSITORY / "target/release/tg")
    command.add_argument("--output", type=Path, default=REPOSITORY / "target/product-review")
    command.add_argument("--cols", type=int, default=120)
    command.add_argument("--rows", type=int, default=30)
    command.add_argument("--no-color", action="store_true")
    command.add_argument("--provider-error", action="store_true")
    command.add_argument("--font")
    for op in ("observe", "type", "press", "resize", "stop", "serve"):
        command = commands.add_parser(op)
        command.add_argument("session", type=Path)
        if op in ("observe", "type", "press", "resize"):
            command.add_argument("--contains")
            command.add_argument("--timeout", type=float, default=3)
        if op == "type":
            command.add_argument("text")
        elif op == "press":
            command.add_argument("keys", nargs="+")
            command.add_argument("--repeat", type=int, default=1)
        elif op == "resize":
            command.add_argument("cols", type=int)
            command.add_argument("rows", type=int)
    commands.add_parser("scenarios")
    command = commands.add_parser("validate-report")
    command.add_argument("path", type=Path)
    command = commands.add_parser("replay")
    command.add_argument("session", type=Path)
    command.add_argument("--binary", type=Path)
    return result


def main():
    args = parser().parse_args()
    local_only()
    if args.op == "serve":
        signal.signal(signal.SIGTERM, terminate)
        signal.signal(signal.SIGINT, terminate)
        serve(args.session.resolve())
        return
    if args.op == "scenarios":
        response = json.loads((SKILL / "assets/scenarios.json").read_text())
    elif args.op == "start":
        response = start(args)
    elif args.op == "validate-report":
        response = validate_report(args.path)
    elif args.op == "replay":
        response = replay(args)
    else:
        request = {key: value for key, value in vars(args).items() if key != "session"}
        response = rpc(args.session.resolve(), request)
    if "text" in response:
        response["text"] = "\n".join(line.rstrip() for line in response["text"].splitlines())
    print(json.dumps(response, indent=2, ensure_ascii=False))
    if response.get("settled") is False:
        raise SystemExit(2)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
