use serde_json::{json, Value};

use super::{optional_string_schema, string_schema};

pub(super) fn openai_codex_projection_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["run_id", "items"],
        "properties": {
            "run_id": string_schema(),
            "thread_id": optional_string_schema(),
            "status": optional_string_schema(),
            "items": {
                "type": "array",
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "id"],
                            "properties": {
                                "type": { "const": "request" },
                                "id": string_schema(),
                                "status": optional_string_schema(),
                                "node_did": optional_string_schema(),
                                "agent_id": optional_string_schema(),
                                "parent_request_id": optional_string_schema(),
                                "parent_tool_call_id": optional_string_schema(),
                                "input": optional_string_schema(),
                                "timestamp": optional_string_schema()
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "id", "role", "content"],
                            "properties": {
                                "type": { "const": "message" },
                                "id": string_schema(),
                                "request_id": optional_string_schema(),
                                "role": string_schema(),
                                "content": string_schema(),
                                "reasoning": optional_string_schema(),
                                "timestamp": optional_string_schema()
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "id", "name", "arguments", "output", "status"],
                            "properties": {
                                "type": { "const": "tool_call" },
                                "id": string_schema(),
                                "request_id": optional_string_schema(),
                                "name": string_schema(),
                                "arguments": string_schema(),
                                "output": string_schema(),
                                "status": string_schema(),
                                "child_run_id": optional_string_schema(),
                                "started_at": optional_string_schema(),
                                "completed_at": optional_string_schema()
                            }
                        }
                    ]
                }
            },
            "child_run_ids": {
                "type": "array",
                "items": string_schema()
            }
        }
    })
}

pub(super) fn langgraph_projection_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["checkpoint_id", "root_request_id", "values", "nodes", "edges", "tasks"],
        "properties": {
            "thread_id": optional_string_schema(),
            "checkpoint_id": string_schema(),
            "root_request_id": string_schema(),
            "values": { "type": "object" },
            "nodes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "kind"],
                    "properties": {
                        "id": string_schema(),
                        "kind": string_schema(),
                        "request_id": optional_string_schema(),
                        "node_did": optional_string_schema(),
                        "agent_id": optional_string_schema(),
                        "parent_request_id": optional_string_schema(),
                        "parent_tool_call_id": optional_string_schema(),
                        "status": optional_string_schema(),
                        "content": optional_string_schema(),
                        "reasoning": optional_string_schema()
                    }
                }
            },
            "edges": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["from", "to", "kind"],
                    "properties": {
                        "from": string_schema(),
                        "to": string_schema(),
                        "kind": string_schema()
                    }
                }
            },
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "name", "status"],
                    "properties": {
                        "id": string_schema(),
                        "request_id": optional_string_schema(),
                        "name": string_schema(),
                        "status": string_schema(),
                        "child_request_id": optional_string_schema()
                    }
                }
            }
        }
    })
}

pub(super) fn multi_agent_projection_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["task_id", "participants", "messages", "delegations", "tool_events"],
        "properties": {
            "task_id": string_schema(),
            "context_id": optional_string_schema(),
            "status": optional_string_schema(),
            "participants": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["role"],
                    "properties": {
                        "node_did": optional_string_schema(),
                        "agent_id": optional_string_schema(),
                        "role": string_schema()
                    }
                }
            },
            "messages": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "role", "content"],
                    "properties": {
                        "id": string_schema(),
                        "request_id": optional_string_schema(),
                        "role": string_schema(),
                        "content": string_schema(),
                        "reasoning": optional_string_schema()
                    }
                }
            },
            "delegations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["parent_request_id", "child_request_id"],
                    "properties": {
                        "parent_request_id": string_schema(),
                        "child_request_id": string_schema(),
                        "parent_tool_call_id": optional_string_schema(),
                        "node_did": optional_string_schema(),
                        "agent_id": optional_string_schema(),
                        "status": optional_string_schema(),
                        "input": optional_string_schema()
                    }
                }
            },
            "tool_events": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "tool_name", "status"],
                    "properties": {
                        "id": string_schema(),
                        "request_id": optional_string_schema(),
                        "tool_name": string_schema(),
                        "status": string_schema(),
                        "selected_service_id": optional_string_schema(),
                        "selected_tool_name": optional_string_schema(),
                        "denial_reason": optional_string_schema(),
                        "child_request_id": optional_string_schema()
                    }
                }
            }
        }
    })
}
