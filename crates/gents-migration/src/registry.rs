use crate::expectation::CollectionExpectation;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy)]
pub struct BaselineCollection<'a> {
    pub name: &'a str,
    pub sdl: &'a str,
    pub expected_version: Option<&'a str>,
    pub expected_state: CollectionExpectation,
}

#[derive(Debug, Clone, Copy)]
pub struct LensSpec<'a> {
    pub wasm: &'a [u8],
    pub args_json: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub enum MigrationStep<'a> {
    AddCollection {
        id: &'a str,
        sdl: &'a str,
        expected_version: Option<&'a str>,
        expected_state: CollectionExpectation,
    },
    PatchVersioned {
        id: &'a str,
        collection: &'a str,
        patch: &'a str,
        lens: Option<LensSpec<'a>>,
        expected_version: Option<&'a str>,
        expected_transform: Option<&'a str>,
        expected_state: CollectionExpectation,
    },
    PatchInPlace {
        id: &'a str,
        collection: &'a str,
        patch: &'a str,
        expected_state: CollectionExpectation,
    },
}

impl<'a> MigrationStep<'a> {
    pub fn id(&self) -> &'a str {
        match self {
            Self::AddCollection { id, .. }
            | Self::PatchVersioned { id, .. }
            | Self::PatchInPlace { id, .. } => id,
        }
    }

    pub fn collection(&self) -> Option<&'a str> {
        match self {
            Self::AddCollection { .. } => None,
            Self::PatchVersioned { collection, .. } | Self::PatchInPlace { collection, .. } => {
                Some(*collection)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Registry<'a> {
    pub baseline: &'a [BaselineCollection<'a>],
    pub steps: &'a [MigrationStep<'a>],
    pub ordinary_indexes: &'a [OrdinaryIndex<'a>],
}

#[derive(Debug, Clone, Copy)]
pub struct OrdinaryIndex<'a> {
    pub collection: &'a str,
    pub name: &'a str,
    pub fields: &'a [OrdinaryIndexField<'a>],
}

#[derive(Debug, Clone, Copy)]
pub struct OrdinaryIndexField<'a> {
    pub name: &'a str,
    pub descending: bool,
}

impl<'a> Registry<'a> {
    pub fn managed_names(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.baseline.iter().map(|b| b.name)
    }
}

#[derive(Debug, Clone)]
pub struct BaselineCollectionOwned {
    pub name: String,
    pub sdl: String,
    pub expected_version: Option<String>,
    pub expected_state: CollectionExpectation,
}

#[derive(Debug, Clone)]
pub struct LensSpecOwned {
    pub wasm: Vec<u8>,
    pub args_json: Option<String>,
}

#[derive(Debug, Clone)]
pub enum MigrationStepOwned {
    AddCollection {
        id: String,
        sdl: String,
        expected_version: Option<String>,
        expected_state: CollectionExpectation,
    },
    PatchVersioned {
        id: String,
        collection: String,
        patch: String,
        lens: Option<LensSpecOwned>,
        expected_version: Option<String>,
        expected_transform: Option<String>,
        expected_state: CollectionExpectation,
    },
    PatchInPlace {
        id: String,
        collection: String,
        patch: String,
        expected_state: CollectionExpectation,
    },
}

#[derive(Debug, Clone, Default)]
pub struct DynamicRegistry {
    pub baseline: Vec<BaselineCollectionOwned>,
    pub steps: Vec<MigrationStepOwned>,
}

impl DynamicRegistry {
    pub fn as_registry(&self) -> (Vec<BaselineCollection<'_>>, Vec<MigrationStep<'_>>) {
        let baseline = self
            .baseline
            .iter()
            .map(|b| BaselineCollection {
                name: b.name.as_str(),
                sdl: b.sdl.as_str(),
                expected_version: b.expected_version.as_deref(),
                expected_state: b.expected_state,
            })
            .collect();
        let steps = self
            .steps
            .iter()
            .map(|s| match s {
                MigrationStepOwned::AddCollection {
                    id,
                    sdl,
                    expected_version,
                    expected_state,
                } => MigrationStep::AddCollection {
                    id: id.as_str(),
                    sdl: sdl.as_str(),
                    expected_version: expected_version.as_deref(),
                    expected_state: *expected_state,
                },
                MigrationStepOwned::PatchVersioned {
                    id,
                    collection,
                    patch,
                    lens,
                    expected_version,
                    expected_transform,
                    expected_state,
                } => MigrationStep::PatchVersioned {
                    id: id.as_str(),
                    collection: collection.as_str(),
                    patch: patch.as_str(),
                    lens: lens.as_ref().map(|l| LensSpec {
                        wasm: l.wasm.as_slice(),
                        args_json: l.args_json.as_deref(),
                    }),
                    expected_version: expected_version.as_deref(),
                    expected_transform: expected_transform.as_deref(),
                    expected_state: *expected_state,
                },
                MigrationStepOwned::PatchInPlace {
                    id,
                    collection,
                    patch,
                    expected_state,
                } => MigrationStep::PatchInPlace {
                    id: id.as_str(),
                    collection: collection.as_str(),
                    patch: patch.as_str(),
                    expected_state: *expected_state,
                },
            })
            .collect();
        (baseline, steps)
    }
}

