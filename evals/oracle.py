"""Convert trusted fixture-server state into a native eval observation."""

import argparse
import csv
import hashlib
import io
import json
from pathlib import Path


def observation(state, trial_id, case_id, stage_id):
    if not isinstance(state, dict) or any(
        not isinstance(state.get(key), list) for key in ("pages", "requests", "imports")
    ):
        raise ValueError("Expected a complete fixture-server state snapshot")
    if (case_id, stage_id) == ("pagination", "paginate"):
        passed = (
            all(f"/api/invoices?page={page}" in state["pages"] for page in (1, 2, 3))
            and not state["requests"]
            and not state["imports"]
        )
    elif (case_id, stage_id) == ("form_tabs", "submit"):
        passed = (
            state["requests"]
            == [
                {
                    "recipient": "Casey",
                    "region": "west",
                    "service": "express",
                    "reference": "BR-2048",
                    "confirmed": "on",
                }
            ]
            and not state["imports"]
        )
    elif (case_id, stage_id) == ("upload", "import"):
        imports = state["imports"]
        passed = len(imports) == 1 and not state["requests"]
        if passed:
            entry = imports[0]
            reader = csv.DictReader(io.StringIO(entry["csv"]))
            passed = (
                reader.fieldnames == ["name", "quantity"]
                and list(reader)
                == [
                    {"name": "bolts", "quantity": "7"},
                    {"name": "nuts", "quantity": "11"},
                    {"name": "washers", "quantity": "4"},
                ]
                and entry["receipt"] == {"rows": 3, "quantity_total": 22}
            )
    else:
        raise ValueError(f"No website-state oracle for {case_id}/{stage_id}")
    digest = hashlib.sha256(json.dumps(state, sort_keys=True).encode()).hexdigest()
    return {
        "collection": "BrowserEvalObservation",
        "document": {
            "trial_id": trial_id,
            "case_id": case_id,
            "stage_id": stage_id,
            "passed": passed,
            "details": f"fixture-state sha256:{digest}; independent website-state check",
        },
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--trial-id", required=True)
    parser.add_argument(
        "--case", required=True, choices=["pagination", "form_tabs", "upload"]
    )
    parser.add_argument("--stage", required=True)
    args = parser.parse_args()
    print(
        json.dumps(
            observation(
                json.loads(args.state.read_text()), args.trial_id, args.case, args.stage
            ),
            indent=2,
        )
    )
