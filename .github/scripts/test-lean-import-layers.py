#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "layers", Path(__file__).with_name("check-lean-import-layers.py"))
layers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(layers)


def manifest(**overrides):
    base = {
        "layers": [
            {"name": "low", "below": [], "directory": "Proofs/Low"},
            {"name": "mid", "below": ["low"], "directory": "Proofs/Mid"},
            {"name": "top", "below": ["mid"], "directory": None},
        ],
        "modules": {"Proofs": "top"},
        "exceptions": [],
    }
    base.update(overrides)
    return base


class ImportLayerTests(unittest.TestCase):
    def tree(self, directory):
        root = Path(directory)
        (root / "Proofs/Low").mkdir(parents=True)
        (root / "Proofs/Mid").mkdir()
        (root / "Proofs.lean").write_text("import Proofs.Mid.B\n")
        (root / "Proofs/Low/A.lean").write_text("import Mathlib\n")
        (root / "Proofs/Mid/B.lean").write_text("import Proofs.Low.A\n")
        return root

    def test_downward_and_transitive_imports_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.tree(directory)
            (root / "Proofs.lean").write_text("import Proofs.Low.A Proofs.Mid.B\n")
            errors, layer_of = layers.check(root, manifest())
            self.assertEqual(errors, [])
            self.assertEqual(layer_of["Proofs.Low.A"], "low")

    def test_upward_import_fails_unless_excepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.tree(directory)
            (root / "Proofs/Low/A.lean").write_text("import Proofs.Mid.B\n")
            errors, _ = layers.check(root, manifest())
            self.assertEqual(errors, ["Proofs.Low.A [low] imports Proofs.Mid.B [mid], a higher layer"])
            excepted = manifest(exceptions=[
                {"module": "Proofs.Low.A", "imports": "Proofs.Mid.B", "reason": "modelled"}])
            self.assertEqual(layers.check(root, excepted)[0], [])

    def test_stale_exception_unassigned_and_misplaced_modules_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.tree(directory)
            (root / "Proofs/Loose.lean").write_text("import Mathlib\n")
            errors, _ = layers.check(root, manifest(
                modules={"Proofs": "top", "Proofs.Mid.B": "low", "Proofs.Gone": "low"},
                exceptions=[{"module": "Proofs.Mid.B", "imports": "Proofs.Low.A", "reason": "x"}]))
            self.assertIn("Proofs.Loose: not assigned to a layer (move it into a layer directory "
                          "or name it in lean-import-layers.json `modules`)", errors)
            self.assertIn("Proofs.Mid.B: listed in layer low but lives in the mid directory", errors)
            self.assertIn("lean-import-layers.json names Proofs.Gone, which does not exist", errors)
            self.assertIn("exception Proofs.Mid.B -> Proofs.Low.A no longer needed; remove it", errors)

    def test_layer_cycles_and_duplicate_keys_fail(self):
        with self.assertRaises(ValueError):
            layers.lower_layers([{"name": "a", "below": ["b"]}, {"name": "b", "below": ["a"]}])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "m.json"
            path.write_text('{"modules": {"Proofs.A": "low", "Proofs.A": "mid"}}')
            with self.assertRaises(ValueError):
                layers.load_manifest(path)

    def test_repository_manifest_layers_are_acyclic(self):
        data = json.loads(layers.MANIFEST.read_text())
        layers.lower_layers(data["layers"])


if __name__ == "__main__":
    unittest.main()
