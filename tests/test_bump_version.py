#!/usr/bin/env python3

from __future__ import annotations

from pathlib import Path
import stat
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from scripts.bump_version import bump_manifest, next_patch, update_manifest


class BumpVersionTests(unittest.TestCase):
    def test_updates_only_the_package_version(self) -> None:
        manifest = """[package]
name = "tg"
version = "1.4.9" # release version

[dependencies]
other = { version = "1.4.9" }
"""
        updated, version = update_manifest(manifest)
        self.assertEqual(version, "1.4.10")
        self.assertIn('version = "1.4.10" # release version', updated)
        self.assertIn('other = { version = "1.4.9" }', updated)

    def test_rejects_non_release_semver(self) -> None:
        with self.assertRaisesRegex(ValueError, "major.minor.patch"):
            next_patch("1.2.3-beta.1")

    def test_requires_package_version(self) -> None:
        with self.assertRaisesRegex(ValueError, "package.version"):
            update_manifest('[package]\nname = "tg"\n')

    def test_atomic_update_preserves_file_mode(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[package]\nname = "tg"\nversion = "2.0.0"\n')
            manifest.chmod(0o640)
            self.assertEqual(bump_manifest(manifest), "2.0.1")
            self.assertIn('version = "2.0.1"', manifest.read_text())
            self.assertEqual(stat.S_IMODE(manifest.stat().st_mode), 0o640)


if __name__ == "__main__":
    unittest.main()
