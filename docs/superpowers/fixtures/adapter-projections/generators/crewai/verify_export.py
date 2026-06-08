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


def verify_capture_export(capture_path: Path, export_dir: Path) -> bool:
    capture = load_json(capture_path)
    mapping = capture.get("mapping")
    if not mapping or capture.get("source", {}).get("system") != "crewai":
        return False

    stem = capture_path.stem
    export = load_json(export_dir / f"{stem}.defra.json")
    jsonl = load_jsonl(export_dir / f"{stem}.defra.jsonl")
    eval_jsonl = load_jsonl(export_dir / f"{stem}.defra.eval-jsonl")

    assert export["projection_id"] == "multi_agent_task", export
    assert export["source_request_id"] == mapping["request_id"], export
    projection = export["output"]["projection"]

    participants = projection["participants"]
    for participant in mapping["participants"]:
        assert any(
            actual.get("role") == participant["role"]
            and actual.get("agent_did") == participant.get("agent_did")
            and actual.get("behavior_id") == participant.get("behavior_id")
            for actual in participants
        ), f"missing participant {participant} in {participants}"

    projected_text = "\n".join(
        message.get("content", "") for message in projection["messages"]
    )
    for task in capture["native"]["tasks"]:
        output = task_output_text(task)
        if output:
            assert output in projected_text, f"missing CrewAI task output {output!r}"
    for response in capture["native"].get("manager_responses", []):
        assert str(response) in projected_text, (
            f"missing CrewAI manager response {response!r}"
        )

    delegations = projection["delegations"]
    for delegation in mapping["delegations"]:
        assert any(
            actual.get("parent_request_id") == delegation["parent_request_id"]
            and actual.get("child_request_id") == delegation["child_request_id"]
            for actual in delegations
        ), f"missing delegation {delegation} in {delegations}"

    tool_events = projection["tool_events"]
    for event in mapping["tool_events"]:
        assert any(
            actual.get("id") == event["id"]
            and actual.get("tool_name") == event["tool_name"]
            for actual in tool_events
        ), f"missing tool event {event} in {tool_events}"

    assert jsonl, f"{stem} produced empty adapter JSONL"
    assert eval_jsonl, f"{stem} produced empty eval JSONL"
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
    print(f"verified {verified} CrewAI Defra export roundtrip captures")


if __name__ == "__main__":
    main()
