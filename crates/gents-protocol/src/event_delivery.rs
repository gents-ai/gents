//! One durable group clock shared by task triggers and callback bindings.
//! Delivery, membership and timeout decisions remain owned by the event engine.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventConsumer {
    Trigger { trigger_id: String },
    CallbackBinding { binding_id: String },
}

/// Replaces the trigger-only group-state vocabulary, not its delivery owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventGroupState {
    pub group_key: String,
    pub agent_did: String,
    pub consumer: EventConsumer,
    pub correlation: String,
    /// Fingerprint of the effective delivery configuration used by the event owner.
    pub consumer_config_key: String,
    pub first_seen_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiesced_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiesced_reason: Option<String>,
}
