use std::time::Duration;

use gents::graphql::escape_graphql_string;
use gents::ConfigAccess;
use gents_protocol::row::{project_node_readiness, NodeReadinessRow, ProjectedAgentReadiness};

use super::error::EvalInitError;

/// Pack publication precedes router reconciliation, so an HTTP-ready server
/// can still reject the author's first request as unassigned.
pub(super) async fn wait_for_author(
    access: &ConfigAccess,
    owner: &str,
    agent: &str,
    timeout: Duration,
    poll: Duration,
) -> Result<(), EvalInitError> {
    let query = format!(
        r#"{{ NodeReadiness(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{ node_did snapshot_json updated_at }} }}"#,
        escape_graphql_string(owner)
    );
    let mut last = "no readiness observation".to_owned();
    let waited = tokio::time::timeout(timeout, async {
        loop {
            let response = access
                .execute(&query)
                .await
                .map_err(EvalInitError::ReadAuthorReadiness)?;
            let row = response
                .pointer("/data/NodeReadiness/0")
                .cloned()
                .map(serde_json::from_value::<NodeReadinessRow>)
                .transpose()
                .map_err(EvalInitError::DecodeAuthorReadiness)?;
            let projection = project_node_readiness(row.as_ref(), owner, [agent], None);
            let state = projection.agents.get(agent).cloned();
            if state == Some(ProjectedAgentReadiness::Ready) {
                return Ok(());
            }
            last = match state {
                Some(ProjectedAgentReadiness::Unavailable(reason)) => {
                    reason.public_message().to_owned()
                }
                _ => "the runtime has not published a ready assignment".to_owned(),
            };
            tokio::time::sleep(poll).await;
        }
    })
    .await;
    match waited {
        Ok(result) => result,
        Err(_) => Err(EvalInitError::AuthorNotReady {
            agent: agent.to_owned(),
            seconds: timeout.as_secs_f64(),
            last,
        }),
    }
}

#[cfg(test)]
mod tests {
    use gents_protocol::error_chain::ErrorChain;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use axum::{routing::post, Json, Router};
    use serde_json::{json, Value};

    const OWNER: &str = "did:key:author-test";
    const AUTHOR: &str = "eval-author";

    fn observation(router_generation: u64, assigned: bool, ready: bool) -> Value {
        let mut agents = vec![json!({"agent_id": "default", "state": "ready", "reason": null})];
        if assigned {
            agents.push(
                json!({"agent_id": AUTHOR, "state": if ready {"ready"} else {"unavailable"},
                "reason": if ready {Value::Null} else {json!("backend_not_configured")}}),
            );
        }
        json!({"data": {"NodeReadiness": [{"node_did": OWNER, "updated_at": "2026-10-16T00:00:00Z",
            "snapshot_json": json!({"format_version": gents_protocol::row::NODE_READINESS_FORMAT_VERSION,
                "process_state": "ready", "active_generation": 2, "router_generation": router_generation,
                "default_agent_id": "default", "agents": agents}).to_string()}]}})
    }

    async fn server(
        responses: Vec<Value>,
    ) -> (ConfigAccess, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app = Router::new().route(
            "/graphql",
            post(move || {
                let index = count
                    .fetch_add(1, Ordering::SeqCst)
                    .min(responses.len() - 1);
                let response = responses[index].clone();
                async move { Json(response) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            ConfigAccess::graphql(format!("http://{address}/graphql")),
            calls,
            server,
        )
    }

    #[tokio::test]
    async fn waits_for_the_author_in_the_active_router_generation() {
        let (access, calls, server) = server(vec![
            json!({"data": {"NodeReadiness": []}}),
            observation(2, false, false),
            observation(1, true, true),
            observation(2, true, false),
            observation(2, true, true),
        ])
        .await;
        let result = wait_for_author(
            &access,
            OWNER,
            AUTHOR,
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .await;
        server.abort();
        result.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn unavailable_author_is_bounded() {
        let (access, _, server) = server(vec![observation(2, true, false)]).await;
        let result = wait_for_author(
            &access,
            OWNER,
            AUTHOR,
            Duration::from_millis(100),
            Duration::from_millis(1),
        )
        .await;
        server.abort();
        let error = format!("{}", ErrorChain(result.unwrap_err().as_ref()));
        assert!(error.contains("did not become ready"), "{error}");
    }
}