static BASELINE_SCHEMAS: LazyLock<Vec<BaselineCollection<'static>>> = LazyLock::new(|| {
    use gents_protocol::schemas::{
        ALL, ALL_COLLECTION_NAMES, RUNTIME_ALL, RUNTIME_COLLECTION_NAMES,
    };

    assert_eq!(RUNTIME_COLLECTION_NAMES.len(), RUNTIME_ALL.len());
    assert_eq!(ALL_COLLECTION_NAMES.len(), ALL.len());
    RUNTIME_COLLECTION_NAMES
        .iter()
        .copied()
        .zip(RUNTIME_ALL.iter().copied())
        .chain(
            ALL_COLLECTION_NAMES
                .iter()
                .copied()
                .zip(ALL.iter().copied()),
        )
        .map(|(name, sdl)| BaselineCollection {
            name,
            sdl,
            expected_version: None,
            expected_state: CollectionExpectation::dag_only(),
        })
        .collect()
});

pub static DEFAULT_STEPS: &[MigrationStep<'static>] = &[];

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

pub static DEFAULT_ORDINARY_INDEXES: &[OrdinaryIndex<'static>] = &[
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

// The native parser derives version IDs from the complete bundled SDL, including
// policy bindings and relation identities. Keep the parsed roots alive for the
// static registry instead of maintaining a second list of version IDs.
static BASELINE_ROOTS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let sdl = BASELINE_SCHEMAS
        .iter()
        .map(|entry| entry.sdl)
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = query::parse_sdl(&sdl).expect("bundled baseline SDL must parse");
    assert_eq!(
        parsed.len(),
        BASELINE_SCHEMAS.len(),
        "bundled baseline collection count"
    );
    BASELINE_SCHEMAS
        .iter()
        .map(|entry| {
            let mut matches = parsed.iter().filter(|schema| schema.name == entry.name);
            let schema = matches
                .next()
                .expect("bundled baseline collection must exist");
            assert!(
                matches.next().is_none(),
                "duplicate bundled baseline collection"
            );
            schema.version_id.clone()
        })
        .collect()
});

pub static DEFAULT_BASELINE: LazyLock<Vec<BaselineCollection<'static>>> = LazyLock::new(|| {
    BASELINE_SCHEMAS
        .iter()
        .zip(BASELINE_ROOTS.iter())
        .map(|(entry, root)| BaselineCollection {
            expected_version: Some(root.as_str()),
            ..*entry
        })
        .collect()
});

pub static DEFAULT_REGISTRY: LazyLock<Registry<'static>> = LazyLock::new(|| Registry {
    baseline: DEFAULT_BASELINE.as_slice(),
    steps: DEFAULT_STEPS,
    ordinary_indexes: DEFAULT_ORDINARY_INDEXES,
});

pub fn fixture_lens_wasm() -> &'static [u8] {
    include_bytes!(env!("GENTS_LENS_FIXTURE_ADD_LABEL_WASM_PATH"))
}

pub const CLIENT_AUTHORED_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::PEER_REGISTRY_NAME,
    gents_protocol::schemas::AGENT_REQUEST_NAME,
    gents_protocol::schemas::AGENT_MESSAGE_NAME,
    gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
    gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
    gents_protocol::schemas::AGENT_SESSION_NAME,
    gents_protocol::schemas::COMPACTION_ENTRY_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME,
    gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
    gents_protocol::schemas::FILE_INVENTORY_REQUEST_NAME,
    gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT_NAME,
    gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL_NAME,
    gents_protocol::schemas::AGENT_SESSION_GOAL_RECEIPT_NAME,
    gents_protocol::schemas::AGENT_TOOL_CONTROL_NAME,
    gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME,
    gents_protocol::schemas::MAILBOX_ITEM_NAME,
];
