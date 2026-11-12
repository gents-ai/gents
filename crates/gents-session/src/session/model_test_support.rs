pub(super) fn model_id(value: &serde_json::Value) -> serde_json::Value {
    if value.is_null() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(value.to_string())
    }
}

pub(super) fn model_time(value: &serde_json::Value) -> serde_json::Value {
    value
        .as_i64()
        .map(|seconds| {
            serde_json::Value::String(
                chrono::DateTime::from_timestamp(seconds, 0)
                    .unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )
        })
        .unwrap_or(serde_json::Value::Null)
}

pub(super) fn model_session(value: &serde_json::Value) -> gents_protocol::session::AgentSession {
    let mut provenance = value["provenance"].clone();
    if let Some(object) = provenance.as_object_mut() {
        for name in ["task_id", "parent_request_doc_id", "opening_request_doc_id"] {
            object.insert(name.into(), model_id(&value["provenance"][name]));
        }
        let starter = &value["provenance"]["started_by"];
        if !starter.is_null() {
            object.insert(
                "started_by".into(),
                serde_json::json!({
                    "node_did": model_id(&starter["node"]),
                    "session_id": model_id(&starter["session"]),
                    "requester_did": model_id(&starter["requester"]),
                }),
            );
        }
        if let Some(fork) = object
            .get_mut("fork")
            .and_then(serde_json::Value::as_object_mut)
        {
            fork.insert(
                "source_session_id".into(),
                model_id(&value["provenance"]["fork"]["source_session_id"]),
            );
        }
    }
    let observation = if value["observation"].is_null() {
        serde_json::Value::Null
    } else {
        let latest = &value["observation"]["latest"];
        serde_json::json!({
            "last_activity_at": model_time(&value["observation"]["activity"]),
            "preview": value["observation"]["preview"],
            "latest_request": if latest.is_null() {serde_json::Value::Null} else {serde_json::json!({
                "request_doc_id": model_id(&latest["doc_id"]), "request_id": model_id(&latest["request_id"]),
                "lifecycle_state": latest["state"],
            })},
        })
    };
    serde_json::from_value(serde_json::json!({
            "session_id": model_id(&value["scope"]["session"]), "node_did": model_id(&value["scope"]["node"]),
            "requester_did": model_id(&value["scope"]["requester"]), "agent_did": model_id(&value["agent"]),
            "created_at": model_time(&value["created_at"]), "closed_at": model_time(&value["closed_at"]),
            "stopped_at": model_time(&value["stopped_at"]),
            "archived_at": model_time(&value["archived_at"]),
            "archive_revision": value["archive_revision"].as_u64().unwrap_or(0),
            "title": value["title"], "tags": value["tags"], "provenance": provenance, "observation": observation,
        })).unwrap()
}

pub(super) fn wire_session(session: &gents_protocol::session::AgentSession) -> serde_json::Value {
    use serde_json::{json, Value};
    let value = serde_json::to_value(session).unwrap();
    let id = |value: &Value| {
        value
            .as_str()
            .map_or(Value::Null, |text| serde_json::from_str(text).unwrap())
    };
    let time = |value: &Value| {
        value.as_str().map_or(Value::Null, |text| {
            json!(chrono::DateTime::parse_from_rfc3339(text)
                .unwrap()
                .timestamp())
        })
    };
    let scope = |node: &Value, session: &Value, requester: &Value| json!({"node": id(node), "session": id(session), "requester": id(requester)});
    let provenance = &value["provenance"];
    let starter = &provenance["started_by"];
    let fork = &provenance["fork"];
    let observation = &value["observation"];
    let latest = &observation["latest_request"];
    json!({
        "scope": scope(&value["node_did"], &value["session_id"], &value["requester_did"]),
        "agent": id(&value["agent_did"]),
        "created_at": time(&value["created_at"]),
        "closed_at": time(&value["closed_at"]),
        "stopped_at": time(&value["stopped_at"]),
        "archived_at": time(&value["archived_at"]),
        "archive_revision": value["archive_revision"].as_u64().unwrap_or(0),
        "provenance": if provenance.is_null() { Value::Null } else { json!({
            "task_id": id(&provenance["task_id"]),
            "parent_request_doc_id": id(&provenance["parent_request_doc_id"]),
            "opening_request_doc_id": id(&provenance["opening_request_doc_id"]),
            "started_by": if starter.is_null() { Value::Null } else {
                scope(&starter["node_did"], &starter["session_id"], &starter["requester_did"])
            },
            "fork": if fork.is_null() { Value::Null } else { json!({
                "source_session_id": id(&fork["source_session_id"]),
                "at_user_turn": fork["at_user_turn"],
            })},
        })},
        "tags": if value["tags"].is_null() { json!([]) } else { value["tags"].clone() },
        "title": value["title"],
        "observation": if observation.is_null() { Value::Null } else { json!({
            "activity": time(&observation["last_activity_at"]),
            "preview": observation["preview"],
            "latest": if latest.is_null() { Value::Null } else { json!({
                "doc_id": id(&latest["request_doc_id"]),
                "request_id": id(&latest["request_id"]),
                "state": latest["lifecycle_state"],
            })},
        })},
    })
}
