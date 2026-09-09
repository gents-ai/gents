//! The durable fact record for one provider call (#840): DefraDB-backed
//! pieces only.
//!
//! The capture mechanism itself (the arm/claim scope, the capturing
//! transport, the pure DTO builder) moved to `gents-loop` (G-1); this module
//! re-exports it so `crate::rendered_request` keeps every symbol this crate's
//! callers already use, and keeps the two pieces that cannot move: the
//! DefraDB-backed sink (`sink`, `commits`) and the admission-controller
//! provenance lookup, since a guest has neither a database nor an admission
//! controller.

pub mod commits;
pub mod sink;

pub use gents_loop::rendered_request::{
    scope, transport, AdmissionJoin, AssemblyBuildPath, AssemblyTrace, AssistantMessageId,
    CaptureOrderKey, CaptureScope, CaptureScopeKind, CaptureSeam, ContextAccounting,
    ContextCompactionReason, ContextInputComponents, ParsedProvenance, ProvenanceManifest,
    ProvenanceStatus, RenderedCompletionRequest, RenderedRequestCaptureFactory,
    RenderedRequestCaptureSink, RenderedRequestCapturingHttpClient, RenderedRequestComponents,
    RenderedRequestContext, RenderedRequestSource, ThreadedToolResult,
    ASSEMBLY_TRACE_VERSION, CAPTURE_VERSION, CONTEXT_ACCOUNTING_VERSION,
    PROVENANCE_MANIFEST_VERSION,
};
pub use gents_loop::rendered_request::{build_rendered_completion_request, capture_key};
pub(crate) use sink::defra_rendered_request_capture_factory;
pub use sink::DefraRenderedRequestSink;

/// The admission identity to stamp into this capture's provenance, if the call
/// in flight on this task belongs to the loop the capture describes.
///
/// The kind guard keeps the task-local admission join honest if a caller wires
/// the wrong capture scope. One-shot captures never join because no admission
/// scope exists there. Installed onto every production `RequestCaptureScope`
/// via `scope_from_factory` below; the loop's own default is `|_| None`.
fn admission_join_for_scope(capture_scope: &str) -> Option<AdmissionJoin> {
    let join = crate::admission::current_call_join()?;
    let scope_kind = capture_scope.parse::<CaptureScope>().ok()?.kind;
    admission_kind_matches_scope(join.call_kind, scope_kind).then(|| AdmissionJoin {
        call_id: join.call_id,
        call_seq: join.call_seq,
    })
}

/// Which admission [`CallKind`](crate::admission::CallKind) legitimately
/// produces captures of which [`CaptureScopeKind`]. `OneShot` maps to nothing:
/// one-shot runs have no admission scope at all, so a join observed under a
/// oneshot capture could only be another loop's call.
pub(crate) fn admission_kind_matches_scope(
    call_kind: crate::admission::CallKind,
    scope_kind: CaptureScopeKind,
) -> bool {
    use crate::admission::CallKind;

    matches!(
        (call_kind, scope_kind),
        (CallKind::Inference, CaptureScopeKind::Inference)
            | (CallKind::Compaction, CaptureScopeKind::Compaction)
            | (CallKind::Compaction, CaptureScopeKind::CompactionFallback)
            | (CallKind::OneOff, CaptureScopeKind::Title)
    )
}

/// `RenderedRequestContext` for a claimed durable request. The loop-side
/// struct dropped this constructor (it names the native `AgentRequest`
/// document type); this is its native replacement.
pub(crate) fn context_for_claimed_request(
    request: &crate::watcher::AgentRequest,
    request_commit_cid: &str,
    model_name: String,
) -> RenderedRequestContext {
    RenderedRequestContext {
        request_doc_id: request.doc_id.clone(),
        request_commit_cid: request_commit_cid.to_string(),
        request_id: request.request_id.clone(),
        agent_did: request.agent_did.clone(),
        requester_did: request.requester_did.clone().unwrap_or_default(),
        behavior_id: request.behavior_id.clone().unwrap_or_default(),
        session_id: request.session_id.clone(),
        model_name,
    }
}

