import hashlib
from pathlib import Path
import tempfile
import unittest

from update_formula import TARGETS, update_formula


class FormulaPublicationTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.formula = self.root / "tap/Formula/meshfox.rb"
        for target in TARGETS:
            (self.root / f"meshfox-{target}.tar.gz").write_bytes(target.encode())

    def test_new_release_has_all_archives_checksums_and_install_marker(self):
        self.assertTrue(update_formula("v0.27.0", self.root, self.formula))
        content = self.formula.read_text()
        self.assertNotIn("@", content)
        for target in TARGETS:
            self.assertIn(f"/download/v0.27.0/meshfox-{target}.tar.gz", content)
            self.assertIn(hashlib.sha256(target.encode()).hexdigest(), content)
        self.assertIn('(share/"meshfox/install-method").write "homebrew\\n"', content)
        self.assertIn("check-updates --yes", content)

    def test_retry_is_a_noop_and_older_release_cannot_downgrade_tap(self):
        update_formula("v0.27.10", self.root, self.formula)
        before = self.formula.read_bytes()
        self.assertFalse(update_formula("v0.27.10", self.root, self.formula))
        # Numeric comparison: 10 > 9, despite lexical ordering.
        self.assertFalse(update_formula("v0.27.9", self.root / "missing", self.formula))
        self.assertEqual(before, self.formula.read_bytes())

    def test_missing_archive_does_not_change_existing_formula(self):
        update_formula("v0.26.1", self.root, self.formula)
        before = self.formula.read_bytes()
        (self.root / f"meshfox-{TARGETS[-1]}.tar.gz").unlink()
        with self.assertRaises(FileNotFoundError):
            update_formula("v0.27.0", self.root, self.formula)
        self.assertEqual(before, self.formula.read_bytes())

    def test_prerelease_or_malformed_tag_cannot_change_formula(self):
        for tag in ["v1.0.0-rc.1", "v1.0.0+build", "1.0.0", "v01.0.0", "v1.0"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                update_formula(tag, self.root, self.formula)
        self.assertFalse(self.formula.exists())

    def test_unrecognized_existing_formula_is_not_overwritten(self):
        self.formula.parent.mkdir(parents=True)
        self.formula.write_text("unexpected format\n")
        with self.assertRaises(ValueError):
            update_formula("v0.27.0", self.root, self.formula)
        self.assertEqual("unexpected format\n", self.formula.read_text())


if __name__ == "__main__":
    unittest.main()
