use std::sync::Arc;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::{AgentRequest, AgentRequestRow};
use serde_json::Value;

use super::claim::ClaimVerdict;
use super::prepared::Decider;
use super::queue_scope::{ScopeAnswers, SessionRequests};
use super::stage::Plugin;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::identity::NodeIdentity;

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

/// `request.create.stage`, after creation: core created `request` (signed,
/// with core's session, Agent and hop fields) in this transaction as the row
/// `doc_id`, at a plugin's request (Lean `Hooks.Resume.resumeCreating`). A
/// plugin is shown its rows as they stand after everything this transaction
/// staged so far, and may refuse; a refusal fails the creation and the
/// transition that requested it.
#[derive(Debug, Clone)]
pub struct RequestCreated {
    pub request: gents_protocol::request_admission::AgentRequestCreate,
    pub doc_id: String,
}

/// `control.session.resume`: core cleared the session's stop latch in this
/// transaction after its own quiescence rule held. Resume starts work, so a
/// plugin may refuse it and the whole resume is not applied.
#[derive(Debug, Clone)]
pub struct SessionResume {
    pub node_did: String,
    pub session_id: String,
    pub at: String,
}

/// What the caller of a lifecycle point holds for core's own operations and
/// never shows a plugin: the plugin's effects name an operation and its
/// arguments, and core performs it with this context under the transaction's
/// actor. `signer` is core's signing identity for that actor, when the caller
/// holds one.
#[derive(Clone, Copy, Default)]
pub struct OperationContext<'a> {
    pub signer: Option<&'a dyn NodeIdentity>,
}

/// A named core operation a plugin may request as an effect (Lean
/// `Hooks.Host.perform`). Core performs it in the lifecycle transaction, under
/// that transaction's actor, from the arguments alone, core's own state and
/// the caller's `OperationContext`. Its checks precede its first write, so a
/// refused operation leaves the transaction as it found it.
#[async_trait::async_trait]
pub trait CoreOperation: Send + Sync {
    fn name(&self) -> &'static str;

    async fn perform(
        &self,
        txn: &ConfigApplyTxn<'_>,
        context: OperationContext<'_>,
        args: &Value,
    ) -> anyhow::Result<()>;
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
    pub request_created: Vec<Plugin<RequestCreated, ()>>,
    pub queue_scope: Vec<Plugin<QueueScopeAsk, ScopeAnswers>>,
    pub request_terminal: Vec<Plugin<RequestTerminal, ()>>,
    pub session_halt: Vec<Plugin<SessionHalt, ()>>,
    pub session_resume: Vec<Plugin<SessionResume, ()>>,
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

    /// Gives `node` its plugins. The composition root attaches them to each
    /// node it starts, before the node serves lifecycle transactions; a node
    /// keeps the registry it was started with.
    pub fn attach(
        node: &Arc<defra_node::Node>,
        registry: Arc<Registry>,
    ) -> Result<(), gents_store::node_extension::NodeExtensionError> {
        gents_store::node_extension::attach(node, registry)
    }

    /// Core's points with no plugins. Tests of the crates below the plugins
    /// have none to attach and run with this.
    pub fn core_only() -> Arc<Registry> {
        Arc::new(Registry::default())
    }

    /// Hands this process the plugins its transactions over a GraphQL
    /// endpoint run with: such a transaction has no node in this process to
    /// carry them. The first registry wins, since bundled plugins are fixed
    /// at build time.
    pub fn install_for_remote_transactions(registry: Arc<Registry>) -> bool {
        remote().set(registry).is_ok()
    }

    /// The registry a lifecycle transaction runs its points against: its
    /// in-process node's, or this process's for a transaction over a GraphQL
    /// endpoint.
    pub(crate) fn for_transaction(
        txn: &ConfigApplyTxn<'_>,
    ) -> Result<Arc<Registry>, super::stage::HookError> {
        if txn.has_local_node() {
            return txn.node_extension::<Registry>().map_or_else(unattached, Ok);
        }
        remote().get().map(Arc::clone).map_or_else(unattached, Ok)
    }

    /// The registry admission, outside any lifecycle transaction, prepares
    /// its decisions against on `node`.
    pub(crate) fn for_node(node: &defra_node::Node) -> Result<Arc<Registry>, super::stage::HookError> {
        gents_store::node_extension::attached::<Registry>(node).map_or_else(unattached, Ok)
    }
}

/// Bundled plugins are always on, so a node started without them is
/// miscomposed and its lifecycle transitions fail rather than run without
/// their plugins. Test builds run an unattached node's points core-only:
/// most test nodes are built directly rather than through the composition
/// root, and a test that needs the plugins attaches them to its node.
fn unattached() -> Result<Arc<Registry>, super::stage::HookError> {
    if cfg!(any(test, feature = "test-fixtures")) {
        Ok(Registry::core_only())
    } else {
        Err(super::stage::HookError::NotInstalled)
    }
}

fn remote() -> &'static std::sync::OnceLock<Arc<Registry>> {
    static REMOTE: std::sync::OnceLock<Arc<Registry>> = std::sync::OnceLock::new();
    &REMOTE
}
