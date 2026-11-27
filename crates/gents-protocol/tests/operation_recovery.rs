use std::path::PathBuf;

use gents_lean_vocab::families;
use gents_protocol::error_chain::PassedThrough;
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::mailbox_question::{MailboxQuestionError, MailboxQuestionReplyError};
use gents_protocol::operations::agent::{AgentCreateOperation, AgentOperationError};
use gents_protocol::operations::document::{DocumentDeleteError, DocumentOperationError};
use gents_protocol::operations::goal::GoalOperationError;
use gents_protocol::operations::inference::{
    AccountOperation, InferenceDocument, InferenceOperationError,
};
use gents_protocol::operations::install_repair::{Blocker, IncompatibleStoreKind};
use gents_protocol::operations::mailbox::MailboxOperationError;
use gents_protocol::operations::mcp_service::McpServiceOperationError;
use gents_protocol::operations::p2p::RegistryError;
use gents_protocol::operations::pack::{PackOperation, PackOperationError};
use gents_protocol::operations::permission::AgentPermissionError;
use gents_protocol::operations::request::{MailboxCauseRefusal, RequestOperationError};
use gents_protocol::operations::schema::SchemaInstallDecision;
use gents_protocol::operations::session::{
    ResumeBlocker, SessionOperationError, SessionState, SessionStopStatus,
};
use gents_protocol::operations::skill::SkillOperationError;
use gents_protocol::operations::task::TaskOperationError;
use gents_protocol::operations::tools::ToolsOperationError;
use gents_protocol::operations::workspace_root::{WorkspaceRootError, WorkspaceRootVerb};
use gents_protocol::operations::NextCall;
use gents_protocol::session::{AgentSession, SessionRowError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum NextCallKind {
    VerbHelp,
    ResourceHelp,
    Inspect,
    List,
    Preview,
    Verb,
}

fn decompose_next_call(call: NextCall) -> (NextCallKind, String, Option<String>) {
    match call {
        NextCall::VerbHelp { resource, verb } => (
            NextCallKind::VerbHelp,
            resource.to_owned(),
            Some(verb.to_owned()),
        ),
        NextCall::ResourceHelp { resource } => {
            (NextCallKind::ResourceHelp, resource.to_owned(), None)
        }
        NextCall::Inspect { resource } => (NextCallKind::Inspect, resource.to_owned(), None),
        NextCall::List { resource } => (NextCallKind::List, resource.to_owned(), None),
        NextCall::Preview { resource, verb } => (
            NextCallKind::Preview,
            resource.to_owned(),
            Some(verb.to_owned()),
        ),
        NextCall::Verb { resource, verb } => (
            NextCallKind::Verb,
            resource.to_owned(),
            Some(verb.to_owned()),
        ),
    }
}

macro_rules! to_family_next_call {
    ($family:ident, $call:expr) => {{
        let (kind, resource, verb) = decompose_next_call($call);
        families::$family::NextCall {
            kind,
            resource,
            verb,
        }
    }};
}

fn passthrough() -> PassedThrough {
    PassedThrough::error(std::io::Error::other("upstream"))
}

fn boxed_error() -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other("cause"))
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanAgentCreateOp {
    Create,
    Clone,
}

