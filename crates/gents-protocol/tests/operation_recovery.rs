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

macro_rules! lean_map {
    ($name:ident => $prod:ty { $($variant:ident => $expr:expr),* $(,)? }) => {
        #[derive(Debug, Clone, Copy, Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum $name {
            $($variant,)*
        }

        impl $name {
            fn into_prod(self) -> $prod {
                match self {
                    $(Self::$variant => $expr,)*
                }
            }
        }
    };
}

macro_rules! lean_doc_delegate_error {
    ($name:ident => $prod:ident { $($variant:ident => $expr:expr),* $(,)? }) => {
        lean_map!($name => $prod {
            $($variant => $expr,)*
            DocumentIdRequired => $prod::Document(LeanDocumentError::IdRequired.into_prod()),
            DocumentExists => $prod::Document(LeanDocumentError::Exists.into_prod()),
            DocumentMissing => $prod::Document(LeanDocumentError::Missing.into_prod()),
            DocumentAmbiguousOwner => $prod::Document(LeanDocumentError::AmbiguousOwner.into_prod()),
            DocumentInvalid => $prod::Document(LeanDocumentError::Invalid.into_prod()),
            DocumentPassthrough => $prod::Document(LeanDocumentError::Passthrough.into_prod()),
            DeleteNoTargets => $prod::Delete(LeanDocumentDeleteError::NoTargets.into_prod()),
            DeleteDuplicateTarget => $prod::Delete(LeanDocumentDeleteError::DuplicateTarget.into_prod()),
            DeleteTargetMissing => $prod::Delete(LeanDocumentDeleteError::TargetMissing.into_prod()),
            DeleteDigestRequired => $prod::Delete(LeanDocumentDeleteError::DigestRequired.into_prod()),
            DeleteDigestStale => $prod::Delete(LeanDocumentDeleteError::DigestStale.into_prod()),
            DeleteStillReferenced => $prod::Delete(LeanDocumentDeleteError::StillReferenced.into_prod()),
            DeleteNotRemoved => $prod::Delete(LeanDocumentDeleteError::NotRemoved.into_prod()),
            DeletePassthrough => $prod::Delete(LeanDocumentDeleteError::Passthrough.into_prod()),
        });
    };
}

fn passthrough() -> PassedThrough {
    PassedThrough::error(std::io::Error::other("upstream"))
}

fn boxed_error() -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other("cause"))
}

lean_map!(LeanAgentCreateOp => AgentCreateOperation {
    Create => AgentCreateOperation::Create,
    Clone => AgentCreateOperation::Clone,
});

