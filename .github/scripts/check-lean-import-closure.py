#!/usr/bin/env python3
"""Reject local proof modules outside the Lake-declared roots' import closure."""
import argparse
from pathlib import Path
import re
import sys


def imports(source):
    # Preserve newlines while removing Lean's nested block and line comments.
    header = []
    depth = 0
    i = 0
    while i < len(source):
        if source.startswith("/-", i):
            depth += 1
            header.append("  ")
            i += 2
        elif depth and source.startswith("-/", i):
            depth -= 1
            header.append("  ")
            i += 2
        elif not depth and source.startswith("--", i):
            end = source.find("\n", i)
            i = len(source) if end < 0 else end
        else:
            header.append(source[i] if not depth or source[i] == "\n" else " ")
            i += 1
    result = []
    for line in "".join(header).splitlines():
        line = line.strip()
        if not line or line == "prelude":
            continue
        if line == "import":
            raise ValueError("put each import command on one line")
        if not re.match(r"import\s", line):
            if line.startswith("Proofs."):
                raise ValueError("put each import command on one line")
            break
        modules = line.split()[1:]
        if not modules or not all(re.fullmatch(r"[A-Za-z_][A-Za-z_0-9.]*", name) for name in modules):
            raise ValueError(f"unsupported import command: {line}")
        result.extend(modules)
    return result


def check(root, roots=("Proofs",)):
    files = [root / "Proofs.lean", *sorted((root / "Proofs").rglob("*.lean"))]
    graph = {".".join(path.relative_to(root).with_suffix("").parts):
             imports(path.read_text()) for path in files}
    missing = sorted({name for deps in [*graph.values(), roots] for name in deps
                      if (name == "Proofs" or name.startswith("Proofs.")) and name not in graph})
    reached = set()
    pending = list(roots)
    while pending:
        module = pending.pop()
        if module in reached or module not in graph:
            continue
        reached.add(module)
        pending.extend(graph[module])
    return missing, sorted(graph.keys() - reached)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("modules", nargs="+", help="the same roots Lake builds")
    args = parser.parse_args()
    try:
        missing, orphans = check(args.root, args.modules)
    except (OSError, ValueError) as error:
        print(f"Proof import check failed: {error}", file=sys.stderr)
        return 1
    for name in missing:
        print(f"Missing local import: {name}", file=sys.stderr)
    for name in orphans:
        print(f"Outside declared build-root import closure: {name}", file=sys.stderr)
    if missing or orphans:
        return 1
    print("Every local proof module is reachable from a declared build root.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
