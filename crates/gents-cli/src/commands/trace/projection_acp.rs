use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::adapter_projection::AdapterProjectionKind;
use gents::graphql::escape_graphql_string;
use gents::run_timeline::{
    RunTimeline, RunTimelineEvent, RunTimelineRows, TimelineRequestEvent, TimelineRequestRow,
    TimelineToolCallEvent,
};
use serde::Deserialize;
use serde_json::json;

use crate::config_writes::ConfigAccess;

use super::load_rows;
#[derive(Debug, Clone)]
pub(super) struct ProjectionAcpReadScope {
    actor_did: String,
    policy_id: String,
    api_base: String,
    resource_names: BTreeMap<String, String>,
}

impl ProjectionAcpReadScope {
    fn resource_name<'a>(&'a self, collection: &'a str) -> &'a str {
        self.resource_names
            .get(collection)
            .map(String::as_str)
            .unwrap_or(collection)
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectionAcpDecisionResponse {
    allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ProjectionAcpBindingRow {
    #[serde(default)]
    binding_id: String,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    projection_id: Option<String>,
    #[serde(default)]
    policy_id: String,
    #[serde(default)]
    staged_policy_id: Option<String>,
    #[serde(default)]
    previous_policy_id: Option<String>,
    #[serde(default)]
    resource_map_json: Option<String>,
    #[serde(default)]
    publication_status: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

pub(super) const PROJECTION_ACP_RUNTIME_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentResponse",
    "InferenceCall",
    "CompactionEntry",
    "AgentSession",
    "AgentConversation",
    "RenderedRequest",
];

pub(super) async fn projection_acp_read_scope(
    access: &ConfigAccess,
    policy_id: Option<&str>,
    actor_did: Option<&str>,
    projection_kind: AdapterProjectionKind,
    request: &TimelineRequestRow,
) -> Result<Option<ProjectionAcpReadScope>> {
    let (policy_id, resource_names) =
        match policy_id.map(str::trim).filter(|value| !value.is_empty()) {
            Some(policy_id) => (policy_id.to_string(), BTreeMap::new()),
            None => {
                let Some(binding) =
                    discover_projection_acp_binding(access, projection_kind, request).await?
                else {
                    return Ok(None);
                };
                (
                    binding.policy_id.trim().to_string(),
                    parse_projection_resource_map(binding.resource_map_json.as_deref())?,
                )
            }
        };
    let actor_did = actor_did
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("projection ACP enforcement requires --actor-did"))?;
    let ConfigAccess::Graphql(graphql) = access else {
        anyhow::bail!(
            "projection ACP enforcement requires --graphql so DefraDB ACP can decide documents"
        );
    };
    Ok(Some(ProjectionAcpReadScope {
        actor_did: actor_did.to_string(),
        policy_id,
        api_base: crate::graphql_access::graphql_api_base(graphql)?,
        resource_names,
    }))
}

pub(super) async fn discover_projection_acp_binding(
    access: &ConfigAccess,
    projection_kind: AdapterProjectionKind,
    request: &TimelineRequestRow,
) -> Result<Option<ProjectionAcpBindingRow>> {
    let Some(agent_did) = normalize_projection_binding_field(request.agent_did.as_deref()) else {
        return Ok(None);
    };
    let query = format!(
        r#"{{
            ProjectionAcpBinding(
                filter: {{
                    enabled: {{ _eq: true }}
                    agent_did: {{ _eq: "{agent_did}" }}
                }}
            ) {{
                binding_id
                agent_did
                behavior_id
                projection_id
                policy_id
                staged_policy_id
                previous_policy_id
                resource_map_json
                publication_status
                enabled
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let rows = load_rows::<ProjectionAcpBindingRow>(access, "ProjectionAcpBinding", &query).await?;
    select_projection_acp_binding(rows, projection_kind, request)
}

pub(super) fn select_projection_acp_binding(
    rows: Vec<ProjectionAcpBindingRow>,
    projection_kind: AdapterProjectionKind,
    request: &TimelineRequestRow,
) -> Result<Option<ProjectionAcpBindingRow>> {
    let mut best = None::<(u8, ProjectionAcpBindingRow)>;
    let projection_id = projection_kind.id();
    for row in rows {
        if row.enabled == Some(false) {
            continue;
        }
        if row.policy_id.trim().is_empty() {
            continue;
        }
        let Some(scope_mask) = projection_binding_scope_mask(&row, projection_id, request) else {
            continue;
        };
        validate_projection_binding_operational_state(&row)?;
        match &best {
            None => best = Some((scope_mask, row)),
            Some((best_mask, _)) if projection_binding_scope_dominates(scope_mask, *best_mask) => {
                best = Some((scope_mask, row));
            }
            Some((best_mask, _)) if projection_binding_scope_dominates(*best_mask, scope_mask) => {}
            Some((_, best_row)) => {
                anyhow::bail!(
                    "ambiguous ProjectionAcpBinding rows for projection {} request {}: {} and {}",
                    projection_id,
                    request.request_id,
                    projection_binding_label(best_row),
                    projection_binding_label(&row)
                );
            }
        }
    }
    Ok(best.map(|(_, row)| row))
}

pub(super) const PROJECTION_ACP_BINDING_PROJECTION_SCOPE: u8 = 0b100;
pub(super) const PROJECTION_ACP_BINDING_AGENT_SCOPE: u8 = 0b010;
pub(super) const PROJECTION_ACP_BINDING_BEHAVIOR_SCOPE: u8 = 0b001;

pub(super) fn projection_binding_scope_mask(
    row: &ProjectionAcpBindingRow,
    projection_id: &str,
    request: &TimelineRequestRow,
) -> Option<u8> {
    let row_agent_did = normalize_projection_binding_field(row.agent_did.as_deref())?;
    if request.agent_did.as_deref() != Some(row_agent_did) {
        return None;
    }
    let mut scope_mask = PROJECTION_ACP_BINDING_AGENT_SCOPE;
    if let Some(row_projection_id) =
        normalize_projection_binding_field(row.projection_id.as_deref())
    {
        if row_projection_id != projection_id {
            return None;
        }
        scope_mask |= PROJECTION_ACP_BINDING_PROJECTION_SCOPE;
    }
    if let Some(row_behavior_id) = normalize_projection_binding_field(row.behavior_id.as_deref()) {
        if request.behavior_id.as_deref() != Some(row_behavior_id) {
            return None;
        }
        scope_mask |= PROJECTION_ACP_BINDING_BEHAVIOR_SCOPE;
    }
    Some(scope_mask)
}

pub(super) fn validate_projection_binding_operational_state(row: &ProjectionAcpBindingRow) -> Result<()> {
    let status = normalize_projection_binding_field(row.publication_status.as_deref())
        .unwrap_or("published");
    let staged_policy_id = normalize_projection_binding_field(row.staged_policy_id.as_deref());
    let previous_policy_id = normalize_projection_binding_field(row.previous_policy_id.as_deref());
    let active_policy_id = row.policy_id.trim();
    match status {
        "published" => {
            if staged_policy_id.is_some() {
                anyhow::bail!(
                    "enabled ProjectionAcpBinding {} is published but still has staged_policy_id",
                    projection_binding_label(row)
                );
            }
        }
        "rotating" => {
            let Some(staged_policy_id) = staged_policy_id else {
                anyhow::bail!(
                    "enabled ProjectionAcpBinding {} is rotating but has no staged_policy_id",
                    projection_binding_label(row)
                );
            };
            if staged_policy_id == active_policy_id {
                anyhow::bail!(
                    "enabled ProjectionAcpBinding {} staged_policy_id must differ from policy_id",
                    projection_binding_label(row)
                );
            }
        }
        "draft" | "staged" | "retired" => {
            anyhow::bail!(
                "enabled ProjectionAcpBinding {} has non-operational publication_status {}; disable it or publish it before projection enforcement",
                projection_binding_label(row),
                status
            );
        }
        _ => {
            anyhow::bail!(
                "enabled ProjectionAcpBinding {} has invalid publication_status {}",
                projection_binding_label(row),
                status
            );
        }
    }
    if previous_policy_id == Some(active_policy_id) {
        anyhow::bail!(
            "enabled ProjectionAcpBinding {} previous_policy_id must differ from policy_id",
            projection_binding_label(row)
        );
    }
    if previous_policy_id == staged_policy_id && previous_policy_id.is_some() {
        anyhow::bail!(
            "enabled ProjectionAcpBinding {} previous_policy_id must differ from staged_policy_id",
            projection_binding_label(row)
        );
    }
    Ok(())
}

pub(super) fn projection_binding_scope_dominates(candidate: u8, current: u8) -> bool {
    candidate != current && (candidate & current) == current
}

pub(super) fn normalize_projection_binding_field(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(super) fn projection_binding_label(row: &ProjectionAcpBindingRow) -> &str {
    normalize_projection_binding_field(Some(&row.binding_id)).unwrap_or("<unnamed>")
}

pub(super) fn parse_projection_resource_map(
    resource_map_json: Option<&str>,
) -> Result<BTreeMap<String, String>> {
    let Some(raw) = resource_map_json
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(BTreeMap::new());
    };
    let raw_map = serde_json::from_str::<BTreeMap<String, String>>(raw)
        .context("parsing ProjectionAcpBinding.resource_map_json")?;
    let mut map = BTreeMap::new();
    for (collection, resource_name) in raw_map {
        let collection = collection.trim();
        let resource_name = resource_name.trim();
        if collection.is_empty() || resource_name.is_empty() {
            anyhow::bail!(
                "ProjectionAcpBinding.resource_map_json must map non-empty collection names to non-empty ACP resource names"
            );
        }
        if !PROJECTION_ACP_RUNTIME_COLLECTIONS.contains(&collection) {
            anyhow::bail!(
                "ProjectionAcpBinding.resource_map_json contains unknown runtime collection {collection}; expected one of {}",
                PROJECTION_ACP_RUNTIME_COLLECTIONS.join(", ")
            );
        }
        map.insert(collection.to_string(), resource_name.to_string());
    }
    Ok(map)
}

pub(super) async fn apply_projection_acp_read_filter(
    rows: RunTimelineRows,
    scope: &ProjectionAcpReadScope,
) -> Result<RunTimelineRows> {
    let mut decider = ProjectionAcpReadDecider::new(scope)?;
    let request_doc_id = required_doc_id(
        "AgentRequest",
        rows.request.request_id.as_str(),
        &rows.request.doc_id,
    )?;
    if !decider
        .read_allowed(scope.resource_name("AgentRequest"), request_doc_id)
        .await?
    {
        anyhow::bail!(
            "DefraDB ACP denied read access to root request {}",
            rows.request.request_id
        );
    }

    let mut filtered_requests = Vec::new();
    for request in rows.requests {
        let doc_id = required_doc_id("AgentRequest", request.request_id.as_str(), &request.doc_id)?;
        if decider
            .read_allowed(scope.resource_name("AgentRequest"), doc_id)
            .await?
        {
            filtered_requests.push(request);
        }
    }

    let mut filtered_messages = Vec::new();
    for message in rows.messages {
        let label = format!("{}:{}", message.session_id, message.sequence);
        let doc_id = required_doc_id("AgentMessage", &label, &message.doc_id)?;
        if decider
            .read_allowed(scope.resource_name("AgentMessage"), doc_id)
            .await?
        {
            filtered_messages.push(message);
        }
    }

    let mut filtered_tool_calls = Vec::new();
    for tool_call in rows.tool_calls {
        let doc_id = required_doc_id(
            "AgentToolCall",
            tool_call.tool_call_id.as_str(),
            &tool_call.doc_id,
        )?;
        if decider
            .read_allowed(scope.resource_name("AgentToolCall"), doc_id)
            .await?
        {
            filtered_tool_calls.push(tool_call);
        }
    }

    let mut filtered_responses = Vec::new();
    for response in rows.responses {
        let doc_id = required_doc_id(
            "AgentResponse",
            response.request_id.as_str(),
            &response.doc_id,
        )?;
        if decider
            .read_allowed(scope.resource_name("AgentResponse"), doc_id)
            .await?
        {
            filtered_responses.push(response);
        }
    }

    let mut filtered_inference_calls = Vec::new();
    for call in rows.inference_calls {
        let label = format!("{}:{}", call.request_id, call.call_seq);
        let doc_id = required_doc_id("InferenceCall", &label, &call.doc_id)?;
        if decider
            .read_allowed(scope.resource_name("InferenceCall"), doc_id)
            .await?
        {
            filtered_inference_calls.push(call);
        }
    }

    let mut filtered_compactions = Vec::new();
    for compaction in rows.compactions {
        let doc_id = required_doc_id(
            "CompactionEntry",
            compaction.compaction_key.as_str(),
            &compaction.doc_id,
        )?;
        if decider
            .read_allowed(scope.resource_name("CompactionEntry"), doc_id)
            .await?
        {
            filtered_compactions.push(compaction);
        }
    }

    let mut filtered_provider_context_reductions = Vec::new();
    for reduction in rows.provider_context_reductions {
        let doc_id = reduction.doc_id.trim();
        if doc_id.is_empty() {
            anyhow::bail!(
                "DefraDB ACP projection decisions require _docID for ProviderContextReduction {}",
                reduction.reduction_key
            );
        }
        if decider
            .read_allowed(scope.resource_name("ProviderContextReduction"), doc_id)
            .await?
        {
            filtered_provider_context_reductions.push(reduction);
        }
    }

    let session = match rows.session {
        Some(session) => {
            let doc_id =
                required_doc_id("AgentSession", session.session_id.as_str(), &session.doc_id)?;
            if decider
                .read_allowed(scope.resource_name("AgentSession"), doc_id)
                .await?
            {
                Some(session)
            } else {
                None
            }
        }
        None => None,
    };
    let conversation = match rows.conversation {
        Some(conversation) => {
            let doc_id = required_doc_id(
                "AgentConversation",
                conversation.session_id.as_str(),
                &conversation.doc_id,
            )?;
            if decider
                .read_allowed(scope.resource_name("AgentConversation"), doc_id)
                .await?
            {
                Some(conversation)
            } else {
                None
            }
        }
        None => None,
    };
    let mut filtered_rendered_request_refs = Vec::new();
    for rendered_request_ref in rows.rendered_request_refs {
        let doc_id = rendered_request_ref.doc_id.trim();
        if doc_id.is_empty() {
            anyhow::bail!(
                "DefraDB ACP projection decisions require _docID for RenderedRequest {}",
                rendered_request_ref.request_commit_cid
            );
        }
        if decider
            .read_allowed(scope.resource_name("RenderedRequest"), doc_id)
            .await?
        {
            filtered_rendered_request_refs.push(rendered_request_ref);
        }
    }

    let mut filtered_rendered_requests = Vec::new();
    for rendered_request in rows.rendered_requests {
        let doc_id = required_doc_id(
            "RenderedRequest",
            rendered_request.capture_key.as_str(),
            &rendered_request.doc_id,
        )?;
        if decider
            .read_allowed(scope.resource_name("RenderedRequest"), doc_id)
            .await?
        {
            filtered_rendered_requests.push(rendered_request);
        }
    }

    Ok(RunTimelineRows {
        request: rows.request,
        session,
        conversation,
        requests: filtered_requests,
        messages: filtered_messages,
        tool_calls: filtered_tool_calls,
        inference_calls: filtered_inference_calls,
        compactions: filtered_compactions,
        provider_context_reductions: filtered_provider_context_reductions,
        responses: filtered_responses,
        rendered_requests: filtered_rendered_requests,
        rendered_request_refs: filtered_rendered_request_refs,
    })
}

pub(super) struct ProjectionAcpReadDecider<'a> {
    scope: &'a ProjectionAcpReadScope,
    client: reqwest::Client,
    cache: BTreeMap<(String, String), bool>,
}

impl<'a> ProjectionAcpReadDecider<'a> {
    fn new(scope: &'a ProjectionAcpReadScope) -> Result<Self> {
        Ok(Self {
            scope,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .context("building ACP decision client")?,
            cache: BTreeMap::new(),
        })
    }

    async fn read_allowed(&mut self, resource_name: &str, doc_id: &str) -> Result<bool> {
        let key = (resource_name.to_string(), doc_id.to_string());
        if let Some(allowed) = self.cache.get(&key) {
            return Ok(*allowed);
        }
        let url = format!(
            "{}/acp/document/decide",
            self.scope.api_base.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .json(&json!({
                "actor": self.scope.actor_did,
                "permission": "read",
                "policyID": self.scope.policy_id,
                "resourceName": resource_name,
                "docID": doc_id,
            }))
            .send()
            .await
            .with_context(|| {
                format!("requesting DefraDB ACP read decision for {resource_name}/{doc_id}")
            })?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("reading DefraDB ACP decision response")?;
        if !status.is_success() {
            anyhow::bail!(
                "DefraDB ACP decision endpoint returned {status} for {resource_name}/{doc_id}: {text}"
            );
        }
        let decision =
            serde_json::from_str::<ProjectionAcpDecisionResponse>(&text).with_context(|| {
                format!("parsing DefraDB ACP decision response for {resource_name}/{doc_id}")
            })?;
        self.cache.insert(key, decision.allowed);
        Ok(decision.allowed)
    }
}

pub(super) fn required_doc_id<'a>(
    resource_name: &str,
    label: &str,
    doc_id: &'a Option<String>,
) -> Result<&'a str> {
    doc_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "DefraDB ACP projection decisions require _docID for {resource_name} {label}"
            )
        })
}

