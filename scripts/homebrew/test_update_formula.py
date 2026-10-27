import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from update_formula import TARGETS, update_formula


class FormulaPublicationTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name).resolve()
        self.addCleanup(os.chdir, Path.cwd())
        os.chdir(self.root)
        self.archives = self.root / "release-archives"
        self.archives.mkdir()
        self.formula = self.root / "homebrew-tap/Formula/meshfox.rb"
        for target in TARGETS:
            (self.archives / f"meshfox-{target}.tar.gz").write_bytes(target.encode())

    def test_new_release_has_all_archives_checksums_and_install_marker(self):
        self.assertTrue(update_formula("v0.27.0", self.archives, self.formula))
        content = self.formula.read_text()
        self.assertNotIn("@", content)
        for target in TARGETS:
            self.assertIn(f"/download/v0.27.0/meshfox-{target}.tar.gz", content)
            self.assertIn(hashlib.sha256(target.encode()).hexdigest(), content)
        self.assertIn('(share/"meshfox/install-method").write "homebrew\\n"', content)
        self.assertIn("check-updates --yes", content)

    def test_retry_is_a_noop_and_older_release_cannot_downgrade_tap(self):
        update_formula("v0.27.10", self.archives, self.formula)
        before = self.formula.read_bytes()
        self.assertFalse(update_formula("v0.27.10", self.archives, self.formula))
        # Numeric comparison: 10 > 9, despite lexical ordering.
        self.assertFalse(update_formula("v0.27.9", self.archives / "missing", self.formula))
        self.assertEqual(before, self.formula.read_bytes())

    def test_missing_archive_does_not_change_existing_formula(self):
        update_formula("v0.26.1", self.archives, self.formula)
        before = self.formula.read_bytes()
        (self.archives / f"meshfox-{TARGETS[-1]}.tar.gz").unlink()
        with self.assertRaises(FileNotFoundError):
            update_formula("v0.27.0", self.archives, self.formula)
        self.assertEqual(before, self.formula.read_bytes())

    def test_prerelease_or_malformed_tag_cannot_change_formula(self):
        for tag in ["v1.0.0-rc.1", "v1.0.0+build", "1.0.0", "v01.0.0", "v1.0"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                update_formula(tag, self.archives, self.formula)
        self.assertFalse(self.formula.exists())

    def test_unrecognized_existing_formula_is_not_overwritten(self):
        self.formula.parent.mkdir(parents=True)
        self.formula.write_text("unexpected format\n")
        with self.assertRaises(ValueError):
            update_formula("v0.27.0", self.archives, self.formula)
        self.assertEqual("unexpected format\n", self.formula.read_text())

    def test_actual_workflow_cli_arguments_and_absolute_paths_work(self):
        script = Path(__file__).resolve().with_name("update_formula.py")
        output = subprocess.run(
            [sys.executable, str(script), "v0.27.0", "release-archives", "homebrew-tap/Formula/meshfox.rb"],
            cwd=self.root, capture_output=True, text=True, check=True,
        )
        self.assertIn("Generated", output.stdout)
        self.assertFalse(update_formula("v0.27.0", self.archives, self.formula))

    def test_arbitrary_output_paths_and_wrong_formula_names_are_rejected(self):
        for path in [
            self.root / "outside/meshfox.rb",
            self.root / "homebrew-tap/../outside/meshfox.rb",
            self.root / "homebrew-tap/Formula/other.rb",
            self.root / "homebrew-tap/other/meshfox.rb",
        ]:
            with self.subTest(path=path), self.assertRaises(ValueError):
                update_formula("v0.27.0", self.archives, path)
            self.assertFalse(path.exists())

    def test_archive_directory_outside_download_root_is_rejected(self):
        for path in [self.root, self.archives / "..", self.root / "release-archives-sibling"]:
            with self.subTest(path=path), self.assertRaises(ValueError):
                update_formula("v0.27.0", path, self.formula)
        self.assertFalse(self.formula.exists())

    def test_formula_file_and_parent_symlinks_cannot_escape_tap(self):
        outside = self.root / "outside"
        outside.mkdir()
        victim = outside / "meshfox.rb"
        victim.write_text("do not overwrite\n")
        self.formula.parent.mkdir(parents=True)
        self.formula.symlink_to(victim)
        with self.assertRaises(ValueError):
            update_formula("v0.27.0", self.archives, self.formula)
        self.formula.unlink()
        self.formula.parent.rmdir()
        self.formula.parent.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError):
            update_formula("v0.27.0", self.archives, self.formula)
        self.assertEqual("do not overwrite\n", victim.read_text())

    def test_archive_symlink_cannot_read_outside_download_directory(self):
        outside = self.root / "secret"
        outside.write_text("not a release archive\n")
        archive = self.archives / f"meshfox-{TARGETS[0]}.tar.gz"
        archive.unlink()
        archive.symlink_to(outside)
        with self.assertRaises(ValueError):
            update_formula("v0.27.0", self.archives, self.formula)
        self.assertFalse(self.formula.exists())

    def test_expected_roots_cannot_be_symlinks_outside_workspace(self):
        with tempfile.TemporaryDirectory() as outside:
            for root in [self.archives, self.root / "homebrew-tap"]:
                with self.subTest(root=root):
                    if root == self.archives:
                        for archive in root.iterdir():
                            archive.unlink()
                        root.rmdir()
                    root.symlink_to(outside, target_is_directory=True)
                    with self.assertRaises(ValueError):
                        update_formula("v0.27.0", self.archives, self.formula)
                    root.unlink()
                    root.mkdir()


if __name__ == "__main__":
    unittest.main()
