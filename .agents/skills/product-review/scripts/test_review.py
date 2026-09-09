import base64
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from PIL import Image

from review import REPOSITORY, SKILL, Session, create_run, dimensions, encode_key, isolated_environment, local_only, validate_report
from screen import Capture, font_path, render_png


class ScreenTests(unittest.TestCase):
    def test_cursor_rewrites_are_not_append_only(self):
        capture = Capture(20, 8)
        capture.feed(b"old\rnew\x1b[2;4Hhere")
        state = capture.snapshot()
        self.assertEqual(state["text"][0].strip(), "new")
        self.assertEqual(state["cells"][1][3]["data"], "h")
        self.assertEqual(state["cursor"]["x"], 7)

    def test_colors_selection_and_dim_are_preserved(self):
        capture = Capture(20, 8)
        capture.feed(b"\x1b[30;46mA\x1b[0;7mB\x1b[0;2;3mC\x1b[22mD\x1b[0mE")
        cells = capture.snapshot()["cells"][0]
        self.assertEqual(cells[0]["bg"], "cyan")
        self.assertTrue(cells[1]["reverse"])
        self.assertTrue(cells[2]["dim"])
        self.assertTrue(cells[2]["italics"])
        self.assertFalse(cells[3]["dim"])
        self.assertFalse(cells[4]["italics"])

    def test_true_color_does_not_become_dim(self):
        capture = Capture(20, 8)
        capture.feed(b"\x1b[38;2;2;22;3mA")
        cell = capture.snapshot()["cells"][0][0]
        self.assertEqual(cell["fg"], "021603")
        self.assertFalse(cell["dim"])

    def test_split_unicode_and_wide_cells(self):
        capture = Capture(20, 8)
        for byte in "é界e\u0301".encode():
            capture.feed(bytes([byte]))
        cells = capture.snapshot()["cells"][0]
        self.assertEqual([cell["data"] for cell in cells[:4]], ["é", "界", "", "é"])

    def test_osc52_is_captured_not_forwarded(self):
        capture = Capture(20, 8)
        data = b"before\x1b]52;c;" + base64.b64encode(b"fixture prompt") + b"\x07after"
        for byte in data:
            capture.feed(bytes([byte]))
        self.assertEqual(capture.clipboard, ["fixture prompt"])
        self.assertEqual(capture.snapshot()["text"][0].strip(), "beforeafter")

    def test_osc_string_terminator(self):
        capture = Capture(20, 8)
        capture.feed(b"\x1b]52;c;aGVsbG8=\x1b\\ok")
        self.assertEqual(capture.clipboard, ["hello"])
        self.assertTrue(capture.snapshot()["text"][0].startswith("ok"))

    def test_terminal_queries_are_answered(self):
        replies = []
        capture = Capture(20, 8, replies.append)
        capture.feed(b"abc\x1b[6n")
        self.assertEqual(replies, [b"\x1b[1;4R"])

    def test_png_matches_dimensions_and_selection(self):
        capture = Capture(30, 8)
        capture.feed(b"\x1b[30;46mselected")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "screen.png"
            render_png(capture.snapshot(), path, font_path())
            with Image.open(path) as image:
                self.assertEqual(image.height, 8 * 22)
                self.assertEqual(image.getpixel((0, 0)), (0, 205, 205))


class DriverTests(unittest.TestCase):
    def test_rejects_ci(self):
        for name in ("CI", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE", "TF_BUILD"):
            with self.assertRaisesRegex(ValueError, "local-only"):
                local_only({name: "true"})
        local_only({"CI": "false"})

    def test_clean_environment_does_not_inherit_credentials(self):
        environment = isolated_environment(Path("/fixture"))
        self.assertEqual(environment["HOME"], "/fixture/home")
        self.assertNotIn("TG_CONFIG", environment)
        self.assertNotIn("GITHUB_TOKEN", environment)
        self.assertNotIn("SSH_AUTH_SOCK", environment)

    def test_named_keys_and_control_keys(self):
        self.assertEqual(encode_key("Ctrl-p"), b"\x10")
        self.assertEqual(encode_key("Down"), b"\x1b[B")
        self.assertEqual(encode_key(":"), b":")
        with self.assertRaises(ValueError):
            encode_key("Ctrl-é")

    def test_dimensions_are_bounded(self):
        dimensions(120, 30)
        for size in ((0, 30), (5000, 30), (40, 2)):
            with self.assertRaises(ValueError):
                dimensions(*size)

    def test_cli_refuses_ci(self):
        result = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("review.py")), "scenarios"],
            env={**os.environ, "CI": "true"}, capture_output=True, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("local-only", result.stderr)


class IntegrationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        binary = Path(os.environ.get("TG_REVIEW_BINARY", REPOSITORY / "target/release/tg"))
        self.run = create_run(binary, Path(self.directory.name), 100, 24)
        self.session = Session(self.run)
        self.addCleanup(self.session.close)
        socket_dir = Path(self.session.manifest["socket"]).parent
        self.addCleanup(socket_dir.rmdir)
        self.act("observe", contains="NORMAL")

    def act(self, op, **args):
        result = self.session.execute({"op": op, **args})
        self.assertTrue(result["settled"], result["text"])
        return result

    def test_real_keyboard_completion_copy_save_and_resize(self):
        self.act("press", keys=["i"])
        self.act("type", text="Inspect @src/orders.rs", contains="Files")
        self.act("press", keys=["Tab"], contains="tokens")
        self.act("type", text="::validate_order", contains="Symbols")
        self.act("press", keys=["Tab"])
        self.act("press", keys=["Escape"])
        resized = self.act("resize", cols=80, rows=24)
        self.assertEqual(resized["cols"], 80)
        self.act("type", text=":copy")
        self.act("press", keys=["Enter"])
        self.act("type", text=":w")
        self.act("press", keys=["Enter"], contains="NORMAL")
        saved = (self.run / "prompt.md").read_text()
        clipboard = json.loads((self.run / "clipboard.json").read_text())
        self.assertEqual(clipboard[-1], saved)
        self.assertIn("orders.rs", saved)
        self.assertNotIn("legacy", saved)
        self.assertTrue(saved.startswith("Inspect "))
        self.assertTrue(list((self.run / "frames").glob("*.png")))
        self.act("type", text=":q")
        self.act("press", keys=["Enter"])
        self.session.process.wait(timeout=3)
        self.assertEqual(self.session.process.returncode, 0)

    def test_reports_allow_zero_findings_and_validate_evidence(self):
        report = json.loads((SKILL / "assets/report-template.json").read_text())
        path = self.run / "report.json"
        path.write_text(json.dumps(report))
        self.assertEqual(validate_report(path)["findings"], 0)
        finding = report.pop("finding_shape")
        finding["evidence"] = [{"session": str(self.run), "frame": 1}]
        report["findings"] = [finding]
        path.write_text(json.dumps(report))
        self.assertEqual(validate_report(path)["findings"], 1)
        finding["evidence"][0]["frame"] = 999
        path.write_text(json.dumps(report))
        with self.assertRaises(OSError):
            validate_report(path)
        finding["evidence"][0]["frame"] = 1
        finding["decision"] = "accepted"
        path.write_text(json.dumps(report))
        with self.assertRaisesRegex(ValueError, "rationale"):
            validate_report(path)

    def test_fixture_ignored_file_exists_but_is_not_git_visible(self):
        self.assertTrue((self.run / "repository/generated/status.txt").is_file())
        tracked = subprocess.check_output(
            ["git", "ls-files"], cwd=self.run / "repository", text=True,
        )
        self.assertNotIn("generated/status.txt", tracked)
        self.assertIn("src/orders.rs", tracked)

    def test_unmet_wait_retains_evidence(self):
        result = self.session.execute({"op": "observe", "contains": "impossible-result", "timeout": 0.25})
        self.assertFalse(result["settled"])
        self.assertTrue(Path(result["screenshot"]).is_file())

    def test_invalid_input_is_rejected_before_sending(self):
        with self.assertRaises(ValueError):
            self.session.execute({"op": "type", "text": "bad\x1b"})
        with self.assertRaises(ValueError):
            self.session.execute({"op": "press", "keys": ["i", "unsupported"]})
        self.assertIn("NORMAL", self.act("observe")["text"])


if __name__ == "__main__":
    local_only()
    unittest.main()