lean_map!(LeanAgentError => AgentOperationError {
    PreviewRequiresOperation => AgentOperationError::PreviewRequiresOperation,
    EditRejectsCloneFrom => AgentOperationError::EditRejectsCloneFrom,
    EditRejectsToolsFields => AgentOperationError::EditRejectsToolsFields,
    DisableAcceptsOnlyTarget => AgentOperationError::DisableAcceptsOnlyTarget,
    CloneRequiresFrom => AgentOperationError::CloneRequiresFrom,
    UnknownOperation => AgentOperationError::UnknownOperation("bogus".into()),
    IdentityBindingRequired => AgentOperationError::IdentityBindingRequired,
    IdentityBindingMissing => AgentOperationError::IdentityBindingMissing,
    IdentityBindingMismatch => AgentOperationError::IdentityBindingMismatch {
        binding: "did:key:a".into(),
        agent: "did:key:b".into(),
    },
    NewAgentDisabled => AgentOperationError::NewAgentDisabled,
    AgentDidRequired => AgentOperationError::AgentDidRequired,
    AgentExists => AgentOperationError::AgentExists("did:key:a".into()),
    DisplayNameRequired => AgentOperationError::DisplayNameRequired,
    ProfileNotPublished => AgentOperationError::ProfileNotPublished,
    SystemPromptRequired => AgentOperationError::SystemPromptRequired,
    CloneSourceNotEnabled => AgentOperationError::CloneSourceNotEnabled,
    AgentMissing => AgentOperationError::AgentMissing("did:key:a".into()),
    CatalogNotHeld => AgentOperationError::CatalogNotHeld,
    AgentNotHeld => AgentOperationError::AgentNotHeld("did:key:a".into()),
    UnsupportedEditField => AgentOperationError::UnsupportedEditField("x".into()),
    DisplayNameEmpty => AgentOperationError::DisplayNameEmpty,
    SystemPromptBlank => AgentOperationError::SystemPromptBlank,
    ContextIdBlank => AgentOperationError::ContextIdBlank,
    DefaultAgentDisabled => AgentOperationError::DefaultAgentDisabled("did:key:a".into()),
    SelfDisable => AgentOperationError::SelfDisable,
    MissingAgentContext => AgentOperationError::MissingDocument {
        collection: "AgentContext",
        id: "ctx-1".into(),
    },
    MissingOtherDocument => AgentOperationError::MissingDocument {
        collection: "Tools",
        id: "tools-1".into(),
    },
    GeneratedContextIdExists => AgentOperationError::GeneratedContextIdExists,
    GeneratedToolsIdExists => AgentOperationError::GeneratedToolsIdExists,
    ClonePresetRefused => AgentOperationError::ClonePresetRefused,
    CloneContextIdRefused => AgentOperationError::CloneContextIdRefused,
    CreateContextIdConflicts => AgentOperationError::CreateContextIdConflicts,
    UnknownPreset => AgentOperationError::UnknownPreset,
    CloneSourceGrantNotHeld => AgentOperationError::CloneSourceGrantNotHeld {
        from: "did:key:src".into(),
        source: boxed_error(),
    },
    ObjectRequired => AgentOperationError::ObjectRequired("agent"),
    ContextPatchMismatch => AgentOperationError::ContextPatchMismatch,
    AgentHasNoContext => AgentOperationError::AgentHasNoContext,
    SystemPromptContextShared => AgentOperationError::SystemPromptContextShared,
    AdmissionActorMismatch => AgentOperationError::AdmissionActorMismatch,
    CandidateIdentityMissing => AgentOperationError::CandidateIdentityMissing,
    CandidateDoesNotResolve => AgentOperationError::CandidateDoesNotResolve,
    Passthrough => AgentOperationError::Passthrough(passthrough()),
});

#[test]
fn agent_operation_recovery_matches_lean() {
    families::operation_recovery_agent_recovery::replay::<
        LeanAgentCreateOp,
        LeanAgentError,
        NextCallKind,
    >(|input| {
        let next = input
            .error
            .into_prod()
            .recovery(input.operation.into_prod());
        to_family_next_call!(operation_recovery_agent_recovery, next)
    });

    families::operation_recovery_agent_edit_recovery::replay::<LeanAgentError, NextCallKind>(
        |input| {
            to_family_next_call!(
                operation_recovery_agent_edit_recovery,
                input.error.into_prod().edit_recovery()
            )
        },
    );
}

lean_map!(LeanDocumentResource => &'static str {
    Context => "context",
    McpService => "mcp-service",
    Skill => "skill",
    Tools => "tools",
});

lean_map!(LeanDocumentError => DocumentOperationError {
    IdRequired => DocumentOperationError::IdRequired { collection: "Doc" },
    Exists => DocumentOperationError::Exists {
        collection: "Doc",
        id: "id-1".into(),
    },
    Missing => DocumentOperationError::Missing {
        collection: "Doc",
        id: "id-1".into(),
        node_did: "did:key:node".into(),
    },
    AmbiguousOwner => DocumentOperationError::AmbiguousOwner {
        collection: "Doc",
        id: "id-1".into(),
        owners: vec!["did:key:n1".into(), "did:key:n2".into()],
    },
    Invalid => DocumentOperationError::Invalid {
        collection: "Doc",
        source: boxed_error(),
    },
    Passthrough => DocumentOperationError::Passthrough(passthrough()),
});

