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


def verify_capture_export(capture_path: Path, export_dir: Path) -> None:
    capture = load_json(capture_path)
    mapping = capture.get("mapping")
    if not mapping:
        return
    if capture.get("source", {}).get("system") != "autogen-agentchat":
        return

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

    messages = projection["messages"]
    projected_text = "\n".join(message["content"] for message in messages)
    for message in capture["native"]["messages"]:
        assert str(message["content"]) in projected_text, (
            f"missing native AutoGen message {message['content']!r}"
        )

    delegations = projection["delegations"]
    for delegation in mapping["delegations"]:
        assert any(
            actual.get("parent_request_id") == delegation["parent_request_id"]
            and actual.get("child_request_id") == delegation["child_request_id"]
            for actual in delegations
        ), f"missing delegation {delegation} in {delegations}"

    assert jsonl, f"{stem} produced empty adapter JSONL"
    assert eval_jsonl, f"{stem} produced empty eval JSONL"
    assert any(
        record.get("record_kind") == "multi_agent_delegation" for record in jsonl
    ), f"{stem} JSONL missing delegation record"
    assert any(
        record.get("sample_kind") == "delegation" for record in eval_jsonl
    ), f"{stem} eval JSONL missing delegation sample"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", type=Path, default=Path("/out"))
    parser.add_argument("--exports", type=Path, default=Path("/exports"))
    args = parser.parse_args()

    captures = sorted(args.fixtures.glob("*.json"))
    verified = 0
    for capture_path in captures:
        before = verified
        verify_capture_export(capture_path, args.exports)
        capture = load_json(capture_path)
        if (
            capture.get("source", {}).get("system") == "autogen-agentchat"
            and capture.get("mapping")
        ):
            verified += 1
        if verified != before:
            print(f"verified {capture_path.name}")
    if verified == 0:
        raise SystemExit("no mapped AutoGen captures were verified")
    print(f"verified {verified} AutoGen Defra export roundtrip captures")


if __name__ == "__main__":
    main()
