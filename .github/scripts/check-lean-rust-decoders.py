#!/usr/bin/env python3
"""Structurally lint generated Lean JSON against a supported Rust type subset.

Introduced for the intentionally red #1571 foundation, this check provides an
early shape diagnostic before native tests compile. Native `lean_vocab_test`
tests remain the authority for actual serde decoding and behavior. This script
supports only the small, explicit serde shape subset used by the reachable
fixture types; unsupported syntax fails closed.

It is a structural lint, not serde or an actual Rust decoder guarantee. It does
not establish native behavior. Types re-exported from `gents-protocol` own their
wire format and are checked for presence only.
"""
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

GROUPS = {
    "current_input_cases": "LeanCurrentInputCase",
    "canonical_execution_gate_cases": "LeanCanonicalExecutionCase",
    "canonical_dispatch_observation_cases": "LeanDispatchObservationCase",
    "interrupt_queue_cases": "LeanInterruptQueueCase",
    "canonical_payload_presentation_cases": "LeanPayloadPresentationCase",
    "terminal_diagnostic_presentation_cases": "LeanTerminalDiagnosticPresentationCase",
    "terminal_diagnostic_replay_cases": "LeanTerminalDiagnosticReplayCase",
    "canonical_output_projection_cases": "LeanCanonicalOutputProjectionCase",
    "reasoning_audit_cases": "LeanReasoningAuditCase",
    "reasoning_signature_cases": "LeanReasoningSignatureCase",
    "auxiliary_output_cases": "LeanAuxiliaryOutputCase",
    "prompt_assembly_claude_wire_start_cases": "LeanPromptAssemblyClaudeWireStartCase",
    "prompt_assembly_claude_thinking_stream_cases": "LeanPromptAssemblyClaudeThinkingStreamCase",
    "prompt_assembly_reasoning_suffix_cases": "LeanPromptAssemblyReasoningSuffixCase",
    "prompt_assembly_replay_shape_cases": "LeanPromptAssemblyReplayShapeCase",
    "prompt_assembly_replay_prefix_cases": "LeanPromptAssemblyReplayPrefixCase",
    "title_request_admission_cases": "LeanTitleRequestAdmissionCase",
    "title_request_purpose_wire_cases": "LeanTitleRequestPurposeWireCase",
    "title_usage_cases": "LeanTitleUsageCase",
    "title_admission_join_cases": "LeanTitleAdmissionJoinCase",
    "compaction_projection_join_cases": "LeanCompactionProjectionJoinCase",
    "compaction_canonical_projection_cases": "LeanCanonicalCompactionCase",
    "repaired_projection_admission_cases": "LeanRepairedProjectionCase",
    "protected_replay_compaction_cases": "LeanProtectedReplayCompactionCase",
    "request_execution_lease_cases": "LeanRequestExecutionLeaseCase",
    "request_execution_lease_trace_cases": "LeanRequestExecutionLeaseTraceCase",
    "queued_steering_trace_cases": "LeanQueuedSteeringTraceCase",
    "queued_steering_guard_cases": "LeanQueuedSteeringGuardCase",
}
INT_RANGES = {
    "u8": (0, 2**8 - 1), "u16": (0, 2**16 - 1),
    "u32": (0, 2**32 - 1), "u64": (0, 2**64 - 1),
    "i32": (-(2**31), 2**31 - 1), "i64": (-(2**63), 2**63 - 1),
    # Generated contracts target the repository's 64-bit Rust platforms.
    "usize": (0, 2**64 - 1), "isize": (-(2**63), 2**63 - 1),
}


def unsupported_serde(attrs, allowed):
    unsupported = []
    for attr in attrs:
        for option in split_top(attr):
            if not any(re.fullmatch(pattern, option.strip()) for pattern in allowed):
                unsupported.append(option.strip())
    return unsupported


def attributes(text):
    return re.findall(r"#\[([^\]]*)\]", text)


def unsupported_non_serde(attrs, allowed=()):
    return [attr.strip() for attr in attrs
            if not attr.strip().startswith("serde(") and
            not any(re.fullmatch(pattern, attr.strip()) for pattern in allowed)]


