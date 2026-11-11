#!/usr/bin/env python3
"""Reject proof imports that point up the crate layering in lean-import-layers.json."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location(
    "closure", Path(__file__).with_name("check-lean-import-closure.py"))
closure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(closure)

MANIFEST = Path(__file__).with_name("lean-import-layers.json")


def unique_keys(pairs):
    seen = {}
    for key, value in pairs:
        if key in seen:
            raise ValueError(f"duplicate manifest key {key}")
        seen[key] = value
    return seen


def load_manifest(path):
    return json.loads(path.read_text(), object_pairs_hook=unique_keys)


def proof_modules(root):
    """Every Lean module of the package: the Proofs tree and the programs beside it."""
    paths = [root / "Proofs.lean", *sorted((root / "Proofs").rglob("*.lean")),
             *sorted(path for path in root.glob("*.lean") if path.name != "lakefile.lean")]
    return {".".join(path.relative_to(root).with_suffix("").parts): path
            for path in paths if path.exists()}


def lower_layers(layers):
    """Layers each layer may import, through `below` transitively; rejects unknown names and cycles."""
    direct = {layer["name"]: layer["below"] for layer in layers}
    reached = {}

    def visit(name, path):
        if name not in direct:
            raise ValueError(f"unknown layer {name}")
        if name in path:
            raise ValueError(f"layer cycle: {' -> '.join([*path, name])}")
        if name not in reached:
            result = set()
            for lower in direct[name]:
                result |= {lower} | visit(lower, [*path, name])
            reached[name] = result
        return reached[name]

    for name in direct:
        visit(name, [])
    return reached


def check(root, manifest):
    """Return (errors, layer of each module)."""
    errors = []
    layers = manifest["layers"]
    names = [layer["name"] for layer in layers]
    if len(set(names)) != len(names):
        errors.append("layer names are not unique")
    allowed = lower_layers(layers)
    prefixes = [(layer["directory"].replace("/", ".") + ".", layer["name"])
                for layer in layers if layer.get("directory")]
    modules = proof_modules(root)
    explicit = manifest.get("modules", {})
    layer_of = {}
    for module in sorted(modules):
        by_directory = [name for prefix, name in prefixes if module.startswith(prefix)]
        named = explicit.get(module)
        if named is not None and named not in allowed:
            errors.append(f"{module}: unknown layer {named}")
        elif by_directory and named is not None and named != by_directory[0]:
            errors.append(f"{module}: listed in layer {named} but lives in the {by_directory[0]} directory")
        elif named is None and not by_directory:
            errors.append(f"{module}: not assigned to a layer (move it into a layer directory "
                          f"or name it in {MANIFEST.name} `modules`)")
        layer_of[module] = named or (by_directory[0] if by_directory else None)
    for module in sorted(explicit.keys() - modules.keys()):
        errors.append(f"{MANIFEST.name} names {module}, which does not exist")
    for name in sorted(manifest.get("placements", {})):
        if not any(module == name or module.startswith(name + ".") for module in modules):
            errors.append(f"placement {name} names no module")

    exceptions = {(entry["module"], entry["imports"]): entry for entry in manifest.get("exceptions", [])}
    used = set()
    for module, path in sorted(modules.items()):
        own = layer_of[module]
        for name in closure.imports(path.read_text()):
            if not (name == "Proofs" or name.startswith("Proofs.")) or name not in layer_of:
                continue
            target = layer_of[name]
            if own is None or target is None or target == own or target in allowed[own]:
                continue
            if (module, name) in exceptions:
                used.add((module, name))
                continue
            errors.append(f"{module} [{own}] imports {name} [{target}], a higher layer")
    for module, name in sorted(exceptions.keys() - used):
        errors.append(f"exception {module} -> {name} no longer needed; remove it")
    for entry in manifest.get("exceptions", []):
        if not entry.get("reason"):
            errors.append(f"exception {entry['module']} -> {entry['imports']} has no reason")
    return errors, layer_of


def summary(root, layer_of, layers):
    files = {layer["name"]: 0 for layer in layers}
    lines = dict(files)
    for module, layer in layer_of.items():
        if layer in files:
            files[layer] += 1
            lines[layer] += proof_modules(root)[module].read_text().count("\n")
    return [f"{name}: {files[name]} files, {lines[name]} lines" for name in files]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, nargs="?",
                        default=Path(__file__).resolve().parents[2] / "crates/gents/proofs")
    parser.add_argument("--manifest", type=Path, default=MANIFEST)
    parser.add_argument("--summary", action="store_true", help="print files and lines per layer")
    args = parser.parse_args()
    try:
        manifest = load_manifest(args.manifest)
        errors, layer_of = check(args.root, manifest)
    except (OSError, ValueError, KeyError) as error:
        print(f"Proof layer check failed: {error}", file=sys.stderr)
        return 1
    if errors:
        print("Proof imports must point down the crate layering:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1
    if args.summary:
        print("\n".join(summary(args.root, layer_of, manifest["layers"])))
    return 0


if __name__ == "__main__":
    sys.exit(main())
