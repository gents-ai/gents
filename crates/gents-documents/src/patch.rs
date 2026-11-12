use serde_json::{Map, Value};

/// Canonical self-configuration targets; collection metadata belongs to the shared catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfConfigTarget {
    Agent,
    AgentContext,
    Compaction,
    Tools,
    AgentTarget,
    InferenceProfile,
    InferenceSampling,
    InferenceExecution,
    InferenceRetryPolicy,
    InferenceBackend,
    ToolServiceRegistry,
    Task,
    Schedule,
    Trigger,
    EventSource,
    DatastoreToolSurface,
    Skill,
    Page,
    PluginSource,
}
pub const SELF_CONFIG_CATEGORIES: [&str; 8] = [
    "agent",
    "tools",
    "profile",
    "backend",
    "mcp_service",
    "automation",
    "node",
    "policy",
];
pub const DEFAULT_SELF_CONFIG_CATEGORIES: [&str; 3] = ["agent", "tools", "profile"];
pub const ALL_SELF_CONFIG_TARGETS: [SelfConfigTarget; 19] = [
    SelfConfigTarget::Agent,
    SelfConfigTarget::AgentContext,
    SelfConfigTarget::Compaction,
    SelfConfigTarget::Tools,
    SelfConfigTarget::AgentTarget,
    SelfConfigTarget::InferenceProfile,
    SelfConfigTarget::InferenceSampling,
    SelfConfigTarget::InferenceExecution,
    SelfConfigTarget::InferenceRetryPolicy,
    SelfConfigTarget::InferenceBackend,
    SelfConfigTarget::ToolServiceRegistry,
    SelfConfigTarget::Task,
    SelfConfigTarget::Schedule,
    SelfConfigTarget::Trigger,
    SelfConfigTarget::EventSource,
    SelfConfigTarget::DatastoreToolSurface,
    SelfConfigTarget::Skill,
    SelfConfigTarget::Page,
    SelfConfigTarget::PluginSource,
];
impl SelfConfigTarget {
    pub fn collection(self) -> gents_store::collection::Collection {
        match self {
            Self::Agent => gents_store::collection::Collection::Agent,
            Self::AgentContext => gents_store::collection::Collection::AgentContext,
            Self::Compaction => gents_store::collection::Collection::Compaction,
            Self::Tools => gents_store::collection::Collection::Tools,
            Self::AgentTarget => gents_store::collection::Collection::AgentTarget,
            Self::InferenceProfile => gents_store::collection::Collection::InferenceProfile,
            Self::InferenceSampling => gents_store::collection::Collection::InferenceSampling,
            Self::InferenceExecution => gents_store::collection::Collection::InferenceExecution,
            Self::InferenceRetryPolicy => gents_store::collection::Collection::InferenceRetryPolicy,
            Self::InferenceBackend => gents_store::collection::Collection::InferenceBackend,
            Self::ToolServiceRegistry => gents_store::collection::Collection::ToolServiceRegistry,
            Self::Task => gents_store::collection::Collection::Task,
            Self::Schedule => gents_store::collection::Collection::Schedule,
            Self::Trigger => gents_store::collection::Collection::Trigger,
            Self::EventSource => gents_store::collection::Collection::EventSource,
            Self::DatastoreToolSurface => gents_store::collection::Collection::DatastoreToolSurface,
            Self::Skill => gents_store::collection::Collection::Skill,
            Self::Page => gents_store::collection::Collection::Page,
            Self::PluginSource => gents_store::collection::Collection::PluginSource,
        }
    }
    pub fn collection_name(self) -> &'static str {
        self.collection().graphql_type()
    }
    pub fn unique_field(self) -> &'static str {
        self.collection().unique_field()
    }
    pub fn from_collection_name(name: &str) -> Option<Self> {
        ALL_SELF_CONFIG_TARGETS
            .into_iter()
            .find(|target| target.collection_name() == name)
    }
    pub fn category(self) -> &'static str {
        match self {
            Self::Agent | Self::AgentContext => "agent",
            Self::Tools
            | Self::AgentTarget
            | Self::DatastoreToolSurface
            | Self::Skill
            | Self::Page
            | Self::PluginSource => "tools",
            Self::Compaction
            | Self::InferenceProfile
            | Self::InferenceSampling
            | Self::InferenceExecution
            | Self::InferenceRetryPolicy => "profile",
            Self::InferenceBackend => "backend",
            Self::ToolServiceRegistry => "mcp_service",
            Self::Task | Self::Schedule | Self::Trigger | Self::EventSource => "automation",
        }
    }
    pub fn all_fields(self) -> &'static [&'static str] {
        super::config_projection(self.collection(), None)
            .expect("canonical config metadata")
            .0
    }
    pub fn writable_fields(self) -> Vec<&'static str> {
        self.all_fields()
            .iter()
            .copied()
            .filter(|field| {
                *field != self.unique_field()
                    && !(self == Self::Task && *field == "agent_did")
                    && !(self == Self::Agent && *field == "creation_nonce")
                    && ![
                        "node_did",
                        "created_at",
                        "updated_at",
                        "created_by",
                        "artifact_bytes",
                        "canonical_args",
                        "signer_did",
                        "provenance",
                    ]
                    .contains(field)
            })
            .collect()
    }
    pub fn protected_fields(self) -> Vec<&'static str> {
        self.all_fields()
            .iter()
            .copied()
            .filter(|field| !self.is_writable(field))
            .collect()
    }
    pub fn is_writable(self, field: &str) -> bool {
        self.writable_fields().contains(&field)
    }
}

