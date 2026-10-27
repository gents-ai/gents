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
        for name in [
            "task_id",
            "parent_request_doc_id",
            "opening_request_doc_id",
        ] {
            object.insert(name.into(), model_id(&value["provenance"][name]));
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
            "requester_did": model_id(&value["scope"]["requester"]), "agent_id": model_id(&value["agent"]),
            "created_at": model_time(&value["created_at"]), "closed_at": model_time(&value["closed_at"]),
            "stopped_at": model_time(&value["stopped_at"]),
            "archived_at": model_time(&value["archived_at"]),
            "archive_revision": value["archive_revision"].as_u64().unwrap_or(0),
            "title": value["title"], "tags": value["tags"], "provenance": provenance, "observation": observation,
        })).unwrap()
}
