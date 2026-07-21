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


def value_to_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True)


def task_output_text(task: dict[str, Any]) -> str | None:
    output = task.get("output")
    if output is None:
        return None
    if isinstance(output, dict) and output.get("raw"):
        return str(output["raw"])
    return value_to_text(output)


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
    if not mapping or capture.get("source", {}).get("system") != "crewai":
        return False

    stem = capture_path.stem
    export = load_json(export_dir / f"{stem}.gents.json")
    jsonl = load_jsonl(export_dir / f"{stem}.gents.jsonl")
    eval_jsonl = load_jsonl(export_dir / f"{stem}.gents.eval-jsonl")

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
    for task in capture["native"]["tasks"]:
        output = task_output_text(task)
        if output:
            assert output in projected_text, f"missing CrewAI task output {output!r}"

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
        raise SystemExit("no mapped CrewAI captures were verified")
    if verified != 2:
        raise SystemExit(f"expected 2 mapped CrewAI captures, verified {verified}")
    print(f"verified {verified} CrewAI Gents export roundtrip captures")


if __name__ == "__main__":
    main()