#[derive(Debug, Default)]
pub(super) struct ProjectionDocumentScope {
    pub(super) agent_did: Option<String>,
    pub(super) behavior_id: Option<String>,
    pub(super) session_id: Option<String>,
}

impl ProjectionDocumentScope {
    fn has_filters(&self) -> bool {
        self.agent_did.is_some() || self.behavior_id.is_some() || self.session_id.is_some()
    }

    fn description(&self) -> String {
        let mut parts = Vec::new();
        if let Some(agent_did) = self.agent_did.as_deref() {
            parts.push(format!("agent_did={agent_did}"));
        }
        if let Some(behavior_id) = self.behavior_id.as_deref() {
            parts.push(format!("behavior_id={behavior_id}"));
        }
        if let Some(session_id) = self.session_id.as_deref() {
            parts.push(format!("session_id={session_id}"));
        }
        parts.join(", ")
    }
}

pub(super) fn apply_projection_document_scope(
    mut timeline: RunTimeline,
    scope: &ProjectionDocumentScope,
) -> Result<RunTimeline> {
    if !scope.has_filters() {
        return Ok(timeline);
    }

    if !timeline_root_matches_scope(&timeline, scope) {
        anyhow::bail!(
            "projection scope denied request {} for {}",
            timeline.request_id,
            scope.description()
        );
    }

    let allowed_request_ids = scoped_request_ids(&timeline, scope);
    timeline.events.retain(|event| {
        should_keep_scoped_timeline_event(event, &timeline.request_id, &allowed_request_ids, scope)
    });
    Ok(timeline)
}

