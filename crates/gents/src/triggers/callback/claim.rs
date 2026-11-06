use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

use super::documents::{update_invocation_mutation, CallbackInvocationDoc};
use super::{
    LIFECYCLE_CLAIMED, LIFECYCLE_DENIED, LIFECYCLE_FAILED, LIFECYCLE_PENDING, LIFECYCLE_RUNNING,
    LIFECYCLE_SUCCEEDED,
};

pub fn invocation_is_claimable(node_did: &str, invocation: &CallbackInvocationDoc) -> bool {
    invocation.owner_node_did == node_did
        && matches!(
            invocation.lifecycle_state.as_str(),
            LIFECYCLE_PENDING | LIFECYCLE_CLAIMED | LIFECYCLE_RUNNING
        )
}

pub fn invocation_is_terminal(state: &str) -> bool {
    matches!(
        state,
        LIFECYCLE_SUCCEEDED | LIFECYCLE_FAILED | LIFECYCLE_DENIED
    )
}

/// A claim is scoped to its owner DID and invocation ID; another principal
/// cannot claim the row. Single-instance enforcement is deferred.
pub async fn claim_invocation(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    node_did: &str,
    invocation: &CallbackInvocationDoc,
    callback: &crate::documents::Callback,
    prepared_module: Option<(&str, &super::documents::CallbackModuleDoc)>,
    prepared_plan: Option<&str>,
) -> Result<Option<CallbackInvocationDoc>> {
    identity::Did::new(invocation.agent_did.clone())?;
    anyhow::ensure!(
        invocation.agent_did != invocation.owner_node_did,
        "callback agent cannot be its routing node"
    );
    anyhow::ensure!(
        node.node_identity_did() != Some(invocation.agent_did.as_str()),
        "callback agent cannot be the runtime node"
    );
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(invocation.agent_did.as_str()),
        "callback claim actor mismatch"
    );
    if invocation.owner_node_did != node_did {
        return Ok(None);
    }
    if invocation_is_terminal(&invocation.lifecycle_state) {
        return Ok(None);
    }
    if matches!(
        invocation.lifecycle_state.as_str(),
        LIFECYCLE_CLAIMED | LIFECYCLE_RUNNING
    ) {
        return Ok(Some(invocation.clone()));
    }
    if invocation.lifecycle_state != LIFECYCLE_PENDING {
        return Ok(None);
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut claimed = invocation.clone();
    claimed.lifecycle_state = LIFECYCLE_CLAIMED.to_string();
    claimed.claimed_at = Some(now);
    claimed.attempts = Some(claimed.attempts.unwrap_or(0).saturating_add(1));
    claimed.claimed_callback = Some(serde_json::to_string(callback)?);
    if matches!(
        &callback.handler,
        crate::documents::CallbackHandler::Module { .. }
    ) {
        claimed.action_plan = Some(
            prepared_plan
                .context("callback module plan missing before claim")?
                .to_owned(),
        );
    }
    let mutation = update_invocation_mutation(&claimed, Some(LIFECYCLE_PENDING));
    let response = access
        .transact_write("callback.claim_invocation", |txn| {
            let mutation = &mutation;
            Box::pin(async move {
                let (binding_doc, binding) =
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::CallbackBinding,
                        node_did,
                        invocation.origin.binding_id(),
                    )
                    .await?
                    .context("callback binding unavailable to selected agent")?;
                let binding: crate::documents::CallbackBinding = serde_json::from_value(binding)?;
                anyhow::ensure!(
                    binding.enabled
                        && binding.agent_did == invocation.agent_did
                        && binding.callback_id == invocation.callback_id,
                    "callback binding changed before claim"
                );
                crate::request::admission::observe_unattended_agent_in_txn(
                    txn,
                    node_did,
                    &invocation.agent_did,
                )
                .await?;
                let (source_doc, source) =
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::EventSource,
                        node_did,
                        &binding.event_source_id,
                    )
                    .await?
                    .context("callback event source unavailable to selected agent")?;
                let source: crate::documents::EventSource = serde_json::from_value(source)?;
                let (callback_doc, current) =
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Callback,
                        node_did,
                        &invocation.callback_id,
                    )
                    .await?
                    .context("callback unavailable to selected agent")?;
                let current: crate::documents::Callback = serde_json::from_value(current)?;
                anyhow::ensure!(
                    current.enabled && &current == callback,
                    "callback changed before claim"
                );
                let expected = invocation
                    .configuration_generation
                    .as_deref()
                    .context("callback configuration generation missing before claim")?;
                anyhow::ensure!(
                    super::documents::configuration_generation(
                        &binding,
                        &source,
                        &current,
                        &super::documents::CallbackConfigDocIds {
                            binding: binding_doc,
                            source: source_doc,
                            callback: callback_doc
                        }
                    )? == expected,
                    "callback configuration changed before claim"
                );
                if let crate::documents::CallbackHandler::Module { module_id, .. } =
                    &current.handler
                {
                    let (prepared_doc, prepared) =
                        prepared_module.context("callback module missing before claim")?;
                    let (module_doc, module) =
                        crate::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            crate::Collection::CallbackModule,
                            node_did,
                            module_id,
                        )
                        .await?
                        .context("callback module unavailable to selected agent")?;
                    let module: crate::documents::CallbackModule = serde_json::from_value(module)?;
                    anyhow::ensure!(
                        module.enabled && module_doc == prepared_doc && &module == prepared,
                        "callback module changed before claim"
                    );
                }
                txn.execute_local_response(mutation)
                    .await
                    .map_err(Into::<anyhow::Error>::into)
            })
        })
        .await?;
    if crate::store::graphql::single_mutation_document(&response, "update_CallbackInvocation")?
        .is_some()
    {
        return Ok(Some(claimed));
    }
    let current = super::documents::load_invocation(
        node,
        &invocation.invocation_id,
        &invocation.owner_node_did,
        &invocation.agent_did,
    )
    .await?;
    Ok(current.filter(|row| {
        row.owner_node_did == node_did
            && matches!(
                row.lifecycle_state.as_str(),
                LIFECYCLE_CLAIMED | LIFECYCLE_RUNNING
            )
    }))
}
