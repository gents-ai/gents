#!/usr/bin/env python3
"""Check #1571's conformance handoff inventory; this does not run native owners."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re
import subprocess
import sys


PROOFS = Path("crates/gents/proofs")
INVENTORIES = {
    "Gate.Operation": ("CanonicalOutput/Execution/Gate.lean", "Operation"),
    "SessionComposition.Trace": ("CanonicalOutput/Execution/SessionComposition.lean", "Trace"),
    "RequestExecutionLease.Action": ("RequestExecutionLease/Transition.lean", "Action"),
    "ToolExecution.ToolCallContext.Action": ("ToolExecution/Executable.lean", "Action"),
    "CanonicalOutput.Execution.ToolDelivery.CloseAuthority":
        ("CanonicalOutput/Execution/ToolDelivery.lean", "CloseAuthority"),
    "Subagent.BridgedState.Event": ("Background/Executable.lean", "Event"),
    "CompletionRetry.Action": ("CompletionRetry/Transition.lean", "Action"),
    "CompletionRetry.CanonicalGate.Operation": ("CompletionRetry/CanonicalGate.lean", "Operation"),
    "SessionQueue.Action": ("Session/Executable.lean", "Action"),
    "StorageWriteGate.Event": ("StorageWriteGate.lean", "Event"),
}
LEAN_NAMES = {name: ("CanonicalOutput.Execution." + name
                    if name in {"Gate.Operation", "SessionComposition.Trace"} else name)
              for name in INVENTORIES}
SEAM_INVENTORIES = set(INVENTORIES) - {
    "Gate.Operation", "SessionComposition.Trace", "RequestExecutionLease.Action"}
MAPS = [f"contracts/canonical-output-map-{area}.json"
        for area in ("execution", "projection", "native", "tools", "control")]
FIELDS = {"id", "model_symbols", "constructors", "generated_groups", "fixture_status",
          "native_owner_paths", "consumer_paths", "native_status", "required_cases",
          "retired_bindings"}


def uncomment(source):
    """Preserve line/column boundaries, including nested Lean block comments."""
    out, depth, quoted, i = [], 0, False, 0
    while i < len(source):
        if not depth and source[i] == '"':
            quoted = not quoted
        if quoted and source[i] == "\\" and i + 1 < len(source):
            out.extend(source[i:i + 2])
            i += 2
            continue
        if not quoted and source.startswith("/-", i):
            depth += 1
            out.append("  ")
            i += 2
        elif depth and source.startswith("-/", i):
            depth -= 1
            out.append("  ")
            i += 2
        elif not quoted and not depth and source.startswith("--", i):
            end = source.find("\n", i)
            end = len(source) if end < 0 else end
            out.append(" " * (end - i))
            i = end
        else:
            out.append(source[i] if not depth or source[i] == "\n" else " ")
            i += 1
    if depth:
        raise ValueError("unterminated Lean block comment")
    return "".join(out)


def constructors(source, declaration):
    """Read explicitly delimited constructor declarations, including grouped lines.

    Unsupported declaration layout fails closed rather than silently dropping
    constructors. Lean remains the authority for parsing/typechecking the model.
    """
    lines = uncomment(source).splitlines()
    starts = [i for i, line in enumerate(lines)
              if re.match(rf"^inductive {re.escape(declaration)}\b", line)]
    if len(starts) != 1 or not lines[starts[0]].rstrip().endswith("where"):
        raise ValueError(f"expected one single-line inductive header for {declaration}")
    result = []
    depth = 0
    for line in lines[starts[0] + 1:]:
        if line and not line[0].isspace():
            break
        # Pipes inside binders are not new constructors. Unsupported strings
        # fail closed; the compiler metadata check below is the final authority.
        if '"' in line:
            raise ValueError(f"unsupported string in constructor declaration: {line}")
        for index, char in enumerate(line):
            if char in "({[⦃":
                depth += 1
            elif char in ")}]⦄":
                depth -= 1
            elif char == "|" and depth == 0:
                match = re.match(r"\s*([A-Za-z_][\w']*)\b", line[index + 1:])
                if not match:
                    raise ValueError(f"unsupported constructor layout: {line}")
                result.append(match[1])
            if depth < 0:
                raise ValueError(f"unbalanced constructor declaration: {line}")
    if depth:
        raise ValueError(f"unbalanced constructor binders in {declaration}")
    if not result or len(result) != len(set(result)):
        raise ValueError(f"missing or repeated constructors in {declaration}")
    return set(result)


def declaration_exists(source, symbol):
    scopes = []
    for line in uncomment(source).splitlines():
        scope = re.match(r"^(namespace|section)\b(?:\s+([\w.]+))?", line)
        if scope:
            scopes.append(scope.group(2) if scope.group(1) == "namespace" else None)
        elif re.match(r"^end\b", line):
            if scopes:
                scopes.pop()
        else:
            declaration = re.match(
                r"^(?:(?:private|protected|noncomputable|partial|unsafe)\s+)*"
                r"(?:def|theorem|lemma|abbrev|inductive|structure)\s+([\w'.?]+)", line)
            if declaration:
                qualified = ".".join([scope for scope in scopes if scope] + [declaration.group(1)])
                if symbol == qualified:
                    return True
    return False


def check(root, entries, exported_groups):
    errors, ids, mapped = [], [], []
    for row in entries:
        if not isinstance(row, dict):
            raise ValueError("mapping entries must be JSON objects")
        identity = row.get("id", "<missing id>")
        ids.append(identity)
        if set(row) - {"admission"} != FIELDS:
            errors.append(f"{identity}: incorrect fields {sorted(set(row) ^ FIELDS)}")
            continue
        if "admission" in row or any(
                name.rsplit(".", 1)[0] in SEAM_INVENTORIES for name in row["constructors"]):
            admission = row.get("admission", {})
            if (not isinstance(admission, dict) or
                    set(admission) != {"boundary", "disposition", "reason"} or
                    not all(isinstance(value, str) and value.strip() for value in admission.values()) or
                    admission.get("disposition") not in {"admitted", "rejected", "routed"} or
                    not admission.get("boundary") or not admission.get("reason")):
                errors.append(f"{identity}: nested seam needs an explicit admission boundary and disposition")
            elif admission["boundary"] not in {ref["symbol"] for ref in row["model_symbols"]}:
                errors.append(f"{identity}: admission boundary must reference a mapped model declaration")
        if row["fixture_status"] not in {"concrete_inputs", "summary_only", "not_exported"}:
            errors.append(f"{identity}: invalid fixture status")
        # Deliberately no 'verified' status: native coverage belongs to the
        # existing consumer registry/ledger, never to a handoff assertion.
        if row["native_status"] not in {"pending", "not_applicable"}:
            errors.append(f"{identity}: native coverage must be established by the consumer ledger")
        if not row["required_cases"]:
            errors.append(f"{identity}: missing required cases or structural explanation")
        if row["native_status"] == "pending" and not row["native_owner_paths"]:
            errors.append(f"{identity}: missing native owner")
        if row["fixture_status"] != "not_exported" and not row["generated_groups"]:
            errors.append(f"{identity}: claims fixtures but names no emitted group")
        for field in ("native_owner_paths", "consumer_paths"):
            for path in row[field]:
                if Path(path).is_absolute() or ".." in Path(path).parts or not (root / path).is_file():
                    errors.append(f"{identity}: missing/invalid {field}: {path}")
        for ref in row["model_symbols"]:
            if set(ref) != {"file", "symbol"}:
                raise ValueError(f"{identity}: model reference requires file and symbol")
            path = root / ref["file"]
            if (Path(ref["file"]).is_absolute() or ".." in Path(ref["file"]).parts or
                    not path.is_file() or not declaration_exists(path.read_text(), ref["symbol"])):
                errors.append(f"{identity}: missing model declaration {ref}")
        for group in row["generated_groups"]:
            if group not in exported_groups:
                errors.append(f"{identity}: unknown emitted group {group}")
        mapped.extend(row["constructors"])
    for identity, count in Counter(ids).items():
        if count != 1:
            errors.append(f"duplicate mapping id: {identity}")
    expected = set()
    for prefix, (file, decl) in INVENTORIES.items():
        expected.update(f"{prefix}.{name}" for name in constructors(
            (root / PROOFS / "Proofs" / file).read_text(), decl))
    for name in sorted(expected - set(mapped)):
        errors.append(f"unmapped constructor: {name}")
    for name in sorted(set(mapped) - expected):
        errors.append(f"unknown constructor: {name}")
    for name, count in Counter(mapped).items():
        if count != 1:
            errors.append(f"constructor mapped {count} times: {name}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--check-export", action="store_true",
                        help="after lake build, validate groups against actual generated JSON")
    args = parser.parse_args()
    try:
        entries = []
        for file in MAPS:
            rows = json.loads((args.root / file).read_text())
            if not isinstance(rows, list):
                raise ValueError(f"{file}: expected a JSON array")
            entries.extend(rows)
        snapshot = args.root / PROOFS / "Proofs/Conformance/Contracts/Json/Snapshot.lean"
        groups = set(re.findall(r'\\"([a-zA-Z_][\w]*)\\":', snapshot.read_text()))
        if args.check_export:
            refs = [ref for row in entries for ref in row["model_symbols"]]
            modules = sorted({".".join(Path(ref["file"]).relative_to(PROOFS).with_suffix("").parts)
                              for ref in refs} |
                             {"Proofs." + file.removesuffix(".lean").replace("/", ".")
                              for file, _ in INVENTORIES.values()})
            checks = "\n".join(["import Lean"] + [f"import {module}" for module in modules] +
                               [f"#check {name}" for name in sorted({ref["symbol"] for ref in refs})])
            for name in LEAN_NAMES.values():
                checks += (f'\nrun_cmd do\n  let info ← Lean.getConstInfoInduct ``{name}\n'
                           '  for ctor in info.ctors do\n'
                           '    Lean.logInfo m!"MAPPED_CTOR {ctor}"\n')
            resolved = subprocess.run(["lake", "env", "lean", "--stdin"], input=checks,
                                      cwd=args.root / PROOFS, check=True, capture_output=True, text=True)
            actual = set(re.findall(r"MAPPED_CTOR ([\w.']+)", resolved.stdout))
            scanned = {f"{LEAN_NAMES[prefix]}.{name}"
                       for prefix, (file, decl) in INVENTORIES.items()
                       for name in constructors((args.root / PROOFS / "Proofs" / file).read_text(), decl)}
            if actual != scanned:
                raise ValueError(f"constructor scanner differs from Lean: {sorted(actual ^ scanned)}")
            result = subprocess.run(["lake", "env", "lean", "--run", "Proofs/Conformance/Contracts.lean"],
                                    cwd=args.root / PROOFS, check=True, capture_output=True, text=True)
            payload = result.stdout.split("---BEGIN GENTS LEAN CONTRACT JSON---", 1)[1].split(
                "---END GENTS LEAN CONTRACT JSON---", 1)[0]
            groups = set(json.loads(payload))
        errors = check(args.root, entries, groups)
    except subprocess.CalledProcessError as error:
        print(error.stdout + error.stderr, file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        print(f"Canonical output mapping check failed: {error}", file=sys.stderr)
        return 1
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Checked {len(entries)} handoff entries and all {len(INVENTORIES)} constructor inventories; "
          "native conformance remains pending.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