impl LeanAgentCreateOp {
    fn into_prod(self) -> AgentCreateOperation {
        match self {
            Self::Create => AgentCreateOperation::Create,
            Self::Clone => AgentCreateOperation::Clone,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanAgentError {
    PreviewRequiresOperation,
    EditRejectsCloneFrom,
    EditRejectsToolsFields,
    DisableAcceptsOnlyTarget,
    CloneRequiresFrom,
    UnknownOperation,
    IdentityBindingRequired,
    IdentityBindingMissing,
    IdentityBindingMismatch,
    NewAgentDisabled,
    AgentDidRequired,
    AgentExists,
    DisplayNameRequired,
    ProfileNotPublished,
    SystemPromptRequired,
    CloneSourceNotEnabled,
    AgentMissing,
    CatalogNotHeld,
    AgentNotHeld,
    UnsupportedEditField,
    DisplayNameEmpty,
    SystemPromptBlank,
    ContextIdBlank,
    DefaultAgentDisabled,
    SelfDisable,
    MissingAgentContext,
    MissingOtherDocument,
    GeneratedContextIdExists,
    GeneratedToolsIdExists,
    ClonePresetRefused,
    CloneContextIdRefused,
    CreateContextIdConflicts,
    UnknownPreset,
    CloneSourceGrantNotHeld,
    ObjectRequired,
    ContextPatchMismatch,
    AgentHasNoContext,
    SystemPromptContextShared,
    AdmissionActorMismatch,
    CandidateIdentityMissing,
    CandidateDoesNotResolve,
    Passthrough,
}

impl LeanAgentError {
    fn into_prod(self) -> AgentOperationError {
        match self {
            Self::PreviewRequiresOperation => AgentOperationError::PreviewRequiresOperation,
            Self::EditRejectsCloneFrom => AgentOperationError::EditRejectsCloneFrom,
            Self::EditRejectsToolsFields => AgentOperationError::EditRejectsToolsFields,
            Self::DisableAcceptsOnlyTarget => AgentOperationError::DisableAcceptsOnlyTarget,
            Self::CloneRequiresFrom => AgentOperationError::CloneRequiresFrom,
            Self::UnknownOperation => AgentOperationError::UnknownOperation("bogus".into()),
            Self::IdentityBindingRequired => AgentOperationError::IdentityBindingRequired,
            Self::IdentityBindingMissing => AgentOperationError::IdentityBindingMissing,
            Self::IdentityBindingMismatch => AgentOperationError::IdentityBindingMismatch {
                binding: "did:key:a".into(),
                agent: "did:key:b".into(),
            },
            Self::NewAgentDisabled => AgentOperationError::NewAgentDisabled,
            Self::AgentDidRequired => AgentOperationError::AgentDidRequired,
            Self::AgentExists => AgentOperationError::AgentExists("did:key:a".into()),
            Self::DisplayNameRequired => AgentOperationError::DisplayNameRequired,
            Self::ProfileNotPublished => AgentOperationError::ProfileNotPublished,
            Self::SystemPromptRequired => AgentOperationError::SystemPromptRequired,
            Self::CloneSourceNotEnabled => AgentOperationError::CloneSourceNotEnabled,
            Self::AgentMissing => AgentOperationError::AgentMissing("did:key:a".into()),
            Self::CatalogNotHeld => AgentOperationError::CatalogNotHeld,
            Self::AgentNotHeld => AgentOperationError::AgentNotHeld("did:key:a".into()),
            Self::UnsupportedEditField => AgentOperationError::UnsupportedEditField("x".into()),
            Self::DisplayNameEmpty => AgentOperationError::DisplayNameEmpty,
            Self::SystemPromptBlank => AgentOperationError::SystemPromptBlank,
            Self::ContextIdBlank => AgentOperationError::ContextIdBlank,
            Self::DefaultAgentDisabled => {
                AgentOperationError::DefaultAgentDisabled("did:key:a".into())
            }
            Self::SelfDisable => AgentOperationError::SelfDisable,
            Self::MissingAgentContext => AgentOperationError::MissingDocument {
                collection: "AgentContext",
                id: "ctx-1".into(),
            },
            Self::MissingOtherDocument => AgentOperationError::MissingDocument {
                collection: "Tools",
                id: "tools-1".into(),
            },
            Self::GeneratedContextIdExists => AgentOperationError::GeneratedContextIdExists,
            Self::GeneratedToolsIdExists => AgentOperationError::GeneratedToolsIdExists,
            Self::ClonePresetRefused => AgentOperationError::ClonePresetRefused,
            Self::CloneContextIdRefused => AgentOperationError::CloneContextIdRefused,
            Self::CreateContextIdConflicts => AgentOperationError::CreateContextIdConflicts,
            Self::UnknownPreset => AgentOperationError::UnknownPreset,
            Self::CloneSourceGrantNotHeld => AgentOperationError::CloneSourceGrantNotHeld {
                from: "did:key:src".into(),
                source: boxed_error(),
            },
            Self::ObjectRequired => AgentOperationError::ObjectRequired("agent"),
            Self::ContextPatchMismatch => AgentOperationError::ContextPatchMismatch,
            Self::AgentHasNoContext => AgentOperationError::AgentHasNoContext,
            Self::SystemPromptContextShared => AgentOperationError::SystemPromptContextShared,
            Self::AdmissionActorMismatch => AgentOperationError::AdmissionActorMismatch,
            Self::CandidateIdentityMissing => AgentOperationError::CandidateIdentityMissing,
            Self::CandidateDoesNotResolve => AgentOperationError::CandidateDoesNotResolve,
            Self::Passthrough => AgentOperationError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn agent_operation_recovery_matches_lean() {
    families::operation_recovery_agent_recovery::replay::<
        LeanAgentCreateOp,
        LeanAgentError,
        NextCallKind,
    >(|input| {
        let error = input.error.into_prod();
        let next = error.recovery(input.operation.into_prod());
        to_family_next_call!(operation_recovery_agent_recovery, next)
    });

    families::operation_recovery_agent_edit_recovery::replay::<LeanAgentError, NextCallKind>(
        |input| {
            let error = input.error.into_prod();
            to_family_next_call!(
                operation_recovery_agent_edit_recovery,
                error.edit_recovery()
            )
        },
    );
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanDocumentResource {
    Context,
    McpService,
    Skill,
    Tools,
}

impl LeanDocumentResource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::McpService => "mcp-service",
            Self::Skill => "skill",
            Self::Tools => "tools",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanDocumentError {
    IdRequired,
    Exists,
    Missing,
    AmbiguousOwner,
    Invalid,
    Passthrough,
}

impl LeanDocumentError {
    fn into_prod(self) -> DocumentOperationError {
        match self {
            Self::IdRequired => DocumentOperationError::IdRequired { collection: "Doc" },
            Self::Exists => DocumentOperationError::Exists {
                collection: "Doc",
                id: "id-1".into(),
            },
            Self::Missing => DocumentOperationError::Missing {
                collection: "Doc",
                id: "id-1".into(),
                node_did: "did:key:node".into(),
            },
            Self::AmbiguousOwner => DocumentOperationError::AmbiguousOwner {
                collection: "Doc",
                id: "id-1".into(),
                owners: vec!["did:key:n1".into(), "did:key:n2".into()],
            },
            Self::Invalid => DocumentOperationError::Invalid {
                collection: "Doc",
                source: boxed_error(),
            },
            Self::Passthrough => DocumentOperationError::Passthrough(passthrough()),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanDocumentDeleteError {
    NoTargets,
    DuplicateTarget,
    TargetMissing,
    DigestRequired,
    DigestStale,
    StillReferenced,
    NotRemoved,
    Passthrough,
}

impl LeanDocumentDeleteError {
    fn into_prod(self) -> DocumentDeleteError {
        match self {
            Self::NoTargets => DocumentDeleteError::NoTargets,
            Self::DuplicateTarget => DocumentDeleteError::DuplicateTarget {
                collection: "Doc",
                id: "id-1".into(),
            },
            Self::TargetMissing => DocumentDeleteError::TargetMissing {
                collection: "Doc",
                id: "id-1".into(),
            },
            Self::DigestRequired => DocumentDeleteError::DigestRequired,
            Self::DigestStale => DocumentDeleteError::DigestStale {
                expected: "a".into(),
                current: "b".into(),
            },
            Self::StillReferenced => DocumentDeleteError::StillReferenced {
                collection: "Agent",
                id: "a-1".into(),
                field: "ref".into(),
                target: "Doc",
                target_id: "id-1".into(),
            },
            Self::NotRemoved => DocumentDeleteError::NotRemoved {
                collection: "Doc",
                id: "id-1".into(),
            },
            Self::Passthrough => DocumentDeleteError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn document_operation_recovery_matches_lean() {
    families::operation_recovery_document_recovery::replay::<
        LeanDocumentError,
        LeanDocumentResource,
        NextCallKind,
    >(|input| {
        let error = input.error.into_prod();
        let next = error.recovery(input.resource.as_str());
        to_family_next_call!(operation_recovery_document_recovery, next)
    });

    families::operation_recovery_document_delete_recovery::replay::<
        LeanDocumentDeleteError,
        LeanDocumentResource,
        NextCallKind,
    >(|input| {
        let error = input.error.into_prod();
        let next = error.recovery(input.resource.as_str());
        to_family_next_call!(operation_recovery_document_delete_recovery, next)
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanGoalError {
    Denied,
    SessionMissing,
    NoGoal,
    Busy,
    LiveGoal,
    Stale,
    Conflict,
    InvalidObjective,
    InvalidBudget,
    InvalidCeiling,
    ConfirmBlocked,
    KeepTrying,
    NothingToChange,
    RuntimeOnly,
    PersonOnly,
    FieldsNeedKeep,
    NotEditable,
    ResumeRequired,
    ObligationsUnmet,
    FirstRequestInvalid,
    FirstRequestMismatch,
    ReasonNotAccepted,
    ReasonRequired,
    IllegalTransition,
    Passthrough,
}

impl LeanGoalError {
    fn into_prod(self) -> GoalOperationError {
        match self {
            Self::Denied => GoalOperationError::Denied,
            Self::SessionMissing => GoalOperationError::SessionMissing("s-1".into()),
            Self::NoGoal => GoalOperationError::NoGoal("s-1".into()),
            Self::Busy => GoalOperationError::Busy,
            Self::LiveGoal => GoalOperationError::LiveGoal,
            Self::Stale => GoalOperationError::Stale,
            Self::Conflict => GoalOperationError::Conflict,
            Self::InvalidObjective => GoalOperationError::InvalidObjective,
            Self::InvalidBudget => GoalOperationError::InvalidBudget,
            Self::InvalidCeiling => GoalOperationError::InvalidCeiling,
            Self::ConfirmBlocked => GoalOperationError::ConfirmBlocked,
            Self::KeepTrying => GoalOperationError::KeepTrying,
            Self::NothingToChange => GoalOperationError::NothingToChange,
            Self::RuntimeOnly => GoalOperationError::RuntimeOnly,
            Self::PersonOnly => GoalOperationError::PersonOnly,
            Self::FieldsNeedKeep => GoalOperationError::FieldsNeedKeep,
            Self::NotEditable => GoalOperationError::NotEditable(GoalStatus::Paused),
            Self::ResumeRequired => GoalOperationError::ResumeRequired(GoalStatus::Paused),
            Self::ObligationsUnmet => GoalOperationError::ObligationsUnmet("unmet".into()),
            Self::FirstRequestInvalid => GoalOperationError::FirstRequestInvalid("invalid"),
            Self::FirstRequestMismatch => GoalOperationError::FirstRequestMismatch,
            Self::ReasonNotAccepted => GoalOperationError::ReasonNotAccepted,
            Self::ReasonRequired => GoalOperationError::ReasonRequired,
            Self::IllegalTransition => GoalOperationError::IllegalTransition {
                target: "active",
                status: "complete",
            },
            Self::Passthrough => GoalOperationError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn goal_operation_recovery_matches_lean() {
    families::operation_recovery_goal_recovery::replay::<LeanGoalError, NextCallKind>(|input| {
        let error = input.error.into_prod();
        to_family_next_call!(operation_recovery_goal_recovery, error.recovery())
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanInferenceDocument {
    Backend,
    Profile,
}

impl LeanInferenceDocument {
    fn into_prod(self) -> InferenceDocument {
        match self {
            Self::Backend => InferenceDocument::Backend,
            Self::Profile => InferenceDocument::Profile,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanInferenceVerb {
    Create,
    Update,
    Remove,
    Rm,
}

impl LeanInferenceVerb {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Remove => "remove",
            Self::Rm => "rm",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanInferenceError {
    RawCredential,
    OauthAccount,
    OauthPool,
    LocalServerOnly,
    StartsEnabled,
    OtherAccount,
    OtherPool,
    AccountOperation,
    ExistsBackend,
    ExistsProfile,
    MissingBackend,
    MissingProfile,
    IdRequiredBackend,
    IdRequiredProfile,
    Invalid,
    Passthrough,
}

impl LeanInferenceError {
    fn into_prod(self) -> InferenceOperationError {
        match self {
            Self::RawCredential => InferenceOperationError::RawCredential,
            Self::OauthAccount => InferenceOperationError::OauthAccount,
            Self::OauthPool => InferenceOperationError::OauthPool,
            Self::LocalServerOnly => InferenceOperationError::LocalServerOnly,
            Self::StartsEnabled => InferenceOperationError::StartsEnabled,
            Self::OtherAccount => InferenceOperationError::OtherAccount {
                backend_id: "b-1".into(),
                provider: "openai".into(),
            },
            Self::OtherPool => InferenceOperationError::OtherPool {
                backend_id: "b-1".into(),
                provider: "anthropic".into(),
            },
            Self::AccountOperation => {
                InferenceOperationError::AccountOperation(AccountOperation::List)
            }
            Self::ExistsBackend => InferenceOperationError::Exists {
                document: InferenceDocument::Backend,
                id: "b-1".into(),
            },
            Self::ExistsProfile => InferenceOperationError::Exists {
                document: InferenceDocument::Profile,
                id: "p-1".into(),
            },
            Self::MissingBackend => InferenceOperationError::Missing {
                document: InferenceDocument::Backend,
                id: "b-1".into(),
            },
            Self::MissingProfile => InferenceOperationError::Missing {
                document: InferenceDocument::Profile,
                id: "p-1".into(),
            },
            Self::IdRequiredBackend => {
                InferenceOperationError::IdRequired(InferenceDocument::Backend)
            }
            Self::IdRequiredProfile => {
                InferenceOperationError::IdRequired(InferenceDocument::Profile)
            }
            Self::Invalid => InferenceOperationError::Invalid(boxed_error()),
            Self::Passthrough => InferenceOperationError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn inference_operation_recovery_matches_lean() {
    families::operation_recovery_inference_recovery::replay::<
        LeanInferenceDocument,
        LeanInferenceError,
        LeanInferenceVerb,
        NextCallKind,
    >(|input| {
        let error = input.error.into_prod();
        let next = error.recovery(input.document.into_prod(), input.verb.as_str());
        to_family_next_call!(operation_recovery_inference_recovery, next)
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanBlocker {
    PortInUse,
    HandStartedNode,
    KeychainUnavailable,
    StoreRefused,
    CannotSetAside,
    KeepsExiting,
    NoAnswer,
    NoServiceManager,
    HomeUnreadable,
    HomeBusy,
    OldNodeStillRunning,
    ServiceUpdateFailed,
    BackgroundApprovalPending,
    NewerNode,
    ServiceServesAnotherHome,
}

impl LeanBlocker {
    fn into_prod(self) -> Blocker {
        let home = PathBuf::from("/tmp/gents-home");
        match self {
            Self::PortInUse => Blocker::PortInUse {
                port: 8080,
                holder: None,
            },
            Self::HandStartedNode => Blocker::HandStartedNode {
                port: 8080,
                home: None,
            },
            Self::KeychainUnavailable => Blocker::KeychainUnavailable,
            Self::StoreRefused => Blocker::StoreRefused {
                kind: IncompatibleStoreKind::LegacyRocksDb,
                message: "refused".into(),
            },
            Self::CannotSetAside => Blocker::CannotSetAside {
                home,
                reason: "busy".into(),
            },
            Self::KeepsExiting => Blocker::KeepsExiting {
                code: Some(1),
                last_error: None,
            },
            Self::NoAnswer => Blocker::NoAnswer {
                seconds: 10,
                last_error: None,
            },
            Self::NoServiceManager => Blocker::NoServiceManager { home },
            Self::HomeUnreadable => Blocker::HomeUnreadable {
                home,
                reason: "denied".into(),
            },
            Self::HomeBusy => Blocker::HomeBusy { home },
            Self::OldNodeStillRunning => Blocker::OldNodeStillRunning { pid: 42 },
            Self::ServiceUpdateFailed => Blocker::ServiceUpdateFailed {
                reason: "failed".into(),
            },
            Self::BackgroundApprovalPending => Blocker::BackgroundApprovalPending { detail: None },
            Self::NewerNode => Blocker::NewerNode {
                running: "2.0.0".into(),
                ours: "1.0.0".into(),
            },
            Self::ServiceServesAnotherHome => Blocker::ServiceServesAnotherHome {
                home,
                serving: PathBuf::from("/tmp/other"),
                free_port: None,
            },
        }
    }
}

#[test]
fn install_repair_blocker_next_call_matches_lean() {
    families::operation_recovery_install_repair_blocker_next_call::replay::<
        LeanBlocker,
        NextCallKind,
    >(|input| {
        let blocker = input.blocker.into_prod();
        blocker.next_call().map(|call| {
            to_family_next_call!(operation_recovery_install_repair_blocker_next_call, call)
        })
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanMailboxError {
    ItemMissing,
    ItemAmbiguous,
    UnknownStatus,
    UnknownAction,
    NotOpen,
    NotReplyable,
    NotAQuestion,
    RouteDiffers,
    QuestionChanged,
    NeedsAnswer,
    QuestionPayloadMissing,
    ActorRequired,
    DismissalLost,
    Question,
    QuestionReply,
    Passthrough,
}

impl LeanMailboxError {
    fn into_prod(self) -> MailboxOperationError {
        match self {
            Self::ItemMissing => MailboxOperationError::ItemMissing,
            Self::ItemAmbiguous => MailboxOperationError::ItemAmbiguous,
            Self::UnknownStatus => MailboxOperationError::UnknownStatus("bad".into()),
            Self::UnknownAction => MailboxOperationError::UnknownAction("bad".into()),
            Self::NotOpen => MailboxOperationError::NotOpen,
            Self::NotReplyable => MailboxOperationError::NotReplyable,
            Self::NotAQuestion => MailboxOperationError::NotAQuestion,
            Self::RouteDiffers => MailboxOperationError::RouteDiffers,
            Self::QuestionChanged => MailboxOperationError::QuestionChanged,
            Self::NeedsAnswer => MailboxOperationError::NeedsAnswer,
            Self::QuestionPayloadMissing => MailboxOperationError::QuestionPayloadMissing,
            Self::ActorRequired => MailboxOperationError::ActorRequired,
            Self::DismissalLost => MailboxOperationError::DismissalLost,
            Self::Question => MailboxOperationError::Question(MailboxQuestionError::Empty),
            Self::QuestionReply => {
                MailboxOperationError::QuestionReply(MailboxQuestionReplyError::VersionChanged)
            }
            Self::Passthrough => MailboxOperationError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn mailbox_operation_recovery_matches_lean() {
    families::operation_recovery_mailbox_recovery::replay::<LeanMailboxError, NextCallKind>(
        |input| {
            let error = input.error.into_prod();
            to_family_next_call!(operation_recovery_mailbox_recovery, error.recovery())
        },
    );
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanMcpServiceError {
    Endpoint,
    DocumentIdRequired,
    DocumentExists,
    DocumentMissing,
    DocumentAmbiguousOwner,
    DocumentInvalid,
    DocumentPassthrough,
    DeleteNoTargets,
    DeleteDuplicateTarget,
    DeleteTargetMissing,
    DeleteDigestRequired,
    DeleteDigestStale,
    DeleteStillReferenced,
    DeleteNotRemoved,
    DeletePassthrough,
}

impl LeanMcpServiceError {
    fn into_prod(self) -> McpServiceOperationError {
        match self {
            Self::Endpoint => McpServiceOperationError::Endpoint {
                source: boxed_error(),
            },
            Self::DocumentIdRequired => {
                McpServiceOperationError::Document(LeanDocumentError::IdRequired.into_prod())
            }
            Self::DocumentExists => {
                McpServiceOperationError::Document(LeanDocumentError::Exists.into_prod())
            }
            Self::DocumentMissing => {
                McpServiceOperationError::Document(LeanDocumentError::Missing.into_prod())
            }
            Self::DocumentAmbiguousOwner => {
                McpServiceOperationError::Document(LeanDocumentError::AmbiguousOwner.into_prod())
            }
            Self::DocumentInvalid => {
                McpServiceOperationError::Document(LeanDocumentError::Invalid.into_prod())
            }
            Self::DocumentPassthrough => {
                McpServiceOperationError::Document(LeanDocumentError::Passthrough.into_prod())
            }
            Self::DeleteNoTargets => {
                McpServiceOperationError::Delete(LeanDocumentDeleteError::NoTargets.into_prod())
            }
            Self::DeleteDuplicateTarget => McpServiceOperationError::Delete(
                LeanDocumentDeleteError::DuplicateTarget.into_prod(),
            ),
            Self::DeleteTargetMissing => {
                McpServiceOperationError::Delete(LeanDocumentDeleteError::TargetMissing.into_prod())
            }
            Self::DeleteDigestRequired => McpServiceOperationError::Delete(
                LeanDocumentDeleteError::DigestRequired.into_prod(),
            ),
            Self::DeleteDigestStale => {
                McpServiceOperationError::Delete(LeanDocumentDeleteError::DigestStale.into_prod())
            }
            Self::DeleteStillReferenced => McpServiceOperationError::Delete(
                LeanDocumentDeleteError::StillReferenced.into_prod(),
            ),
            Self::DeleteNotRemoved => {
                McpServiceOperationError::Delete(LeanDocumentDeleteError::NotRemoved.into_prod())
            }
            Self::DeletePassthrough => {
                McpServiceOperationError::Delete(LeanDocumentDeleteError::Passthrough.into_prod())
            }
        }
    }
}

#[test]
fn mcp_service_operation_recovery_matches_lean() {
    families::operation_recovery_mcp_service_recovery::replay::<LeanMcpServiceError, NextCallKind>(
        |input| {
            let error = input.error.into_prod();
            to_family_next_call!(operation_recovery_mcp_service_recovery, error.recovery())
        },
    );
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanRegistryError {
    PeerIdUnknown,
    UnknownTemplate,
    Passthrough,
}

impl LeanRegistryError {
    fn into_prod(self) -> RegistryError {
        match self {
            Self::PeerIdUnknown => RegistryError::PeerIdUnknown,
            Self::UnknownTemplate => RegistryError::UnknownTemplate("tmpl".into()),
            Self::Passthrough => RegistryError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn registry_error_recovery_matches_lean() {
    families::operation_recovery_registry_recovery::replay::<NextCallKind, LeanRegistryError>(
        |input| {
            let error = input.error.into_prod();
            to_family_next_call!(operation_recovery_registry_recovery, error.recovery())
        },
    );
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanPackError {
    PackageBlank,
    PreviewRequired,
    DigestChanged,
    EditedUndecided,
    EditedWord,
    VariableMalformed,
    VariableName,
    VariableInference,
    VariableTooLarge,
    VariableDuplicate,
    TooManyVariables,
    TooManySlots,
    SlotMalformed,
    SlotDuplicate,
    Passthrough,
}

impl LeanPackError {
    fn into_prod(self, operation: PackOperation) -> PackOperationError {
        match self {
            Self::PackageBlank => PackOperationError::PackageBlank,
            Self::PreviewRequired => PackOperationError::PreviewRequired(operation),
            Self::DigestChanged => PackOperationError::DigestChanged {
                expected: "a".into(),
                resolved: "b".into(),
            },
            Self::EditedUndecided => PackOperationError::EditedUndecided {
                coordinate: "ns/p".into(),
                documents: vec!["doc-1".into()],
                removal: false,
            },
            Self::EditedWord => PackOperationError::EditedWord("bad".into()),
            Self::VariableMalformed => PackOperationError::VariableMalformed("bad".into()),
            Self::VariableName => PackOperationError::VariableName("bad".into()),
            Self::VariableInference => PackOperationError::VariableInference("bad".into()),
            Self::VariableTooLarge => PackOperationError::VariableTooLarge("bad".into()),
            Self::VariableDuplicate => PackOperationError::VariableDuplicate("bad".into()),
            Self::TooManyVariables => PackOperationError::TooManyVariables,
            Self::TooManySlots => PackOperationError::TooManySlots,
            Self::SlotMalformed => PackOperationError::SlotMalformed("bad".into()),
            Self::SlotDuplicate => PackOperationError::SlotDuplicate("bad".into()),
            Self::Passthrough => PackOperationError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn pack_operation_recovery_matches_lean() {
    families::operation_recovery_pack_recovery::replay::<NextCallKind, LeanPackError, PackOperation>(
        |input| {
            let error = input.error.into_prod(input.operation);
            let next = error.recovery(input.operation);
            to_family_next_call!(operation_recovery_pack_recovery, next)
        },
    );
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanAgentPermissionError {
    InvalidAgentDid,
    AgentUnavailable,
    NodeUnavailable,
    Passthrough,
}

impl LeanAgentPermissionError {
    fn into_prod(self) -> AgentPermissionError {
        match self {
            Self::InvalidAgentDid => {
                AgentPermissionError::InvalidAgentDid("bad".into(), boxed_error())
            }
            Self::AgentUnavailable => AgentPermissionError::AgentUnavailable("did:key:a".into()),
            Self::NodeUnavailable => AgentPermissionError::NodeUnavailable("did:key:n".into()),
            Self::Passthrough => AgentPermissionError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn agent_permission_recovery_matches_lean() {
    families::operation_recovery_agent_permission_recovery::replay::<
        LeanAgentPermissionError,
        NextCallKind,
    >(|input| {
        let error = input.error.into_prod();
        to_family_next_call!(
            operation_recovery_agent_permission_recovery,
            error.recovery()
        )
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanRequestError {
    FieldRequired,
    ContentRequired,
    IdentityRequired,
    QueueEntryOwned,
    ConflictingActiveRequests,
    NodeUnavailable,
    NoDefaultAgent,
    AgentUnavailable,
    SessionAgentMismatch,
    MailboxCause,
    NotFound,
    Ambiguous,
    ReissueParentState,
    ReissueNotInteractive,
    RetryBudgetExhausted,
    ReissueDeadlineClosed,
    ReissueNotLatest,
    WaitTimedOut,
    OutputUnavailable,
    SuccessorInvalid,
    ReceiptMismatch,
    InterruptRefused,
    Unconfirmed,
    IdentityConflict,
    Passthrough,
}

impl LeanRequestError {
    fn into_prod(self) -> RequestOperationError {
        match self {
            Self::FieldRequired => RequestOperationError::FieldRequired("node_did"),
            Self::ContentRequired => RequestOperationError::ContentRequired,
            Self::IdentityRequired => RequestOperationError::IdentityRequired,
            Self::QueueEntryOwned => RequestOperationError::QueueEntryOwned,
            Self::ConflictingActiveRequests => RequestOperationError::ConflictingActiveRequests {
                session_id: "s-1".into(),
            },
            Self::NodeUnavailable => RequestOperationError::NodeUnavailable {
                node_did: "did:key:n".into(),
            },
            Self::NoDefaultAgent => RequestOperationError::NoDefaultAgent {
                node_did: "did:key:n".into(),
            },
            Self::AgentUnavailable => RequestOperationError::AgentUnavailable {
                node_did: "did:key:n".into(),
                agent_did: "did:key:a".into(),
            },
            Self::SessionAgentMismatch => RequestOperationError::SessionAgentMismatch {
                session_id: "s-1".into(),
                session_agent: "did:key:a".into(),
                requested: "did:key:b".into(),
            },
            Self::MailboxCause => RequestOperationError::MailboxCause {
                item_id: "m-1".into(),
                refusal: MailboxCauseRefusal::Closed,
            },
            Self::NotFound => RequestOperationError::NotFound {
                node_did: "did:key:n".into(),
                request_id: "r-1".into(),
            },
            Self::Ambiguous => RequestOperationError::Ambiguous {
                node_did: "did:key:n".into(),
                request_id: "r-1".into(),
            },
            Self::ReissueParentState => RequestOperationError::ReissueParentState {
                kind: "retry",
                request_id: "r-1".into(),
                state: "completed",
                failure_reason: "none".into(),
                needs: "failed",
            },
            Self::ReissueNotInteractive => RequestOperationError::ReissueNotInteractive {
                request_id: "r-1".into(),
            },
            Self::RetryBudgetExhausted => RequestOperationError::RetryBudgetExhausted {
                request_id: "r-1".into(),
                retry_count: 3,
                max_retries: 3,
            },
            Self::ReissueDeadlineClosed => RequestOperationError::ReissueDeadlineClosed {
                request_id: "r-1".into(),
                deadline: "2026-01-01T00:00:00Z".into(),
            },
            Self::ReissueNotLatest => RequestOperationError::ReissueNotLatest {
                request_id: "r-1".into(),
                session_id: "s-1".into(),
                latest_request_id: "r-2".into(),
            },
            Self::WaitTimedOut => RequestOperationError::WaitTimedOut {
                request_id: "r-1".into(),
                timeout_secs: 30,
                state: "executing".into(),
            },
            Self::OutputUnavailable => RequestOperationError::OutputUnavailable {
                request_id: "r-1".into(),
                observation: "incomplete",
            },
            Self::SuccessorInvalid => RequestOperationError::SuccessorInvalid {
                request_id: "r-1".into(),
                reason: "cycle",
            },
            Self::ReceiptMismatch => RequestOperationError::ReceiptMismatch {
                request_id: "r-1".into(),
                reason: "digest",
            },
            Self::InterruptRefused => RequestOperationError::InterruptRefused {
                request_id: "r-1".into(),
                reason: "terminal".into(),
            },
            Self::Unconfirmed => RequestOperationError::Unconfirmed {
                request_id: "r-1".into(),
                source: boxed_error(),
            },
            Self::IdentityConflict => RequestOperationError::IdentityConflict {
                request_id: "r-1".into(),
                source: boxed_error(),
            },
            Self::Passthrough => RequestOperationError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn request_operation_error_code_and_next_call_match_lean() {
    families::operation_recovery_request_error_code::replay::<LeanRequestError>(|input| {
        input.error.into_prod().code().to_owned()
    });

    families::operation_recovery_request_next_call::replay::<NextCallKind, LeanRequestError>(
        |input| {
            let error = input.error.into_prod();
            to_family_next_call!(operation_recovery_request_next_call, error.next_call())
        },
    );
}

#[test]
fn schema_install_recovery_matches_lean() {
    families::operation_recovery_schema_install_recovery::replay::<
        NextCallKind,
        SchemaInstallDecision,
    >(|input| {
        input
            .decision
            .recovery()
            .map(|call| to_family_next_call!(operation_recovery_schema_install_recovery, call))
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanSessionError {
    FieldRequired,
    IdentityRequired,
    NotFound,
    Ambiguous,
    InvalidDocument,
    RequesterMissing,
    RevisionOutOfRange,
    RequesterMismatch,
    ForkSourceBusy,
    ForkTurnOutOfRange,
    ForkMessageNotEditable,
    ForkClientMismatch,
    ForkChildNotCanonical,
    CrossAgentForkUnavailable,
    AgentUnavailable,
    ForkRelationshipsIncomplete,
    ForkRecoveryIntentRequired,
    RemoteNodeUnsupported,
    ResumeBlocked,
    ProcessOwnershipUnverifiable,
    Passthrough,
}

impl LeanSessionError {
    fn into_prod(self) -> SessionOperationError {
        match self {
            Self::FieldRequired => SessionOperationError::FieldRequired("session_id"),
            Self::IdentityRequired => SessionOperationError::IdentityRequired,
            Self::NotFound => SessionOperationError::NotFound {
                node_did: "did:key:n".into(),
                session_id: "s-1".into(),
            },
            Self::Ambiguous => SessionOperationError::Ambiguous {
                node_did: "did:key:n".into(),
                session_id: "s-1".into(),
            },
            Self::InvalidDocument => SessionOperationError::InvalidDocument {
                session_id: "s-1".into(),
                source: SessionRowError::Blank("session_id"),
            },
            Self::RequesterMissing => SessionOperationError::RequesterMissing {
                session_id: "s-1".into(),
            },
            Self::RevisionOutOfRange => SessionOperationError::RevisionOutOfRange { revision: 9 },
            Self::RequesterMismatch => SessionOperationError::RequesterMismatch {
                session_id: "s-1".into(),
            },
            Self::ForkSourceBusy => SessionOperationError::ForkSourceBusy {
                session_id: "s-1".into(),
            },
            Self::ForkTurnOutOfRange => SessionOperationError::ForkTurnOutOfRange {
                session_id: "s-1".into(),
                at_user_turn: 5,
                user_messages: 2,
            },
            Self::ForkMessageNotEditable => SessionOperationError::ForkMessageNotEditable {
                message_key: "m-1".into(),
                reason: "missing".into(),
            },
            Self::ForkClientMismatch => SessionOperationError::ForkClientMismatch {
                client_did: "did:key:c".into(),
            },
            Self::ForkChildNotCanonical => SessionOperationError::ForkChildNotCanonical {
                child_session_id: " bad ".into(),
            },
            Self::CrossAgentForkUnavailable => SessionOperationError::CrossAgentForkUnavailable,
            Self::AgentUnavailable => SessionOperationError::AgentUnavailable {
                agent_did: "did:key:a".into(),
            },
            Self::ForkRelationshipsIncomplete => {
                SessionOperationError::ForkRelationshipsIncomplete {
                    session_id: "s-1".into(),
                    source: boxed_error(),
                }
            }
            Self::ForkRecoveryIntentRequired => SessionOperationError::ForkRecoveryIntentRequired {
                session_id: "s-1".into(),
            },
            Self::RemoteNodeUnsupported => SessionOperationError::RemoteNodeUnsupported {
                node_did: "did:key:n".into(),
            },
            Self::ResumeBlocked => SessionOperationError::ResumeBlocked {
                session_id: "s-1".into(),
                blocker: ResumeBlocker::Requests,
            },
            Self::ProcessOwnershipUnverifiable => {
                SessionOperationError::ProcessOwnershipUnverifiable {
                    session_id: "s-1".into(),
                    source: boxed_error(),
                }
            }
            Self::Passthrough => SessionOperationError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn session_operations_match_lean() {
    families::operation_recovery_session_state::replay::<SessionState>(|input| {
        let session = AgentSession {
            session_id: "s-1".into(),
            node_did: "did:key:n".into(),
            requester_did: None,
            agent_did: "did:key:a".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            closed_at: input.has_closed_at.then(|| "2026-01-02T00:00:00Z".into()),
            archived_at: input.has_archived_at.then(|| "2026-01-03T00:00:00Z".into()),
            archive_revision: 0,
            stopped_at: None,
            title: None,
            tags: Vec::new(),
            provenance: None,
            observation: None,
        };
        SessionState::of(&session)
    });

    families::operation_recovery_session_stop_status::replay::<SessionStopStatus>(|input| {
        SessionStopStatus::of(input.interrupted_a_request)
    });

    families::operation_recovery_session_error_code::replay::<LeanSessionError>(|input| {
        input.error.into_prod().code().to_owned()
    });

    families::operation_recovery_session_next_call::replay::<NextCallKind, LeanSessionError>(
        |input| {
            let error = input.error.into_prod();
            to_family_next_call!(operation_recovery_session_next_call, error.next_call())
        },
    );
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanSkillError {
    ImportExists,
    OutsideToolRoot,
    FileReadNotGranted,
    ToolRootMissing,
    Source,
    DocumentIdRequired,
    DocumentExists,
    DocumentMissing,
    DocumentAmbiguousOwner,
    DocumentInvalid,
    DocumentPassthrough,
    DeleteNoTargets,
    DeleteDuplicateTarget,
    DeleteTargetMissing,
    DeleteDigestRequired,
    DeleteDigestStale,
    DeleteStillReferenced,
    DeleteNotRemoved,
    DeletePassthrough,
}

impl LeanSkillError {
    fn into_prod(self) -> SkillOperationError {
        match self {
            Self::ImportExists => SkillOperationError::ImportExists {
                skill_id: "sk-1".into(),
            },
            Self::OutsideToolRoot => SkillOperationError::OutsideToolRoot {
                path: "/outside".into(),
                source: boxed_error(),
            },
            Self::FileReadNotGranted => SkillOperationError::FileReadNotGranted,
            Self::ToolRootMissing => SkillOperationError::ToolRootMissing,
            Self::Source => SkillOperationError::Source {
                path: "/tmp/s.md".into(),
                source: boxed_error(),
            },
            Self::DocumentIdRequired => {
                SkillOperationError::Document(LeanDocumentError::IdRequired.into_prod())
            }
            Self::DocumentExists => {
                SkillOperationError::Document(LeanDocumentError::Exists.into_prod())
            }
            Self::DocumentMissing => {
                SkillOperationError::Document(LeanDocumentError::Missing.into_prod())
            }
            Self::DocumentAmbiguousOwner => {
                SkillOperationError::Document(LeanDocumentError::AmbiguousOwner.into_prod())
            }
            Self::DocumentInvalid => {
                SkillOperationError::Document(LeanDocumentError::Invalid.into_prod())
            }
            Self::DocumentPassthrough => {
                SkillOperationError::Document(LeanDocumentError::Passthrough.into_prod())
            }
            Self::DeleteNoTargets => {
                SkillOperationError::Delete(LeanDocumentDeleteError::NoTargets.into_prod())
            }
            Self::DeleteDuplicateTarget => {
                SkillOperationError::Delete(LeanDocumentDeleteError::DuplicateTarget.into_prod())
            }
            Self::DeleteTargetMissing => {
                SkillOperationError::Delete(LeanDocumentDeleteError::TargetMissing.into_prod())
            }
            Self::DeleteDigestRequired => {
                SkillOperationError::Delete(LeanDocumentDeleteError::DigestRequired.into_prod())
            }
            Self::DeleteDigestStale => {
                SkillOperationError::Delete(LeanDocumentDeleteError::DigestStale.into_prod())
            }
            Self::DeleteStillReferenced => {
                SkillOperationError::Delete(LeanDocumentDeleteError::StillReferenced.into_prod())
            }
            Self::DeleteNotRemoved => {
                SkillOperationError::Delete(LeanDocumentDeleteError::NotRemoved.into_prod())
            }
            Self::DeletePassthrough => {
                SkillOperationError::Delete(LeanDocumentDeleteError::Passthrough.into_prod())
            }
        }
    }
}

#[test]
fn skill_operation_recovery_matches_lean() {
    families::operation_recovery_skill_recovery::replay::<NextCallKind, LeanSkillError>(|input| {
        let error = input.error.into_prod();
        to_family_next_call!(operation_recovery_skill_recovery, error.recovery())
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanTaskError {
    TaskMissing,
    TaskDisabled,
    AgentMissing,
    AgentDisabled,
    CommandTask,
    ArgsNotObject,
    StableKeyRequired,
    SessionLabelAndContinue,
    ScheduleMissing,
    ScheduleAmbiguous,
    NoEnabledTrigger,
    ManyEnabledTriggers,
    Passthrough,
}

impl LeanTaskError {
    fn into_prod(self) -> TaskOperationError {
        match self {
            Self::TaskMissing => TaskOperationError::TaskMissing {
                task_id: "t-1".into(),
                node_did: "did:key:n".into(),
            },
            Self::TaskDisabled => TaskOperationError::TaskDisabled("t-1".into()),
            Self::AgentMissing => TaskOperationError::AgentMissing {
                agent_did: "did:key:a".into(),
                node_did: "did:key:n".into(),
            },
            Self::AgentDisabled => TaskOperationError::AgentDisabled("did:key:a".into()),
            Self::CommandTask => TaskOperationError::CommandTask("t-1".into()),
            Self::ArgsNotObject => TaskOperationError::ArgsNotObject(serde_json::json!(42)),
            Self::StableKeyRequired => TaskOperationError::StableKeyRequired("t-1".into()),
            Self::SessionLabelAndContinue => TaskOperationError::SessionLabelAndContinue,
            Self::ScheduleMissing => TaskOperationError::ScheduleMissing {
                schedule_id: "sc-1".into(),
                node_did: "did:key:n".into(),
            },
            Self::ScheduleAmbiguous => TaskOperationError::ScheduleAmbiguous {
                schedule_id: "sc-1".into(),
                node_did: "did:key:n".into(),
            },
            Self::NoEnabledTrigger => TaskOperationError::NoEnabledTrigger("sc-1".into()),
            Self::ManyEnabledTriggers => TaskOperationError::ManyEnabledTriggers("sc-1".into()),
            Self::Passthrough => TaskOperationError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn task_operation_recovery_matches_lean() {
    families::operation_recovery_task_recovery::replay::<NextCallKind, LeanTaskError>(|input| {
        let error = input.error.into_prod();
        to_family_next_call!(operation_recovery_task_recovery, error.recovery())
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanToolsError {
    SharedUnacknowledged,
    SharedWithoutCatalog,
    GrantNotHeld,
    LosesOwnControl,
    DropsSettings,
    PluginNotInstalled,
    DocumentIdRequired,
    DocumentExists,
    DocumentMissing,
    DocumentAmbiguousOwner,
    DocumentInvalid,
    DocumentPassthrough,
    DeleteNoTargets,
    DeleteDuplicateTarget,
    DeleteTargetMissing,
    DeleteDigestRequired,
    DeleteDigestStale,
    DeleteStillReferenced,
    DeleteNotRemoved,
    DeletePassthrough,
}

impl LeanToolsError {
    fn into_prod(self) -> ToolsOperationError {
        match self {
            Self::SharedUnacknowledged => ToolsOperationError::SharedUnacknowledged {
                agents: vec!["did:key:a".into()],
            },
            Self::SharedWithoutCatalog => ToolsOperationError::SharedWithoutCatalog {
                agents: vec!["did:key:a".into()],
            },
            Self::GrantNotHeld => ToolsOperationError::GrantNotHeld {
                grants: vec!["shell".into()],
            },
            Self::LosesOwnControl => ToolsOperationError::LosesOwnControl {
                source: boxed_error(),
            },
            Self::DropsSettings => ToolsOperationError::DropsSettings {
                dropped: vec!["host".into()],
                groups: vec!["host".into()],
            },
            Self::PluginNotInstalled => ToolsOperationError::PluginNotInstalled {
                plugin: "acme/p".into(),
                reason: "missing".into(),
            },
            Self::DocumentIdRequired => {
                ToolsOperationError::Document(LeanDocumentError::IdRequired.into_prod())
            }
            Self::DocumentExists => {
                ToolsOperationError::Document(LeanDocumentError::Exists.into_prod())
            }
            Self::DocumentMissing => {
                ToolsOperationError::Document(LeanDocumentError::Missing.into_prod())
            }
            Self::DocumentAmbiguousOwner => {
                ToolsOperationError::Document(LeanDocumentError::AmbiguousOwner.into_prod())
            }
            Self::DocumentInvalid => {
                ToolsOperationError::Document(LeanDocumentError::Invalid.into_prod())
            }
            Self::DocumentPassthrough => {
                ToolsOperationError::Document(LeanDocumentError::Passthrough.into_prod())
            }
            Self::DeleteNoTargets => {
                ToolsOperationError::Delete(LeanDocumentDeleteError::NoTargets.into_prod())
            }
            Self::DeleteDuplicateTarget => {
                ToolsOperationError::Delete(LeanDocumentDeleteError::DuplicateTarget.into_prod())
            }
            Self::DeleteTargetMissing => {
                ToolsOperationError::Delete(LeanDocumentDeleteError::TargetMissing.into_prod())
            }
            Self::DeleteDigestRequired => {
                ToolsOperationError::Delete(LeanDocumentDeleteError::DigestRequired.into_prod())
            }
            Self::DeleteDigestStale => {
                ToolsOperationError::Delete(LeanDocumentDeleteError::DigestStale.into_prod())
            }
            Self::DeleteStillReferenced => {
                ToolsOperationError::Delete(LeanDocumentDeleteError::StillReferenced.into_prod())
            }
            Self::DeleteNotRemoved => {
                ToolsOperationError::Delete(LeanDocumentDeleteError::NotRemoved.into_prod())
            }
            Self::DeletePassthrough => {
                ToolsOperationError::Delete(LeanDocumentDeleteError::Passthrough.into_prod())
            }
        }
    }
}

#[test]
fn tools_operation_recovery_matches_lean() {
    families::operation_recovery_tools_recovery::replay::<NextCallKind, LeanToolsError>(|input| {
        let error = input.error.into_prod();
        to_family_next_call!(operation_recovery_tools_recovery, error.recovery())
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanWorkspaceRootVerb {
    Publish,
    GrantReader,
    RevokeReader,
    Remove,
    Show,
}

impl LeanWorkspaceRootVerb {
    fn into_prod(self) -> WorkspaceRootVerb {
        match self {
            Self::Publish => WorkspaceRootVerb::Publish,
            Self::GrantReader => WorkspaceRootVerb::GrantReader,
            Self::RevokeReader => WorkspaceRootVerb::RevokeReader,
            Self::Remove => WorkspaceRootVerb::Remove,
            Self::Show => WorkspaceRootVerb::Show,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LeanWorkspaceRootError {
    PathNotAbsolute,
    RootMissing,
    ForeignRoot,
    NodeUnavailable,
    AgentUnavailable,
    NodeActor,
    Passthrough,
}

impl LeanWorkspaceRootError {
    fn into_prod(self) -> WorkspaceRootError {
        match self {
            Self::PathNotAbsolute => WorkspaceRootError::PathNotAbsolute("rel/path".into()),
            Self::RootMissing => WorkspaceRootError::RootMissing("/work".into()),
            Self::ForeignRoot => WorkspaceRootError::ForeignRoot("/work".into()),
            Self::NodeUnavailable => WorkspaceRootError::NodeUnavailable("did:key:n".into()),
            Self::AgentUnavailable => WorkspaceRootError::AgentUnavailable("did:key:a".into()),
            Self::NodeActor => WorkspaceRootError::NodeActor,
            Self::Passthrough => WorkspaceRootError::Passthrough(passthrough()),
        }
    }
}

#[test]
fn workspace_root_operation_recovery_matches_lean() {
    families::operation_recovery_workspace_root_recovery::replay::<
        NextCallKind,
        LeanWorkspaceRootError,
        LeanWorkspaceRootVerb,
    >(|input| {
        let error = input.error.into_prod();
        let next = error.recovery(input.verb.into_prod());
        to_family_next_call!(operation_recovery_workspace_root_recovery, next)
    });
}