/// A self-config patch: field → `Some(value)` to set, `None` to clear. Later entries win.
pub type SelfConfigPatch = Vec<(String, Option<Value>)>;

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("field {field} on {collection} is protected (identity, runtime-owned, secret, or apply-managed) and cannot be patched via self-config{recovery}")]
    ProtectedField {
        field: String,
        collection: &'static str,
        recovery: &'static str,
    },
    #[error("unknown field {field} for {collection}; writable fields: {writable}{recovery}")]
    UnknownField {
        field: String,
        collection: &'static str,
        writable: String,
        recovery: &'static str,
    },
}

pub fn ensure_admissible(
    target: SelfConfigTarget,
    patch: &SelfConfigPatch,
) -> Result<(), PatchError> {
    for (field, _) in patch {
        if !target.is_writable(field) {
            if target.all_fields().contains(&field.as_str()) {
                let recovery = if target == SelfConfigTarget::Task && field == "agent_did" {
                    "; choose the task owner with options.agent when creating it, not set.agent_did. To run a different agent, create a separate Task and Trigger; keep the current workflow until the replacement is valid"
                } else if field == target.unique_field() {
                    "; put the document ID in target_id, not set. IDs cannot be changed by update"
                } else {
                    ""
                };
                return Err(PatchError::ProtectedField {
                    field: field.clone(),
                    collection: target.collection_name(),
                    recovery,
                });
            }
            let recovery = if target == SelfConfigTarget::Agent
                && matches!(field.as_str(), "system_prompt" | "instructions")
            {
                "; prompts belong to Context.system_prompt: agent get shows context_id, then context update that ID with set.system_prompt"
            } else if target == SelfConfigTarget::EventSource && field == "concurrency" {
                "; concurrency belongs to Trigger: trigger list then trigger get shows source bindings; update the matching Trigger with set.concurrency"
            } else if target == SelfConfigTarget::Trigger && field == "emit_outcome" {
                "; emit_outcome belongs to Task: trigger get shows task_id, then task update that ID with set.emit_outcome"
            } else if target == SelfConfigTarget::DatastoreToolSurface
                && field == "output_obligation"
            {
                "; output_obligation belongs inside an entries create declaration, not on the surface. It requires stored output; omit it for inbox writers. See help datastore"
            } else if target == SelfConfigTarget::Tools
                && matches!(field.as_str(), "root" | "files" | "bash")
            {
                "; host tool fields belong inside set.host. Read tools get first and preserve the other fields in that group"
            } else if target == SelfConfigTarget::AgentTarget && field == "display_name" {
                "; use set.name for the name passed to agent_new"
            } else if target == SelfConfigTarget::DatastoreToolSurface && field == "name" {
                "; use set.display_name for the surface label; each entry has its own tool_name"
            } else if target == SelfConfigTarget::InferenceProfile
                && SelfConfigTarget::InferenceSampling.is_writable(field)
            {
                "; sampling fields belong to InferenceSampling: profile get shows sampling_id, then sampling update that ID"
            } else {
                ""
            };
            return Err(PatchError::UnknownField {
                field: field.clone(),
                collection: target.collection_name(),
                writable: target.writable_fields().join(", "),
                recovery,
            });
        }
    }
    Ok(())
}