lean_map!(LeanDocumentDeleteError => DocumentDeleteError {
    NoTargets => DocumentDeleteError::NoTargets,
    DuplicateTarget => DocumentDeleteError::DuplicateTarget {
        collection: "Doc",
        id: "id-1".into(),
    },
    TargetMissing => DocumentDeleteError::TargetMissing {
        collection: "Doc",
        id: "id-1".into(),
    },
    DigestRequired => DocumentDeleteError::DigestRequired,
    DigestStale => DocumentDeleteError::DigestStale {
        expected: "a".into(),
        current: "b".into(),
    },
    StillReferenced => DocumentDeleteError::StillReferenced {
        collection: "Agent",
        id: "a-1".into(),
        field: "ref".into(),
        target: "Doc",
        target_id: "id-1".into(),
    },
    NotRemoved => DocumentDeleteError::NotRemoved {
        collection: "Doc",
        id: "id-1".into(),
    },
    Passthrough => DocumentDeleteError::Passthrough(passthrough()),
});

#[test]
fn document_operation_recovery_matches_lean() {
    families::operation_recovery_document_recovery::replay::<
        LeanDocumentError,
        LeanDocumentResource,
        NextCallKind,
    >(|input| {
        let next = input.error.into_prod().recovery(input.resource.into_prod());
        to_family_next_call!(operation_recovery_document_recovery, next)
    });

    families::operation_recovery_document_delete_recovery::replay::<
        LeanDocumentDeleteError,
        LeanDocumentResource,
        NextCallKind,
    >(|input| {
        let next = input.error.into_prod().recovery(input.resource.into_prod());
        to_family_next_call!(operation_recovery_document_delete_recovery, next)
    });
}

lean_map!(LeanGoalError => GoalOperationError {
    Denied => GoalOperationError::Denied,
    SessionMissing => GoalOperationError::SessionMissing("s-1".into()),
    NoGoal => GoalOperationError::NoGoal("s-1".into()),
    Busy => GoalOperationError::Busy,
    LiveGoal => GoalOperationError::LiveGoal,
    Stale => GoalOperationError::Stale,
    Conflict => GoalOperationError::Conflict,
    InvalidObjective => GoalOperationError::InvalidObjective,
    InvalidBudget => GoalOperationError::InvalidBudget,
    InvalidCeiling => GoalOperationError::InvalidCeiling,
    ConfirmBlocked => GoalOperationError::ConfirmBlocked,
    KeepTrying => GoalOperationError::KeepTrying,
    NothingToChange => GoalOperationError::NothingToChange,
    RuntimeOnly => GoalOperationError::RuntimeOnly,
    PersonOnly => GoalOperationError::PersonOnly,
    FieldsNeedKeep => GoalOperationError::FieldsNeedKeep,
    NotEditable => GoalOperationError::NotEditable(GoalStatus::Paused),
    ResumeRequired => GoalOperationError::ResumeRequired(GoalStatus::Paused),
    ObligationsUnmet => GoalOperationError::ObligationsUnmet("unmet".into()),
    FirstRequestInvalid => GoalOperationError::FirstRequestInvalid("invalid"),
    FirstRequestMismatch => GoalOperationError::FirstRequestMismatch,
    ReasonNotAccepted => GoalOperationError::ReasonNotAccepted,
    ReasonRequired => GoalOperationError::ReasonRequired,
    IllegalTransition => GoalOperationError::IllegalTransition {
        target: "active",
        status: "complete",
    },
    Passthrough => GoalOperationError::Passthrough(passthrough()),
});

#[test]
fn goal_operation_recovery_matches_lean() {
    families::operation_recovery_goal_recovery::replay::<LeanGoalError, NextCallKind>(|input| {
        to_family_next_call!(
            operation_recovery_goal_recovery,
            input.error.into_prod().recovery()
        )
    });
}

lean_map!(LeanInferenceDocument => InferenceDocument {
    Backend => InferenceDocument::Backend,
    Profile => InferenceDocument::Profile,
});