pub(super) fn timeline_root_matches_scope(timeline: &RunTimeline, scope: &ProjectionDocumentScope) -> bool {
    scope_value_matches(
        scope.agent_did.as_deref(),
        [
            timeline.request.agent_did.as_deref(),
            timeline.agent_did.as_deref(),
        ],
    ) && scope_value_matches(
        scope.behavior_id.as_deref(),
        [
            timeline.request.behavior_id.as_deref(),
            timeline.behavior_id.as_deref(),
            timeline
                .session
                .as_ref()
                .and_then(|session| session.behavior_id.as_deref()),
        ],
    ) && scope_value_matches(
        scope.session_id.as_deref(),
        [
            timeline.request.session_id.as_deref(),
            timeline.session_id.as_deref(),
        ],
    )
}

pub(super) fn scoped_request_ids(timeline: &RunTimeline, scope: &ProjectionDocumentScope) -> BTreeSet<String> {
    let mut allowed = BTreeSet::from([timeline.request_id.clone()]);
    for event in &timeline.events {
        if let RunTimelineEvent::Request(request) = event {
            if request_event_matches_scope(request, scope) {
                allowed.insert(request.request_id.clone());
            }
        }
    }
    allowed
}

pub(super) fn request_event_matches_scope(
    request: &TimelineRequestEvent,
    scope: &ProjectionDocumentScope,
) -> bool {
    scope_value_matches(scope.agent_did.as_deref(), [request.agent_did.as_deref()])
        && scope_value_matches(
            scope.behavior_id.as_deref(),
            [request.behavior_id.as_deref()],
        )
        && scope_value_matches(scope.session_id.as_deref(), [request.session_id.as_deref()])
}

