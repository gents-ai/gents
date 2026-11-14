use std::error::Error;

use serde::Serialize;

use super::{NextCall, StringUpdate};

/// What the Agent materializer did, as its receipt names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentOperationKind {
    Create,
    Clone,
    Edit,
    Disable,
}

impl AgentOperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Clone => "clone",
            Self::Edit => "edit",
            Self::Disable => "disable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCreateOperation {
    Create,
    Clone,
}

impl AgentCreateOperation {
    pub fn for_verb(verb: &str) -> Option<Self> {
        [Self::Create, Self::Clone]
            .into_iter()
            .find(|operation| operation.declaration().verb == verb)
    }

    pub fn kind(self) -> AgentOperationKind {
        match self {
            Self::Create => AgentOperationKind::Create,
            Self::Clone => AgentOperationKind::Clone,
        }
    }

    pub fn declaration(self) -> &'static AgentCreateDeclaration {
        match self {
            Self::Create => &AGENT_CREATE,
            Self::Clone => &AGENT_CLONE,
        }
    }
}

/// The caller's request for a new Agent. The Agent's identity binding is not
/// part of it: each front end obtains one through its own authorized route
/// and passes it beside the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCreateInput {
    pub operation: AgentCreateOperation,
    /// The enabled Agent whose Context and Tools a clone copies. Core refuses
    /// a clone without it and ignores it on create.
    pub clone_from: Option<String>,
    pub display_name: String,
    /// On a clone, `Omitted` keeps the source's value and `Clear` removes it.
    pub description: StringUpdate,
    /// On a clone, `Omitted` keeps the source's value and `Clear` removes it.
    pub system_prompt: StringUpdate,
    pub profile_id: String,
    pub preset: Option<String>,
    /// On a clone, `Omitted` keeps the source's value and `Clear` removes it.
    pub root: StringUpdate,
    pub make_default: bool,
}

/// Lean `SelfConfig.AgentEditFields`: the one agent edit, over every writable
/// Agent field and `system_prompt`, which writes the Agent's selected Context.
/// Disabling is `enabled: Some(false)`; an omitted field is preserved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentEdit {
    pub display_name: StringUpdate,
    pub description: StringUpdate,
    pub system_prompt: StringUpdate,
    pub inference_profile_id: StringUpdate,
    /// `Clear` selects no Context.
    pub context_id: StringUpdate,
    pub enabled: Option<bool>,
    /// Replaces the whole list; an empty list clears it.
    pub tags: Option<Vec<String>>,
}

/// Lean `SelfConfig.agentOutcome` for a preview. A refused preview is a
/// successful answer carrying the refusal, the message and the next call the
/// refused commit's error would carry (`preview_refusal_matches_commit`).
/// Failures that are not admission refusals stay errors.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentOutcome<R> {
    Admitted(R),
    Refused {
        refusal: AgentRefusal,
        message: String,
        next_call: NextCall,
    },
}

/// The materializer's answer. `A` is the Agent document as the caller's crate
/// types it. Fields serialize in declaration order, which is the order every
/// front end shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentReceipt<A> {
    pub committed: bool,
    pub admitted: bool,
    pub operation: AgentOperationKind,
    pub agent_did: String,
    /// Root admission's own step for the User, present only when the new
    /// Agent's host tools wait on a WorkspaceRoot reader grant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_root_access: Option<serde_json::Value>,
    pub agent: A,
    pub make_default: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentContextPatch {
    pub context_id: String,
    pub changes: serde_json::Map<String, serde_json::Value>,
}

/// Lean `SelfConfig.AgentRefusal`: the admission check that refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRefusal {
    /// The actor lacks `nodes manage_agents`.
    CatalogNotHeld,
    /// The actor lacks `agents update` on the target.
    AgentNotHeld,
    TargetExists,
    TargetMissing,
    InvalidInput,
    /// The request would leave the node's default agent disabled.
    DefaultDisabled,
    /// The request would disable the acting actor.
    SelfLockout,
}

