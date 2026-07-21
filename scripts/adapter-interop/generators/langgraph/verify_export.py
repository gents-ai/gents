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


def latest_snapshot(native: dict[str, Any]) -> dict[str, Any]:
    return native["history"][0]


def latest_values(native: dict[str, Any]) -> dict[str, Any]:
    return latest_snapshot(native)["values"]


def message_texts(values: dict[str, Any]) -> list[str]:
    texts: list[str] = []
    for message in values.get("messages", []):
        if isinstance(message, str):
            texts.append(message)
        elif isinstance(message, dict) and message.get("content"):
            texts.append(str(message["content"]))
    return texts


def node_key(value: dict[str, Any]) -> tuple[Any, ...]:
    return (
        value.get("id"),
        value.get("kind"),
        value.get("request_id"),
        value.get("status"),
    )


def edge_key(value: dict[str, Any]) -> tuple[Any, ...]:
    return (
        value.get("from"),
        value.get("to"),
        value.get("kind"),
    )


def task_key(value: dict[str, Any]) -> tuple[Any, ...]:
    return (
        value.get("request_id"),
        value.get("name"),
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
    if not mapping or capture.get("source", {}).get("system") != "langgraph":
        return False

    stem = capture_path.stem
    export = load_json(export_dir / f"{stem}.gents.json")
    jsonl = load_jsonl(export_dir / f"{stem}.gents.jsonl")
    eval_jsonl = load_jsonl(export_dir / f"{stem}.gents.eval-jsonl")

    assert export["projection_id"] == "langgraph_state_history", export
    assert export["source_request_id"] == mapping["request_id"], export
    assert export["redaction_mode"] == capture["envelope"]["redaction_mode"], export
    for key in ("source_session_id", "source_agent_did", "source_behavior_id"):
        assert export.get(key) == capture["envelope"].get(key), (
            f"{key} mismatch: expected {capture['envelope'].get(key)!r}, "
            f"got {export.get(key)!r}"
        )
    projection = export["output"]["projection"]
    expected = capture["envelope"]["output"]["projection"]
    native = capture["native"]
    values = latest_values(native)

    checkpoint_id = (
        latest_snapshot(native)
        .get("config", {})
        .get("configurable", {})
        .get("checkpoint_id")
    )
    assert projection["thread_id"] == native["thread_id"], projection
    assert projection["checkpoint_id"] == checkpoint_id, projection
    assert projection["root_request_id"] == mapping["request_id"], projection
    assert projection["values"] == expected["values"], (
        f"LangGraph values mismatch for {stem}"
    )
    if export["redaction_mode"] == "full":
        assert projection["values"]["status"] == values["status"], projection["values"]
        if "final_output" in values:
            assert projection["values"]["final_output"] == values["final_output"]
        for text in message_texts(values):
            assert text in json.dumps(projection["values"], sort_keys=True), (
                f"missing LangGraph state message {text!r}"
            )
    else:
        serialized_values = json.dumps(projection["values"], sort_keys=True)
        assert "[training_safe_redacted]" in serialized_values, projection["values"]
        for text in message_texts(values):
            assert text not in serialized_values, (
                f"unredacted LangGraph state message leaked {text!r}"
            )

    projected_node_ids = {node["id"] for node in projection["nodes"]}
    for edge in native["graph"]["edges"]:
        assert edge["from"] in projected_node_ids, f"missing edge source {edge['from']}"
        assert edge["to"] in projected_node_ids, f"missing edge target {edge['to']}"

    projected_task_names = {task["name"] for task in projection["tasks"]}
    for name in native["graph"].get("nodes", []):
        assert name in projected_task_names, f"missing LangGraph task {name}"
    for subgraph in native["graph"].get("subgraphs", {}).values():
        for name in subgraph.get("nodes", []):
            assert name in projected_task_names, f"missing subgraph task {name}"
    assert_equal_multiset(
        "nodes",
        [node_key(item) for item in expected["nodes"]],
        [node_key(item) for item in projection["nodes"]],
    )
    assert_equal_multiset(
        "edges",
        [edge_key(item) for item in expected["edges"]],
        [edge_key(item) for item in projection["edges"]],
    )
    assert_equal_multiset(
        "tasks",
        [task_key(item) for item in expected["tasks"]],
        [task_key(item) for item in projection["tasks"]],
    )

    assert jsonl, f"{stem} produced empty adapter JSONL"
    assert eval_jsonl, f"{stem} produced empty eval JSONL"
    expected_record_count = 1 + sum(
        len(expected[key]) for key in ("nodes", "edges", "tasks")
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
        record.get("record_kind") == "langgraph_edge" for record in jsonl
    ), f"{stem} JSONL missing edge record"
    assert any(
        record.get("sample_kind") in {"task", "state_transition"}
        for record in eval_jsonl
    ), f"{stem} eval JSONL missing task/state sample"
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
        raise SystemExit("no mapped LangGraph captures were verified")
    if verified != 3:
        raise SystemExit(f"expected 3 mapped LangGraph captures, verified {verified}")
    print(f"verified {verified} LangGraph Gents export roundtrip captures")


if __name__ == "__main__":
    main()