lean_map!(LeanInferenceVerb => &'static str {
    Create => "create",
    Update => "update",
    Remove => "remove",
    Rm => "rm",
});

lean_map!(LeanInferenceError => InferenceOperationError {
    RawCredential => InferenceOperationError::RawCredential,
    OauthAccount => InferenceOperationError::OauthAccount,
    OauthPool => InferenceOperationError::OauthPool,
    LocalServerOnly => InferenceOperationError::LocalServerOnly,
    StartsEnabled => InferenceOperationError::StartsEnabled,
    OtherAccount => InferenceOperationError::OtherAccount {
        backend_id: "b-1".into(),
        provider: "openai".into(),
    },
    OtherPool => InferenceOperationError::OtherPool {
        backend_id: "b-1".into(),
        provider: "anthropic".into(),
    },
    AccountOperation => InferenceOperationError::AccountOperation(AccountOperation::List),
    ExistsBackend => InferenceOperationError::Exists {
        document: InferenceDocument::Backend,
        id: "b-1".into(),
    },
    ExistsProfile => InferenceOperationError::Exists {
        document: InferenceDocument::Profile,
        id: "p-1".into(),
    },
    MissingBackend => InferenceOperationError::Missing {
        document: InferenceDocument::Backend,
        id: "b-1".into(),
    },
    MissingProfile => InferenceOperationError::Missing {
        document: InferenceDocument::Profile,
        id: "p-1".into(),
    },
    IdRequiredBackend => InferenceOperationError::IdRequired(InferenceDocument::Backend),
    IdRequiredProfile => InferenceOperationError::IdRequired(InferenceDocument::Profile),
    Invalid => InferenceOperationError::Invalid(boxed_error()),
    Passthrough => InferenceOperationError::Passthrough(passthrough()),
});

#[test]
fn inference_operation_recovery_matches_lean() {
    families::operation_recovery_inference_recovery::replay::<
        LeanInferenceDocument,
        LeanInferenceError,
        LeanInferenceVerb,
        NextCallKind,
    >(|input| {
        let next = input
            .error
            .into_prod()
            .recovery(input.document.into_prod(), input.verb.into_prod());
        to_family_next_call!(operation_recovery_inference_recovery, next)
    });
}

fn test_home() -> PathBuf {
    PathBuf::from("/tmp/gents-home")
}

lean_map!(LeanBlocker => Blocker {
    PortInUse => Blocker::PortInUse { port: 8080, holder: None },
    HandStartedNode => Blocker::HandStartedNode { port: 8080, home: None },
    KeychainUnavailable => Blocker::KeychainUnavailable,
    StoreRefused => Blocker::StoreRefused {
        kind: IncompatibleStoreKind::LegacyRocksDb,
        message: "refused".into(),
    },
    CannotSetAside => Blocker::CannotSetAside { home: test_home(), reason: "busy".into() },
    KeepsExiting => Blocker::KeepsExiting { code: Some(1), last_error: None },
    NoAnswer => Blocker::NoAnswer { seconds: 10, last_error: None },
    NoServiceManager => Blocker::NoServiceManager { home: test_home() },
    HomeUnreadable => Blocker::HomeUnreadable { home: test_home(), reason: "denied".into() },
    HomeBusy => Blocker::HomeBusy { home: test_home() },
    OldNodeStillRunning => Blocker::OldNodeStillRunning { pid: 42 },
    ServiceUpdateFailed => Blocker::ServiceUpdateFailed { reason: "failed".into() },
    BackgroundApprovalPending => Blocker::BackgroundApprovalPending { detail: None },
    NewerNode => Blocker::NewerNode { running: "2.0.0".into(), ours: "1.0.0".into() },
    ServiceServesAnotherHome => Blocker::ServiceServesAnotherHome {
        home: test_home(),
        serving: PathBuf::from("/tmp/other"),
        free_port: None,
    },
});

