use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

pub(crate) mod acp;
mod binding;
mod goals;
mod job_runs;
mod observation_cursor;
pub(crate) mod projection;
pub(crate) mod protocol;
pub(crate) mod server;
mod session_record;
mod sessions;
mod task_control;
#[cfg(test)]
pub(crate) use super::test_shim_fixtures as test_fixtures;
pub(crate) mod turn;
mod usage;
mod workflow_catalog;
mod workflow_command;

#[cfg(test)]
use super::test_shim_fixtures::seed_test_agent_configuration;

use crate::commands::grok_shim::projection::resolve_bound_model_context;
use crate::commands::grok_shim::server::{
    spawn_leader, AcpDelegate, LeaderHandle, LeaderServerConfig, Registration,
};

#[derive(Clone)]
pub(crate) struct GrokShimBindArgs {
    pub(crate) background_executions: gents::tools::call::hook::BackgroundExecutionRegistry,
    pub(crate) node: Arc<EmbeddedNode>,
    pub(crate) actor: identity::Did,
    pub(crate) graphql: gents::store::config_client::GraphqlEndpoint,
    pub(crate) agent_did: Option<String>,
    pub(crate) node_did: String,
    pub(crate) node_name: String,
    pub(crate) socket_path: std::path::PathBuf,
    /// Gents home plugins are installed under.
    pub(crate) home: std::path::PathBuf,
}

/// Bind and spawn the Grok shim leader. The agent must exist and select a
/// model and backend before the socket is published, so a misconfigured home
/// fails fast instead of serving a fabricated model catalog.
pub(crate) async fn bind_grok_shim(args: GrokShimBindArgs) -> Result<LeaderHandle> {
    let node = args.node.clone();
    let agent_did =
        resolve_grok_shim_agent_did(node.as_ref(), args.agent_did.as_deref(), &args.node_did)
            .await?;
    let bound = resolve_bound_model_context(&node, &args.node_did, &agent_did)
        .await
        .with_context(|| {
            format!(
                "binding the Grok shim to agent {agent_did:?}; fix the agent with \
                 `gents config agent set --agent-did {agent_did} ...`"
            )
        })?;
    tracing::info!(
        node_name = %args.node_name,
        agent_did = %agent_did,
        model_id = %bound.model_id,
        total_context_tokens = bound.total_context_tokens,
        socket = %args.socket_path.display(),
        "grok shim leader binding"
    );
    let factory_inputs = AcpDelegateFactoryInputs {
        background_executions: args.background_executions.clone(),
        node: args.node.clone(),
        actor: args.actor.clone(),
        graphql: args.graphql.clone(),
        node_did: args.node_did.clone(),
        agent_did: agent_did.clone(),
        bound: bound.clone(),
        plugin_home: args.home.clone(),
    };
    let leader = spawn_leader(
        LeaderServerConfig::new(args.socket_path.clone()),
        Arc::new(production_acp_delegate_factory(factory_inputs)),
    )
    .with_context(|| {
        format!(
            "spawning the Grok shim leader on socket {}",
            args.socket_path.display()
        )
    })?;
    tracing::info!(
        socket = %args.socket_path.display(),
        "grok shim leader is accepting pager connections"
    );
    Ok(leader)
}

#[derive(Clone)]
struct AcpDelegateFactoryInputs {
    background_executions: gents::tools::call::hook::BackgroundExecutionRegistry,
    node: Arc<EmbeddedNode>,
    actor: identity::Did,
    graphql: gents::store::config_client::GraphqlEndpoint,
    node_did: String,
    agent_did: String,
    bound: crate::commands::grok_shim::projection::BoundModelContext,
    plugin_home: std::path::PathBuf,
}

fn production_acp_delegate_factory(
    inputs: AcpDelegateFactoryInputs,
) -> impl Fn(u64, &Registration) -> Result<Arc<dyn AcpDelegate>> {
    move |client_id, registration: &Registration| {
        Ok(Arc::new(binding::AgentConnection::new(
            inputs.clone(),
            client_id,
            registration.clone(),
        )) as Arc<dyn AcpDelegate>)
    }
}

impl AcpDelegateFactoryInputs {
    fn service(&self, client_id: u64, registration: &Registration) -> Arc<acp::AcpService> {
        let inputs = self;
        let turns = Arc::new(crate::commands::grok_shim::turn::TurnManager::new(
            inputs.node.clone(),
            crate::commands::grok_shim::turn::TurnManagerConfig {
                actor: inputs.actor.clone(),
                node_did: inputs.node_did.clone(),
                agent_did: inputs.agent_did.clone(),
                graphql: inputs.graphql.clone(),
            },
        ));
        let projections = Arc::new(
            crate::commands::grok_shim::projection::ProjectionEngine::new(
                inputs.node.clone(),
                inputs.bound.clone(),
                inputs.actor.clone(),
            )
            .with_background_executions(inputs.background_executions.clone()),
        );
        let mut service = crate::commands::grok_shim::acp::AcpService::new(
            crate::commands::grok_shim::acp::AcpServiceConfig {
                node: inputs.node.clone(),
                node_did: Arc::from(inputs.node_did.as_str()),
                agent_did: Arc::from(inputs.agent_did.as_str()),
                current_model: crate::commands::grok_shim::acp::BoundModel {
                    model_id: inputs.bound.model_id.clone(),
                    name: inputs.bound.model_name.clone(),
                    total_context_tokens: inputs.bound.total_context_tokens,
                },
                grok_home: crate::commands::grok_shim::session_record::default_grok_home(),
                plugin_home: inputs.plugin_home.clone(),
            },
            turns,
            projections,
        );
        service.register_client_identity(client_id, registration);
        Arc::new(service)
    }
}

pub(crate) use crate::commands::inference_binding::resolve_bound_agent_did as resolve_grok_shim_agent_did;