impl AgentRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CatalogNotHeld => "catalog_not_held",
            Self::AgentNotHeld => "agent_not_held",
            Self::TargetExists => "target_exists",
            Self::TargetMissing => "target_missing",
            Self::InvalidInput => "invalid_input",
            Self::DefaultDisabled => "default_disabled",
            Self::SelfLockout => "self_lockout",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentOperationError {
    #[error("agent preview requires operation create, clone, edit, or disable")]
    PreviewRequiresOperation,
    #[error("agent edit does not accept clone_from; use agent clone")]
    EditRejectsCloneFrom,
    #[error("agent edit changes Agent fields and its system_prompt; change root and tool grants through tools update")]
    EditRejectsToolsFields,
    #[error("agent disable accepts only the target ID")]
    DisableAcceptsOnlyTarget,
    #[error("clone requires from")]
    CloneRequiresFrom,
    #[error("unknown agent operation {0:?}")]
    UnknownOperation(String),
    #[error("agent creation requires a runtime-provisioned identity binding")]
    IdentityBindingRequired,
    #[error("agent creation identity binding missing")]
    IdentityBindingMissing,
    #[error("the identity binding names agent {binding:?}, not the supplied Agent {agent:?}; provision a binding for this Agent")]
    IdentityBindingMismatch { binding: String, agent: String },
    #[error("a new Agent is created enabled; save it enabled, then disable it with an edit")]
    NewAgentDisabled,
    #[error("agent operation requires an exact agent_did")]
    AgentDidRequired,
    #[error("agent {0:?} already exists")]
    AgentExists(String),
    #[error("display_name is required")]
    DisplayNameRequired,
    #[error("select a published inference profile")]
    ProfileNotPublished,
    #[error("fresh agents require system_prompt")]
    SystemPromptRequired,
    #[error("clone source must be an enabled agent")]
    CloneSourceNotEnabled,
    #[error("agent {0:?} does not exist")]
    AgentMissing(String),
    #[error("creating or cloning an agent and choosing the default agent need the agent catalog (manage_agents on the Node), which this actor does not hold and cannot grant itself; it comes only with administration of the Node: the Node's owner or an administrator adds this actor as administrator on the Node")]
    CatalogNotHeld,
    #[error("this actor may not configure agent {0:?}: it holds no update grant on that Agent; configure an agent it created or its own, or ask a User who administers {0:?} to make the change")]
    AgentNotHeld(String),
    #[error("unsupported agent edit field {0:?}")]
    UnsupportedEditField(String),
    #[error("display_name cannot be empty")]
    DisplayNameEmpty,
    #[error("system_prompt cannot be blank")]
    SystemPromptBlank,
    #[error("context_id cannot be blank; clear it to select no context")]
    ContextIdBlank,
    #[error("agent {0:?} is or would become the default agent, which must stay enabled; choose another enabled default first, or set enabled true in the same edit")]
    DefaultAgentDisabled(String),
    #[error("an actor cannot disable itself; another actor holding a grant on this agent can")]
    SelfDisable,
    #[error("missing {collection} {id:?}")]
    MissingDocument {
        collection: &'static str,
        id: String,
    },
    #[error("generated context ID already exists")]
    GeneratedContextIdExists,
    #[error("generated tools ID already exists")]
    GeneratedToolsIdExists,
    #[error("clone inherits its source tools; edit the cloned Tools to change grants")]
    ClonePresetRefused,
    #[error("unknown permission preset")]
    UnknownPreset,
    #[error("clone source {from:?} carries an operator grant this agent does not hold; clone a source without it or ask the operator to grant it")]
    CloneSourceGrantNotHeld {
        from: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error("{0} object required")]
    ObjectRequired(&'static str),
    #[error("the context patch must name the context selected by the saved Agent")]
    ContextPatchMismatch,
    #[error("agent has no context; create and select a context first")]
    AgentHasNoContext,
    #[error("system_prompt edit requires an unshared context")]
    SystemPromptContextShared,
    #[error("agent admission was observed for a different actor than the transaction's")]
    AdmissionActorMismatch,
    #[error("candidate identity missing")]
    CandidateIdentityMissing,
    #[error("candidate agent does not resolve; inspect its context, profile and backend")]
    CandidateDoesNotResolve,
    /// Another owner's failure (root policy, the account and compaction
    /// fences, plan validation, the store), unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
}

impl AgentOperationError {
    /// The call that lets the caller correct this failure of `operation`.
    pub fn recovery(&self, operation: AgentCreateOperation) -> NextCall {
        match self {
            Self::PreviewRequiresOperation
            | Self::EditRejectsCloneFrom
            | Self::EditRejectsToolsFields
            | Self::DisableAcceptsOnlyTarget
            | Self::CloneRequiresFrom
            | Self::UnknownOperation(_)
            | Self::IdentityBindingRequired
            | Self::IdentityBindingMissing
            | Self::IdentityBindingMismatch { .. }
            | Self::NewAgentDisabled
            | Self::AgentDidRequired
            | Self::AgentExists(_)
            | Self::DisplayNameRequired
            | Self::ProfileNotPublished
            | Self::SystemPromptRequired
            | Self::CloneSourceNotEnabled
            | Self::AgentMissing(_)
            | Self::UnsupportedEditField(_)
            | Self::DisplayNameEmpty
            | Self::SystemPromptBlank
            | Self::ContextIdBlank
            | Self::MissingDocument { .. }
            | Self::GeneratedContextIdExists
            | Self::GeneratedToolsIdExists
            | Self::ClonePresetRefused
            | Self::UnknownPreset
            | Self::CloneSourceGrantNotHeld { .. }
            | Self::ObjectRequired(_)
            | Self::ContextPatchMismatch
            | Self::AgentHasNoContext
            | Self::SystemPromptContextShared
            | Self::CandidateIdentityMissing
            | Self::AdmissionActorMismatch
            | Self::CandidateDoesNotResolve
            | Self::Passthrough(_) => operation.declaration().help,
            Self::CatalogNotHeld | Self::AgentNotHeld(_) | Self::SelfDisable => {
                NextCall::Inspect { resource: "agent" }
            }
            Self::DefaultAgentDisabled(_) => NextCall::List { resource: "agent" },
        }
    }

    /// The call that lets the caller correct this failure of an edit,
    /// disable or default choice on an existing Agent.
    pub fn edit_recovery(&self) -> NextCall {
        match self {
            Self::CatalogNotHeld | Self::AgentNotHeld(_) | Self::SelfDisable => {
                NextCall::Inspect { resource: "agent" }
            }
            Self::DefaultAgentDisabled(_) | Self::AgentMissing(_) => {
                NextCall::List { resource: "agent" }
            }
            _ => NextCall::VerbHelp {
                resource: "agent",
                verb: "update",
            },
        }
    }

    /// Lean `SelfConfig.AgentRefusal`: which admission check refused, when
    /// this failure is an admission refusal.
    pub fn refusal(&self) -> Option<AgentRefusal> {
        match self {
            Self::CatalogNotHeld => Some(AgentRefusal::CatalogNotHeld),
            Self::AgentNotHeld(_) => Some(AgentRefusal::AgentNotHeld),
            Self::AgentExists(_) => Some(AgentRefusal::TargetExists),
            Self::AgentMissing(_) => Some(AgentRefusal::TargetMissing),
            Self::DisplayNameRequired
            | Self::ProfileNotPublished
            | Self::SystemPromptRequired
            | Self::CloneSourceNotEnabled
            | Self::UnsupportedEditField(_)
            | Self::DisplayNameEmpty
            | Self::SystemPromptBlank
            | Self::ContextIdBlank => Some(AgentRefusal::InvalidInput),
            Self::DefaultAgentDisabled(_) => Some(AgentRefusal::DefaultDisabled),
            Self::SelfDisable => Some(AgentRefusal::SelfLockout),
            _ => None,
        }
    }

    /// Lean `SelfConfig.agentOutcome` in preview mode: an admission refusal
    /// becomes the successful refused answer, with `next_call` the recovery
    /// the commit's error declares; any other failure stays an error.
    pub fn preview_outcome<R>(self, next_call: NextCall) -> Result<AgentOutcome<R>, Self> {
        match self.refusal() {
            Some(refusal) => Ok(AgentOutcome::Refused {
                refusal,
                message: crate::error_chain::ErrorChain(&self).to_string(),
                next_call,
            }),
            None => Err(self),
        }
    }

    /// The other owner's failure a `Passthrough` carries. `source()` skips
    /// it, so a boundary that classifies that owner's errors asks here.
    pub fn passthrough(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
        match self {
            Self::Passthrough(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionRequirement {
    /// Core refuses the operation without it.
    Required,
    /// Core accepts its absence; help lists it with the required options.
    Expected,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationOption {
    pub name: &'static str,
    pub requirement: OptionRequirement,
    pub values: Option<&'static str>,
}

const fn option(name: &'static str, requirement: OptionRequirement) -> OperationOption {
    OperationOption {
        name,
        requirement,
        values: None,
    }
}

/// What ACP and the node grants already enforce for the operation. It
/// describes those owners; it is not a second authorization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationPermission {
    pub document_access: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentCreateDeclaration {
    pub name: &'static str,
    pub resource: &'static str,
    pub verb: &'static str,
    pub options: &'static [OperationOption],
    /// A remark help prints after the leading options.
    pub option_note: Option<&'static str>,
    pub switches: &'static [&'static str],
    /// The sentence a front end adds when it refuses an argument.
    pub usage: &'static str,
    /// Lines verb help prints after the syntax line.
    pub notes: &'static [&'static str],
    pub help: NextCall,
    pub permission: OperationPermission,
}

impl AgentCreateDeclaration {
    /// The verb's syntax line: leading options, the remark, optional options,
    /// then switches.
    pub fn syntax_line(&self) -> String {
        let names = |leading: bool| {
            self.options
                .iter()
                .filter(|option| (option.requirement != OptionRequirement::Optional) == leading)
                .map(|option| match option.values {
                    Some(values) => format!("{} ({values})", option.name),
                    None => option.name.to_owned(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut line = format!(
            "{} [preview] {}  options: {}",
            self.resource,
            self.verb,
            names(true)
        );
        for part in [
            self.option_note.map(str::to_owned),
            Some(names(false))
                .filter(|names| !names.is_empty())
                .map(|names| format!("optional {names}")),
            Some(self.switches)
                .filter(|switches| !switches.is_empty())
                .map(|switches| {
                    let switches = switches
                        .iter()
                        .map(|switch| format!("--{switch}"))
                        .collect::<Vec<_>>();
                    format!("argv switch {}", switches.join(", "))
                }),
        ]
        .into_iter()
        .flatten()
        {
            line.push_str("; ");
            line.push_str(&part);
        }
        line
    }
}

const CREATION_NOTES: &[&str] = &[
    "ID is allocated by the runtime as a fresh agent_did, not from options.display-name; the receipt returns it with the new Context ID in agent.context_id. Omit target_id and set/clear. For later changes use agent update or context update with the returned ID.",
    "A new Agent with host tools needs a User to grant it WorkspaceRoot access; the receipt's host_root_access gives the User's command. Never widen your own Tools or categories for it.",
];

const CREATION_PERMISSION: OperationPermission = OperationPermission {
    document_access: "manage_agents on the Node for the signing actor (owner or administrator), which also authorizes the Node's DefaultAgentSelection with the default switch; DefraDB document ACP on the new Agent, AgentContext and Tools",
};

pub const AGENT_CREATE: AgentCreateDeclaration = AgentCreateDeclaration {
    name: "agent.create",
    resource: "agent",
    verb: "create",
    options: &[
        option("display-name", OptionRequirement::Required),
        option("system-prompt", OptionRequirement::Required),
        OperationOption {
            name: "preset",
            requirement: OptionRequirement::Expected,
            values: Some("readonly|write"),
        },
        option("profile", OptionRequirement::Required),
        option("description", OptionRequirement::Optional),
        option("root", OptionRequirement::Optional),
    ],
    option_note: Some("readonly permits shell commands; to forbid shell set host.bash.mode Off"),
    switches: &["default"],
    usage: "agent create takes options.display-name, system-prompt, preset and profile; it allocates the ID and Context/Tools. Use agent update for an existing role",
    notes: CREATION_NOTES,
    help: NextCall::VerbHelp {
        resource: "agent",
        verb: "create",
    },
    permission: CREATION_PERMISSION,
};

pub const AGENT_CLONE: AgentCreateDeclaration = AgentCreateDeclaration {
    name: "agent.clone",
    resource: "agent",
    verb: "clone",
    options: &[
        option("from", OptionRequirement::Required),
        option("display-name", OptionRequirement::Required),
        option("profile", OptionRequirement::Required),
        option("system-prompt", OptionRequirement::Optional),
        option("root", OptionRequirement::Optional),
        option("description", OptionRequirement::Optional),
    ],
    option_note: None,
    switches: &["default"],
    usage: "agent clone takes options.from and display-name, with profile, optional system-prompt and root overrides; it inherits source tools and allocates a new ID",
    notes: CREATION_NOTES,
    help: NextCall::ResourceHelp { resource: "agent" },
    permission: CREATION_PERMISSION,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_lines_render_the_declared_options() {
        assert_eq!(
            AGENT_CREATE.syntax_line(),
            "agent [preview] create  options: display-name, system-prompt, preset (readonly|write), profile; readonly permits shell commands; to forbid shell set host.bash.mode Off; optional description, root; argv switch --default"
        );
        assert_eq!(
            AGENT_CLONE.syntax_line(),
            "agent [preview] clone  options: from, display-name, profile; optional system-prompt, root, description; argv switch --default"
        );
    }

    #[test]
    fn receipt_serializes_in_reading_order_without_an_absent_root_step() {
        let receipt = AgentReceipt {
            committed: true,
            admitted: true,
            operation: AgentOperationKind::Clone,
            agent_did: "did:key:new".to_owned(),
            host_root_access: None,
            agent: serde_json::json!({"agent_did": "did:key:new"}),
            make_default: false,
        };
        assert_eq!(
            serde_json::to_string(&receipt).unwrap(),
            r#"{"committed":true,"admitted":true,"operation":"clone","agent_did":"did:key:new","agent":{"agent_did":"did:key:new"},"make_default":false}"#
        );
    }

    #[test]
    fn a_refusal_recovers_through_its_operation_help_and_keeps_its_cause() {
        let refusal = AgentOperationError::CloneSourceGrantNotHeld {
            from: "did:key:source".to_owned(),
            source: "pack installation is not held".into(),
        };
        assert_eq!(
            crate::error_chain::ErrorChain(&refusal).to_string(),
            "clone source \"did:key:source\" carries an operator grant this agent does not hold; clone a source without it or ask the operator to grant it: pack installation is not held"
        );
        assert_eq!(
            refusal.recovery(AgentCreateOperation::Create),
            NextCall::VerbHelp {
                resource: "agent",
                verb: "create"
            }
        );
        assert_eq!(
            refusal.recovery(AgentCreateOperation::Clone),
            NextCall::ResourceHelp { resource: "agent" }
        );
        let other = AgentOperationError::Passthrough("root is not published".into());
        assert_eq!(other.to_string(), "root is not published");
        assert_eq!(
            other.passthrough().map(ToString::to_string),
            Some("root is not published".to_owned())
        );
    }

    #[test]
    fn verbs_name_their_declarations() {
        assert_eq!(
            AgentCreateOperation::for_verb("clone"),
            Some(AgentCreateOperation::Clone)
        );
        assert_eq!(AgentCreateOperation::for_verb("disable"), None);
        assert_eq!(
            AgentCreateOperation::Create.declaration().name,
            "agent.create"
        );
    }
}
