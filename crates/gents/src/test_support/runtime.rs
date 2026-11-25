use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use anyhow::{Context as _, Result};
use gents_documents::TaskHook;
use gents_protocol::row::AgentRequest;
use gents_session::request::watcher::Watcher;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::call::task_hooks::{HookAttempt, ManagedTaskHookExec, TaskHookExec};
use tokio::sync::{mpsc, Notify};

use crate::runtime::agent::SlotFailurePolicy;
use crate::runtime::status::RuntimeStatusHandle;

pub(crate) struct ScriptedWatcher {
    rx: mpsc::Receiver<Result<AgentRequest>>,
    calls: Option<Arc<AtomicUsize>>,
    waiting: Option<mpsc::UnboundedSender<()>>,
}

impl ScriptedWatcher {
    pub(crate) fn new(rx: mpsc::Receiver<Result<AgentRequest>>) -> Self {
        Self {
            rx,
            calls: None,
            waiting: None,
        }
    }

    pub(crate) fn with_calls(mut self, calls: Arc<AtomicUsize>) -> Self {
        self.calls = Some(calls);
        self
    }

    pub(crate) fn with_waiting(mut self, waiting: mpsc::UnboundedSender<()>) -> Self {
        self.waiting = Some(waiting);
        self
    }
}

impl Watcher for ScriptedWatcher {
    async fn next_request(&mut self) -> Option<Result<AgentRequest>> {
        if let Some(calls) = &self.calls {
            calls.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(waiting) = &self.waiting {
            waiting.send(()).unwrap();
        }
        self.rx.recv().await
    }
}

pub(crate) struct PublisherBackedSlotFailurePolicy {
    pub(crate) runtime_status: RuntimeStatusHandle,
}

#[async_trait::async_trait]
impl SlotFailurePolicy for PublisherBackedSlotFailurePolicy {
    fn build_failure_budget(&self) -> u32 {
        1
    }

    async fn on_slot_created(&self, agent_did: &str, generation: u64) -> Result<()> {
        self.runtime_status
            .readiness()
            .register_slot(agent_did, generation)
            .await
            .context("register test slot standing")?;
        Ok(())
    }

    async fn try_demote(&self, agent_did: &str, generation: u64, error: &str) -> Result<bool> {
        self.runtime_status
            .readiness()
            .demote_slot(agent_did, generation, error.to_string())
            .await
            .context("demote test slot")
    }

    async fn on_slot_retired(&self, agent_did: &str, generation: u64, _recreated: bool) {
        self.runtime_status
            .readiness()
            .retire_slot(agent_did, generation)
            .await
            .expect("retire test slot");
    }
}

pub(crate) struct FailSecondRegistrationPolicy {
    pub(crate) registrations: AtomicUsize,
    pub(crate) retired: Mutex<Vec<(String, u64)>>,
}

#[async_trait::async_trait]
impl SlotFailurePolicy for FailSecondRegistrationPolicy {
    fn build_failure_budget(&self) -> u32 {
        1
    }

    async fn on_slot_created(&self, _agent_did: &str, _generation: u64) -> Result<()> {
        if self.registrations.fetch_add(1, Ordering::SeqCst) == 1 {
            anyhow::bail!("injected slot registration failure");
        }
        Ok(())
    }

    async fn try_demote(&self, _agent_did: &str, _generation: u64, _error: &str) -> Result<bool> {
        Ok(false)
    }

    async fn on_slot_retired(&self, agent_did: &str, generation: u64, _recreated: bool) {
        self.retired
            .lock()
            .unwrap()
            .push((agent_did.to_string(), generation));
    }
}

pub(crate) struct RecordingSlotFailurePolicy {
    pub(crate) retired: Mutex<Vec<(String, bool)>>,
    pub(crate) retired_notify: Notify,
    pub(crate) demote_calls: AtomicUsize,
}

#[async_trait::async_trait]
impl SlotFailurePolicy for RecordingSlotFailurePolicy {
    fn build_failure_budget(&self) -> u32 {
        3
    }

    async fn on_slot_created(&self, _agent_did: &str, _generation: u64) -> Result<()> {
        Ok(())
    }

    async fn try_demote(&self, _agent_did: &str, _generation: u64, _error: &str) -> Result<bool> {
        self.demote_calls.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }

    async fn on_slot_retired(&self, agent_did: &str, _generation: u64, recreated: bool) {
        self.retired
            .lock()
            .expect("retired mutex")
            .push((agent_did.to_string(), recreated));
        self.retired_notify.notify_waiters();
    }
}

pub(crate) struct RevokingTaskHookExec<'a> {
    pub(crate) managed: ManagedTaskHookExec,
    pub(crate) access: ConfigAccess,
    pub(crate) request_doc_id: &'a str,
    pub(crate) revoked: &'a [String],
}

#[async_trait::async_trait]
impl TaskHookExec for RevokingTaskHookExec<'_> {
    async fn attempt(&self, hook: &TaskHook) -> HookAttempt {
        if self.revoked.contains(&hook.hook_id) {
            let response = self
                .access
                .write(
                    "test.task_hook_launch_revocation",
                    &format!(
                        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "revoked-generation" }}) {{ _docID }} }}"#,
                        escape_graphql_string(self.request_doc_id),
                    ),
                )
                .await
                .unwrap();
            assert_eq!(
                response["data"]["update_AgentRequest"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
        self.managed.attempt(hook).await
    }
}

pub(crate) struct ObservedTicks {
    pub(crate) waiting: Arc<Notify>,
    pub(crate) ticks: futures::channel::mpsc::UnboundedReceiver<()>,
}

impl futures::Stream for ObservedTicks {
    type Item = ();

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<()>> {
        let result = Pin::new(&mut self.ticks).poll_next(context);
        if result.is_pending() {
            self.waiting.notify_one();
        }
        result
    }
}
