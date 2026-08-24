//! Runtime-authored turns must use this entry point so persistence and visibility follow policy.

use super::*;

pub(crate) const STEERING_WAKE_PROMPT: &str = "Continue with the new steering message.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationKind {
    Steering,
    BackgroundCompletion,
    Goal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationInputVisibility {
    VisibleInput,
    RuntimeControl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationParentStrategy {
    GenerationOwner,
    PreviousRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationProviderInputStrategy {
    PromptOnce,
    HistoryThenControl,
    ControlOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContinuationPolicy {
    pub(crate) source: QueueSource,
    pub(crate) queue_policy: QueuePolicy,
    pub(crate) execution_origin: ExecutionOrigin,
    pub(crate) input_visibility: Option<ContinuationInputVisibility>,
    pub(crate) control_visibility: ContinuationInputVisibility,
    pub(crate) parent_strategy: ContinuationParentStrategy,
    pub(crate) requires_durable_input: bool,
    pub(crate) provider_input_strategy: ContinuationProviderInputStrategy,
}

impl ContinuationKind {
    pub(crate) const fn from_source(source: QueueSource) -> Option<Self> {
        match source {
            QueueSource::Steering => Some(Self::Steering),
            QueueSource::BackgroundCompletion => Some(Self::BackgroundCompletion),
            QueueSource::Goal => Some(Self::Goal),
            QueueSource::User => None,
        }
    }

    pub(crate) fn from_metadata(metadata: Option<&str>) -> Option<Self> {
        Self::from_source(parse_queue_hints(metadata)?.source)
    }

    pub(crate) const fn policy(self) -> ContinuationPolicy {
        match self {
            Self::Steering => ContinuationPolicy {
                source: QueueSource::Steering,
                queue_policy: QueuePolicy::Append,
                execution_origin: ExecutionOrigin::Interactive,
                input_visibility: Some(ContinuationInputVisibility::VisibleInput),
                control_visibility: ContinuationInputVisibility::RuntimeControl,
                parent_strategy: ContinuationParentStrategy::GenerationOwner,
                requires_durable_input: true,
                provider_input_strategy: ContinuationProviderInputStrategy::PromptOnce,
            },
            Self::BackgroundCompletion => ContinuationPolicy {
                source: QueueSource::BackgroundCompletion,
                queue_policy: QueuePolicy::Coalesce,
                execution_origin: ExecutionOrigin::Scheduled,
                input_visibility: Some(ContinuationInputVisibility::RuntimeControl),
                control_visibility: ContinuationInputVisibility::RuntimeControl,
                parent_strategy: ContinuationParentStrategy::GenerationOwner,
                requires_durable_input: true,
                provider_input_strategy: ContinuationProviderInputStrategy::HistoryThenControl,
            },
            Self::Goal => ContinuationPolicy {
                source: QueueSource::Goal,
                queue_policy: QueuePolicy::Coalesce,
                execution_origin: ExecutionOrigin::Scheduled,
                input_visibility: None,
                control_visibility: ContinuationInputVisibility::RuntimeControl,
                parent_strategy: ContinuationParentStrategy::PreviousRequest,
                requires_durable_input: false,
                provider_input_strategy: ContinuationProviderInputStrategy::ControlOnly,
            },
        }
    }

    pub(crate) fn contract_signature(self) -> String {
        let policy = self.policy();
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            queue_source_contract_name(policy.source),
            queue_policy_contract_name(policy.queue_policy),
            execution_origin_contract_name(policy.execution_origin),
            policy
                .input_visibility
                .map_or("none", ContinuationInputVisibility::contract_name),
            policy.control_visibility.contract_name(),
            policy.parent_strategy.contract_name(),
            if policy.requires_durable_input {
                "durable_input"
            } else {
                "request_only"
            },
            policy.provider_input_strategy.contract_name(),
        )
    }

    pub(crate) fn is_automated_wakeup(self, hints: &QueueHints) -> bool {
        self == Self::BackgroundCompletion
            && hints.policy == QueuePolicy::Coalesce
            && hints
                .key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty())
    }

    pub(crate) fn is_subagent_owned(self, hints: &QueueHints) -> bool {
        self == Self::Steering || self.is_automated_wakeup(hints)
    }

    pub(crate) fn is_request_only_control(self, hints: &QueueHints) -> bool {
        matches!(self, Self::Steering | Self::Goal) || self.is_automated_wakeup(hints)
    }
}

impl ContinuationInputVisibility {
    const fn projection(self) -> ConversationProjection {
        match self {
            Self::VisibleInput => ConversationProjection::VisibleInput,
            Self::RuntimeControl => ConversationProjection::RuntimeControl,
        }
    }

    const fn contract_name(self) -> &'static str {
        match self {
            Self::VisibleInput => "visible_input",
            Self::RuntimeControl => "runtime_control",
        }
    }
}