pub fn apply_patch(
    target: SelfConfigTarget,
    doc: &Map<String, Value>,
    patch: &SelfConfigPatch,
) -> Map<String, Value> {
    let mut merged = doc.clone();
    for (field, value) in patch {
        if !target.is_writable(field) {
            continue;
        }
        match value {
            Some(value) => {
                merged.insert(field.clone(), value.clone());
            }
            None => {
                merged.remove(field);
            }
        }
    }
    merged
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FieldDelta {
    pub field: String,
    pub from: Value,
    pub to: Value,
}

pub fn diff_docs(
    target: SelfConfigTarget,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Vec<FieldDelta> {
    target
        .all_fields()
        .iter()
        .filter_map(|field| {
            let from = before.get(*field).cloned().unwrap_or(Value::Null);
            let to = after.get(*field).cloned().unwrap_or(Value::Null);
            (from != to).then(|| FieldDelta {
                field: (*field).to_string(),
                from,
                to,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn contracts_self_config_field_table_matches_lean() {
        let mut modeled = std::collections::BTreeSet::new();
        for case in gents_lean_vocab::families::contracts_self_config_field_table::cases() {
            modeled.insert(case.input.collection.clone());
            let owned = |fields: &[&str]| -> Vec<String> {
                fields.iter().map(|field| (*field).to_owned()).collect()
            };
            case.check(
                SelfConfigTarget::from_collection_name(&case.input.collection).map(|target| {
                    gents_lean_vocab::families::contracts_self_config_field_table::FieldTable {
                        unique_field: target.unique_field().to_owned(),
                        category: target.category().to_owned(),
                        all_fields: owned(target.all_fields()),
                        writable_fields: owned(&target.writable_fields()),
                        protected_fields: owned(&target.protected_fields()),
                    }
                }),
            );
        }
        let runtime = ALL_SELF_CONFIG_TARGETS
            .iter()
            .map(|target| target.collection_name().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(runtime, modeled, "runtime self-config target inventory");
    }

    #[test]
    fn merge_sets_clears_and_contains() {
        let doc = json!({
            "agent_did": "beh-1",
            "node_did": "did:key:z6M",
            "display_name": "old",
            "description": "m-small",
        });
        let Value::Object(doc) = doc else {
            unreachable!()
        };
        let patch: SelfConfigPatch = vec![
            ("display_name".to_string(), Some(json!("new"))),
            ("description".to_string(), None),
            ("node_did".to_string(), Some(json!("did:key:attacker"))),
        ];
        let merged = apply_patch(SelfConfigTarget::Agent, &doc, &patch);
        assert_eq!(merged.get("display_name"), Some(&json!("new")));
        assert!(!merged.contains_key("description"));
        assert_eq!(
            merged.get("node_did"),
            Some(&json!("did:key:z6M")),
            "protected field must survive even an inadmissible entry"
        );
        assert!(ensure_admissible(SelfConfigTarget::Agent, &patch).is_err());

        let deltas = diff_docs(SelfConfigTarget::Agent, &doc, &merged);
        assert_eq!(
            deltas,
            vec![
                FieldDelta {
                    field: "display_name".into(),
                    from: json!("old"),
                    to: json!("new"),
                },
                FieldDelta {
                    field: "description".into(),
                    from: json!("m-small"),
                    to: Value::Null,
                },
            ]
        );
    }
}