#[test]
fn install_repair_blocker_next_call_matches_lean() {
    families::operation_recovery_install_repair_blocker_next_call::replay::<
        LeanBlocker,
        NextCallKind,
    >(|input| {
        input.blocker.into_prod().next_call().map(|call| {
            to_family_next_call!(operation_recovery_install_repair_blocker_next_call, call)
        })
    });
}

lean_map!(LeanMailboxError => MailboxOperationError {
    ItemMissing => MailboxOperationError::ItemMissing,
    ItemAmbiguous => MailboxOperationError::ItemAmbiguous,
    UnknownStatus => MailboxOperationError::UnknownStatus("bad".into()),
    UnknownAction => MailboxOperationError::UnknownAction("bad".into()),
    NotOpen => MailboxOperationError::NotOpen,
    NotReplyable => MailboxOperationError::NotReplyable,
    NotAQuestion => MailboxOperationError::NotAQuestion,
    RouteDiffers => MailboxOperationError::RouteDiffers,
    QuestionChanged => MailboxOperationError::QuestionChanged,
    NeedsAnswer => MailboxOperationError::NeedsAnswer,
    QuestionPayloadMissing => MailboxOperationError::QuestionPayloadMissing,
    ActorRequired => MailboxOperationError::ActorRequired,
    DismissalLost => MailboxOperationError::DismissalLost,
    Question => MailboxOperationError::Question(MailboxQuestionError::Empty),
    QuestionReply => MailboxOperationError::QuestionReply(MailboxQuestionReplyError::VersionChanged),
    Passthrough => MailboxOperationError::Passthrough(passthrough()),
});

#[test]
fn mailbox_operation_recovery_matches_lean() {
    families::operation_recovery_mailbox_recovery::replay::<LeanMailboxError, NextCallKind>(
        |input| {
            to_family_next_call!(
                operation_recovery_mailbox_recovery,
                input.error.into_prod().recovery()
            )
        },
    );
}

lean_doc_delegate_error!(LeanMcpServiceError => McpServiceOperationError {
    Endpoint => McpServiceOperationError::Endpoint { source: boxed_error() },
});

#[test]
fn mcp_service_operation_recovery_matches_lean() {
    families::operation_recovery_mcp_service_recovery::replay::<LeanMcpServiceError, NextCallKind>(
        |input| {
            to_family_next_call!(
                operation_recovery_mcp_service_recovery,
                input.error.into_prod().recovery()
            )
        },
    );
}

lean_map!(LeanRegistryError => RegistryError {
    PeerIdUnknown => RegistryError::PeerIdUnknown,
    UnknownTemplate => RegistryError::UnknownTemplate("tmpl".into()),
    Passthrough => RegistryError::Passthrough(passthrough()),
});

#[test]
fn registry_error_recovery_matches_lean() {
    families::operation_recovery_registry_recovery::replay::<NextCallKind, LeanRegistryError>(
        |input| {
            to_family_next_call!(
                operation_recovery_registry_recovery,
                input.error.into_prod().recovery()
            )
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
            let next = input
                .error
                .into_prod(input.operation)
                .recovery(input.operation);
            to_family_next_call!(operation_recovery_pack_recovery, next)
        },
    );
}

lean_map!(LeanAgentPermissionError => AgentPermissionError {
    InvalidAgentDid => AgentPermissionError::InvalidAgentDid("bad".into(), boxed_error()),
    AgentUnavailable => AgentPermissionError::AgentUnavailable("did:key:a".into()),
    NodeUnavailable => AgentPermissionError::NodeUnavailable("did:key:n".into()),
    Passthrough => AgentPermissionError::Passthrough(passthrough()),
});

#[test]
fn agent_permission_recovery_matches_lean() {
    families::operation_recovery_agent_permission_recovery::replay::<
        LeanAgentPermissionError,
        NextCallKind,
    >(|input| {
        to_family_next_call!(
            operation_recovery_agent_permission_recovery,
            input.error.into_prod().recovery()
        )
    });
}

