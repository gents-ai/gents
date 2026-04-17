use std::time::Instant;

use crate::chat::controller as chat_controller;
use crate::client::{
    ClientCore, ClientCoreOptions, ClientStore, DesktopPaths, P2PHealth, P2PHealthStatus,
};
use crate::operator::controller as operator_controller;
use crate::state::{Activity, ShellState};
use crate::views;

#[derive(Debug, Clone)]
pub(super) struct ClientRestartPlan {
    pub(super) paths: DesktopPaths,
    pub(super) options: ClientCoreOptions,
}

impl ClientRestartPlan {
    pub(super) fn from_client(client: &ClientCore) -> Self {
        Self {
            paths: client.paths().clone(),
            options: client.options().clone(),
        }
    }

    pub(super) fn discovered_default() -> Option<Self> {
        DesktopPaths::discover().ok().map(|paths| Self {
            paths,
            options: ClientCoreOptions::default(),
        })
    }

    pub(super) async fn start(&self) -> anyhow::Result<ClientCore> {
        ClientCore::start_with_paths_and_options(self.paths.clone(), self.options.clone()).await
    }
}

pub(super) fn apply_bootstrap_state(state: &mut ShellState, client: &ClientCore) {
    let error_count = live_error_count(client);

    apply_client_transport_state(state, client);
    apply_p2p_health_state(state, &client.p2p_health());
    state.status.active_agent = "desktop client".to_string();
    state.status.runtime_state = if !client.bootstrap_errors().is_empty() {
        "client core degraded".to_string()
    } else if client.peer_issue_count() > 0 {
        "peer repair active".to_string()
    } else {
        "client core online".to_string()
    };
    state.status.replication_state = "subscriptions armed".to_string();
    state.status.error_count = error_count;
}

pub(super) fn apply_bootstrap_failure_state(state: &mut ShellState) {
    state.identity.did_short = "identity unavailable".to_string();
    state.status.peered_now = 0;
    state.status.peered_target = 0;
    state.status.p2p_state = "offline".to_string();
    state.status.p2p_warning = true;
    state.status.active_agent = "desktop client".to_string();
    state.status.runtime_state = "bootstrap failed".to_string();
    state.status.replication_state = "offline".to_string();
    state.status.error_count = 1;
    state.status.build_label = "bootstrap".to_string();
}

pub(super) fn apply_snapshot_state(state: &mut ShellState, store: &ClientStore) {
    state.status.runtime_state = format!(
        "{} agents / {} conversations",
        store.agent_principals.len(),
        store.conversations.len()
    );
    state.status.replication_state = if store.requests.is_empty() {
        "subscriptions armed".to_string()
    } else {
        format!("{} requests observed", store.requests.len())
    };
}

pub(super) fn apply_client_transport_state(state: &mut ShellState, client: &ClientCore) {
    state.identity.did_short = client.principal().short_did();
    state.status.peered_now = client.dialed_peer_count();
    state.status.peered_target = client.configured_peer_count();
    state.status.did_short = client.principal().short_did();
    state.status.build_label = format!("peer:{}", abbreviate_id(client.local_peer_id()));
}

pub(super) fn apply_p2p_health_state(state: &mut ShellState, health: &P2PHealth) {
    state.status.p2p_state = if health.status == P2PHealthStatus::Healthy {
        format!(
            "{} · {} peers / {} reps",
            health.status.label(),
            health.connected_peer_count,
            health.replicator_count
        )
    } else {
        format!(
            "{} · {} fails",
            health.status.label(),
            health.consecutive_failures
        )
    };
    state.status.p2p_warning = health.status != P2PHealthStatus::Healthy;
}

pub(super) fn sync_shell_state_from_client(state: &mut ShellState, client: &ClientCore) {
    let snapshot = client.store().snapshot();
    let store = snapshot.as_ref();
    apply_snapshot_state(state, store);

    chat_controller::sync_from_snapshot(&mut state.chat, store, true);
    operator_controller::sync_from_snapshot(&mut state.operator, &client.peer_statuses(), store);
    apply_first_launch_focus(state, client, store);
    views::prepare_state(state, Some(client), Some(store));
}

pub(super) fn live_error_count(client: &ClientCore) -> usize {
    client.bootstrap_errors().len()
        + client.peer_issue_count()
        + usize::from(client.p2p_health().status != P2PHealthStatus::Healthy)
}

pub(crate) fn should_auto_restart_p2p(
    previous: Option<&P2PHealth>,
    next: &P2PHealth,
    last_attempt: Option<Instant>,
    now: Instant,
    cooldown: std::time::Duration,
) -> bool {
    if next.status != P2PHealthStatus::Wedged {
        return false;
    }

    if last_attempt
        .is_some_and(|attempted_at| now.saturating_duration_since(attempted_at) < cooldown)
    {
        return false;
    }

    match previous {
        Some(previous) => {
            previous.status != P2PHealthStatus::Wedged
                || previous.consecutive_failures != next.consecutive_failures
                || previous.last_error != next.last_error
        }
        None => true,
    }
}

pub(super) fn apply_first_launch_focus(
    state: &mut ShellState,
    client: &ClientCore,
    store: &ClientStore,
) {
    if !state.onboarding.first_launch_redirect_done
        && state.activity == Activity::Chat
        && should_focus_first_launch(client, store)
    {
        state.activity = Activity::Peers;
        state.peers.show_add_form = true;
        state.onboarding.first_launch_redirect_done = true;
    }
}

fn should_focus_first_launch(client: &ClientCore, store: &ClientStore) -> bool {
    client.configured_peer_count() == 0
        && store.agent_principals.is_empty()
        && store.conversations.is_empty()
        && store.requests.is_empty()
        && store.responses.is_empty()
}

fn abbreviate_id(value: &str) -> String {
    if value.len() <= 12 {
        return value.to_string();
    }

    format!("{}..{}", &value[..8], &value[value.len() - 2..])
}