pub(super) fn should_keep_scoped_timeline_event(
    event: &RunTimelineEvent,
    root_request_id: &str,
    allowed_request_ids: &BTreeSet<String>,
    scope: &ProjectionDocumentScope,
) -> bool {
    match event {
        RunTimelineEvent::Request(request) => {
            request.request_id == root_request_id
                || allowed_request_ids.contains(&request.request_id)
                || request
                    .parent_request_id
                    .as_deref()
                    .is_some_and(|parent_request_id| {
                        allowed_request_ids.contains(parent_request_id)
                    })
        }
        RunTimelineEvent::InferenceCall(call) => allowed_request_ids.contains(&call.request_id),
        RunTimelineEvent::RenderedRequest(rendered) => rendered
            .request_id
            .as_deref()
            .is_some_and(|request_id| allowed_request_ids.contains(request_id)),
        RunTimelineEvent::Compaction(compaction) => {
            allowed_request_ids.contains(&compaction.request_id)
        }
        RunTimelineEvent::ProviderContextReduction(reduction) => {
            allowed_request_ids.contains(&reduction.request_id)
        }
        RunTimelineEvent::Message(message) => scoped_request_id_allowed(
            message.request_id.as_deref(),
            Some(message.session_id.as_str()),
            allowed_request_ids,
            scope,
        ),
        RunTimelineEvent::ToolCall(tool_call) => {
            scoped_tool_call_allowed(tool_call, allowed_request_ids, scope)
        }
        RunTimelineEvent::Response(response) => allowed_request_ids.contains(&response.request_id),
    }
}