lean_map!(LeanRequestError => RequestOperationError {
    FieldRequired => RequestOperationError::FieldRequired("node_did"),
    ContentRequired => RequestOperationError::ContentRequired,
    IdentityRequired => RequestOperationError::IdentityRequired,
    QueueEntryOwned => RequestOperationError::QueueEntryOwned,
    ConflictingActiveRequests => RequestOperationError::ConflictingActiveRequests {
        session_id: "s-1".into(),
    },
    NodeUnavailable => RequestOperationError::NodeUnavailable {
        node_did: "did:key:n".into(),
    },
    NoDefaultAgent => RequestOperationError::NoDefaultAgent {
        node_did: "did:key:n".into(),
    },
    AgentUnavailable => RequestOperationError::AgentUnavailable {
        node_did: "did:key:n".into(),
        agent_did: "did:key:a".into(),
    },
    SessionAgentMismatch => RequestOperationError::SessionAgentMismatch {
        session_id: "s-1".into(),
        session_agent: "did:key:a".into(),
        requested: "did:key:b".into(),
    },
    MailboxCause => RequestOperationError::MailboxCause {
        item_id: "m-1".into(),
        refusal: MailboxCauseRefusal::Closed,
    },
    NotFound => RequestOperationError::NotFound {
        node_did: "did:key:n".into(),
        request_id: "r-1".into(),
    },
    Ambiguous => RequestOperationError::Ambiguous {
        node_did: "did:key:n".into(),
        request_id: "r-1".into(),
    },
    ReissueParentState => RequestOperationError::ReissueParentState {
        kind: "retry",
        request_id: "r-1".into(),
        state: "completed",
        failure_reason: "none".into(),
        needs: "failed",
    },
    ReissueNotInteractive => RequestOperationError::ReissueNotInteractive {
        request_id: "r-1".into(),
    },
    RetryBudgetExhausted => RequestOperationError::RetryBudgetExhausted {
        request_id: "r-1".into(),
        retry_count: 3,
        max_retries: 3,
    },
    ReissueDeadlineClosed => RequestOperationError::ReissueDeadlineClosed {
        request_id: "r-1".into(),
        deadline: "2026-01-01T00:00:00Z".into(),
    },
    ReissueNotLatest => RequestOperationError::ReissueNotLatest {
        request_id: "r-1".into(),
        session_id: "s-1".into(),
        latest_request_id: "r-2".into(),
    },
    WaitTimedOut => RequestOperationError::WaitTimedOut {
        request_id: "r-1".into(),
        timeout_secs: 30,
        state: "executing".into(),
    },
    OutputUnavailable => RequestOperationError::OutputUnavailable {
        request_id: "r-1".into(),
        observation: "incomplete",
    },
    SuccessorInvalid => RequestOperationError::SuccessorInvalid {
        request_id: "r-1".into(),
        reason: "cycle",
    },
    ReceiptMismatch => RequestOperationError::ReceiptMismatch {
        request_id: "r-1".into(),
        reason: "digest",
    },
    InterruptRefused => RequestOperationError::InterruptRefused {
        request_id: "r-1".into(),
        reason: "terminal".into(),
    },
    Unconfirmed => RequestOperationError::Unconfirmed {
        request_id: "r-1".into(),
        source: boxed_error(),
    },
    IdentityConflict => RequestOperationError::IdentityConflict {
        request_id: "r-1".into(),
        source: boxed_error(),
    },
    Passthrough => RequestOperationError::Passthrough(passthrough()),
});

