pub(crate) use gents::config_client::{
    mint_recreate_identity, write_agent_behavior_document, write_event_trigger_document,
    write_inference_backend_document, write_inference_profile_document, write_schedule_document,
    write_task_document, write_tool_selection_document,
    write_tool_selection_document_with_clear_fields, ConfigAccess, ConfigApplyTxn,
    ExistingDocumentRef, InferenceBackendUpsertDocument,
};
