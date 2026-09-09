#!/usr/bin/env python3
"""Isolated installer and optional shell-setup tests."""

import errno
import os
import pathlib
import pty
import select
import shutil
import subprocess
import tempfile
import time
import unittest


REPOSITORY = pathlib.Path(__file__).resolve().parents[1]
INSTALLER = REPOSITORY / "install.sh"


class InstallResult:
    def __init__(
        self,
        output: str,
        root: pathlib.Path,
        install_dir: pathlib.Path,
        environment: dict[str, str],
        returncode: int,
    ):
        self.output = output.replace("\r\n", "\n")
        self.root = root
        self.home = pathlib.Path(environment["HOME"])
        self.install_dir = install_dir
        self.environment = environment
        self.returncode = returncode

    def rc(self, shell: str) -> pathlib.Path:
        if shell.endswith("zsh"):
            return pathlib.Path(self.environment.get("ZDOTDIR", str(self.home))) / ".zshrc"
        return self.home / ".bashrc"

    @property
    def calls(self) -> list[str]:
        path = pathlib.Path(self.environment["TG_TEST_CALLS"])
        return path.read_text().splitlines() if path.exists() else []


class InstallerTests(unittest.TestCase):
    def run_installer(
        self,
        *,
        shell: str = "/bin/bash",
        response: str | None = "\n",
        editor: str | None = None,
        visual: str | None = None,
        rc_text: str | None = None,
        no_prompt: bool = False,
        tty: bool = True,
        missing_commands: tuple[str, ...] = (),
        checksum_mismatch: bool = False,
        checksum_status: int = 0,
        version_status: int = 0,
        existing_binary: bytes | None = None,
        success: bool = True,
        piped: bool = False,
        stdin_text: str | None = None,
        relative_install_dir: bool = False,
        install_name: str = "install dir",
        zdotdir: bool = False,
        install_on_path: bool = False,
        destination_file: bool = False,
    ) -> InstallResult:
        temporary = tempfile.TemporaryDirectory(prefix="tg-installer-test-")
        self.addCleanup(temporary.cleanup)
        root = pathlib.Path(temporary.name).resolve()
        home = root / "home"
        tools = root / "tools"
        install_dir = root / install_name
        home.mkdir()
        tools.mkdir()
        (root / "tmp").mkdir()
        for name in ("awk", "mktemp", "chmod", "mv", "mkdir", "rm", "sed", "grep", "tail", "od", "tr"):
            if name not in missing_commands:
                executable = shutil.which(name, path="/usr/bin:/bin")
                self.assertIsNotNone(executable, f"fixture requires system utility {name}")
                (tools / name).symlink_to(executable)
        fake_tools = {
            "uname": """#!/bin/sh
printf 'uname:%s\n' "$*" >> "$TG_TEST_CALLS"
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; *) exit 1 ;; esac
""",
            "curl": """#!/bin/sh
printf 'curl:%s\n' "$*" >> "$TG_TEST_CALLS"
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) output=$2; shift 2 ;;
    -fsSL) shift ;;
    https://*) url=$1; shift ;;
    *) exit 90 ;;
  esac
done
case "$url" in
  https://github.com/example/tg/releases/latest/download/tg-x86_64-unknown-linux-gnu.sha256)
    printf '%s  asset\n' "$TG_TEST_CHECKSUM" > "$output" ;;
  https://github.com/example/tg/releases/latest/download/tg-x86_64-unknown-linux-gnu)
    /bin/cat "$TG_TEST_CANDIDATE" > "$output" ;;
  *) exit 91 ;;
esac
""",
        }
        checksum_source = """#!/bin/sh
printf 'checksum:%s:%s\n' "${0##*/}" "$*" >> "$TG_TEST_CALLS"
if [ "${0##*/}" = shasum ]; then
  [ "$1" = -a ] && [ "$2" = 256 ] || exit 92
  shift 2
fi
[ "$#" -eq 1 ] && [ -f "$1" ] || exit 93
printf 'verified-checksum  %s\n' "$1"
exit "$TG_TEST_CHECKSUM_STATUS"
"""
        fake_tools["sha256sum"] = checksum_source
        fake_tools["shasum"] = checksum_source
        for name, source in fake_tools.items():
            if name not in missing_commands:
                self.write_tool(tools / name, source)
        candidate = root / "candidate"
        self.write_tool(
            candidate,
            """#!/bin/sh
printf 'tg:%s\n' "$*" >> "$TG_TEST_CALLS"
printf '%s\n' "$PATH" > "$TG_TEST_CANDIDATE_PATH"
[ "$#" -eq 1 ] && [ "$1" = --version ] || exit 94
if IFS= read -r unexpected_input; then
  printf 'consumed-stdin:%s\n' "$unexpected_input" >> "$TG_TEST_CALLS"
  exit 95
fi
printf 'tscodeselection fixture\n'
exit "$TG_TEST_VERSION_STATUS"
""",
        )
        if existing_binary is not None:
            install_dir.mkdir()
            (install_dir / "tg").write_bytes(existing_binary)
            (install_dir / "tg").chmod(0o755)
        if destination_file:
            install_dir.write_text("not a directory\n")
        environment = {
            "HOME": str(home),
            "PATH": f"{install_dir}:{tools}" if install_on_path else str(tools),
            "SHELL": shell,
            "TERM": "xterm-256color",
            "LC_ALL": "C",
            "TMPDIR": str(root / "tmp"),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "TG_INSTALL_DIR": install_name if relative_install_dir else str(install_dir),
            "TG_REPOSITORY": "example/tg",
            "TG_TEST_CALLS": str(root / "calls"),
            "TG_TEST_CANDIDATE": str(candidate),
            "TG_TEST_CANDIDATE_PATH": str(root / "candidate-path"),
            "TG_TEST_CHECKSUM": "wrong-checksum" if checksum_mismatch else "verified-checksum",
            "TG_TEST_CHECKSUM_STATUS": str(checksum_status),
            "TG_TEST_VERSION_STATUS": str(version_status),
        }
        if zdotdir:
            startup_dir = home / "custom zsh"
            startup_dir.mkdir()
            environment["ZDOTDIR"] = str(startup_dir)
        rc_name = ".zshrc" if shell.endswith("zsh") else ".bashrc"
        rc_parent = pathlib.Path(environment.get("ZDOTDIR", str(home))) if shell.endswith("zsh") else home
        if rc_text is not None:
            (rc_parent / rc_name).write_text(rc_text, encoding="utf-8")
        if editor is not None:
            environment["EDITOR"] = editor
        if visual is not None:
            environment["VISUAL"] = visual
        if no_prompt:
            environment["TG_NO_EDITOR_PROMPT"] = "1"
        return self.invoke_installer(
            environment,
            root,
            install_dir,
            response=response or "",
            tty=tty,
            success=success,
            piped=piped,
            stdin_text=stdin_text,
        )

    def invoke_installer(
        self,
        environment: dict[str, str],
        root: pathlib.Path,
        install_dir: pathlib.Path,
        *,
        response: str = "\n",
        tty: bool = False,
        success: bool = True,
        piped: bool = False,
        stdin_text: str | None = None,
    ) -> InstallResult:
        if tty:
            self.assertFalse(piped)
            self.assertIsNone(stdin_text)
            output, returncode = self.run_pty(environment, root, response)
        else:
            argv = ["/bin/sh", str(INSTALLER)]
            input_text = stdin_text
            if piped:
                argv = ["/bin/sh"]
                input_text = INSTALLER.read_text() + "\nprintf 'pipeline-tail-preserved\\n'\n"
            elif stdin_text is not None:
                argv = [
                    "/bin/sh",
                    "-c",
                    '/bin/sh "$1" || exit $?; IFS= read -r remaining; printf "remaining:%s\\n" "$remaining"',
                    "installer-test",
                    str(INSTALLER),
                ]
            completed = subprocess.run(
                argv,
                env=environment,
                cwd=root,
                input=input_text if input_text is not None else "",
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                timeout=10,
                check=False,
            )
            output, returncode = completed.stdout, completed.returncode
        if success:
            self.assertEqual(returncode, 0, output)
            self.assertTrue((install_dir / "tg").is_file(), output)
        else:
            self.assertNotEqual(returncode, 0, output)
        return InstallResult(output, root, install_dir, environment, returncode)

    def run_pty(
        self, environment: dict[str, str], root: pathlib.Path, response: str
    ) -> tuple[str, int]:
        master, slave = pty.openpty()
        process = subprocess.Popen(
            ["/bin/sh", str(INSTALLER)],
            env=environment,
            cwd=root,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
        )
        os.close(slave)
        chunks: list[bytes] = []
        try:
            if response:
                os.write(master, response.encode())
            deadline = time.monotonic() + 10
            while True:
                remaining = deadline - time.monotonic()
                self.assertGreater(remaining, 0, b"".join(chunks).decode(errors="replace"))
                readable, _, _ = select.select([master], [], [], min(remaining, 0.2))
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
            returncode = process.wait(timeout=2)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=2)
            os.close(master)
        return b"".join(chunks).decode(errors="replace"), returncode

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
        for shell in ("/bin/zsh", "zsh"):
            with self.subTest(shell=shell):
                zsh = self.run_installer(shell=shell, response="e\n")
                self.assertEqual(zsh.rc("zsh").read_text(), self.expected_line(zsh, "EDITOR") + "\n")
                self.assertFalse((zsh.home / ".bashrc").exists())
        for shell in ("/usr/local/bin/bash", "bash"):
            with self.subTest(shell=shell):
                bash = self.run_installer(shell=shell, response="v\n")
                self.assertEqual(bash.rc("bash").read_text(), self.expected_line(bash, "VISUAL") + "\n")

    def test_unknown_shell_and_no_controlling_terminal_never_mutate(self):
        unknown = self.run_installer(shell="/bin/tcsh", response="b\n", rc_text="keep\n")
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

    def test_fish_guidance_uses_native_syntax_without_changing_startup_files(self):
        for shell in ("/bin/fish", "fish"):
            with self.subTest(shell=shell):
                result = self.run_installer(shell=shell, response="b\n", rc_text="keep\n")
                self.assertIn("automatic editor setup skipped for fish", result.output)
                self.assertIn(f'set -gx PATH "{result.install_dir}" $PATH', result.output)
                self.assertIn(f'set -gx EDITOR "{result.install_dir}/tg"', result.output)
                self.assertIn(f'set -gx VISUAL "{result.install_dir}/tg"', result.output)
                self.assertIn(f"{result.home}/.config/fish/config.fish", result.output)
                self.assertNotIn("export EDITOR=", result.output)
                self.assertNotIn("Configure tg as", result.output)
                self.assertEqual(result.rc("bash").read_text(), "keep\n")
                self.assertFalse((result.home / ".config").exists())

    def test_fish_manual_guidance_preserves_existing_environment_values(self):
        result = self.run_installer(shell="fish", editor="vim", visual="nano", tty=False)
        self.assertNotIn("set -gx EDITOR", result.output)
        self.assertNotIn("set -gx VISUAL", result.output)
        self.assertEqual(list(result.home.iterdir()), [])

    def test_absolute_quoted_onboarding_handoff_is_local_and_does_not_edit_path(self):
        result = self.run_installer(
            tty=False,
            no_prompt=True,
            rc_text="keep\n",
            relative_install_dir=True,
            install_name='install "quoted" $dollar `tick` \\slash',
        )
        for command in ("setup", "doctor"):
            line = next(
                line.strip()
                for line in result.output.splitlines()
                if line.startswith('  "') and line.endswith(f" {command}")
            )
            parsed = subprocess.run(
                ["/bin/sh", "-c", f"set -- {line}; printf '%s\\n' \"$@\""],
                env=result.environment,
                cwd=result.root,
                capture_output=True,
                text=True,
                timeout=10,
                check=True,
            )
            arguments = parsed.stdout.splitlines()
            self.assertEqual(arguments, [str(result.install_dir / "tg"), command])
            self.assertTrue(pathlib.Path(arguments[0]).is_absolute())
        self.assertIn("doctor reports local readiness without network requests", result.output)
        self.assertIn("Remote checks are opt-in", result.output)
        self.assertIn("doctor --check jira (or --check all)", result.output)
        self.assertIn(":providers", result.output)
        self.assertIn("Space then p", result.output)
        self.assertEqual([call for call in result.calls if call.startswith("tg:")], ["tg:--version"])
        self.assertEqual(len([call for call in result.calls if call.startswith("curl:")]), 2)
        self.assertLess(
            next(index for index, call in enumerate(result.calls) if call.startswith("checksum:")),
            result.calls.index("tg:--version"),
        )
        self.assertEqual(
            pathlib.Path(result.environment["TG_TEST_CANDIDATE_PATH"]).read_text(),
            result.environment["PATH"] + "\n",
        )
        self.assertEqual(result.rc("bash").read_text(), "keep\n")
        self.assertEqual(list(result.home.iterdir()), [result.rc("bash")])
        self.assertEqual(list(result.install_dir.glob(".tg-install.*")), [])

    def test_posix_path_guidance_is_advice_only(self):
        for shell, startup in (
            ("bash", ".bashrc"),
            ("zsh", ".zshrc"),
            ("/bin/sh", ".profile"),
            ("/bin/dash", ".profile"),
            ("/bin/ksh", ".profile"),
        ):
            with self.subTest(shell=shell):
                result = self.run_installer(shell=shell, tty=False, no_prompt=True)
                self.assertIn(f'export PATH="{result.install_dir}":"$PATH"', result.output)
                self.assertIn(str(result.home / startup), result.output)
                self.assertEqual(list(result.home.iterdir()), [])

    def test_unknown_shell_path_guidance_has_a_manual_fallback(self):
        for shell in ("/bin/tcsh", ""):
            with self.subTest(shell=shell):
                result = self.run_installer(shell=shell, tty=False, no_prompt=True)
                self.assertIn("Unrecognized shell", result.output)
                self.assertIn("shell's PATH configuration", result.output)
                self.assertIn("commands below use sh quoting", result.output)
                self.assertNotIn("export PATH=", result.output)
                self.assertEqual(list(result.home.iterdir()), [])

    def test_existing_path_entry_needs_no_path_guidance_but_keeps_next_steps(self):
        result = self.run_installer(tty=False, no_prompt=True, install_on_path=True)
        self.assertNotIn("not on PATH", result.output)
        self.assertNotIn("export PATH=", result.output)
        self.assertIn(f'"{result.install_dir}/tg" setup', result.output)
        self.assertIn(f'"{result.install_dir}/tg" doctor', result.output)
        self.assertEqual(list(result.home.iterdir()), [])

    def test_required_dependency_preflight_fails_before_any_command_or_download(self):
        missing_sets = [(name,) for name in ("curl", "uname", "awk", "mktemp", "chmod", "mv", "mkdir", "rm", "sed")]
        missing_sets.append(("sha256sum", "shasum"))
        for missing in missing_sets:
            with self.subTest(missing=missing):
                result = self.run_installer(tty=False, missing_commands=missing, success=False)
                expected = "sha256sum-or-shasum" if len(missing) == 2 else missing[0]
                self.assertIn(f"missing required commands: {expected}", result.output)
                self.assertIn("system package manager", result.output)
                self.assertEqual(result.calls, [])
                self.assertFalse(result.install_dir.exists())
                self.assertEqual(list(result.home.iterdir()), [])

    def test_missing_dependencies_are_reported_together(self):
        result = self.run_installer(
            tty=False,
            missing_commands=("curl", "awk", "sha256sum", "shasum"),
            success=False,
        )
        self.assertIn("missing required commands: curl awk sha256sum-or-shasum", result.output)
        self.assertEqual(result.calls, [])

    def test_shasum_fallback_uses_sha256_and_still_smokes_the_candidate(self):
        result = self.run_installer(tty=False, no_prompt=True, missing_commands=("sha256sum",))
        self.assertTrue(any(call.startswith("checksum:shasum:-a 256 ") for call in result.calls))
        self.assertFalse(any(call.startswith("checksum:sha256sum:") for call in result.calls))
        self.assertEqual([call for call in result.calls if call.startswith("tg:")], ["tg:--version"])

    def test_missing_optional_editor_tools_skip_without_mutation(self):
        for command in ("grep", "tail", "od", "tr"):
            with self.subTest(command=command):
                result = self.run_installer(missing_commands=(command,), rc_text="keep\n")
                self.assertIn(f"automatic editor setup skipped: missing {command}", result.output)
                self.assertNotIn("Configure tg as", result.output)
                self.assertEqual(result.rc("bash").read_text(), "keep\n")
                self.assertIn(f'"{result.install_dir}/tg" setup', result.output)

    def test_version_failure_preserves_existing_binary_and_cleans_staging(self):
        original = b"original installed binary\n"
        result = self.run_installer(
            tty=False,
            version_status=23,
            existing_binary=original,
            rc_text="keep\n",
            success=False,
        )
        self.assertIn("failed its --version check", result.output)
        self.assertIn("installed tg was not changed", result.output)
        self.assertEqual((result.install_dir / "tg").read_bytes(), original)
        self.assertEqual((result.install_dir / "tg").stat().st_mode & 0o777, 0o755)
        self.assertEqual(result.rc("bash").read_text(), "keep\n")
        self.assertEqual([call for call in result.calls if call.startswith("tg:")], ["tg:--version"])
        self.assertEqual(list(result.install_dir.glob(".tg-install.*")), [])
        self.assertNotIn("Installed tg to", result.output)
        self.assertNotIn("Next steps", result.output)

    def test_checksum_mismatch_preserves_existing_binary_without_executing_candidate(self):
        original = b"original installed binary\n"
        result = self.run_installer(
            tty=False,
            checksum_mismatch=True,
            existing_binary=original,
            rc_text="keep\n",
            success=False,
        )
        self.assertIn("checksum mismatch", result.output)
        self.assertEqual((result.install_dir / "tg").read_bytes(), original)
        self.assertEqual(result.rc("bash").read_text(), "keep\n")
        self.assertFalse(any(call.startswith("tg:") for call in result.calls))
        self.assertEqual(list(result.install_dir.glob(".tg-install.*")), [])
        self.assertNotIn("Installed tg to", result.output)

    def test_checksum_command_failure_is_not_masked_by_valid_stdout(self):
        for missing, tool in (((), "sha256sum"), (("sha256sum",), "shasum")):
            with self.subTest(tool=tool):
                original = b"original installed binary\n"
                result = self.run_installer(
                    tty=False,
                    missing_commands=missing,
                    checksum_status=24,
                    existing_binary=original,
                    success=False,
                )
                self.assertIn(f"{tool} failed", result.output)
                self.assertEqual((result.install_dir / "tg").read_bytes(), original)
                self.assertFalse(any(call.startswith("tg:") for call in result.calls))
                self.assertEqual(list(result.install_dir.glob(".tg-install.*")), [])

    def test_invalid_destination_is_diagnosed_before_download(self):
        result = self.run_installer(tty=False, destination_file=True, success=False)
        self.assertIn("cannot create install directory", result.output)
        self.assertIn("TG_INSTALL_DIR", result.output)
        self.assertEqual(result.install_dir.read_text(), "not a directory\n")
        self.assertEqual(result.calls, ["uname:-s", "uname:-m"])
        self.assertEqual(list(result.home.iterdir()), [])

    def test_piped_install_preserves_script_tail_and_never_prompts(self):
        result = self.run_installer(tty=False, piped=True, rc_text="keep\n")
        self.assertIn("pipeline-tail-preserved", result.output)
        self.assertIn("no controlling terminal", result.output)
        self.assertNotIn("Configure tg as", result.output)
        self.assertIn(f'"{result.install_dir}/tg" setup', result.output)
        self.assertIn(f'"{result.install_dir}/tg" doctor', result.output)
        self.assertEqual(result.rc("bash").read_text(), "keep\n")
        self.assertEqual([call for call in result.calls if call.startswith("tg:")], ["tg:--version"])
        self.assertFalse(any(call.startswith("consumed-stdin:") for call in result.calls))

    def test_candidate_smoke_and_noninteractive_editor_setup_leave_stdin_unread(self):
        result = self.run_installer(tty=False, stdin_text="b\n", rc_text="keep\n")
        self.assertIn("remaining:b", result.output)
        self.assertNotIn("Configure tg as", result.output)
        self.assertFalse(any(call.startswith("consumed-stdin:") for call in result.calls))
        self.assertEqual(result.rc("bash").read_text(), "keep\n")

    def test_repeated_editor_setup_is_idempotent(self):
        for shell in ("bash", "zsh"):
            with self.subTest(shell=shell):
                first = self.run_installer(shell=shell, response="b\n", rc_text="seed")
                original = first.rc(shell).read_text()
                self.assertEqual(original.count("EDITOR="), 1)
                self.assertEqual(original.count("VISUAL="), 1)
                second = self.invoke_installer(
                    first.environment,
                    first.root,
                    first.install_dir,
                    tty=True,
                    response="b\n",
                )
                self.assertEqual(second.rc(shell).read_text(), original)
                self.assertNotIn("Optional editor setup", second.output)
                self.assertNotIn("Configure tg as", second.output)
                self.assertNotIn("export PATH=", original)
                self.assertEqual([call for call in second.calls if call.startswith("tg:")], ["tg:--version"] * 2)

    def test_zdotdir_controls_both_editor_destination_and_path_guidance(self):
        for shell in ("/bin/zsh", "zsh"):
            with self.subTest(shell=shell):
                result = self.run_installer(shell=shell, zdotdir=True, response="b\n", rc_text="seed\n")
                rc = result.rc("zsh")
                self.assertIn(f"Optional editor setup destination: {rc}", result.output)
                self.assertIn(f"To persist, add that line to {rc}.", result.output)
                self.assertEqual(
                    rc.read_text(),
                    "seed\n" + self.expected_line(result, "EDITOR") + "\n" + self.expected_line(result, "VISUAL") + "\n",
                )
                self.assertFalse((result.home / ".zshrc").exists())
                self.assertFalse((result.home / ".bashrc").exists())

    def test_explicitly_empty_editor_variables_are_not_overwritten(self):
        result = self.run_installer(editor="", visual="", rc_text="keep\n")
        self.assertEqual(result.rc("bash").read_text(), "keep\n")
        self.assertNotIn("Optional editor setup", result.output)

    def test_fixture_environment_contains_only_allowlisted_values(self):
        result = self.run_installer(tty=False)
        self.assertEqual(
            set(result.environment),
            {
                "HOME", "PATH", "SHELL", "TERM", "LC_ALL", "TMPDIR", "XDG_CONFIG_HOME",
                "TG_INSTALL_DIR", "TG_REPOSITORY", "TG_TEST_CALLS", "TG_TEST_CANDIDATE",
                "TG_TEST_CANDIDATE_PATH", "TG_TEST_CHECKSUM", "TG_TEST_CHECKSUM_STATUS",
                "TG_TEST_VERSION_STATUS",
            },
        )
        self.assertEqual(result.environment["PATH"], str(result.root / "tools"))
        self.assertEqual(result.environment["HOME"], str(result.root / "home"))


if __name__ == "__main__":
    unittest.main()
