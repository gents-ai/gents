use super::{OrdinaryIndex, OrdinaryIndexField};

static AGENT_REQUEST_NODE_SESSION_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
];

static AGENT_REQUEST_TRIGGER_HISTORY_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "caused_by_trigger_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "purpose",
        descending: false,
    },
    OrdinaryIndexField {
        name: "created_at",
        descending: true,
    },
    OrdinaryIndexField {
        name: "request_id",
        descending: true,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
];

static AGENT_REQUEST_PREDECESSOR_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "requester_did",
        descending: false,
    },
];

static AGENT_REQUEST_RETRY_CHILD_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "retry_parent_request",
        descending: false,
    },
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "requester_did",
        descending: false,
    },
];

static MAILBOX_NODE_STATUS_ITEM_KEY_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "status",
        descending: false,
    },
    OrdinaryIndexField {
        name: "item_key",
        descending: false,
    },
];

static AGENT_OUTPUT_SOURCE_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_doc_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "source",
        descending: false,
    },
];

static AGENT_OUTPUT_SOURCE_KEY_ORDINAL_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_doc_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "source_key",
        descending: false,
    },
    OrdinaryIndexField {
        name: "ordinal",
        descending: true,
    },
];

static AGENT_OUTPUT_CLOSE_SOURCE_KEY_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_doc_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "is_close",
        descending: false,
    },
    OrdinaryIndexField {
        name: "source_key",
        descending: false,
    },
];

static AGENT_OUTPUT_CREATED_SOURCE_KEY_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_doc_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "created_at",
        descending: false,
    },
    OrdinaryIndexField {
        name: "source_key",
        descending: false,
    },
];

static MESSAGE_SESSION_SEQUENCE_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "sequence",
        descending: false,
    },
];

static TOOL_CALL_SESSION_ORDER_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "message_sequence",
        descending: false,
    },
    OrdinaryIndexField {
        name: "tool_call_key",
        descending: false,
    },
];

static TOOL_CALL_BACKGROUND_STATUS_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "await_mode",
        descending: false,
    },
    OrdinaryIndexField {
        name: "status",
        descending: false,
    },
];

static SESSION_ARCHIVED_AT_FIELDS: &[OrdinaryIndexField<'static>] = &[OrdinaryIndexField {
    name: "archived_at",
    descending: false,
}];

static SESSION_STOPPED_AT_FIELDS: &[OrdinaryIndexField<'static>] = &[OrdinaryIndexField {
    name: "stopped_at",
    descending: false,
}];

/// The metrics scrape reads terminal inference calls that ended inside its
/// trailing window; without this range every scrape read every call.
static INFERENCE_CALL_TERMINAL_WINDOW_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "call_kind",
        descending: false,
    },
    OrdinaryIndexField {
        name: "ended_at",
        descending: false,
    },
];

static RENDERED_REQUEST_CALL_FIELDS: &[OrdinaryIndexField<'static>] = &[
    OrdinaryIndexField {
        name: "request_doc_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "node_did",
        descending: false,
    },
    OrdinaryIndexField {
        name: "session_id",
        descending: false,
    },
    OrdinaryIndexField {
        name: "turn_index",
        descending: false,
    },
    OrdinaryIndexField {
        name: "attempt",
        descending: false,
    },
];

pub static ORDINARY_INDEXES: &[OrdinaryIndex<'static>] = &[
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_SESSION_NAME,
        name: "AgentSession_started_by_node_did_ASC",
        fields: &[OrdinaryIndexField {
            name: "started_by_node_did",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_SESSION_NAME,
        name: "AgentSession_started_by_session_id_ASC",
        fields: &[OrdinaryIndexField {
            name: "started_by_session_id",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        name: "InferenceCall_session_id_ASC",
        fields: &[OrdinaryIndexField {
            name: "session_id",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        name: "InferenceCall_model_name_ASC",
        fields: &[OrdinaryIndexField {
            name: "model_name",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        name: "InferenceCall_profile_id_ASC",
        fields: &[OrdinaryIndexField {
            name: "profile_id",
            descending: false,
        }],
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        name: "InferenceCall_kind_ended_at",
        fields: INFERENCE_CALL_TERMINAL_WINDOW_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
        name: "AgentToolCall_session_sequence_key",
        fields: TOOL_CALL_SESSION_ORDER_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_MESSAGE_NAME,
        name: "AgentMessage_session_sequence",
        fields: MESSAGE_SESSION_SEQUENCE_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
        name: "AgentToolCall_node_await_status",
        fields: TOOL_CALL_BACKGROUND_STATUS_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_SESSION_NAME,
        name: "AgentSession_stopped_at_ASC",
        fields: SESSION_STOPPED_AT_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_SESSION_NAME,
        name: "AgentSession_archived_at_ASC",
        fields: SESSION_ARCHIVED_AT_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
        name: "AgentOutputSegment_request_doc_id_source",
        fields: AGENT_OUTPUT_SOURCE_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
        name: "AgentOutputSegment_request_source_key_ordinal",
        fields: AGENT_OUTPUT_SOURCE_KEY_ORDINAL_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
        name: "AgentOutputSegment_request_close_source_key",
        fields: AGENT_OUTPUT_CLOSE_SOURCE_KEY_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
        name: "AgentOutputSegment_request_created_source_key",
        fields: AGENT_OUTPUT_CREATED_SOURCE_KEY_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        name: "AgentRequest_node_did_session_id",
        fields: AGENT_REQUEST_NODE_SESSION_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        name: "AgentRequest_node_trigger_created_request",
        fields: AGENT_REQUEST_TRIGGER_HISTORY_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        name: "AgentRequest_request_node_session_requester",
        fields: AGENT_REQUEST_PREDECESSOR_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        name: "AgentRequest_retry_parent_node_session_requester",
        fields: AGENT_REQUEST_RETRY_CHILD_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::MAILBOX_ITEM_NAME,
        name: "MailboxItem_node_status_item_key",
        fields: MAILBOX_NODE_STATUS_ITEM_KEY_FIELDS,
    },
    OrdinaryIndex {
        collection: gents_protocol::schemas::RENDERED_REQUEST_NAME,
        name: "RenderedRequest_request_node_session_turn_attempt",
        fields: RENDERED_REQUEST_CALL_FIELDS,
    },
];
