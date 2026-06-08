#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def final_output(native: dict[str, Any]) -> str | None:
    for event in reversed(native.get("events", [])):
        if event.get("type") != "output":
            continue
        data = event.get("data")
        if isinstance(data, dict) and data.get("text"):
            return str(data["text"])
        if isinstance(data, dict):
            for content in data.get("contents", []):
                if isinstance(content, dict) and content.get("text"):
                    return str(content["text"])
    return None


def participant_key(value: dict[str, Any]) -> tuple[Any, ...]:
    return (
        value.get("role"),
        value.get("agent_did"),
        value.get("behavior_id"),
    )


def message_key(value: dict[str, Any]) -> tuple[Any, ...]:
    return (
        value.get("role"),
        value.get("request_id"),
        value.get("content"),
    )


def delegation_key(value: dict[str, Any]) -> tuple[Any, ...]:
    return (
        value.get("parent_request_id"),
        value.get("child_request_id"),
        value.get("parent_tool_call_id"),
        value.get("agent_did"),
        value.get("behavior_id"),
        value.get("status"),
    )


def tool_event_key(value: dict[str, Any]) -> tuple[Any, ...]:
    return (
        value.get("request_id"),
        value.get("tool_name"),
        value.get("status"),
        value.get("child_request_id"),
    )


def assert_equal_multiset(
    label: str,
    expected: list[tuple[Any, ...]],
    actual: list[tuple[Any, ...]],
) -> None:
    expected_sorted = sorted(expected, key=repr)
    actual_sorted = sorted(actual, key=repr)
    assert actual_sorted == expected_sorted, (
        f"{label} mismatch\nexpected={expected_sorted!r}\nactual={actual_sorted!r}"
    )


def verify_capture_export(capture_path: Path, export_dir: Path) -> bool:
    capture = load_json(capture_path)
    mapping = capture.get("mapping")
    if (
        not mapping
        or capture.get("source", {}).get("system") != "microsoft-agent-framework"
    ):
        return False

    stem = capture_path.stem
    export = load_json(export_dir / f"{stem}.defra.json")
    jsonl = load_jsonl(export_dir / f"{stem}.defra.jsonl")
    eval_jsonl = load_jsonl(export_dir / f"{stem}.defra.eval-jsonl")

    assert export["projection_id"] == "multi_agent_task", export
    assert export["source_request_id"] == mapping["request_id"], export
    assert export["redaction_mode"] == capture["envelope"]["redaction_mode"], export
    for key in ("source_session_id", "source_agent_did", "source_behavior_id"):
        assert export.get(key) == capture["envelope"].get(key), (
            f"{key} mismatch: expected {capture['envelope'].get(key)!r}, "
            f"got {export.get(key)!r}"
        )
    projection = export["output"]["projection"]
    expected = capture["envelope"]["output"]["projection"]

    assert_equal_multiset(
        "participants",
        [participant_key(item) for item in expected["participants"]],
        [participant_key(item) for item in projection["participants"]],
    )
    assert [message_key(item) for item in projection["messages"]] == [
        message_key(item) for item in expected["messages"]
    ], projection["messages"]
    projected_text = "\n".join(message["content"] for message in projection["messages"])
    assert capture["native"]["task"] in projected_text, "missing MSAF user task"
    for outputs in capture["native"]["agent_outputs"].values():
        for output in outputs:
            assert output["text"] in projected_text, (
                f"missing MSAF agent output {output['text']!r}"
            )
    expected_final = final_output(capture["native"])
    if expected_final:
        assert expected_final in projected_text, (
            f"missing MSAF final output {expected_final!r}"
        )

    assert_equal_multiset(
        "delegations",
        [delegation_key(item) for item in expected["delegations"]],
        [delegation_key(item) for item in projection["delegations"]],
    )
    assert_equal_multiset(
        "tool_events",
        [tool_event_key(item) for item in expected["tool_events"]],
        [tool_event_key(item) for item in projection["tool_events"]],
    )

    assert jsonl, f"{stem} produced empty adapter JSONL"
    assert eval_jsonl, f"{stem} produced empty eval JSONL"
    expected_record_count = sum(
        len(expected[key])
        for key in ("participants", "messages", "delegations", "tool_events")
    )
    assert len(jsonl) == expected_record_count, (
        f"{stem} JSONL record count mismatch: "
        f"expected {expected_record_count}, got {len(jsonl)}"
    )
    assert len(eval_jsonl) == expected_record_count, (
        f"{stem} eval JSONL record count mismatch: "
        f"expected {expected_record_count}, got {len(eval_jsonl)}"
    )
    assert any(
        record.get("record_kind") == "multi_agent_delegation" for record in jsonl
    ), f"{stem} JSONL missing delegation record"
    assert any(
        record.get("sample_kind") == "delegation" for record in eval_jsonl
    ), f"{stem} eval JSONL missing delegation sample"
    return True


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", type=Path, default=Path("/out"))
    parser.add_argument("--exports", type=Path, default=Path("/exports"))
    args = parser.parse_args()

    verified = 0
    for capture_path in sorted(args.fixtures.glob("*.json")):
        if verify_capture_export(capture_path, args.exports):
            verified += 1
            print(f"verified {capture_path.name}")
    if verified == 0:
        raise SystemExit("no mapped Microsoft Agent Framework captures were verified")
    if verified != 1:
        raise SystemExit(
            f"expected 1 mapped Microsoft Agent Framework capture, verified {verified}"
        )
    print(
        f"verified {verified} Microsoft Agent Framework Defra export roundtrip captures"
    )


if __name__ == "__main__":
    main()
