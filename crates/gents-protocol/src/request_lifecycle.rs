use std::error::Error;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The TypeScript union is derived from the variant names, not from the
/// manual serde impls; `typescript_union_matches_wire_tokens` binds it to `as_str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(rename_all = "camelCase"))]
pub enum RequestLifecycleState {
    WorkspaceBindingPending,
    Pending,
    Claimed,
    Processing,
    WaitingForBackend,
    ResumingBackend,
    Completed,
    Failed,
    Superseded,
    Dead,
    Interrupted,
}

impl RequestLifecycleState {
    pub const ALL: [Self; 11] = [
        Self::WorkspaceBindingPending,
        Self::Pending,
        Self::Claimed,
        Self::Processing,
        Self::WaitingForBackend,
        Self::ResumingBackend,
        Self::Completed,
        Self::Failed,
        Self::Superseded,
        Self::Dead,
        Self::Interrupted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceBindingPending => "workspaceBindingPending",
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Processing => "processing",
            Self::WaitingForBackend => "waitingForBackend",
            Self::ResumingBackend => "resumingBackend",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
            Self::Dead => "dead",
            Self::Interrupted => "interrupted",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Superseded | Self::Dead | Self::Interrupted
        )
    }

    /// A live terminal owner can certify no provider work only before the
    /// Processing fence, which precedes title dispatch and compaction as well
    /// as inference. Mirrors Lean `Request.Projection.stoppedBeforeRunning`.
    pub const fn certifies_stopped_before_running(self, target: Self, owned: bool) -> bool {
        owned && matches!(self, Self::Claimed) && matches!(target, Self::Interrupted)
    }

    pub const fn is_claimable(self) -> bool {
        matches!(self, Self::Pending)
    }

    pub const fn is_active_runtime(self) -> bool {
        matches!(
            self,
            Self::Pending
                | Self::Claimed
                | Self::Processing
                | Self::WaitingForBackend
                | Self::ResumingBackend
        )
    }

    /// A claimed execution that has not settled. It holds its session's head
    /// whether it is running, parked for its backend or resuming, so later
    /// input queues behind it and no other request in the session may start.
    pub const fn is_unsettled_execution(self) -> bool {
        matches!(
            self,
            Self::Claimed | Self::Processing | Self::WaitingForBackend | Self::ResumingBackend
        )
    }

    pub fn parse(value: &str) -> Result<Self, InvalidRequestLifecycleState> {
        match value {
            "workspaceBindingPending" => Ok(Self::WorkspaceBindingPending),
            "pending" => Ok(Self::Pending),
            "claimed" => Ok(Self::Claimed),
            "processing" => Ok(Self::Processing),
            "waitingForBackend" => Ok(Self::WaitingForBackend),
            "resumingBackend" => Ok(Self::ResumingBackend),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "superseded" => Ok(Self::Superseded),
            "dead" => Ok(Self::Dead),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(InvalidRequestLifecycleState {
                state: value.to_string(),
            }),
        }
    }

    pub fn parse_opt(value: Option<&str>) -> Option<Self> {
        value.and_then(|value| Self::parse(value).ok())
    }

    pub fn is_terminal_str(value: Option<&str>) -> bool {
        Self::parse_opt(value).is_some_and(Self::is_terminal)
    }

    pub fn graphql_list(states: impl IntoIterator<Item = Self>) -> String {
        let quoted: Vec<String> = states
            .into_iter()
            .map(|state| format!("\"{}\"", state.as_str()))
            .collect();
        format!("[{}]", quoted.join(", "))
    }

    pub fn terminal_graphql_list() -> String {
        Self::graphql_list(Self::ALL.into_iter().filter(|state| state.is_terminal()))
    }

    pub fn active_runtime_graphql_list() -> String {
        Self::graphql_list(
            Self::ALL
                .into_iter()
                .filter(|state| state.is_active_runtime()),
        )
    }

    pub fn unsettled_execution() -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(|state| state.is_unsettled_execution())
    }

    pub fn unsettled_execution_graphql_list() -> String {
        Self::graphql_list(Self::unsettled_execution())
    }

    pub fn nonterminal_graphql_list() -> String {
        Self::graphql_list(Self::ALL.into_iter().filter(|state| !state.is_terminal()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRequestLifecycleState {
    state: String,
}

impl InvalidRequestLifecycleState {
    pub fn value(&self) -> &str {
        &self.state
    }
}

impl Display for InvalidRequestLifecycleState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid request lifecycle state: {}", self.state)
    }
}

impl Error for InvalidRequestLifecycleState {}

impl TryFrom<&str> for RequestLifecycleState {
    type Error = InvalidRequestLifecycleState;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl Display for RequestLifecycleState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Serialize for RequestLifecycleState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RequestLifecycleState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// The single request action that takes a request between two lifecycle
/// states. Mirrors Lean `Conformance.RequestTransition.action?`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestTransitionAction {
    BindWorkspace,
    Claim,
    DedupLose,
    AdmissionReject,
    BeginInference,
    ContinueProcessing,
    ParkBackendClosed,
    ResumeBackendReady,
    BeginResumed,
    InterruptResuming,
    ExpireResuming,
    InterruptBackendWait,
    ExpireBackendWait,
    Finish,
    Fail,
    FailBeforeStream,
    Expire,
    InterruptBeforeClaim,
    InterruptClaimed,
    InterruptProcessing,
}

impl RequestTransitionAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BindWorkspace => "bindWorkspace",
            Self::Claim => "claim",
            Self::DedupLose => "dedupLose",
            Self::AdmissionReject => "admissionReject",
            Self::BeginInference => "beginInference",
            Self::ContinueProcessing => "continueProcessing",
            Self::ParkBackendClosed => "parkBackendClosed",
            Self::ResumeBackendReady => "resumeBackendReady",
            Self::BeginResumed => "beginResumed",
            Self::InterruptResuming => "interruptResuming",
            Self::ExpireResuming => "expireResuming",
            Self::InterruptBackendWait => "interruptBackendWait",
            Self::ExpireBackendWait => "expireBackendWait",
            Self::Finish => "finish",
            Self::Fail => "fail",
            Self::FailBeforeStream => "failBeforeStream",
            Self::Expire => "expire",
            Self::InterruptBeforeClaim => "interruptBeforeClaim",
            Self::InterruptClaimed => "interruptClaimed",
            Self::InterruptProcessing => "interruptProcessing",
        }
    }
}

