use std::sync::Arc;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::{AgentRequest, AgentRequestRow};
use serde_json::Value;

use super::claim::ClaimVerdict;
use super::prepared::Decider;
use super::queue_scope::{ScopeAnswers, SessionRequests};
use super::stage::Plugin;
use gents_store::config_client::ConfigApplyTxn;

/// `request.terminal.stage`: a request reached a terminal state in this
/// transaction. The terminal always commits.
#[derive(Debug, Clone)]
pub struct RequestTerminal {
    pub node_did: String,
    pub request_id: String,
    pub state: RequestLifecycleState,
    pub reason: String,
    pub at: String,
}

/// `request.claim.gate`: the request core is about to claim. `client` is
/// core's observation that the request was admitted from a client.
#[derive(Debug, Clone)]
pub struct ClaimCandidate {
    pub request: AgentRequest,
    pub client: bool,
    pub at: String,
}

/// `request.claim.applied`: core's claim affected the request's row in this
/// transaction. `fire` is the request's Task fire receipt as core read it
/// (`_docID session_id goal_id goal_objective goal_token_budget
/// goal_assignment_applied`), if it has one; `actor` is the transaction's
/// actor.
#[derive(Debug, Clone)]
pub struct ClaimApplied {
    pub request: AgentRequest,
    pub fire: Option<Value>,
    pub actor: Option<String>,
    pub at: String,
}

/// `request.admit.verify`: what core shows a plugin that claimed a request
/// at admission, the signed request row and the exact parent request row it
/// names, both read as the request's agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdmitShown {
    pub request: AgentRequestRow,
    pub parent: AgentRequestRow,
}

/// `request.queue.scope`: core asks for the queue partition of the requests
/// `asked` names before it compares them. `requests` are the session's request
/// rows core read in this transaction; a plugin may decide from them which
/// further rows it needs.
#[derive(Debug, Clone)]
pub struct QueueScopeAsk {
    pub node_did: String,
    pub session_id: String,
    pub requests: Arc<SessionRequests>,
    pub asked: Vec<String>,
}

/// `control.session.halt`: an interrupt or stop latch is written in this
/// transaction. The latch always commits.
#[derive(Debug, Clone)]
pub struct SessionHalt {
    pub node_did: String,
    pub session_id: String,
    pub at: String,
}

/// A named core operation a plugin may request as an effect (Lean
/// `Hooks.Host.perform`). Core performs it in the lifecycle transaction, under
/// that transaction's actor, from the arguments alone and core's own state.
/// Its checks precede its first write, so a refused operation leaves the
/// transaction as it found it.
#[async_trait::async_trait]
pub trait CoreOperation: Send + Sync {
    fn name(&self) -> &'static str;

    async fn perform(&self, txn: &ConfigApplyTxn<'_>, args: &Value) -> anyhow::Result<()>;

    /// How many of the requesting plugin's bound these arguments consume: an
    /// operation over a list counts its entries.
    fn extent(&self, _args: &Value) -> usize {
        1
    }
}

/// A named core read a plugin may declare (Lean `Hooks.Read.core`). Core
/// answers it in the lifecycle transaction from the arguments and its own
/// rows; the rows it returns are data, never write targets.
#[async_trait::async_trait]
pub trait CoreRead: Send + Sync {
    fn name(&self) -> &'static str;

    async fn read(&self, txn: &ConfigApplyTxn<'_>, args: &Value) -> anyhow::Result<Vec<Value>>;
}

/// The bundled plugins at each point, in the fixed order the composition root
/// registered them, and the core operations their effects may name.
#[derive(Default)]
pub struct Registry {
    pub request_admit_verify: Vec<Decider<AgentRequestRow, AdmitShown, ()>>,
    pub claim_gate: Vec<Plugin<ClaimCandidate, ClaimVerdict>>,
    pub claim_applied: Vec<Plugin<ClaimApplied, ()>>,
    pub queue_scope: Vec<Plugin<QueueScopeAsk, ScopeAnswers>>,
    pub request_terminal: Vec<Plugin<RequestTerminal, ()>>,
    pub session_halt: Vec<Plugin<SessionHalt, ()>>,
    pub operations: Vec<Arc<dyn CoreOperation>>,
    pub reads: Vec<Arc<dyn CoreRead>>,
}

impl Registry {
    pub fn operation(&self, name: &str) -> Option<&Arc<dyn CoreOperation>> {
        self.operations
            .iter()
            .find(|operation| operation.name() == name)
    }

    pub fn read(&self, name: &str) -> Option<&Arc<dyn CoreRead>> {
        self.reads.iter().find(|read| read.name() == name)
    }

    /// Hands the process its bundled plugins. The composition root calls this
    /// before a node serves lifecycle transactions; the first list wins, since
    /// bundled plugins are fixed at build time.
    pub fn install(registry: Arc<Registry>) -> bool {
        installed().set(registry).is_ok()
    }

    /// The registry a lifecycle transaction runs its points against.
    ///
    /// Bundled plugins are always on, so a process that never installed them
    /// is miscomposed and its lifecycle transitions fail rather than run
    /// without their plugins. Test builds of the crates below the composition
    /// root have no plugins to install and run the points with none.
    pub(crate) fn for_transaction(
        _txn: &ConfigApplyTxn<'_>,
    ) -> Result<Arc<Registry>, super::stage::HookError> {
        Self::current()
    }

    /// The registry a step outside any lifecycle transaction (admission)
    /// prepares its decisions against, under the same composition rule.
    pub(crate) fn current() -> Result<Arc<Registry>, super::stage::HookError> {
        if let Some(registry) = installed().get() {
            return Ok(Arc::clone(registry));
        }
        if cfg!(any(test, feature = "test-fixtures")) {
            Ok(Arc::new(Registry::default()))
        } else {
            Err(super::stage::HookError::NotInstalled)
        }
    }
}

fn installed() -> &'static std::sync::OnceLock<Arc<Registry>> {
    static INSTALLED: std::sync::OnceLock<Arc<Registry>> = std::sync::OnceLock::new();
    &INSTALLED
}