impl ContinuationParentStrategy {
    const fn contract_name(self) -> &'static str {
        match self {
            Self::GenerationOwner => "generation_owner",
            Self::PreviousRequest => "previous_request",
        }
    }
}

impl ContinuationProviderInputStrategy {
    const fn contract_name(self) -> &'static str {
        match self {
            Self::PromptOnce => "prompt_once",
            Self::HistoryThenControl => "history_then_control",
            Self::ControlOnly => "control_only",
        }
    }
}

const fn queue_source_contract_name(source: QueueSource) -> &'static str {
    match source {
        QueueSource::User => "user",
        QueueSource::BackgroundCompletion => "background_completion",
        QueueSource::Steering => "steering",
        QueueSource::Goal => "goal",
    }
}

const fn queue_policy_contract_name(policy: QueuePolicy) -> &'static str {
    match policy {
        QueuePolicy::Append => "append",
        QueuePolicy::Coalesce => "coalesce",
    }
}

const fn execution_origin_contract_name(origin: ExecutionOrigin) -> &'static str {
    match origin {
        ExecutionOrigin::Interactive => "interactive",
        ExecutionOrigin::Scheduled => "scheduled",
    }
}

#[doc(hidden)]
pub fn continuation_policy_contract(kind: &str) -> Option<String> {
    match kind {
        "steering" => Some(ContinuationKind::Steering.contract_signature()),
        "background_completion" => {
            Some(ContinuationKind::BackgroundCompletion.contract_signature())
        }
        "goal" => Some(ContinuationKind::Goal.contract_signature()),
        _ => None,
    }
}

pub(crate) fn metadata_is_request_only_control(metadata: Option<&str>) -> bool {
    let Some(hints) = parse_queue_hints(metadata) else {
        return super::request_is_steering_continuation(metadata);
    };
    ContinuationKind::from_source(hints.source)
        .is_some_and(|kind| kind.is_request_only_control(&hints))
}

pub(crate) fn request_uses_durable_input_as_prompt(metadata: Option<&str>) -> bool {
    super::continuation_version(metadata).is_some()
        && ContinuationKind::from_metadata(metadata).is_some_and(|kind| {
            kind.policy().provider_input_strategy == ContinuationProviderInputStrategy::PromptOnce
        })
}

/// Payloads that may cause the runtime to continue an existing conversation.
pub(crate) enum ConversationContinuation<'a> {
    Steering {
        message: &'a str,
        interrupted_request_id: Option<String>,
    },
    BackgroundCompletion {
        notification: &'a str,
        notification_key: &'a str,
        queued_after_request_id: &'a str,
    },
    Goal {
        goal_id: &'a str,
        prompt: &'a str,
        continuation_sequence: i64,
        wrapup: bool,
    },
}

#[derive(Debug)]
pub(crate) struct EnqueuedConversationContinuation {
    pub(crate) request: EnqueuedAgentRequest,
    pub(crate) input_sequence: Option<u32>,
    pub(crate) created_request: bool,
    pub(crate) created_input: bool,
}

