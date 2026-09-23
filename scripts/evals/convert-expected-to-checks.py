#!/usr/bin/env python3
"""Convert the M3 pre-work cases from the retired `expected` shorthand to the
contract's `checks` format (spec 3a, section 2).

Run once; kept so the conversion can be read and re-derived. The input is
`packs/eval_monitor/cases/*.json` at eval/30-monitor-prework 0425499b1; the
output is `packs/eval_monitor_findings/cases/`. Standard library only, and
deterministic: the same input writes byte-identical files.
"""

import argparse
import json
import pathlib
import sys

# The one capture every stage reads: the trial's mailbox rows, with every
# field a mailbox check reads.
CAPTURE = {
    "kind": "documents",
    "name": "items",
    "collection": "MailboxItem",
    "filter": {"requester_did": {"_eq": "$trial"}},
    "fields": ["title", "summary", "payload", "status"],
}

# The pre-work capture, identical on all thirty stages.
LEGACY_CAPTURE = [{"name": "items", "collection": "MailboxItem", "filter": {"requester_did": "$trial"}}]

# `test` is not an EvalSplit; the held-out split is what it meant.
SPLITS = {"train": "train", "validation": "validation", "test": "held_out"}

# Split overrides, applied after SPLITS. The pre-work split 8/8/4; held-out
# confirmation at alpha 0.05 over three rounds needs at least six cases, so two
# validation cases move to held_out (8/6/6). The pre-work records no difficulty;
# the proxy is the case's findings-matcher count, ties broken by total check
# refs. Each move keeps its axis on validation. case_id -> (split, reason).
SPLIT_OVERRIDES = {
    "file-without-contradiction-multi": (
        "held_out",
        "higher-matcher case of the workspace-file validation pair (3 vs 2); "
        "the axis stays on validation through file-with-contradicting-input",
    ),
    "three-conditions-with-service": (
        "held_out",
        "higher-matcher case of the finding-content validation pair (3 vs 1); "
        "the axis stays on validation through numeric-threshold-stated-normal",
    ),
}

# The split each override was ruled against; a different pre-work split is refused.
OVERRIDDEN_FROM = "validation"

CASE_KEYS = {"case_id", "axis", "split", "reducer", "fixtures", "stages"}
STAGE_KEYS = {"stage_id", "prompt", "deadline_secs", "capture", "expected"}
EXPECTED_KEYS = {
    "item_count",
    "payload_well_formed",
    "pairs_unique",
    "finding_count",
    "open_count",
    "resolved_count",
    "findings",
    "findings_exact",
    "no_finding_containing",
}


def check(name, params):
    return {"check": name, "params": params, "tier": "acceptance", "weight": 1}


def refuse_unknown(found, allowed, where):
    unknown = sorted(set(found) - allowed)
    if unknown:
        raise ValueError(f"{where}: unknown keys {unknown}")


def convert_expected(expected, where):
    """One `expected` key to one check, in a fixed order; two merges:
    open/resolved counts, and findings/findings_exact."""
    refuse_unknown(expected, EXPECTED_KEYS, where)
    checks = []
    if "item_count" in expected:
        checks.append(check("mailbox_item_count", {"count": expected["item_count"]}))
    if "payload_well_formed" in expected:
        if expected["payload_well_formed"] is not True:
            raise ValueError(f"{where}: payload_well_formed must be true")
        checks.append(check("payload_well_formed", {"version": 1}))
    if "pairs_unique" in expected:
        if expected["pairs_unique"] is not True:
            raise ValueError(f"{where}: pairs_unique must be true")
        checks.append(check("finding_pairs_unique", {}))
    if "finding_count" in expected:
        checks.append(check("finding_count", {"count": expected["finding_count"]}))
    if ("open_count" in expected) != ("resolved_count" in expected):
        raise ValueError(f"{where}: open_count and resolved_count come together")
    if "open_count" in expected:
        checks.append(
            check("finding_state_counts", {"open": expected["open_count"], "resolved": expected["resolved_count"]})
        )
    if "findings" in expected or "findings_exact" in expected:
        matchers = [
            {"correlation": m["correlation"], "state": m["state"], "keywords_all": list(m["keywords_all"])}
            for m in expected.get("findings", [])
        ]
        checks.append(check("findings_match", {"matchers": matchers, "exact": bool(expected.get("findings_exact", False))}))
    if expected.get("no_finding_containing"):
        checks.append(check("mailbox_text_excludes", {"keywords": list(expected["no_finding_containing"])}))
    return checks


def convert_stage(stage, case_id):
    where = f"{case_id}/{stage.get('stage_id')}"
    refuse_unknown(stage, STAGE_KEYS, where)
    if stage["capture"] != LEGACY_CAPTURE:
        raise ValueError(f"{where}: capture is not the pre-work items capture")
    return {
        "stage_id": stage["stage_id"],
        "prompt": stage["prompt"],
        "deadline_secs": stage["deadline_secs"],
        "capture": [json.loads(json.dumps(CAPTURE))],
        "checks": convert_expected(stage["expected"], where),
    }


def convert_split(legacy_split, case_id):
    if legacy_split not in SPLITS:
        raise ValueError(f"{case_id}: unknown split {legacy_split!r}")
    split = SPLITS[legacy_split]
    if case_id in SPLIT_OVERRIDES:
        if legacy_split != OVERRIDDEN_FROM:
            raise ValueError(f"{case_id}: override expects split {OVERRIDDEN_FROM!r}, found {legacy_split!r}")
        split = SPLIT_OVERRIDES[case_id][0]
    return split


def convert_case(legacy):
    case_id = legacy["case_id"]
    refuse_unknown(legacy, CASE_KEYS, case_id)
    case = {"case_id": case_id, "split": convert_split(legacy["split"], case_id), "reducer": legacy["reducer"]}
    if "fixtures" in legacy:
        case["fixtures"] = legacy["fixtures"]
    case["stages"] = [convert_stage(stage, case_id) for stage in legacy["stages"]]
    return case


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--input", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)
    args.output.mkdir(parents=True, exist_ok=True)
    for source in sorted(args.input.glob("*.json")):
        case = convert_case(json.loads(source.read_text(encoding="utf-8")))
        target = args.output / (case["case_id"].replace("-", "_") + ".json")
        target.write_text(json.dumps(case, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(target.name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