pub(super) fn scoped_tool_call_allowed(
    tool_call: &TimelineToolCallEvent,
    allowed_request_ids: &BTreeSet<String>,
    scope: &ProjectionDocumentScope,
) -> bool {
    scoped_request_id_allowed(
        tool_call.request_id.as_deref(),
        Some(tool_call.session_id.as_str()),
        allowed_request_ids,
        scope,
    )
}

pub(super) fn scoped_request_id_allowed(
    request_id: Option<&str>,
    session_id: Option<&str>,
    allowed_request_ids: &BTreeSet<String>,
    scope: &ProjectionDocumentScope,
) -> bool {
    request_id
        .map(|request_id| allowed_request_ids.contains(request_id))
        .unwrap_or_else(|| {
            scope.agent_did.is_none()
                && scope.behavior_id.is_none()
                && scope_value_matches(scope.session_id.as_deref(), [session_id])
        })
}

pub(super) fn scope_value_matches<'a>(
    expected: Option<&str>,
    actual_values: impl IntoIterator<Item = Option<&'a str>>,
) -> bool {
    let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    actual_values
        .into_iter()
        .flatten()
        .any(|actual| actual.trim() == expected)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
    use gents::run_timeline::{
        TimelineConversationRow, TimelineInferenceCallRow, TimelineMessageRow,
        TimelineRenderedRequestRef, TimelineResponseRow, TimelineSessionRow, TimelineToolCallRow,
    };
    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::*;

    #[derive(Debug, Deserialize)]
    struct MockAcpDecisionRequest {
        actor: String,
        permission: String,
        #[serde(rename = "policyID")]
        policy_id: String,
        #[serde(rename = "resourceName")]
        resource_name: String,
        #[serde(rename = "docID")]
        doc_id: String,
    }

    async fn mock_acp_decide(
        State(allowed): State<Arc<BTreeMap<(String, String), bool>>>,
        Json(body): Json<MockAcpDecisionRequest>,
    ) -> (StatusCode, Json<Value>) {
        if body.actor != "did:test:projection-reader"
            || body.permission != "read"
            || body.policy_id != "projection-policy"
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "unexpected ACP decision request" })),
            );
        }
        let allowed = allowed
            .get(&(body.resource_name, body.doc_id))
            .copied()
            .unwrap_or(false);
        (StatusCode::OK, Json(json!({ "allowed": allowed })))
    }

    async fn spawn_mock_acp(
        allowed: BTreeMap<(String, String), bool>,
    ) -> Result<ProjectionAcpReadScope> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let router = Router::new()
            .route("/api/v0/acp/document/decide", post(mock_acp_decide))
            .with_state(Arc::new(allowed));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(ProjectionAcpReadScope {
            actor_did: "did:test:projection-reader".to_string(),
            policy_id: "projection-policy".to_string(),
            api_base: format!("http://{addr}/api/v0"),
            resource_names: BTreeMap::new(),
        })
    }

    #[test]
    fn projection_acp_binding_selects_most_specific_matching_row() -> Result<()> {
        let request = TimelineRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:test:amy".to_string()),
            behavior_id: Some("amy:default".to_string()),
            ..TimelineRequestRow::default()
        };
        let selected = select_projection_acp_binding(
            vec![
                projection_binding("global", None, None, None),
                projection_binding("agent", Some("did:test:amy"), None, None),
                projection_binding(
                    "exact",
                    Some("did:test:amy"),
                    Some("amy:default"),
                    Some("openai_codex_run_trace"),
                ),
                projection_binding(
                    "other-projection",
                    Some("did:test:amy"),
                    Some("amy:default"),
                    Some("langgraph_state_history"),
                ),
            ],
            AdapterProjectionKind::OpenAiCodexRunTrace,
            &request,
        )?
        .expect("binding");

        assert_eq!(selected.binding_id, "exact");
        Ok(())
    }

    #[test]
    fn projection_acp_binding_rejects_ambiguous_rows() {
        let request = TimelineRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:test:amy".to_string()),
            ..TimelineRequestRow::default()
        };
        let error = select_projection_acp_binding(
            vec![
                projection_binding("first", Some("did:test:amy"), None, None),
                projection_binding("second", Some("did:test:amy"), None, None),
            ],
            AdapterProjectionKind::OpenAiCodexRunTrace,
            &request,
        )
        .expect_err("ambiguous rows should fail");

        assert!(
            error
                .to_string()
                .contains("ambiguous ProjectionAcpBinding rows"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn projection_acp_binding_rejects_incomparable_matching_scopes() {
        let request = TimelineRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:test:amy".to_string()),
            behavior_id: Some("amy:default".to_string()),
            ..TimelineRequestRow::default()
        };
        let error = select_projection_acp_binding(
            vec![
                projection_binding("behavior", Some("did:test:amy"), Some("amy:default"), None),
                projection_binding(
                    "projection",
                    Some("did:test:amy"),
                    None,
                    Some("openai_codex_run_trace"),
                ),
            ],
            AdapterProjectionKind::OpenAiCodexRunTrace,
            &request,
        )
        .expect_err("overlapping incomparable scopes should fail closed");

        assert!(
            error
                .to_string()
                .contains("ambiguous ProjectionAcpBinding rows"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn projection_acp_binding_ignores_unscoped_rows() -> Result<()> {
        let request = TimelineRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:test:amy".to_string()),
            ..TimelineRequestRow::default()
        };
        let selected = select_projection_acp_binding(
            vec![
                projection_binding("global", None, None, Some("openai_codex_run_trace")),
                projection_binding("agent", Some("did:test:amy"), None, None),
            ],
            AdapterProjectionKind::OpenAiCodexRunTrace,
            &request,
        )?
        .expect("agent-scoped binding");

        assert_eq!(selected.binding_id, "agent");
        Ok(())
    }

    #[test]
    fn projection_acp_binding_rejects_enabled_non_operational_status() {
        let request = TimelineRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:test:amy".to_string()),
            ..TimelineRequestRow::default()
        };
        let mut binding = projection_binding("draft", Some("did:test:amy"), None, None);
        binding.publication_status = Some("draft".to_string());

        let error = select_projection_acp_binding(
            vec![binding],
            AdapterProjectionKind::OpenAiCodexRunTrace,
            &request,
        )
        .expect_err("enabled draft binding should fail closed");

        assert!(
            error
                .to_string()
                .contains("non-operational publication_status draft"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn projection_resource_map_parses_nonempty_collection_resource_pairs() -> Result<()> {
        let map = parse_projection_resource_map(Some(
            r#"{"AgentRequest":"runtime_request"," AgentToolCall ":" runtime_tool_call "}"#,
        ))?;

        assert_eq!(
            map.get("AgentRequest").map(String::as_str),
            Some("runtime_request")
        );
        assert_eq!(
            map.get("AgentToolCall").map(String::as_str),
            Some("runtime_tool_call")
        );
        Ok(())
    }

    #[test]
    fn projection_resource_map_rejects_empty_resource_names() {
        let error = parse_projection_resource_map(Some(r#"{"AgentRequest":""}"#))
            .expect_err("empty resource names should fail");

        assert!(
            error
                .to_string()
                .contains("must map non-empty collection names"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn projection_resource_map_rejects_unknown_collection_names() {
        let error = parse_projection_resource_map(Some(r#"{"AgentMesage":"messages"}"#))
            .expect_err("unknown collection names should fail");

        assert!(
            error
                .to_string()
                .contains("unknown runtime collection AgentMesage"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn projection_acp_filter_omits_rows_denied_by_defradb_acp() -> Result<()> {
        let mut allowed = BTreeMap::new();
        for (resource_name, doc_id) in [
            ("AgentRequest", "doc-request-root"),
            ("AgentMessage", "doc-message-allowed"),
            ("AgentToolCall", "doc-tool-allowed"),
            ("AgentResponse", "doc-response-allowed"),
            ("InferenceCall", "doc-inference-allowed"),
            ("AgentConversation", "doc-conversation"),
            ("RenderedRequest", "doc-rendered-allowed"),
        ] {
            allowed.insert((resource_name.to_string(), doc_id.to_string()), true);
        }
        let scope = spawn_mock_acp(allowed).await?;

        let filtered = apply_projection_acp_read_filter(acp_fixture_rows(), &scope).await?;

        assert_eq!(
            filtered
                .requests
                .iter()
                .map(|request| request.request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["req-root"]
        );
        assert_eq!(
            filtered
                .messages
                .iter()
                .map(|message| message.sequence)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            filtered
                .tool_calls
                .iter()
                .map(|tool_call| tool_call.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["call-allowed"]
        );
        assert_eq!(
            filtered
                .responses
                .iter()
                .map(|response| response.request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["req-root"]
        );
        assert_eq!(
            filtered
                .inference_calls
                .iter()
                .map(|call| call.call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["inference-allowed"]
        );
        assert!(
            filtered.session.is_none(),
            "session row should be omitted when ACP denies it"
        );
        assert!(
            filtered.conversation.is_some(),
            "conversation row should remain when ACP allows it"
        );
        assert_eq!(filtered.rendered_request_refs.len(), 1);
        assert_eq!(
            filtered.rendered_request_refs[0].doc_id,
            "doc-rendered-allowed"
        );
        Ok(())
    }

    #[tokio::test]
    async fn projection_acp_filter_uses_configured_resource_names() -> Result<()> {
        let mut allowed = BTreeMap::new();
        for (resource_name, doc_id) in [
            ("runtime_request", "doc-request-root"),
            ("runtime_message", "doc-message-allowed"),
            ("runtime_tool_call", "doc-tool-allowed"),
            ("runtime_response", "doc-response-allowed"),
            ("runtime_inference_call", "doc-inference-allowed"),
            ("runtime_conversation", "doc-conversation"),
            ("runtime_rendered_request", "doc-rendered-allowed"),
        ] {
            allowed.insert((resource_name.to_string(), doc_id.to_string()), true);
        }
        let mut scope = spawn_mock_acp(allowed).await?;
        scope.resource_names = BTreeMap::from([
            ("AgentRequest".to_string(), "runtime_request".to_string()),
            ("AgentMessage".to_string(), "runtime_message".to_string()),
            ("AgentToolCall".to_string(), "runtime_tool_call".to_string()),
            ("AgentResponse".to_string(), "runtime_response".to_string()),
            (
                "InferenceCall".to_string(),
                "runtime_inference_call".to_string(),
            ),
            (
                "AgentConversation".to_string(),
                "runtime_conversation".to_string(),
            ),
            (
                "RenderedRequest".to_string(),
                "runtime_rendered_request".to_string(),
            ),
        ]);

        let filtered = apply_projection_acp_read_filter(acp_fixture_rows(), &scope).await?;

        assert_eq!(filtered.requests.len(), 1);
        assert_eq!(filtered.messages.len(), 1);
        assert_eq!(filtered.tool_calls.len(), 1);
        assert_eq!(filtered.inference_calls.len(), 1);
        assert_eq!(filtered.responses.len(), 1);
        assert!(filtered.conversation.is_some());
        assert_eq!(filtered.rendered_request_refs.len(), 1);
        assert!(filtered.session.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn projection_acp_filter_denies_root_request_fail_closed() -> Result<()> {
        let scope = spawn_mock_acp(BTreeMap::new()).await?;
        let error = apply_projection_acp_read_filter(acp_fixture_rows(), &scope)
            .await
            .expect_err("root request should be denied");

        assert!(
            error
                .to_string()
                .contains("DefraDB ACP denied read access to root request req-root"),
            "unexpected error: {error:#}"
        );
        Ok(())
    }

    fn acp_fixture_rows() -> RunTimelineRows {
        RunTimelineRows {
            request: TimelineRequestRow {
                doc_id: Some("doc-request-root".to_string()),
                request_id: "req-root".to_string(),
                session_id: Some("session-acp".to_string()),
                ..TimelineRequestRow::default()
            },
            session: Some(TimelineSessionRow {
                doc_id: Some("doc-session".to_string()),
                session_id: "session-acp".to_string(),
                ..TimelineSessionRow::default()
            }),
            conversation: Some(TimelineConversationRow {
                doc_id: Some("doc-conversation".to_string()),
                session_id: "session-acp".to_string(),
                ..TimelineConversationRow::default()
            }),
            requests: vec![
                TimelineRequestRow {
                    doc_id: Some("doc-request-root".to_string()),
                    request_id: "req-root".to_string(),
                    session_id: Some("session-acp".to_string()),
                    ..TimelineRequestRow::default()
                },
                TimelineRequestRow {
                    doc_id: Some("doc-request-child".to_string()),
                    request_id: "req-child".to_string(),
                    session_id: Some("session-acp".to_string()),
                    caused_by_parent_request_id: Some("req-root".to_string()),
                    ..TimelineRequestRow::default()
                },
            ],
            messages: vec![
                TimelineMessageRow {
                    doc_id: Some("doc-message-allowed".to_string()),
                    session_id: "session-acp".to_string(),
                    request_id: Some("req-root".to_string()),
                    request_doc_id: Some("doc-request-root".to_string()),
                    sequence: 1,
                    role: "user".to_string(),
                    content: "allowed".to_string(),
                    reasoning: None,
                    timestamp: None,
                },
                TimelineMessageRow {
                    doc_id: Some("doc-message-denied".to_string()),
                    session_id: "session-acp".to_string(),
                    request_id: Some("req-child".to_string()),
                    request_doc_id: Some("doc-request-child".to_string()),
                    sequence: 2,
                    role: "assistant".to_string(),
                    content: "denied".to_string(),
                    reasoning: None,
                    timestamp: None,
                },
            ],
            tool_calls: vec![
                TimelineToolCallRow {
                    doc_id: Some("doc-tool-allowed".to_string()),
                    request_id: Some("req-root".to_string()),
                    session_id: "session-acp".to_string(),
                    tool_call_id: "call-allowed".to_string(),
                    tool_name: "handoff".to_string(),
                    status: "completed".to_string(),
                    ..TimelineToolCallRow::default()
                },
                TimelineToolCallRow {
                    doc_id: Some("doc-tool-denied".to_string()),
                    request_id: Some("req-child".to_string()),
                    session_id: "session-acp".to_string(),
                    tool_call_id: "call-denied".to_string(),
                    tool_name: "review".to_string(),
                    status: "completed".to_string(),
                    ..TimelineToolCallRow::default()
                },
            ],
            responses: vec![
                TimelineResponseRow {
                    doc_id: Some("doc-response-allowed".to_string()),
                    request_id: "req-root".to_string(),
                    session_id: Some("session-acp".to_string()),
                    status: Some("completed".to_string()),
                    ..TimelineResponseRow::default()
                },
                TimelineResponseRow {
                    doc_id: Some("doc-response-denied".to_string()),
                    request_id: "req-child".to_string(),
                    session_id: Some("session-acp".to_string()),
                    status: Some("completed".to_string()),
                    ..TimelineResponseRow::default()
                },
            ],
            inference_calls: vec![
                TimelineInferenceCallRow {
                    doc_id: Some("doc-inference-allowed".to_string()),
                    call_id: "inference-allowed".to_string(),
                    request_id: "req-root".to_string(),
                    call_seq: 1,
                    attempt: 1,
                    call_state: "failed".to_string(),
                    failure_reason: Some("sensitive transient".to_string()),
                    call_kind: "inference".to_string(),
                    ..TimelineInferenceCallRow::default()
                },
                TimelineInferenceCallRow {
                    doc_id: Some("doc-inference-denied".to_string()),
                    call_id: "inference-denied".to_string(),
                    request_id: "req-child".to_string(),
                    call_seq: 1,
                    attempt: 1,
                    call_state: "completed".to_string(),
                    call_kind: "inference".to_string(),
                    ..TimelineInferenceCallRow::default()
                },
            ],
            rendered_requests: Vec::new(),
            compactions: Vec::new(),
            provider_context_reductions: Vec::new(),
            rendered_request_refs: vec![
                TimelineRenderedRequestRef {
                    doc_id: "doc-rendered-allowed".to_string(),
                    request_doc_id: "doc-request-root".to_string(),
                    request_commit_cid: "bafy-allowed".to_string(),
                },
                TimelineRenderedRequestRef {
                    doc_id: "doc-rendered-denied".to_string(),
                    request_doc_id: "doc-request-root".to_string(),
                    request_commit_cid: "bafy-denied".to_string(),
                },
            ],
        }
    }

    fn projection_binding(
        binding_id: &str,
        agent_did: Option<&str>,
        behavior_id: Option<&str>,
        projection_id: Option<&str>,
    ) -> ProjectionAcpBindingRow {
        ProjectionAcpBindingRow {
            binding_id: binding_id.to_string(),
            agent_did: agent_did.map(ToOwned::to_owned),
            behavior_id: behavior_id.map(ToOwned::to_owned),
            projection_id: projection_id.map(ToOwned::to_owned),
            policy_id: "projection-policy".to_string(),
            staged_policy_id: None,
            previous_policy_id: None,
            resource_map_json: None,
            publication_status: None,
            enabled: Some(true),
        }
    }
}
