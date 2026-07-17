//! Thin re-export of the shared control-plane write client.
//!
//! The per-collection writers, `ConfigAccess`, and `ConfigApplyTxn` moved to
//! `defra_agent::config_client` (#654) so the runtime self-configuration
//! tools and the CLI apply/imperative paths share one proven write path.

pub(crate) use defra_agent::config_client::{
    mint_recreate_identity, mint_recreate_identity_timestamp, write_agent_behavior_document,
    write_event_trigger_document, write_inference_backend_document, write_schedule_document,
    write_task_document, write_tool_selection_document,
    write_tool_selection_document_with_clear_fields, ConfigAccess, ConfigApplyTxn,
    ExistingDocumentRef, InferenceBackendUpsertDocument,
};
