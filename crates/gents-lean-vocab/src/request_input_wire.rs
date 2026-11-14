//! The `request_input_contracts_*` families carry the Lean request input with
//! every field present, an absent value as null and snake_case field names.
//! Production omits absent fields and the default queue delivery, and spells
//! capture and attachment fields in camelCase; these adapters translate between
//! the two representations without interpreting either.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

const CAPTURE_FIELDS: [(&str, &str); 3] = [
    ("captured_at", "capturedAt"),
    ("terminal_name", "terminalName"),
    ("workspace_uri", "workspaceUri"),
];
const ATTACHMENT_FIELDS: [(&str, &str); 1] = [("media_type", "mediaType")];
const UNMODELED_FIELDS: [&str; 3] = ["session_creation", "started_by", "question_reply"];
const LIST_FIELDS: [&str; 3] = ["selected_skill_ids", "captured_context", "attachments"];
const OPTIONAL_FIELDS: [&str; 5] = [
    "cwd",
    "initial_title",
    "queue",
    "goal_continuation",
    "goal_assignment",
];
const OPTIONAL_QUEUE_FIELDS: [&str; 5] = [
    "position",
    "key",
    "queued_after_request_id",
    "interrupted_request_id",
    "background_completion_wake_version",
];

/// Decodes a family's request-input wire value as the production input type.
pub fn production_request_input<T, U>(wire: &T) -> U
where
    T: Serialize,
    U: DeserializeOwned,
{
    let mut value = serde_json::to_value(wire).expect("request input wire encodes");
    respell(&mut value, true);
    serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("request input wire {value} does not decode: {error}"))
}

/// Encodes a production input as a family's request-input wire value.
pub fn request_input_wire<T, U>(production: &T) -> U
where
    T: Serialize,
    U: DeserializeOwned,
{
    let mut value = serde_json::to_value(production).expect("production request input encodes");
    let fields = value
        .as_object_mut()
        .expect("production request input is an object");
    for name in UNMODELED_FIELDS {
        assert!(
            !fields.contains_key(name),
            "the Lean request input has no {name}"
        );
    }
    for name in LIST_FIELDS {
        fields
            .entry(name)
            .or_insert_with(|| Value::Array(Vec::new()));
    }
    for name in OPTIONAL_FIELDS {
        fields.entry(name).or_insert(Value::Null);
    }
    if let Some(Value::Object(queue)) = fields.get_mut("queue") {
        queue.entry("delivery").or_insert_with(|| {
            serde_json::to_value(gents_protocol::request_input::QueueDelivery::default())
                .expect("queue delivery encodes")
        });
        for name in OPTIONAL_QUEUE_FIELDS {
            queue.entry(name).or_insert(Value::Null);
        }
    }
    respell(&mut value, false);
    serde_json::from_value(value.clone()).unwrap_or_else(|error| {
        panic!("production request input {value} is not the Lean wire: {error}")
    })
}

fn respell(value: &mut Value, to_production: bool) {
    for (list, names) in [
        ("captured_context", &CAPTURE_FIELDS[..]),
        ("attachments", &ATTACHMENT_FIELDS[..]),
    ] {
        let Some(Value::Array(items)) = value.get_mut(list) else {
            continue;
        };
        for item in items.iter_mut().filter_map(Value::as_object_mut) {
            for (wire, production) in names {
                let (from, to) = if to_production {
                    (wire, production)
                } else {
                    (production, wire)
                };
                if let Some(field) = item.remove(*from) {
                    item.insert((*to).to_owned(), field);
                }
            }
        }
    }
}
