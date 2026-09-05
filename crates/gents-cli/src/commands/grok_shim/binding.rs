//! Stock --agent is sent as session/new|load _meta.agentProfile. Select one
//! immutable behavior-scoped service per connection, before exposing history.
//! This is connection routing, not a second runtime behavior/identity owner.
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::acp::{internal_method, AcpRequest, AcpService};
use super::server::{AcpDelegate, AcpOutbound, Registration};
use super::AcpDelegateFactoryInputs;

#[derive(Default)]
struct Binding {
    selected: Option<(String, Arc<AcpService>)>,
    closed: bool,
}

pub(super) struct BehaviorConnection {
    inputs: AcpDelegateFactoryInputs,
    client_id: u64,
    registration: Registration,
    bootstrap: Arc<AcpService>,
    binding: Mutex<Binding>,
}

impl BehaviorConnection {
    pub(super) fn new(
        inputs: AcpDelegateFactoryInputs,
        client_id: u64,
        registration: Registration,
    ) -> Self {
        let bootstrap = inputs.service(client_id, &registration);
        Self {
            inputs,
            client_id,
            registration,
            bootstrap,
            binding: Mutex::new(Binding::default()),
        }
    }

    async fn route(&self, request: &AcpRequest) -> Result<Option<Arc<AcpService>>> {
        let method = internal_method(&request.method);
        let selecting = matches!(method, "session/new" | "session/load");
        let profile = if selecting {
            request.params.pointer("/_meta/agentProfile").map(|value| {
                value.as_str().filter(|value| !value.trim().is_empty())
                    .map(str::trim).context("--agent must name a registered Gents behavior; inline agent definitions are not supported")
            }).transpose()?
        } else {
            None
        };
        let profile = profile.map(|name| match name {
            "default" | "grok-build" => self.inputs.behavior_id.as_str(),
            name => name,
        });
        let mut binding = self.binding.lock().await;
        anyhow::ensure!(!binding.closed, "connection already disconnected");
        if let Some((behavior, service)) = &binding.selected {
            anyhow::ensure!(profile.is_none_or(|requested| requested == behavior),
                "connection is bound to behavior {behavior}; open another Grok connection with --agent to select a different behavior");
            return Ok(Some(service.clone()));
        }
        if !selecting {
            // History requests carry no --agent selector. Never leak the
            // default history while the intended session/new is in flight.
            if matches!(method, "x.ai/session/list" | "x.ai/sessions/list") {
                return Ok(None);
            }
            return Ok(Some(self.bootstrap.clone()));
        }
        let behavior = profile.unwrap_or(&self.inputs.behavior_id).to_owned();
        let escaped = gents::graphql::escape_graphql_string(&behavior);
        let response = self.inputs.node.execute(&format!(
            "{{AgentBehavior(filter:{{behavior_id:{{_eq:\"{escaped}\"}}}},limit:2){{agent_did enabled}}}}"
        )).await;
        gents::graphql::ensure_no_errors(&response, "select Grok behavior")?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data["AgentBehavior"].as_array())
            .context("missing behavior selection result")?;
        anyhow::ensure!(
            rows.len() == 1
                && rows[0]["agent_did"].as_str() == Some(self.inputs.agent_did.as_str())
                && rows[0]["enabled"].as_bool() == Some(true),
            "unknown, disabled, or unauthorized Gents behavior: {behavior}"
        );
        let service = if behavior == self.inputs.behavior_id {
            self.bootstrap.clone()
        } else {
            let mut inputs = self.inputs.clone();
            inputs.bound =
                super::projection::resolve_bound_model_context(&inputs.node, &behavior).await?;
            inputs.behavior_id = behavior.clone();
            inputs.service(self.client_id, &self.registration)
        };
        binding.selected = Some((behavior, service.clone()));
        // Never hold the selection lock across dispatch: prompt cancellation
        // and disconnect must not wait for inference or outbound delivery.
        Ok(Some(service))
    }
}

impl AcpDelegate for BehaviorConnection {
    fn handle_acp<'a>(
        &'a self,
        payload: &'a str,
        outbound: AcpOutbound,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let request = match AcpRequest::from_payload(payload) {
                Ok(request) => request,
                Err(_) => return self.bootstrap.handle_acp(payload, outbound).await,
            };
            match self.route(&request).await {
                Ok(Some(service)) => service.handle_acp(payload, outbound).await,
                route => {
                    if let Some(id) = request.id {
                        let response: Value = match route {
                            Ok(None) => json!({"jsonrpc":"2.0", "id":id, "result":{
                                "sessions":[], "nextCursor":null,
                                "_meta":{"gents/behaviorSelectionRequired":true}
                            }}),
                            Err(error) => json!({"jsonrpc":"2.0", "id":id,
                                "error":{"code":-32602, "message":error.to_string()}}),
                            _ => unreachable!(),
                        };
                        outbound.send(response.to_string()).await?;
                    }
                    Ok(())
                }
            }
        })
    }

    fn on_disconnect(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let service = {
                let mut binding = self.binding.lock().await;
                binding.closed = true;
                binding
                    .selected
                    .as_ref()
                    .map(|(_, service)| service.clone())
                    .unwrap_or_else(|| self.bootstrap.clone())
            };
            service.on_disconnect().await;
        })
    }
}
