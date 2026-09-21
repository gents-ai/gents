#!/usr/bin/env python3
"""Negative controls for the canonical-output handoff inventory checker."""
import copy
import importlib.util
import json
from pathlib import Path
import re
import unittest

spec = importlib.util.spec_from_file_location(
    "mapping", Path(__file__).with_name("check-canonical-output-map.py"))
mapping = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mapping)
ROOT = Path(__file__).resolve().parents[2]


class MappingTests(unittest.TestCase):
    def setUp(self):
        self.rows = [row for file in mapping.MAPS
                     for row in json.loads((ROOT / file).read_text())]
        source = (ROOT / mapping.PROOFS /
                  "Proofs/Conformance/Contracts/Json/Snapshot.lean").read_text()
        self.groups = set(re.findall(r'\\"([a-zA-Z_][\w]*)\\":', source))

    def errors(self):
        return "\n".join(mapping.check(ROOT, self.rows, self.groups))

    def test_repository_map(self):
        self.assertEqual(self.errors(), "")

    def test_missing_constructor_fails(self):
        row = next(row for row in self.rows if row["constructors"])
        missing = row["constructors"].pop()
        self.assertIn(f"unmapped constructor: {missing}", self.errors())

    def test_duplicate_assignment_fails(self):
        row = next(row for row in self.rows if row["constructors"])
        row["constructors"].append(row["constructors"][0])
        self.assertIn("constructor mapped 2 times", self.errors())

    def test_stale_references_fail(self):
        row = self.rows[0]
        row["model_symbols"] = [{"file": "AGENTS.md", "symbol": "NoSuchDeclaration"}]
        row["generated_groups"] = ["deleted_fixture_group"]
        row["native_owner_paths"] = ["no-such-owner.rs"]
        errors = self.errors()
        self.assertIn("missing model declaration", errors)
        self.assertIn("unknown emitted group", errors)
        self.assertIn("missing/invalid native_owner_paths", errors)

    def test_duplicate_id_and_fake_coverage_fail(self):
        self.rows.append(copy.deepcopy(self.rows[0]))
        self.rows[0]["native_status"] = "verified"
        self.assertIn("duplicate mapping id", self.errors())
        self.assertIn("consumer ledger", self.errors())

    def test_constructor_extraction_ignores_comments(self):
        source = """inductive Example where
  | first
  /- nested /- | phantom -/ | alsoPhantom -/
  | second (parameter : Nat) -- | commentOnly
  deriving Repr

def unrelated := "-- not a comment"
"""
        self.assertEqual(mapping.constructors(source, "Example"), {"first", "second"})
        self.assertEqual(mapping.constructors(source.replace("  deriving", "  | newCase\n  deriving"),
                                              "Example"), {"first", "second", "newCase"})
        with self.assertRaises(ValueError):
            mapping.constructors("inductive Example where | hidden", "Example")
        with self.assertRaises(ValueError):
            mapping.constructors("inductive Example where\n  | first | hidden", "Example")

    def test_declaration_requires_real_namespace(self):
        source = """namespace Actual
section Example
theorem Trace.coherent : True := by trivial
end Example
end Actual
"""
        self.assertTrue(mapping.declaration_exists(source, "Actual.Trace.coherent"))
        self.assertFalse(mapping.declaration_exists(source, "Wrong.Trace.coherent"))
        self.assertFalse(mapping.declaration_exists(source, "Actual.Example.Trace.coherent"))


if __name__ == "__main__":
    unittest.main()