/// Build a capture scope from a context and an optional factory, with the
/// native admission-join lookup installed. `None` when capture is not
/// configured.
pub(crate) fn scope_from_factory(
    context: RenderedRequestContext,
    factory: Option<&RenderedRequestCaptureFactory>,
) -> Option<std::sync::Arc<scope::RequestCaptureScope>> {
    let factory = factory?;
    let sink = factory(context.clone());
    Some(std::sync::Arc::new(
        scope::RequestCaptureScope::new(context, sink)
            .with_admission_join_lookup(std::sync::Arc::new(admission_join_for_scope)),
    ))
}

/// Run `future` under a capture scope (with the native admission-join lookup
/// installed) when one can be built, and unchanged otherwise.
pub(crate) async fn scope_request_if_configured<T>(
    context: RenderedRequestContext,
    factory: Option<&RenderedRequestCaptureFactory>,
    future: impl std::future::Future<Output = T>,
) -> T {
    match scope_from_factory(context, factory) {
        Some(scope) => scope::scope_request(scope, future).await,
        None => future.await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_request() -> crate::watcher::AgentRequest {
        crate::watcher::AgentRequest {
            doc_id: "doc-1".to_string(),
            request_id: "request-1".to_string(),
            agent_did: "did:key:test".to_string(),
            requester_did: None,
            behavior_id: Some("behavior".to_string()),
            session_id: "session".to_string(),
            content: "hi".to_string(),
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            seed: None,
            max_total_tokens: None,
            metadata: None,
            execution_origin: None,
            created_at: String::new(),
            deadline: None,
            execution_generation: None,
            execution_lease_expires_at: None,
            execution_progress_seq: 0,
            subagent_depth: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_request_doc_id: None,
            caused_by_parent_tool_call_id: None,
            caused_by_parent_tool_call_doc_id: None,
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            caused_by_source_doc_id: None,
            caused_by_correlation: None,
            caused_by_trigger_context: None,
            workspace_id: None,
            workspace_authority: None,
            workspace_owner_deployment_id: None,
            workspace_seal_hash: None,
        }
    }

    #[test]
    fn context_for_request_carries_an_absent_requester_as_empty() {
        let mut request = agent_request();
        request.requester_did = None;
        let context = context_for_claimed_request(&request, "", "test-model".to_string());
        assert_eq!(context.requester_did, "");

        request.requester_did = Some("did:key:requester".to_string());
        let context = context_for_claimed_request(&request, "", "test-model".to_string());
        assert_eq!(context.requester_did, "did:key:requester");
    }

    /// The kind guard: a join is stamped only when the admitted call's kind
    /// legitimately produces the capture's loop. A wrong join would be worse
    /// than none.
    #[test]
    fn admission_kinds_map_to_their_capture_scopes() {
        use crate::admission::CallKind;

        let cases = [
            (CallKind::Inference, CaptureScopeKind::Inference, true),
            (CallKind::Inference, CaptureScopeKind::Compaction, false),
            (CallKind::Compaction, CaptureScopeKind::Compaction, true),
            (
                CallKind::Compaction,
                CaptureScopeKind::CompactionFallback,
                true,
            ),
            (CallKind::Compaction, CaptureScopeKind::Inference, false),
            (CallKind::OneOff, CaptureScopeKind::Title, true),
            (CallKind::OneOff, CaptureScopeKind::OneShot, false),
            (CallKind::Inference, CaptureScopeKind::OneShot, false),
            (CallKind::Scheduled, CaptureScopeKind::Inference, false),
        ];
        for (call_kind, scope_kind, expected) in cases {
            assert_eq!(
                admission_kind_matches_scope(call_kind, scope_kind),
                expected,
                "{call_kind:?} vs {scope_kind:?}"
            );
        }
    }
}
