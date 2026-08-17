#!/usr/bin/env python3
"""Isolated installer and optional shell-setup tests."""

import errno
import os
import pathlib
import pty
import select
import subprocess
import tempfile
import unittest


REPOSITORY = pathlib.Path(__file__).resolve().parents[1]
INSTALLER = REPOSITORY / "install.sh"


class InstallResult:
    def __init__(self, output: str, home: pathlib.Path, install_dir: pathlib.Path):
        self.output = output.replace("\r\n", "\n")
        self.home = home
        self.install_dir = install_dir

    def rc(self, shell: str) -> pathlib.Path:
        return self.home / (".zshrc" if shell.endswith("zsh") else ".bashrc")


class InstallerTests(unittest.TestCase):
    def run_installer(
        self,
        *,
        shell: str = "/bin/bash",
        response: str | None = "",
        editor: str | None = None,
        visual: str | None = None,
        rc_text: str | None = None,
        no_prompt: bool = False,
        tty: bool = True,
    ) -> InstallResult:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(temporary.name)
        home = root / "home"
        tools = root / "tools"
        install_dir = root / "install dir"
        home.mkdir()
        tools.mkdir()
        self.write_tool(
            tools / "uname",
            """#!/bin/sh
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; *) exit 1 ;; esac
""",
        )
        self.write_tool(
            tools / "curl",
            """#!/bin/sh
output=
while [ "$#" -gt 0 ]; do
  case "$1" in -o) output=$2; shift 2 ;; *) shift ;; esac
done
case "$output" in
  *.sha256) printf 'verified-checksum  asset\n' > "$output" ;;
  *) printf '#!/bin/sh\necho fake tg\n' > "$output" ;;
esac
""",
        )
        self.write_tool(
            tools / "sha256sum",
            """#!/bin/sh
printf 'verified-checksum  %s\n' "$1"
""",
        )
        rc_name = ".zshrc" if shell.endswith("zsh") else ".bashrc"
        rc = home / rc_name
        if rc_text is not None:
            rc.write_text(rc_text, encoding="utf-8")

        environment = os.environ.copy()
        environment.update(
            {
                "HOME": str(home),
                "PATH": f"{tools}:/usr/bin:/bin",
                "SHELL": shell,
                "TG_INSTALL_DIR": str(install_dir),
                "TG_REPOSITORY": "example/tg",
            }
        )
        environment.pop("EDITOR", None)
        environment.pop("VISUAL", None)
        environment.pop("TG_NO_EDITOR_PROMPT", None)
        if editor is not None:
            environment["EDITOR"] = editor
        if visual is not None:
            environment["VISUAL"] = visual
        if no_prompt:
            environment["TG_NO_EDITOR_PROMPT"] = "1"

        if tty:
            output = self.run_pty(environment, response or "")
        else:
            completed = subprocess.run(
                ["/bin/sh", str(INSTALLER)],
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=True,
            )
            output = completed.stdout
        self.assertTrue((install_dir / "tg").is_file())
        return InstallResult(output, home, install_dir)

    def run_pty(self, environment: dict[str, str], response: str) -> str:
        master, slave = pty.openpty()
        process = subprocess.Popen(
            ["/bin/sh", str(INSTALLER)],
            env=environment,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
        )
        os.close(slave)
        if response:
            os.write(master, response.encode())
        chunks: list[bytes] = []
        while True:
            readable, _, _ = select.select([master], [], [], 2)
            if readable:
                try:
                    chunk = os.read(master, 4096)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                chunks.append(chunk)
            if process.poll() is not None and not readable:
                break
        os.close(master)
        self.assertEqual(process.wait(timeout=2), 0)
        return b"".join(chunks).decode(errors="replace")

    @staticmethod
    def write_tool(path: pathlib.Path, source: str) -> None:
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)

    @staticmethod
    def expected_line(result: InstallResult, variable: str) -> str:
        return f'export {variable}="{result.install_dir.resolve()}/tg"'

    def test_default_enter_refuses_after_showing_exact_bash_destination_and_lines(self):
        result = self.run_installer(response="\n", rc_text="existing\n")
        editor = self.expected_line(result, "EDITOR")
        visual = self.expected_line(result, "VISUAL")
        self.assertIn(f"Optional editor setup destination: {result.home}/.bashrc", result.output)
        self.assertIn(f"  {editor}", result.output)
        self.assertIn(f"  {visual}", result.output)
        self.assertLess(result.output.index(editor), result.output.index("Configure tg as"))
        self.assertLess(result.output.index(visual), result.output.index("Configure tg as"))
        self.assertEqual(result.rc("bash").read_text(), "existing\n")

    def test_explicit_editor_visual_and_both_choices_append_only_selected_lines(self):
        for choice, expected in [("e\n", ["EDITOR"]), ("v\n", ["VISUAL"]), ("b\n", ["EDITOR", "VISUAL"])]:
            with self.subTest(choice=choice):
                result = self.run_installer(response=choice, rc_text="seed")
                content = result.rc("bash").read_text()
                self.assertTrue(content.startswith("seed\n"))
                for variable in ["EDITOR", "VISUAL"]:
                    self.assertEqual(
                        self.expected_line(result, variable) in content,
                        variable in expected,
                    )

    def test_each_independently_unset_variable_is_the_only_available_choice(self):
        editor_set = self.run_installer(editor="vim", response="v\n")
        self.assertNotIn("EDITOR=", editor_set.rc("bash").read_text())
        self.assertEqual(
            editor_set.rc("bash").read_text(),
            self.expected_line(editor_set, "VISUAL") + "\n",
        )
        visual_set = self.run_installer(visual="vim", response="e\n")
        self.assertEqual(
            visual_set.rc("bash").read_text(),
            self.expected_line(visual_set, "EDITOR") + "\n",
        )
        self.assertNotIn("VISUAL=", visual_set.rc("bash").read_text())

    def test_both_environment_variables_set_skip_without_mutation(self):
        result = self.run_installer(editor="vim", visual="vim", rc_text="keep\n")
        self.assertEqual(result.rc("bash").read_text(), "keep\n")
        self.assertNotIn("Optional editor setup", result.output)

    def test_existing_assignments_are_never_overwritten_or_duplicated(self):
        existing = "export EDITOR=/usr/bin/vim\nVISUAL=nano\n"
        result = self.run_installer(response="b\n", rc_text=existing)
        self.assertEqual(result.rc("bash").read_text(), existing)
        self.assertNotIn("Optional editor setup", result.output)

        one_existing = self.run_installer(response="v\n", rc_text="EDITOR=vim\n")
        content = one_existing.rc("bash").read_text()
        self.assertEqual(content.count("EDITOR="), 1)
        self.assertEqual(content.count("VISUAL="), 1)

    def test_zsh_uses_zshrc_and_bash_uses_bashrc(self):
        zsh = self.run_installer(shell="/bin/zsh", response="e\n")
        self.assertEqual(zsh.rc("zsh").read_text(), self.expected_line(zsh, "EDITOR") + "\n")
        self.assertFalse((zsh.home / ".bashrc").exists())
        bash = self.run_installer(shell="/usr/local/bin/bash", response="v\n")
        self.assertEqual(bash.rc("bash").read_text(), self.expected_line(bash, "VISUAL") + "\n")

    def test_unknown_shell_and_no_controlling_terminal_never_mutate(self):
        unknown = self.run_installer(shell="/bin/fish", response="b\n", rc_text="keep\n")
        self.assertEqual(unknown.rc("bash").read_text(), "keep\n")
        self.assertIn("unrecognized shell", unknown.output)
        self.assertIn("configure manually", unknown.output)
        self.assertIn(self.expected_line(unknown, "EDITOR"), unknown.output)

        noninteractive = self.run_installer(tty=False, rc_text="keep\n")
        self.assertEqual(noninteractive.rc("bash").read_text(), "keep\n")
        self.assertIn("no controlling terminal", noninteractive.output)
        self.assertIn(self.expected_line(noninteractive, "EDITOR"), noninteractive.output)

    def test_no_prompt_escape_hatch_never_waits_or_mutates(self):
        result = self.run_installer(no_prompt=True, tty=False, rc_text="keep\n")
        self.assertEqual(result.rc("bash").read_text(), "keep\n")
        self.assertNotIn("Optional editor setup", result.output)

    def test_unrecognized_choice_is_an_explicit_refusal_without_mutation(self):
        result = self.run_installer(response="x\n", rc_text="keep\n")
        self.assertEqual(result.rc("bash").read_text(), "keep\n")
        self.assertIn("editor setup declined", result.output)


if __name__ == "__main__":
    unittest.main()