/// Persist a continuation according to its policy, including any input row
/// that must become durable atomically with the consuming request.
pub(crate) async fn enqueue_conversation_continuation(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    continuation: ConversationContinuation<'_>,
) -> Result<EnqueuedConversationContinuation> {
    match continuation {
        ConversationContinuation::Steering {
            message,
            interrupted_request_id,
        } => {
            let policy = ContinuationKind::Steering.policy();
            let enqueued = enqueue_steering_request_with_message(
                node,
                parent,
                message,
                QueueHints {
                    source: policy.source,
                    policy: policy.queue_policy,
                    key: None,
                    queued_after_request_id: None,
                    interrupted_request_id,
                },
            )
            .await?;
            Ok(EnqueuedConversationContinuation {
                request: enqueued.request,
                input_sequence: Some(enqueued.message_sequence),
                created_request: true,
                created_input: true,
            })
        }
        ConversationContinuation::BackgroundCompletion {
            notification,
            notification_key,
            queued_after_request_id,
        } => {
            let policy = ContinuationKind::BackgroundCompletion.policy();
            let enqueued = enqueue_background_completion_with_message(
                node,
                parent,
                notification,
                notification_key,
                crate::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
                QueueHints {
                    source: policy.source,
                    policy: policy.queue_policy,
                    key: Some(format!("background_completion:{}", parent.session_id)),
                    queued_after_request_id: Some(queued_after_request_id.to_string()),
                    interrupted_request_id: None,
                },
            )
            .await?;
            Ok(EnqueuedConversationContinuation {
                request: enqueued.request,
                input_sequence: Some(enqueued.message_sequence),
                created_request: enqueued.created_request,
                created_input: enqueued.created_message,
            })
        }
        ConversationContinuation::Goal {
            goal_id,
            prompt,
            continuation_sequence,
            wrapup,
        } => {
            let (request, created_request) = enqueue_goal_continuation(
                node,
                parent,
                goal_id,
                prompt,
                continuation_sequence,
                wrapup,
            )
            .await?;
            Ok(EnqueuedConversationContinuation {
                request,
                input_sequence: None,
                created_request,
                created_input: false,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationProjection {
    Ordinary,
    VisibleInput,
    RuntimeControl,
}

pub fn classify_continuation_request(
    metadata: Option<&str>,
    request_content: &str,
) -> ConversationProjection {
    let Some(kind) = ContinuationKind::from_metadata(metadata) else {
        return ConversationProjection::Ordinary;
    };
    if kind == ContinuationKind::Steering
        && super::continuation_version(metadata).is_none()
        && request_content != STEERING_WAKE_PROMPT
    {
        return ConversationProjection::VisibleInput;
    }
    kind.policy().control_visibility.projection()
}

pub fn classify_continuation_message(
    metadata: Option<&str>,
    request_id: Option<&str>,
    request_content: Option<&str>,
    message_role: &str,
    message_content: &str,
    message_key: &str,
) -> ConversationProjection {
    if !message_role.trim().eq_ignore_ascii_case("user") {
        return ConversationProjection::Ordinary;
    }
    if crate::background_completion::is_background_completion_notification_message_key(message_key)
    {
        return ConversationProjection::RuntimeControl;
    }
    let Some(kind) = ContinuationKind::from_metadata(metadata) else {
        return ConversationProjection::Ordinary;
    };
    match kind {
        ContinuationKind::Steering => {
            let exact_key = request_id.map(steering_input_message_key);
            if exact_key.as_deref() == Some(message_key) {
                kind.policy()
                    .input_visibility
                    .expect("steering has a durable input")
                    .projection()
            } else if super::continuation_version(metadata).is_none()
                && request_content.is_some_and(|content| {
                    content != STEERING_WAKE_PROMPT
                        && (content == message_content
                            || gents_protocol::transcript::present_persisted_message(
                                message_role,
                                message_content,
                            )
                            .body_markdown
                                == content)
                })
            {
                kind.policy()
                    .input_visibility
                    .expect("steering has a durable input")
                    .projection()
            } else {
                kind.policy().control_visibility.projection()
            }
        }
        ContinuationKind::BackgroundCompletion => kind
            .policy()
            .input_visibility
            .expect("background completions have a durable input")
            .projection(),
        ContinuationKind::Goal => kind.policy().control_visibility.projection(),
    }
}