/// Mirrors Lean `Conformance.RequestTransition.classify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestTransitionClassification {
    Legal(RequestTransitionAction),
    /// Taken by no single request action, only by an execution-lease
    /// authority: the expired-generation recovery sweep, latest-only trigger
    /// supersession of a running or parked request, or the re-park of an
    /// expired resume.
    RecoveryReachable,
    Illegal,
}

impl RequestTransitionClassification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legal(_) => "legal",
            Self::RecoveryReachable => "recoveryReachable",
            Self::Illegal => "illegal",
        }
    }

    pub const fn action(self) -> Option<RequestTransitionAction> {
        match self {
            Self::Legal(action) => Some(action),
            Self::RecoveryReachable | Self::Illegal => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestTransitionError {
    #[error("request lifecycle may not change from {from} to {to}")]
    Illegal {
        from: RequestLifecycleState,
        to: RequestLifecycleState,
    },
}

/// A lifecycle change the transition owner permits. Every writer of
/// `AgentRequest.lifecycle_state` filters on `from` and writes `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestTransition {
    from: RequestLifecycleState,
    to: RequestLifecycleState,
    classification: RequestTransitionClassification,
}

impl RequestTransition {
    pub const fn from(self) -> RequestLifecycleState {
        self.from
    }

    pub const fn to(self) -> RequestLifecycleState {
        self.to
    }

    pub const fn classification(self) -> RequestTransitionClassification {
        self.classification
    }
}

impl RequestLifecycleState {
    pub const fn transition_action(self, target: Self) -> Option<RequestTransitionAction> {
        use RequestTransitionAction as Action;
        Some(match (self, target) {
            (Self::WorkspaceBindingPending, Self::Pending) => Action::BindWorkspace,
            (Self::Pending, Self::Claimed) => Action::Claim,
            (Self::Pending | Self::WorkspaceBindingPending, Self::Superseded) => Action::DedupLose,
            (Self::Pending, Self::Failed) => Action::AdmissionReject,
            (Self::Claimed, Self::Processing) => Action::BeginInference,
            (Self::Processing, Self::Processing) => Action::ContinueProcessing,
            (Self::Processing, Self::WaitingForBackend) => Action::ParkBackendClosed,
            (Self::WaitingForBackend, Self::ResumingBackend) => Action::ResumeBackendReady,
            (Self::ResumingBackend, Self::Processing) => Action::BeginResumed,
            (Self::ResumingBackend, Self::Interrupted) => Action::InterruptResuming,
            (Self::ResumingBackend, Self::Failed) => Action::ExpireResuming,
            (Self::WaitingForBackend, Self::Interrupted) => Action::InterruptBackendWait,
            (Self::WaitingForBackend, Self::Failed) => Action::ExpireBackendWait,
            (Self::Processing, Self::Completed) => Action::Finish,
            (Self::Processing, Self::Failed) => Action::Fail,
            (Self::Claimed, Self::Failed) => Action::FailBeforeStream,
            (Self::Pending, Self::Dead) => Action::Expire,
            (Self::Pending, Self::Interrupted) => Action::InterruptBeforeClaim,
            (Self::Claimed, Self::Interrupted) => Action::InterruptClaimed,
            (Self::Processing, Self::Interrupted) => Action::InterruptProcessing,
            _ => return None,
        })
    }

    const fn lease_authority_reaches(self, target: Self) -> bool {
        matches!(
            (self, target),
            (
                Self::Claimed | Self::Processing | Self::ResumingBackend,
                Self::Dead | Self::Superseded
            ) | (Self::ResumingBackend, Self::WaitingForBackend)
                | (Self::WaitingForBackend, Self::Superseded)
        )
    }

    pub const fn classify_transition(self, target: Self) -> RequestTransitionClassification {
        match self.transition_action(target) {
            Some(action) => RequestTransitionClassification::Legal(action),
            None if self.lease_authority_reaches(target) => {
                RequestTransitionClassification::RecoveryReachable
            }
            None => RequestTransitionClassification::Illegal,
        }
    }

    /// The owner every request lifecycle writer consults before it writes;
    /// an illegal change is refused rather than written.
    pub const fn transition_to(
        self,
        target: Self,
    ) -> Result<RequestTransition, RequestTransitionError> {
        match self.classify_transition(target) {
            RequestTransitionClassification::Illegal => Err(RequestTransitionError::Illegal {
                from: self,
                to: target,
            }),
            classification => Ok(RequestTransition {
                from: self,
                to: target,
                classification,
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EnqueuedAgentRequest {
    pub doc_id: String,
    pub request_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestHopCause {
    Root,
    CrossSession { cause_hop: u32 },
    Return,
    Continuation,
}

pub fn next_request_hop(cause: RequestHopCause, own: u32) -> u32 {
    match cause {
        RequestHopCause::Root => 0,
        RequestHopCause::CrossSession { cause_hop } => own.max(cause_hop.saturating_add(1)),
        RequestHopCause::Return | RequestHopCause::Continuation => own,
    }
}

pub fn request_hop_within_bound(max_request_hop: Option<u32>, hop: u32) -> bool {
    max_request_hop.is_none_or(|bound| hop <= bound)
}

/// Lean `CausalHop` observation of a Node's `max_request_hop`: only a Node the
/// reader can see supplies a cap, so a hidden or absent Node is unavailable
/// rather than unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestHopCapObservation {
    Unavailable,
    Visible(Option<u32>),
}

impl RequestHopCapObservation {
    /// The cap a reader observed: `None` when it could not read the Node.
    pub fn of(observed: Option<Option<u32>>) -> Self {
        observed.map_or(Self::Unavailable, Self::Visible)
    }

    /// Lean `CausalHop.admitObservedHop`: no decision without a visible Node.
    pub fn admit(self, hop: u32) -> Option<bool> {
        match self {
            Self::Unavailable => None,
            Self::Visible(cap) => Some(request_hop_within_bound(cap, hop)),
        }
    }

    /// Lean `CausalHop.observedHopBound`.
    pub fn bound(self) -> Option<u32> {
        match self {
            Self::Unavailable => Some(0),
            Self::Visible(cap) => cap,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionMessageCause {
    pub caller_node_did: String,
    pub caller_agent_did: String,
    pub caller_session_id: String,
    pub caller_requester_did: Option<String>,
    pub caller_request_id: String,
    pub caller_request_doc_id: String,
    pub caller_hop: u32,
    pub tool_call_id: String,
    pub tool_call_doc_id: String,
    pub correlation: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionMessageTarget {
    pub node_did: String,
    pub agent_did: String,
    pub session_id: String,
}

#[cfg(all(test, feature = "typescript"))]
mod tests {
    use super::RequestLifecycleState;
    use ts_rs::TS;

    #[test]
    fn typescript_union_matches_wire_tokens() {
        let expected = RequestLifecycleState::ALL
            .iter()
            .map(|state| format!("\"{}\"", state.as_str()))
            .collect::<Vec<_>>()
            .join(" | ");
        assert_eq!(RequestLifecycleState::inline(), expected);
    }
}

#[cfg(test)]
mod conformance {
    use super::{
        next_request_hop, request_hop_within_bound, RequestHopCapObservation, RequestHopCause,
        RequestLifecycleState, RequestTransition, RequestTransitionClassification,
    };

    #[test]
    fn lifecycle_states_are_the_lean_request_states() {
        gents_lean_vocab::families::request_state_vocabulary::replay(&RequestLifecycleState::ALL);
    }

    #[test]
    fn lifecycle_terminal_partition_matches_the_lean_request_states() {
        let mut nonterminal = Vec::new();
        for case in
            gents_lean_vocab::families::request_state_terminal::cases::<RequestLifecycleState>()
        {
            let terminal = case.input.state.is_terminal();
            if !terminal {
                nonterminal.push(case.input.state);
            }
            case.check(terminal);
        }
        assert_eq!(
            RequestLifecycleState::nonterminal_graphql_list(),
            RequestLifecycleState::graphql_list(nonterminal)
        );
    }

    #[test]
    fn transitions_follow_the_lean_request_transition_model() {
        gents_lean_vocab::families::request_transition_classification::replay::<
            RequestLifecycleState,
        >(|case| {
            let classification = case.from_state.classify_transition(case.to_state);
            assert_eq!(
                case.from_state
                    .transition_to(case.to_state)
                    .ok()
                    .map(RequestTransition::classification),
                (classification != RequestTransitionClassification::Illegal)
                    .then_some(classification),
                "{case:?}"
            );
            gents_lean_vocab::families::request_transition_classification::TransitionOutput {
                action: classification
                    .action()
                    .map(|action| action.as_str().to_owned()),
                classification: classification.as_str().to_owned(),
                stopped_before_running: case
                    .from_state
                    .certifies_stopped_before_running(case.to_state, true),
                stopped_before_running_unowned: case
                    .from_state
                    .certifies_stopped_before_running(case.to_state, false),
            }
        });
    }

    #[test]
    fn hop_cap_observations_follow_the_lean_causal_hop_model() {
        gents_lean_vocab::families::causal_hop_contracts_cap_observation::replay(|case| {
            let hop = u32::try_from(case.hop).unwrap();
            let observation = if case.visible {
                RequestHopCapObservation::Visible(case.cap.map(|cap| u32::try_from(cap).unwrap()))
            } else {
                RequestHopCapObservation::Unavailable
            };
            let expected = observation.admit(hop);
            let bound = observation.bound();
            if hop > 0 {
                assert_eq!(
                    request_hop_within_bound(bound, hop),
                    expected.unwrap_or(false),
                    "{}",
                    case.name
                );
            }
            gents_lean_vocab::families::causal_hop_contracts_cap_observation::CapObservationOutput {
                expected,
                bound: bound.map(u64::from),
            }
        });
    }

    #[test]
    fn causal_hop_chains_follow_the_lean_causal_hop_model() {
        gents_lean_vocab::families::causal_hop_contracts_chain::replay::<String>(|chain| {
            let max_request_hop = chain
                .max_request_hop
                .map(|bound| u32::try_from(bound).unwrap());
            let mut hop = 0;
            let mut hops = Vec::new();
            let mut admitted = Vec::new();
            for step in &chain.steps {
                hop = match step.kind.as_str() {
                    "cross_session" => next_request_hop(
                        RequestHopCause::CrossSession { cause_hop: hop },
                        u32::try_from(
                            step.own_predecessor_hop
                                .expect("cross-session step names its own predecessor hop"),
                        )
                        .unwrap(),
                    ),
                    "continuation" => next_request_hop(RequestHopCause::Continuation, hop),
                    other => panic!("unknown Lean causal-hop step {other:?}"),
                };
                hops.push(u64::from(hop));
                admitted.push(request_hop_within_bound(max_request_hop, hop));
            }
            gents_lean_vocab::families::causal_hop_contracts_chain::HopTrace { hops, admitted }
        });
    }

    #[test]
    fn causal_hop_calls_follow_the_lean_causal_hop_model() {
        gents_lean_vocab::families::causal_hop_contracts_call::replay::<String>(|call| {
            let max_request_hop = call
                .max_request_hop
                .map(|bound| u32::try_from(bound).unwrap());
            let (mut a, mut b) = (0, 0);
            let mut hops = Vec::new();
            let mut admitted = Vec::new();
            for event in &call.events {
                let (target, cause) = match event.as_str() {
                    "a_sends_b" => (&mut b, RequestHopCause::CrossSession { cause_hop: a }),
                    "b_sends_a" => (&mut a, RequestHopCause::CrossSession { cause_hop: b }),
                    "return_to_a" => (&mut a, RequestHopCause::Return),
                    "return_to_b" => (&mut b, RequestHopCause::Return),
                    other => panic!("unknown Lean call event {other:?}"),
                };
                *target = next_request_hop(cause, *target);
                hops.push(u64::from(*target));
                admitted.push(request_hop_within_bound(max_request_hop, *target));
            }
            gents_lean_vocab::families::causal_hop_contracts_call::HopTrace { hops, admitted }
        });
    }

    #[test]
    fn causal_hop_step_admission_matches_lean_model() {
        use gents_lean_vocab::families::causal_hop_contracts_step_admission::StepOutput;
        gents_lean_vocab::families::causal_hop_contracts_step_admission::replay(|case| {
            let cause = match case.cause.as_str() {
                "root" => RequestHopCause::Root,
                "cross_session" => RequestHopCause::CrossSession {
                    cause_hop: u32::try_from(
                        case.cause_hop
                            .expect("cross-session step names its cause hop"),
                    )
                    .expect("cause hop fits a request hop"),
                },
                "return" => RequestHopCause::Return,
                "continuation" => RequestHopCause::Continuation,
                other => panic!("unknown Lean causal-hop cause {other:?}"),
            };
            let hop = next_request_hop(
                cause,
                u32::try_from(case.own_predecessor_hop).expect("predecessor hop fits u32"),
            );
            StepOutput {
                hop: hop.into(),
                admitted: request_hop_within_bound(
                    case.max_request_hop
                        .map(|bound| u32::try_from(bound).expect("bound fits u32")),
                    hop,
                ),
            }
        });
    }
}
