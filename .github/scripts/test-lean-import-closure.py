#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "guard", Path(__file__).with_name("check-lean-import-closure.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class ImportClosureTests(unittest.TestCase):
    def test_unsupported_imports_fail_closed(self):
        for source in ["import\nProofs.A\n", "import Proofs.«A»\n"]:
            with self.assertRaises(ValueError):
                guard.imports(source)
        self.assertEqual(guard.imports("import\tProofs.A\n"), ["Proofs.A"])

    def test_comments_and_import_lists(self):
        self.assertEqual(guard.imports(
            "/- import Proofs.Fake /- nested -/ -/\nprelude\n"
            "import Proofs.A Proofs.B -- import Proofs.Fake\n"
            "namespace Example\nimport Proofs.NotAHeader\n"), ["Proofs.A", "Proofs.B"])

    def test_transitive_closure_missing_and_orphans(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Proofs").mkdir()
            (root / "Proofs.lean").write_text("import Proofs.A\n")
            (root / "Proofs/A.lean").write_text("import Proofs.B\n")
            (root / "Proofs/B.lean").write_text("import Mathlib\n")
            self.assertEqual(guard.check(root), ([], []))
            (root / "Proofs/Orphan.lean").write_text("import Proofs.Missing\n")
            self.assertEqual(guard.check(root), (["Proofs.Missing"], ["Proofs.Orphan"]))
            (root / "Proofs/A.lean").write_text("import Mathlib\n")
            self.assertEqual(guard.check(root),
                             (["Proofs.Missing"], ["Proofs.B", "Proofs.Orphan"]))
            (root / "Proofs/Orphan.lean").write_text("def main : IO Unit := pure ()\n")
            self.assertEqual(guard.check(root, ("Proofs", "Proofs.B", "Proofs.Orphan")), ([], []))
            self.assertEqual(guard.check(root, ("Proofs", "Proofs.Unknown"))[0], ["Proofs.Unknown"])


if __name__ == "__main__":
    unittest.main()
