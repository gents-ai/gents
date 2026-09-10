use serde::Deserialize;
use ts_rs::TS;

/// Local-runtime init request. Filesystem paths are **not** accepted from the
/// webview — they come from `BridgeConfig` resolved at plugin init.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInitRequest {
    pub label: Option<String>,
    pub dangerously_overwrite: bool,
    pub reset: bool,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ManagedServerStartRequest {
    pub agent_name: String,
}

/// Fetch peer runtime status by **saved peer id** only — read grants never
/// accept arbitrary addresses (SSRF).
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PeerStatusFetchRequest {
    pub peer_id: String,
}

/// Fleet-admin request to authenticate a server status offer and author a
/// pending enrollment request. Lives only in `fleet-admin`.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentStatusRequest {
    pub server_address: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendRequest {
    pub agent_did: String,
    pub behavior_id: Option<String>,
    pub session_id: Option<String>,
    pub content: String,
    #[serde(default)]
    pub caused_by_source_doc_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MailboxItemRequest {
    pub item_id: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRenameRequest {
    pub agent_did: String,
    pub session_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct AgentConfigSaveRequest {
    pub document: gents::document_config::AgentPrincipal,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct BehaviorSaveRequest {
    pub document: gents::AgentBehaviorDocument,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillDeleteRequest {
    pub skill_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskDeleteRequest {
    pub task_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleDeleteRequest {
    pub schedule_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EventTriggerDeleteRequest {
    pub trigger_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackendDeleteRequest {
    pub backend_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InferenceProfileDeleteRequest {
    pub profile_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolsDeleteRequest {
    pub tools_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolServiceDeleteRequest {
    pub service_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorDeleteRequest {
    pub behavior_id: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct BackendSaveRequest {
    pub document: gents::InferenceBackend,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct InferenceProfileSaveRequest {
    pub document: gents::InferenceProfile,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ToolsSaveRequest {
    pub document: gents::Tools,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ToolServiceSaveRequest {
    pub document: gents::document_config::ToolServiceRegistry,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolServiceTestRequest {
    pub service_id: String,
    pub hostname: Option<String>,
    pub tailscale_ip: Option<String>,
    pub lan_ip: Option<String>,
    pub mcp_port: Option<i64>,
    pub mcp_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct TaskSaveRequest {
    pub document: gents::document_config::Task,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct SkillSaveRequest {
    pub document: gents::document_config::SkillDocument,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunRequest {
    pub task_id: String,
    #[ts(type = "unknown", optional)]
    pub args: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ScheduleSaveRequest {
    pub document: gents::document_config::Schedule,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRunRequest {
    pub schedule_id: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct EventTriggerSaveRequest {
    pub document: gents::document_config::Trigger,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopOperationsSnapshotRequest {
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    /// Accepted from the client but not yet consumed: snapshot filtering by
    /// root request / terminal inclusion is staged (operator-surfaces spec).
    #[allow(dead_code)]
    pub root_request_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub include_terminal: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopListSubagentTreeRequest {
    pub root_request_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub include_terminal: Option<bool>,
    #[serde(default)]
    pub max_depth: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPreviewInterruptCascadeRequest {
    pub request_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub include_terminal: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInterruptRequest {
    pub request_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    /// Currently always `"userCancelled"` per spec line 907. Kept as a String
    /// so future cause variants don't require an enum migration here.
    pub cause: String,
    pub cascade: bool,
    #[serde(default)]
    pub expected_preview_signature: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopProbeMcpServiceRequest {
    pub service_id: String,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    macro_rules! assert_source_routed_delete_request {
        ($request_type:ty, $id_key:literal, $id_field:ident, $id:literal) => {{
            let mut value = json!({ "agentDid": "did:test:source" });
            value[$id_key] = json!($id);
            let request: $request_type = serde_json::from_value(value).unwrap();

            assert_eq!(request.$id_field, $id);
            assert_eq!(request.agent_did, "did:test:source");

            let mut missing_source = json!({});
            missing_source[$id_key] = json!($id);
            assert!(serde_json::from_value::<$request_type>(missing_source).is_err());
        }};
    }

    #[test]
    fn tool_goal_capabilities_use_canonical_optional_group() {
        let omitted: ToolsSaveRequest =
            serde_json::from_value(json!({"document":{"agent_did":"owner","tools_id":"tools"}}))
                .unwrap();
        assert!(omitted.document.built_ins.is_none());
        let explicit: ToolsSaveRequest = serde_json::from_value(json!({"document":{"agent_did":"owner","tools_id":"tools","built_ins":{"enable_goal_tools":true,"enable_goal_creation":false}}})).unwrap();
        let built_ins = explicit.document.built_ins.unwrap();
        assert_eq!(built_ins.enable_goal_tools, Some(true));
        assert_eq!(built_ins.enable_goal_creation, Some(false));
        let explicit_null: ToolsSaveRequest = serde_json::from_value(
            json!({"document":{"agent_did":"owner","tools_id":"tools","built_ins":null}}),
        )
        .unwrap();
        assert!(explicit_null.document.built_ins.is_none());
    }

    #[test]
    fn task_goal_fields_use_canonical_replacement_optionality() {
        let mut value = json!({"document":{"agent_did":"owner","task_id":"task","behavior_id":"behavior","prompt_template":"Do work"}});
        let omitted: TaskSaveRequest = serde_json::from_value(value.clone()).unwrap();
        assert!(omitted.document.goal_objective_template.is_none());
        value["document"]["goal_objective_template"] = Value::Null;
        value["document"]["goal_token_budget"] = Value::Null;
        let cleared: TaskSaveRequest = serde_json::from_value(value.clone()).unwrap();
        assert!(cleared.document.goal_objective_template.is_none());
        assert!(cleared.document.goal_token_budget.is_none());
        value["document"]["goal_objective_template"] = json!("Finish work");
        value["document"]["goal_token_budget"] = json!(10000);
        let explicit: TaskSaveRequest = serde_json::from_value(value).unwrap();
        assert_eq!(
            explicit.document.goal_objective_template.as_deref(),
            Some("Finish work")
        );
        assert_eq!(explicit.document.goal_token_budget, Some(10000));
    }

    #[test]
    fn config_delete_requests_require_camel_case_source_agent_did() {
        assert_source_routed_delete_request!(SkillDeleteRequest, "skillId", skill_id, "skill-a");
        assert_source_routed_delete_request!(TaskDeleteRequest, "taskId", task_id, "task-a");
        assert_source_routed_delete_request!(
            ScheduleDeleteRequest,
            "scheduleId",
            schedule_id,
            "schedule-a"
        );
        assert_source_routed_delete_request!(
            EventTriggerDeleteRequest,
            "triggerId",
            trigger_id,
            "trigger-a"
        );
        assert_source_routed_delete_request!(
            BackendDeleteRequest,
            "backendId",
            backend_id,
            "backend-a"
        );
        assert_source_routed_delete_request!(
            InferenceProfileDeleteRequest,
            "profileId",
            profile_id,
            "profile-a"
        );
        assert_source_routed_delete_request!(
            ToolsDeleteRequest,
            "toolsId",
            tools_id,
            "selection-a"
        );
        assert_source_routed_delete_request!(
            ToolServiceDeleteRequest,
            "serviceId",
            service_id,
            "service-a"
        );
        assert_source_routed_delete_request!(
            BehaviorDeleteRequest,
            "behaviorId",
            behavior_id,
            "behavior-a"
        );
    }
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct EventSourceSaveRequest {
    pub document: gents::document_config::EventSource,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct EventSourceDeleteRequest {
    pub event_source_id: String,
    pub agent_did: String,
}

/// Atomic component edits for an existing principal, not pack installation.
/// Supply agent_principal with only agent_did/default-valued fields. Principal
/// settings use AgentConfigSaveRequest. Omitted documents remain untouched;
/// graph_intents/graph_capabilities are rejected, not compiled or ignored.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ConfigComponentsApplyRequest {
    pub document: gents::document_config::PackConfig,
}

/// Existing-document edits only. Canonical patch admission protects identities;
/// omitted values remain unchanged, including redacted credentials. All edits
/// are validated and committed together. No implicit creation or removal.
#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ConfigComponentsPatchRequest {
    pub agent_did: String,
    pub patches: Vec<ConfigComponentPatch>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(tag = "collection")]
#[serde(deny_unknown_fields)]
pub enum ConfigComponentPatch {
    AgentBehavior {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./AgentBehavior.js\").AgentBehavior, \"agent_did\" | \"behavior_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    AgentContext {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./AgentContext.js\").AgentContext, \"agent_did\" | \"context_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    Compaction {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./CompactionConfig.js\").CompactionConfig, \"agent_did\" | \"compaction_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    Tools {
        id: String,
        #[ts(type = "Partial<Omit<import(\"./Tools.js\").Tools, \"agent_did\" | \"tools_id\">>")]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    InferenceProfile {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./InferenceProfile.js\").InferenceProfile, \"agent_did\" | \"profile_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    InferenceSampling {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./InferenceSampling.js\").InferenceSampling, \"agent_did\" | \"sampling_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    InferenceExecution {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./InferenceExecution.js\").InferenceExecution, \"agent_did\" | \"execution_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    InferenceRetryPolicy {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./InferenceRetryPolicy.js\").InferenceRetryPolicy, \"agent_did\" | \"retry_policy_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    InferenceBackend {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./InferenceBackend.js\").InferenceBackend, \"agent_did\" | \"backend_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    ToolServiceRegistry {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./ToolServiceRegistry.js\").ToolServiceRegistry, \"agent_did\" | \"service_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    Task {
        id: String,
        #[ts(type = "Partial<Omit<import(\"./Task.js\").Task, \"agent_did\" | \"task_id\">>")]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    Schedule {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./Schedule.js\").Schedule, \"agent_did\" | \"schedule_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    Trigger {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./Trigger.js\").Trigger, \"agent_did\" | \"trigger_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
    EventSource {
        id: String,
        #[ts(
            type = "Partial<Omit<import(\"./EventSource.js\").EventSource, \"agent_did\" | \"event_source_id\">>"
        )]
        changes: serde_json::Map<String, serde_json::Value>,
    },
}

impl ConfigComponentPatch {
    pub(crate) fn into_patch(
        self,
    ) -> (
        gents::config_client::patch::SelfConfigTarget,
        String,
        gents::config_client::patch::SelfConfigPatch,
    ) {
        use gents::config_client::patch::SelfConfigTarget;
        let (target, id, changes) = match self {
            Self::AgentBehavior { id, changes } => (SelfConfigTarget::AgentBehavior, id, changes),
            Self::AgentContext { id, changes } => (SelfConfigTarget::AgentContext, id, changes),
            Self::Compaction { id, changes } => (SelfConfigTarget::Compaction, id, changes),
            Self::Tools { id, changes } => (SelfConfigTarget::Tools, id, changes),
            Self::InferenceProfile { id, changes } => {
                (SelfConfigTarget::InferenceProfile, id, changes)
            }
            Self::InferenceSampling { id, changes } => {
                (SelfConfigTarget::InferenceSampling, id, changes)
            }
            Self::InferenceExecution { id, changes } => {
                (SelfConfigTarget::InferenceExecution, id, changes)
            }
            Self::InferenceRetryPolicy { id, changes } => {
                (SelfConfigTarget::InferenceRetryPolicy, id, changes)
            }
            Self::InferenceBackend { id, changes } => {
                (SelfConfigTarget::InferenceBackend, id, changes)
            }
            Self::ToolServiceRegistry { id, changes } => {
                (SelfConfigTarget::ToolServiceRegistry, id, changes)
            }
            Self::Task { id, changes } => (SelfConfigTarget::Task, id, changes),
            Self::Schedule { id, changes } => (SelfConfigTarget::Schedule, id, changes),
            Self::Trigger { id, changes } => (SelfConfigTarget::Trigger, id, changes),
            Self::EventSource { id, changes } => (SelfConfigTarget::EventSource, id, changes),
        };
        (
            target,
            id,
            changes
                .into_iter()
                .map(|(field, value)| (field, Some(value)))
                .collect(),
        )
    }
}
