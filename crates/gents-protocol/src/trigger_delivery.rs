use serde::{Deserialize, Serialize};

/// One source document delivered to one owner's trigger. Human-readable trigger
/// IDs are unique only within an owner; source IDs are scoped by collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireIdentity {
    pub owner_did: String,
    pub trigger_id: String,
    pub source_collection: String,
    pub source_doc_id: String,
}

impl FireIdentity {
    /// Character-count framing is shared with `Triggers.Durable.Identity`.
    /// Every component, including the owner, participates in admission identity.
    pub fn fire_key(&self) -> String {
        [
            &self.owner_did,
            &self.trigger_id,
            &self.source_collection,
            &self.source_doc_id,
        ]
        .into_iter()
        .map(|value| format!("{}:{value}", value.chars().count()))
        .collect()
    }

    pub fn request_id(&self) -> String {
        format!("trigger-request:{}", self.fire_key())
    }

    pub fn session_id(&self) -> String {
        format!("trigger-session:{}", self.fire_key())
    }

    pub fn outcome_id(&self) -> String {
        format!("outcome:{}", self.fire_key())
    }
}

/// The fire key and task of a `TriggerFire` row, when the row is the fire
/// `trigger` admitted for `request_id` into `session` on `node`: its key and
/// request id must be the ones its identity derives.
pub fn verified_task_fire<'a>(
    fire: &'a serde_json::Value,
    node: &str,
    session: &str,
    request_id: &str,
    trigger: &str,
) -> Option<(&'a str, &'a str)> {
    let key = fire["fire_key"]
        .as_str()
        .filter(|value| !value.is_empty())?;
    let task = fire["task_id"].as_str().filter(|value| !value.is_empty())?;
    let identity = FireIdentity {
        owner_did: node.to_owned(),
        trigger_id: trigger.to_owned(),
        source_collection: fire["source_collection"].as_str()?.to_owned(),
        source_doc_id: fire["source_doc_id"].as_str()?.to_owned(),
    };
    if fire["trigger_id"].as_str() != Some(trigger)
        || fire["session_id"].as_str() != Some(session)
        || key != identity.fire_key()
        || request_id != identity.request_id()
    {
        return None;
    }
    Some((key, task))
}

/// Admission fields are immutable and committed atomically with their AgentRequest.
/// The sole mutable claim marker is set with the deferred Goal assignment; it
/// records which assignment owns the outcome, not a second execution lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerFire {
    pub fire_key: String,
    #[serde(flatten)]
    pub identity: FireIdentity,
    pub task_id: String,
    pub request_id: String,
    pub session_id: String,
    pub goal_id: Option<String>,
    pub goal_objective: Option<String>,
    pub goal_token_budget: Option<i64>,
    /// Only the winning request claim may mark the queued assignment applied,
    /// in the transaction that updates its Goal and acquires execution.
    #[serde(default)]
    pub goal_assignment_applied: bool,
    pub emit_outcome: bool,
    pub queued_serial: bool,
    pub source_handoff_id: Option<String>,
    pub reply_session_id: Option<String>,
    pub shard_id: Option<String>,
    pub attempt: Option<i64>,
    pub created_at: String,
}

/// Immutable terminal observation for an opted-in fire. A goal-backed fire
/// observes its Goal assignment after claim; before claim, request termination
/// ends the handoff. Ordinary continuing request boundaries do not end a Goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireOutcome {
    pub handoff_id: String,
    pub fire_key: String,
    #[serde(flatten)]
    pub identity: FireIdentity,
    pub request_id: String,
    pub session_id: String,
    pub goal_id: Option<String>,
    pub terminal_state: String,
    pub reason: String,
    pub source_handoff_id: String,
    pub reply_session_id: Option<String>,
    pub shard_id: Option<String>,
    pub attempt: Option<i64>,
    pub created_at: String,
}

/// The handoff route a native workflow document (CallbackResult,
/// IsolatedWorkspace, WorkspaceReceipt) carries so that an outcome-enabled
/// consumer reads it from the observed document alone, never from the
/// producer's CallbackInvocation (Lean `Triggers.NativeRoute`). Its fields are
/// the same `handoff_id`, `reply_session_id` and `attempt` columns authored
/// collections declare.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRoute {
    #[serde(default)]
    pub handoff_id: Option<String>,
    #[serde(default)]
    pub reply_session_id: Option<String>,
    #[serde(default)]
    pub attempt: Option<i64>,
}

impl HandoffRoute {
    /// Lean `stamp (projectRoute fields)` over a JSON object: a frozen
    /// invocation input or an observed document. A blank or absent handoff
    /// carries no route; a blank reply session is absent.
    pub fn from_fields(fields: &serde_json::Value) -> Self {
        let non_blank = |field: &str| {
            fields
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        };
        let Some(handoff_id) = non_blank("handoff_id") else {
            return Self::default();
        };
        Self {
            handoff_id: Some(handoff_id),
            reply_session_id: non_blank("reply_session_id"),
            attempt: fields.get("attempt").and_then(serde_json::Value::as_i64),
        }
    }
}

/// Position in DefraDB's receiving-node document-arrival journal. Each
/// per-document consumer (trigger or callback binding) checkpoints
/// independently in a non-branchable runtime document. Advancement requires a
/// complete prefix of admitted or explicitly excluded arrivals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSourceCursor {
    pub cursor_key: String,
    pub owner_did: String,
    pub consumer: crate::event_delivery::EventConsumer,
    pub source_collection: String,
    pub after: String,
}

pub fn durable_fire_key(namespace: &str, components: &[&str]) -> String {
    let mut key = format!("{}:{namespace}", namespace.chars().count());
    for component in components {
        key.push_str(&format!(":{}:{component}", component.chars().count()));
    }
    key
}

/// One per-document event invocation per binding and arrival; an edit after
/// admission is not another arrival. Keys use `:` separators, unambiguous
/// because collection and document IDs carry none.
pub fn idempotency_key(binding_id: &str, source_collection: &str, source_doc_id: &str) -> String {
    format!("{binding_id}:{source_collection}:{source_doc_id}")
}
