use std::time::{Duration, Instant};

use chrono::Utc;
use gents_protocol::operations::mcp_service::McpProbeReceipt;
use gents_store::config_client::ConfigAccess;

use super::health::{
    run_health_check_cycle_for_actor, HealthCheckerOptions, HealthStatus, McpHealthCheckService,
    ServiceHealthMap,
};
use super::pool::McpPool;

/// The enabled MCP services of `node_did`, ordered by service ID, or only
/// `service_id` when it names one.
pub async fn enabled_services(
    access: &ConfigAccess,
    node_did: &str,
    service_id: Option<&str>,
) -> anyhow::Result<Vec<gents_documents::ToolServiceRegistry>> {
    let mut services = super::registry::configured_mcp_services_on(access, node_did).await?;
    services.retain(|service| {
        service.enabled && service_id.is_none_or(|id| service.service_id == id.trim())
    });
    services.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    Ok(services)
}

/// Probes one enabled MCP service of `node_did` once as `actor`. The service
/// is read again immediately before the probe, so a service disabled or
/// removed since it was listed is not contacted. `timeout` bounds the probe;
/// the whole check is abandoned after twice that.
pub async fn probe(
    access: &ConfigAccess,
    node_did: &str,
    service_id: &str,
    actor: &str,
    timeout: Duration,
    local_hostname: &str,
    local_subnet: Option<&str>,
    pool: &McpPool,
) -> McpProbeReceipt {
    let unreachable = |latency_ms, error: String| McpProbeReceipt {
        service_id: service_id.to_owned(),
        health_state: HealthStatus::Unreachable.to_string(),
        latency_ms,
        last_error: Some(error),
    };
    let service = match enabled_services(access, node_did, Some(service_id)).await {
        Ok(mut services) if services.len() == 1 => services.remove(0),
        Ok(_) => {
            return unreachable(
                0,
                "service is unavailable, disabled, or ambiguous".to_owned(),
            )
        }
        Err(error) => return unreachable(0, format!("service recheck failed: {error:#}")),
    };
    let health_map = ServiceHealthMap::new();
    let options = HealthCheckerOptions {
        probe_timeout: timeout,
        failure_threshold_k: 1,
        ..HealthCheckerOptions::default()
    };
    let started = Instant::now();
    let result = tokio::time::timeout(
        timeout.saturating_mul(2),
        run_health_check_cycle_for_actor(
            vec![McpHealthCheckService::from(service)],
            Utc::now(),
            pool,
            &health_map,
            local_hostname,
            local_subnet,
            &options,
            actor,
        ),
    )
    .await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(Ok(())) => match health_map.get(service_id).await {
            Some(health) => McpProbeReceipt {
                service_id: service_id.to_owned(),
                health_state: health.status.to_string(),
                latency_ms,
                last_error: health.last_error,
            },
            None => unreachable(latency_ms, "probe produced no health snapshot".to_owned()),
        },
        Ok(Err(error)) => unreachable(latency_ms, error.to_string()),
        Err(_) => unreachable(latency_ms, format!("probe timed out after {latency_ms}ms")),
    }
}

/// The default bound of one probe.
pub fn default_timeout() -> Duration {
    HealthCheckerOptions::default().probe_timeout
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gents_documents::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents_store::identity::NodeIdentity;

    use super::*;

    #[tokio::test]
    async fn a_probe_reads_its_owners_enabled_service_again_before_contacting_it() {
        let directory = tempfile::tempdir().unwrap();
        let root = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("runtime"),
            None,
        )
        .unwrap();
        let other = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("other"),
            None,
        )
        .unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(root.did())
                .with_node_acp_owner(root.did().to_owned())
                .build()
                .await
                .unwrap(),
        );
        gents_store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        let access = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap());
        let did = gents_store::graphql::escape_graphql_string(root.did());
        access
            .write(
                "test.mcp.node",
                &format!(r#"mutation {{ create_Node(input: {{node_did: "{did}"}}) {{_docID}} }}"#),
            )
            .await
            .unwrap();
        let value = serde_json::json!({"node_did":root.did(),"service_id":"svc-1","hostname":"real-host","mcp_port":1,"enabled":true});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::ToolServiceRegistry,
            add: value.clone(),
            update: value,
        }])
        .unwrap();
        access
            .transact_write("test.mcp.service", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let services = enabled_services(&access, root.did(), Some("svc-1"))
            .await
            .unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].hostname.as_deref(), Some("real-host"));
        assert!(enabled_services(&access, other.did(), Some("svc-1"))
            .await
            .unwrap()
            .is_empty());
        access.write("test.mcp.disable", r#"mutation { update_ToolServiceRegistry(filter:{service_id:{_eq:"svc-1"}}, input:{enabled:false}) {_docID} }"#).await.unwrap();
        let receipt = probe(
            &access,
            root.did(),
            "svc-1",
            root.did(),
            Duration::from_secs(1),
            "local",
            None,
            &McpPool::new(),
        )
        .await;
        assert_eq!(
            receipt.last_error.as_deref(),
            Some("service is unavailable, disabled, or ambiguous")
        );
        assert_eq!(receipt.latency_ms, 0);
        node.shutdown().await.unwrap();
    }
}