#[test]
fn request_operation_error_code_and_next_call_match_lean() {
    families::operation_recovery_request_error_code::replay::<LeanRequestError>(|input| {
        input.error.into_prod().code().to_owned()
    });

    families::operation_recovery_request_next_call::replay::<NextCallKind, LeanRequestError>(
        |input| {
            to_family_next_call!(
                operation_recovery_request_next_call,
                input.error.into_prod().next_call()
            )
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

lean_map!(LeanSessionError => SessionOperationError {
    FieldRequired => SessionOperationError::FieldRequired("session_id"),
    IdentityRequired => SessionOperationError::IdentityRequired,
    NotFound => SessionOperationError::NotFound {
        node_did: "did:key:n".into(),
        session_id: "s-1".into(),
    },
    Ambiguous => SessionOperationError::Ambiguous {
        node_did: "did:key:n".into(),
        session_id: "s-1".into(),
    },
    InvalidDocument => SessionOperationError::InvalidDocument {
        session_id: "s-1".into(),
        source: SessionRowError::Blank("session_id"),
    },
    RequesterMissing => SessionOperationError::RequesterMissing {
        session_id: "s-1".into(),
    },
    RevisionOutOfRange => SessionOperationError::RevisionOutOfRange { revision: 9 },
    RequesterMismatch => SessionOperationError::RequesterMismatch {
        session_id: "s-1".into(),
    },
    ForkSourceBusy => SessionOperationError::ForkSourceBusy {
        session_id: "s-1".into(),
    },
    ForkTurnOutOfRange => SessionOperationError::ForkTurnOutOfRange {
        session_id: "s-1".into(),
        at_user_turn: 5,
        user_messages: 2,
    },
    ForkMessageNotEditable => SessionOperationError::ForkMessageNotEditable {
        message_key: "m-1".into(),
        reason: "missing".into(),
    },
    ForkClientMismatch => SessionOperationError::ForkClientMismatch {
        client_did: "did:key:c".into(),
    },
    ForkChildNotCanonical => SessionOperationError::ForkChildNotCanonical {
        child_session_id: " bad ".into(),
    },
    CrossAgentForkUnavailable => SessionOperationError::CrossAgentForkUnavailable,
    AgentUnavailable => SessionOperationError::AgentUnavailable {
        agent_did: "did:key:a".into(),
    },
    ForkRelationshipsIncomplete => SessionOperationError::ForkRelationshipsIncomplete {
        session_id: "s-1".into(),
        source: boxed_error(),
    },
    ForkRecoveryIntentRequired => SessionOperationError::ForkRecoveryIntentRequired {
        session_id: "s-1".into(),
    },
    RemoteNodeUnsupported => SessionOperationError::RemoteNodeUnsupported {
        node_did: "did:key:n".into(),
    },
    ResumeBlocked => SessionOperationError::ResumeBlocked {
        session_id: "s-1".into(),
        blocker: ResumeBlocker::Requests,
    },
    ProcessOwnershipUnverifiable => SessionOperationError::ProcessOwnershipUnverifiable {
        session_id: "s-1".into(),
        source: boxed_error(),
    },
    Passthrough => SessionOperationError::Passthrough(passthrough()),
});

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
            to_family_next_call!(
                operation_recovery_session_next_call,
                input.error.into_prod().next_call()
            )
        },
    );
}

lean_doc_delegate_error!(LeanSkillError => SkillOperationError {
    ImportExists => SkillOperationError::ImportExists { skill_id: "sk-1".into() },
    OutsideToolRoot => SkillOperationError::OutsideToolRoot {
        path: "/outside".into(),
        source: boxed_error(),
    },
    FileReadNotGranted => SkillOperationError::FileReadNotGranted,
    ToolRootMissing => SkillOperationError::ToolRootMissing,
    Source => SkillOperationError::Source {
        path: "/tmp/s.md".into(),
        source: boxed_error(),
    },
});

#[test]
fn skill_operation_recovery_matches_lean() {
    families::operation_recovery_skill_recovery::replay::<NextCallKind, LeanSkillError>(|input| {
        to_family_next_call!(
            operation_recovery_skill_recovery,
            input.error.into_prod().recovery()
        )
    });
}