def snake(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def split_top(text, sep=","):
    parts, depth, cur = [], 0, ""
    for ch in text:
        if ch in "<({[":
            depth += 1
        elif ch in ">)}]":
            depth -= 1
        if ch == sep and depth == 0:
            parts.append(cur)
            cur = ""
        else:
            cur += ch
    if cur.strip():
        parts.append(cur)
    return [p.strip() for p in parts if p.strip()]


def parse_fields(body):
    fields, errors = [], []
    for part in split_top(body):
        attrs = re.findall(r"#\[serde\(([^\]]*)\)\]", part)
        all_attrs = attributes(part)
        part = re.sub(r"#\[[^\]]*\]", "", part)
        part = re.sub(r"///[^\n]*", "", part).strip()
        match = re.match(r"(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*(.+)$", part, re.S)
        if not match:
            if part:
                errors.append(f"unsupported field layout `{part}`")
            continue
        name, ty = match.group(1), " ".join(match.group(2).split())
        joined = ",".join(attrs)
        rename = re.search(r'rename\s*=\s*"([^"]+)"', joined)
        required_nullable = bool(re.search(
            r'deserialize_with\s*=\s*"(?:crate::lean_vocab_test::)?required_nullable"', joined))
        fields.append({
            "name": rename.group(1) if rename else name,
            "type": ty,
            "default": "default" in joined,
            "required_nullable": required_nullable,
            "unsupported": unsupported_serde(attrs, [
                r'default', r'rename\s*=\s*"[^"]+"',
                r'deserialize_with\s*=\s*"(?:crate::lean_vocab_test::)?required_nullable"',
            ]) + unsupported_non_serde(all_attrs) + (
                ["required_nullable on non-Option field"]
                if required_nullable and not ty.startswith("Option<") else []),
        })
    return fields, errors


def strip_comments(text):
    """Comments may contain commas and brackets, which would corrupt splitting."""
    return re.sub(r"//[^\n]*", "", text)


def parse_items(source):
    items = {}
    # A re-exported protocol type owns its own wire format (often a hand-written
    # `Deserialize`), so it is checked for presence only.
    for alias in re.findall(r"pub(?:\([^)]*\))?\s+use\s+[A-Za-z0-9_:]+\s+as\s+([A-Za-z0-9_]+)\s*;", source):
        items[alias] = {"kind": "external"}
    pattern = re.compile(
        r"((?:\s*(?:#\[[^\]]*\]|///[^\n]*)\s*\n)*)\s*pub(?:\([^)]*\))?\s+(struct|enum)\s+"
        r"([A-Za-z0-9_]+)(?:<([A-Za-z0-9_, ]+)>)?\s*\{", re.M)
    for match in pattern.finditer(source):
        attrs, kind, name, generics = match.groups()
        start, depth, index = match.end(), 1, match.end()
        while depth and index < len(source):
            depth += {"{": 1, "}": -1}.get(source[index], 0)
            index += 1
        body = strip_comments(source[start:index - 1])
        serde = ",".join(re.findall(r"#\[serde\(([^\]]*)\)\]", attrs))
        serde_attrs = re.findall(r"#\[serde\(([^\]]*)\)\]", attrs)
        all_attrs = attributes(attrs)
        tag = re.search(r'tag\s*=\s*"([^"]+)"', serde)
        item = {
            "kind": kind,
            "generics": [g.strip() for g in generics.split(",")] if generics else [],
            "deny": "deny_unknown_fields" in serde,
            "tag": tag.group(1) if tag else None,
            "snake": 'rename_all = "snake_case"' in serde,
            "camel": 'rename_all = "camelCase"' in serde,
            "unsupported": [],
        }
        allowed_serde = [r'deny_unknown_fields']
        if kind == "enum":
            allowed_serde += [r'tag\s*=\s*"[^"]+"',
                              r'rename_all\s*=\s*"(?:snake_case|camelCase)"']
        item["unsupported"].extend(unsupported_serde(serde_attrs, allowed_serde))
        item["unsupported"].extend(unsupported_non_serde(all_attrs, [r'derive\([^)]*\)']))
        derives = [attr for attr in all_attrs if re.fullmatch(r'derive\([^)]*\)', attr.strip())]
        if not any(re.search(r'\bDeserialize\b', derive) for derive in derives):
            item["unsupported"].append("missing derived Deserialize")
        if kind == "struct":
            item["fields"], field_errors = parse_fields(body)
            item["unsupported"].extend(field_errors)
        else:
            variants = []
            for part in split_top(body):
                variant_attrs = re.findall(r"#\[serde\(([^\]]*)\)\]", part)
                all_variant_attrs = attributes(part)
                part = re.sub(r"///[^\n]*", "", part)
                part = re.sub(r"#\[[^\]]*\]", "", part).strip()
                vm = re.match(r"([A-Z][A-Za-z0-9_]*)\s*(?:\{(.*)\}|\((.*)\))?\s*$", part, re.S)
                if not vm:
                    if part:
                        item["unsupported"].append(f"unsupported variant layout `{part}`")
                    continue
                vname, vfields, vnew = vm.groups()
                renamed = re.search(r'rename\s*=\s*"([^"]+)"', ",".join(variant_attrs))
                parsed_fields, field_errors = parse_fields(vfields) if vfields is not None else (None, [])
                item["unsupported"].extend(field_errors)
                variants.append({
                    "name": (renamed.group(1) if renamed else
                             snake(vname) if item["snake"] else
                             vname[0].lower() + vname[1:] if item["camel"] else vname),
                    "fields": parsed_fields,
                    "newtype": " ".join(vnew.split()) if vnew else None,
                    "unsupported": unsupported_serde(variant_attrs, [r'rename\s*=\s*"[^"]+"']) +
                                   unsupported_non_serde(all_variant_attrs),
                })
                item["unsupported"].extend(variants[-1]["unsupported"])
            item["variants"] = variants
        items[name] = item
    for name in re.findall(
            r"impl(?:<[^>]+>)?\s+(?:serde::)?Deserialize(?:<[^>]+>)?\s+for\s+([A-Za-z0-9_]+)",
            source):
        if name in items and items[name]["kind"] != "external":
            items[name]["unsupported"].append("custom Deserialize implementation")
    return items


class Checker:
    def __init__(self, items):
        self.items, self.errors = items, []

    def fail(self, path, message):
        if len(self.errors) < 40:
            self.errors.append(f"{path}: {message}")

    def check(self, value, ty, path, env=None):
        env = env or {}
        ty = env.get(ty, ty)
        if ty.startswith("(") and ty.endswith(")"):
            members = split_top(ty[1:-1])
            if not isinstance(value, list) or len(value) != len(members):
                return self.fail(path, f"expected {len(members)}-tuple array for {ty}")
            for index, member in enumerate(members):
                self.check(value[index], member, f"{path}.{index}", env)
            return None
        generic = re.match(r"([A-Za-z0-9_:]+)<(.+)>$", ty)
        base, args = (generic.group(1), split_top(generic.group(2))) if generic else (ty, [])
        base = base.split("::")[-1]
        if base == "Option":
            return None if value is None else self.check(value, args[0], path, env)
        if base in ("Vec", "BTreeSet", "HashSet"):
            if not isinstance(value, list):
                return self.fail(path, f"expected array for {ty}")
            for index, element in enumerate(value):
                self.check(element, args[0], f"{path}[{index}]", env)
            return None
        if base in INT_RANGES:
            lower, upper = INT_RANGES[base]
            if (isinstance(value, bool) or not isinstance(value, int) or
                    not lower <= value <= upper):
                self.fail(path, f"expected {base}, got {value!r}")
            return None
        if base == "bool":
            return None if isinstance(value, bool) else self.fail(path, f"expected bool, got {value!r}")
        if base == "String":
            return None if isinstance(value, str) else self.fail(path, f"expected string, got {value!r}")
        if base == "Value":
            return None
        item = self.items.get(base)
        if item is None:
            return self.fail(path, f"unknown Rust type {base}")
        if item["kind"] == "external":
            return None if value is not None else self.fail(path, f"expected {base}, got null")
        if item["unsupported"]:
            return self.fail(path, "unsupported serde syntax: " + ", ".join(item["unsupported"]))
        scope = dict(env)
        scope.update({g: env.get(a, a) for g, a in zip(item["generics"], args)})
        if item["kind"] == "struct":
            return self.check_fields(value, item["fields"], item["deny"], path, scope, set())
        return self.check_enum(value, item, path, scope)

    def check_fields(self, value, fields, deny, path, env, ignore):
        if not isinstance(value, dict):
            return self.fail(path, "expected object")
        names = {f["name"] for f in fields}
        if deny:
            for key in value:
                if key not in names and key not in ignore:
                    self.fail(path, f"unknown field `{key}`")
        for field in fields:
            if field["unsupported"]:
                self.fail(
                    f"{path}.{field['name']}",
                    "unsupported serde syntax: " + ", ".join(field["unsupported"]))
                continue
            optional = field["default"] or (
                env.get(field["type"], field["type"]).startswith("Option<")
                and not field["required_nullable"])
            if field["name"] not in value:
                if not optional:
                    self.fail(path, f"missing field `{field['name']}`")
                continue
            self.check(value[field["name"]], field["type"], f"{path}.{field['name']}", env)

    def check_enum(self, value, item, path, env):
        variants = {v["name"]: v for v in item["variants"]}
        if item["tag"]:
            if not isinstance(value, dict) or item["tag"] not in value:
                return self.fail(path, f"missing tag `{item['tag']}`")
            variant = variants.get(value[item["tag"]])
            if variant is None:
                return self.fail(path, f"unknown variant `{value[item['tag']]}`")
            if variant["unsupported"]:
                return self.fail(path, "unsupported serde syntax on variant: " +
                                 ", ".join(variant["unsupported"]))
            if variant["newtype"]:
                inner = {k: v for k, v in value.items() if k != item["tag"]}
                return self.check(inner, variant["newtype"], path, env)
            return self.check_fields(value, variant["fields"] or [], item["deny"], path, env,
                                     {item["tag"]})
        if isinstance(value, str):
            variant = variants.get(value)
            if variant and variant["unsupported"]:
                return self.fail(path, "unsupported serde syntax on variant: " +
                                 ", ".join(variant["unsupported"]))
            return None if variant and variant["fields"] is None else self.fail(
                path, f"unknown unit variant `{value}`")
        return self.fail(path, "unsupported externally tagged enum value")


BEGIN, END = "---BEGIN GENTS LEAN CONTRACT JSON---", "---END GENTS LEAN CONTRACT JSON---"


def extract_contract(raw):
    if BEGIN in raw:
        raw = raw.split(BEGIN, 1)[1].split(END, 1)[0]
    return json.loads(raw[raw.index("{"):raw.rindex("}") + 1])


def validate(items, payload, groups=None):
    checker = Checker(items)
    for group, rust in (groups or GROUPS).items():
        if group not in payload:
            checker.fail(group, "group absent from the generated contract")
            continue
        checker.check(payload[group], f"Vec<{rust}>", group)
    return checker.errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--contract", type=Path,
                        help="saved generator stdout; omitted, the contract is generated after lake build")
    args = parser.parse_args()
    items = {}
    for source in sorted((args.root / "crates/gents/src/lean_vocab_test").glob("*.rs")):
        items.update(parse_items(source.read_text()))
    try:
        if args.contract:
            raw = args.contract.read_text()
        else:
            raw = subprocess.run(
                ["lake", "env", "lean", "--run", "Proofs/Conformance/Contracts.lean"],
                cwd=args.root / "crates/gents/proofs", check=True, capture_output=True,
                text=True).stdout
        payload = extract_contract(raw)
    except subprocess.CalledProcessError as error:
        print(error.stdout + error.stderr, file=sys.stderr)
        return 1
    except (OSError, ValueError) as error:
        print(f"Lean/Rust decoder check failed: {error}", file=sys.stderr)
        return 1
    errors = validate(items, payload)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    counts = ", ".join(f"{g}={len(payload[g])}" for g in GROUPS)
    print(f"Lean/Rust fixture shapes pass the supported structural lint ({counts}).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
