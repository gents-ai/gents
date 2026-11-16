use crate::backends::registry::backend_fields;
use crate::resolved::ResolvedAgent;

pub fn agent_connection_fingerprint(agent_config: &ResolvedAgent) -> String {
    crate::admission::backend_connection_fingerprint(&crate::backends::registry::BackendFields {
        backend_id: agent_config.backend_id.clone(),
        backend_provider_kind: agent_config.backend_provider_kind,
        openai_wire_api: agent_config.openai_wire_api,
        backend_endpoint: agent_config.backend_endpoint.clone(),
        backend_auth: agent_config.backend_auth.clone(),
        claude_platform: agent_config.claude_platform.clone(),
    })
}

/// Identity of everything an agent_config slot builds once and keeps: its resolved
/// configuration plus the keyed connection identity of its inference and
/// compaction clients. `ResolvedAgent`'s Debug redacts credentials, so a
/// key rotation is visible only through the keyed fingerprints.
pub fn agent_slot_fingerprint(agent_config: &ResolvedAgent) -> String {
    let compaction = agent_config.compaction_inference.as_ref().map(|inference| {
        crate::admission::backend_connection_fingerprint(&backend_fields(&inference.backend))
    });
    format!(
        "{agent_config:?}|connection={}|compaction_connection={compaction:?}",
        agent_connection_fingerprint(agent_config)
    )
}