lean_map!(LeanTaskError => TaskOperationError {
    TaskMissing => TaskOperationError::TaskMissing {
        task_id: "t-1".into(),
        node_did: "did:key:n".into(),
    },
    TaskDisabled => TaskOperationError::TaskDisabled("t-1".into()),
    AgentMissing => TaskOperationError::AgentMissing {
        agent_did: "did:key:a".into(),
        node_did: "did:key:n".into(),
    },
    AgentDisabled => TaskOperationError::AgentDisabled("did:key:a".into()),
    CommandTask => TaskOperationError::CommandTask("t-1".into()),
    ArgsNotObject => TaskOperationError::ArgsNotObject(serde_json::json!(42)),
    StableKeyRequired => TaskOperationError::StableKeyRequired("t-1".into()),
    SessionLabelAndContinue => TaskOperationError::SessionLabelAndContinue,
    ScheduleMissing => TaskOperationError::ScheduleMissing {
        schedule_id: "sc-1".into(),
        node_did: "did:key:n".into(),
    },
    ScheduleAmbiguous => TaskOperationError::ScheduleAmbiguous {
        schedule_id: "sc-1".into(),
        node_did: "did:key:n".into(),
    },
    NoEnabledTrigger => TaskOperationError::NoEnabledTrigger("sc-1".into()),
    ManyEnabledTriggers => TaskOperationError::ManyEnabledTriggers("sc-1".into()),
    Passthrough => TaskOperationError::Passthrough(passthrough()),
});

#[test]
fn task_operation_recovery_matches_lean() {
    families::operation_recovery_task_recovery::replay::<NextCallKind, LeanTaskError>(|input| {
        to_family_next_call!(
            operation_recovery_task_recovery,
            input.error.into_prod().recovery()
        )
    });
}

lean_doc_delegate_error!(LeanToolsError => ToolsOperationError {
    SharedUnacknowledged => ToolsOperationError::SharedUnacknowledged {
        agents: vec!["did:key:a".into()],
    },
    SharedWithoutCatalog => ToolsOperationError::SharedWithoutCatalog {
        agents: vec!["did:key:a".into()],
    },
    GrantNotHeld => ToolsOperationError::GrantNotHeld {
        grants: vec!["shell".into()],
    },
    LosesOwnControl => ToolsOperationError::LosesOwnControl { source: boxed_error() },
    DropsSettings => ToolsOperationError::DropsSettings {
        dropped: vec!["host".into()],
        groups: vec!["host".into()],
    },
    PluginNotInstalled => ToolsOperationError::PluginNotInstalled {
        plugin: "acme/p".into(),
        reason: "missing".into(),
    },
});

#[test]
fn tools_operation_recovery_matches_lean() {
    families::operation_recovery_tools_recovery::replay::<NextCallKind, LeanToolsError>(|input| {
        to_family_next_call!(
            operation_recovery_tools_recovery,
            input.error.into_prod().recovery()
        )
    });
}

lean_map!(LeanWorkspaceRootVerb => WorkspaceRootVerb {
    Publish => WorkspaceRootVerb::Publish,
    GrantReader => WorkspaceRootVerb::GrantReader,
    RevokeReader => WorkspaceRootVerb::RevokeReader,
    Remove => WorkspaceRootVerb::Remove,
    Show => WorkspaceRootVerb::Show,
});

lean_map!(LeanWorkspaceRootError => WorkspaceRootError {
    PathNotAbsolute => WorkspaceRootError::PathNotAbsolute("rel/path".into()),
    RootMissing => WorkspaceRootError::RootMissing("/work".into()),
    ForeignRoot => WorkspaceRootError::ForeignRoot("/work".into()),
    NodeUnavailable => WorkspaceRootError::NodeUnavailable("did:key:n".into()),
    AgentUnavailable => WorkspaceRootError::AgentUnavailable("did:key:a".into()),
    NodeActor => WorkspaceRootError::NodeActor,
    Passthrough => WorkspaceRootError::Passthrough(passthrough()),
});

#[test]
fn workspace_root_operation_recovery_matches_lean() {
    families::operation_recovery_workspace_root_recovery::replay::<
        NextCallKind,
        LeanWorkspaceRootError,
        LeanWorkspaceRootVerb,
    >(|input| {
        let next = input.error.into_prod().recovery(input.verb.into_prod());
        to_family_next_call!(operation_recovery_workspace_root_recovery, next)
    });
}
