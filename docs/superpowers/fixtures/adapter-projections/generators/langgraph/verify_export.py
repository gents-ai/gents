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


def verify_capture_export(capture_path: Path, export_dir: Path) -> bool:
    capture = load_json(capture_path)
    mapping = capture.get("mapping")
    if not mapping or capture.get("source", {}).get("system") != "langgraph":
        return False

    stem = capture_path.stem
    export = load_json(export_dir / f"{stem}.defra.json")
    jsonl = load_jsonl(export_dir / f"{stem}.defra.jsonl")
    eval_jsonl = load_jsonl(export_dir / f"{stem}.defra.eval-jsonl")

    assert export["projection_id"] == "langgraph_state_history", export
    assert export["source_request_id"] == mapping["request_id"], export
    projection = export["output"]["projection"]
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
    assert projection["values"]["status"] == values["status"], projection["values"]
    if "final_output" in values:
        assert projection["values"]["final_output"] == values["final_output"]
    for text in message_texts(values):
        assert text in json.dumps(projection["values"], sort_keys=True), (
            f"missing LangGraph state message {text!r}"
        )

    projected_edges = {
        (edge["from"], edge["to"], edge["kind"]) for edge in projection["edges"]
    }
    for edge in native["graph"]["edges"]:
        expected = (edge["from"], edge["to"], edge["kind"])
        assert expected in projected_edges, f"missing LangGraph edge {expected}"

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

    assert jsonl, f"{stem} produced empty adapter JSONL"
    assert eval_jsonl, f"{stem} produced empty eval JSONL"
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
    print(f"verified {verified} LangGraph Defra export roundtrip captures")


if __name__ == "__main__":
    main()
