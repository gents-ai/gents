#!/usr/bin/env python3
"""Print the group-level import graph of the proof tree. Reports only; never fails on layering."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location(
    "closure", Path(__file__).with_name("check-lean-import-closure.py"))
closure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(closure)

FEATURE_GROUPS = ("Goals", "GoalAutomation", "Workspace", "Mailbox")


LAYER_DIRECTORIES = {layer["directory"].split("/")[-1] for layer in json.loads(
    Path(__file__).with_name("lean-import-layers.json").read_text())["layers"]
    if layer.get("directory") and layer["name"] != "conformance"}


def group(module):
    """Model group of a module: the directory or file under its layer directory."""
    parts = module.split(".")
    if len(parts) < 2 or parts[0] != "Proofs":
        return None
    if parts[1] in LAYER_DIRECTORIES and len(parts) > 2:
        return parts[2]
    return parts[1]


def modules(root):
    return {".".join(path.relative_to(root).with_suffix("").parts):
            [name for name in closure.imports(path.read_text()) if group(name)]
            for path in sorted((root / "Proofs").rglob("*.lean"))}


def graph(imported):
    """Groups each group's files import directly."""
    edges = {}
    for module, names in imported.items():
        targets = edges.setdefault(group(module), set())
        targets.update(group(name) for name in names if group(name) != group(module))
    return edges


def reach(imported):
    """Groups each group's files load through their transitive file imports."""
    loaded = {}

    def load(module):
        if module not in loaded:
            loaded[module] = set()
            for name in imported.get(module, ()):
                loaded[module] |= {name} | load(name)
        return loaded[module]

    edges = {}
    for module in imported:
        targets = edges.setdefault(group(module), set())
        targets.update(group(name) for name in load(module) if group(name) != group(module))
    return edges


def components(edges):
    """Strongly connected components with more than one group (Tarjan, iterative)."""
    index, low, on_stack, stack, found = {}, {}, set(), [], []
    for start in sorted(edges):
        if start in index:
            continue
        work = [(start, iter(sorted(edges.get(start, ()))))]
        index[start] = low[start] = len(index)
        stack.append(start)
        on_stack.add(start)
        while work:
            node, successors = work[-1]
            for successor in successors:
                if successor not in index:
                    index[successor] = low[successor] = len(index)
                    stack.append(successor)
                    on_stack.add(successor)
                    work.append((successor, iter(sorted(edges.get(successor, ())))))
                    break
                if successor in on_stack:
                    low[node] = min(low[node], index[successor])
            else:
                work.pop()
                if work:
                    low[work[-1][0]] = min(low[work[-1][0]], low[node])
                if low[node] == index[node]:
                    component = []
                    while True:
                        member = stack.pop()
                        on_stack.discard(member)
                        component.append(member)
                        if member == node:
                            break
                    if len(component) > 1:
                        found.append(sorted(component))
    return sorted(found)


def core_importers(edges, features):
    return {feature: sorted(source for source, targets in edges.items() if feature in targets)
            for feature in features}


def report(root, features=FEATURE_GROUPS):
    imported = modules(root)
    edges = graph(imported)
    loaded = reach(imported)
    lines = [f"{source} -> {', '.join(sorted(targets)) or '-'}"
             for source, targets in sorted(edges.items())]
    mutual = sorted((a, b) for a in edges for b in edges[a] if a < b and a in edges.get(b, ()))
    lines += ["", "Mutual pairs:"] + [f"  {a} <-> {b}" for a, b in mutual]
    cycles = components(edges)
    lines += ["", "Cycles (strongly connected groups):"] + [
        f"  {len(cycle)}: {', '.join(cycle)}" for cycle in cycles]
    counts = []
    for title, view in (("Importers of feature groups:", edges),
                        ("Groups loading a feature group through file imports:", loaded)):
        lines += ["", title]
        core = set()
        for feature, importers in core_importers(view, features).items():
            core.update(name for name in importers
                        if name not in features and name != "Conformance")
            lines.append(f"  {feature} <- {', '.join(importers) or '-'}")
        counts.append(len(core))
    lines += ["", "summary: groups={} edges={} mutual_pairs={} cycles={} groups_in_cycles={} "
              "core_groups_importing_feature={} loaded_edges={} "
              "core_groups_loading_feature={}".format(
                  len(edges), sum(len(targets) for targets in edges.values()), len(mutual),
                  len(cycles), sum(len(cycle) for cycle in cycles), counts[0],
                  sum(len(targets) for targets in loaded.values()), counts[1])]
    return lines


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, nargs="?",
                        default=Path(__file__).resolve().parents[2] / "crates/gents/proofs")
    args = parser.parse_args()
    try:
        lines = report(args.root)
    except (OSError, ValueError) as error:
        print(f"Proof import report failed: {error}", file=sys.stderr)
        return 1
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
