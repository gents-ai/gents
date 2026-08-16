use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use defra_node::ExecuteRetryPolicy;

#[derive(Clone, Copy)]
enum ScriptedGraphqlResult {
    Conflict(&'static str),
    Error(&'static str),
    Success,
}

struct ScriptedGraphqlExecution {
    results: StdMutex<VecDeque<ScriptedGraphqlResult>>,
    attempts: AtomicUsize,
    policies: StdMutex<Vec<ExecuteRetryPolicy>>,
}

impl ScriptedGraphqlExecution {
    fn new(results: impl IntoIterator<Item = ScriptedGraphqlResult>) -> Self {
        Self {
            results: StdMutex::new(results.into_iter().collect()),
            attempts: AtomicUsize::new(0),
            policies: StdMutex::new(Vec::new()),
        }
    }

    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    fn policies(&self) -> Vec<ExecuteRetryPolicy> {
        self.policies.lock().unwrap().clone()
    }
}

impl crate::graphql::GraphqlExecution for ScriptedGraphqlExecution {
    async fn execute(&self, _graphql: &str, retry_policy: ExecuteRetryPolicy) -> QueryResponse {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.policies.lock().unwrap().push(retry_policy);
        match self
            .results
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted GraphQL result")
        {
            ScriptedGraphqlResult::Conflict(message) => {
                QueryResponse::transaction_conflict(message)
            }
            ScriptedGraphqlResult::Error(message) => QueryResponse::error(message),
            ScriptedGraphqlResult::Success => {
                QueryResponse::success(serde_json::json!({ "ok": true }))
            }
        }
    }
}

#[test]
fn default_policy() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_retries, 3);
    assert_eq!(policy.base_delay_ms, 1000);
    assert!(policy.has_retries());
}

#[test]
fn no_retry_policy() {
    let policy = RetryPolicy {
        max_retries: 0,
        ..Default::default()
    };
    assert!(!policy.has_retries());
}

#[test]
fn exponential_backoff_increases() {
    let policy = RetryPolicy {
        max_retries: 5,
        base_delay_ms: 1000,
        max_delay_ms: 60_000,
    };

    // Run multiple times to average out jitter
    let mut avg_delays = Vec::new();
    for attempt in 0..4 {
        let mut total = 0u128;
        let runs = 100;
        for _ in 0..runs {
            total += policy.delay_for_attempt(attempt).as_millis();
        }
        avg_delays.push(total / runs);
    }

    // Each attempt should roughly double (within jitter bounds)
    assert!(avg_delays[1] > avg_delays[0], "attempt 1 > attempt 0");
    assert!(avg_delays[2] > avg_delays[1], "attempt 2 > attempt 1");
}

#[test]
fn delay_respects_max() {
    let policy = RetryPolicy {
        max_retries: 3,
        base_delay_ms: 1000,
        max_delay_ms: 5000,
    };

    // Even at high attempt count, should not exceed max + jitter
    for _ in 0..50 {
        let delay = policy.delay_for_attempt(20);
        // max_delay + 25% jitter ceiling
        assert!(delay.as_millis() <= 6250);
    }
}

#[test]
fn delay_has_minimum_floor() {
    let policy = RetryPolicy {
        max_retries: 1,
        base_delay_ms: 10,
        max_delay_ms: 10,
    };

    for _ in 0..50 {
        assert!(policy.delay_for_attempt(0).as_millis() >= 100);
    }
}

#[test]
fn defradb_transaction_conflict_classifier_matches_prescribed_retry_error() {
    assert!(is_defradb_transaction_conflict_text(
        "commit error: datastore error: storage error: transaction conflict. Please retry"
    ));
    assert!(is_defradb_transaction_conflict_text(
        "TRANSACTION CONFLICT. PLEASE RETRY"
    ));
    assert!(!is_defradb_transaction_conflict_text(
        "unique constraint violation"
    ));
}

#[test]
fn defradb_conflict_backoff_is_exponential() {
    assert_eq!(defradb_conflict_retry_backoff(0).as_millis(), 100);
    assert_eq!(defradb_conflict_retry_backoff(1).as_millis(), 200);
    assert_eq!(defradb_conflict_retry_backoff(2).as_millis(), 400);
}

#[tokio::test]
async fn failure_reason_persistence_retries_non_conflict_transient_failures() {
    let mut attempts = 0u32;
    let value = retry_terminal_persistence_operation(
        "record_request_failure_reason",
        3,
        Duration::ZERO,
        || {
            attempts += 1;
            let current = attempts;
            async move {
                if current < 3 {
                    anyhow::bail!("temporary disk unavailable");
                }
                Ok("persisted")
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(value, "persisted");
    assert_eq!(attempts, 3);
}

#[tokio::test]
async fn terminal_lifecycle_persistence_exhaustion_is_bounded() {
    let mut attempts = 0u32;
    let error = retry_terminal_persistence_operation::<(), _, _>(
        "transition_request_terminal_status",
        3,
        Duration::ZERO,
        || {
            attempts += 1;
            async { anyhow::bail!("storage remains unavailable") }
        },
    )
    .await
    .expect_err("the bounded retry must exhaust");

    assert!(error.to_string().contains("storage remains unavailable"));
    assert_eq!(attempts, 4, "initial attempt plus three retries");
}

#[tokio::test(start_paused = true)]
async fn terminal_graphql_conflicts_have_one_total_attempt_budget() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let executor = ScriptedGraphqlExecution::new([
        ScriptedGraphqlResult::Conflict("persistent conflict 1"),
        ScriptedGraphqlResult::Conflict("persistent conflict 2"),
        ScriptedGraphqlResult::Conflict("persistent conflict 3"),
        ScriptedGraphqlResult::Conflict("persistent conflict 4"),
    ]);

    let error = execute_graphql_with_terminal_persistence_retry_using(
        &node,
        &executor,
        "mutation { terminal }",
        "persist terminal state",
    )
    .await
    .expect_err("persistent conflicts must exhaust the terminal budget");

    assert!(error.to_string().contains("persistent conflict 4"));
    assert_eq!(
        executor.attempts(),
        (TERMINAL_PERSISTENCE_MAX_RETRIES + 1) as usize
    );
    assert!(
        executor
            .policies()
            .iter()
            .all(|policy| policy.max_retries == 0),
        "each terminal attempt must disable DefraDB's nested retry budget"
    );
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn terminal_graphql_retries_non_conflict_storage_errors_until_recovery() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let executor = ScriptedGraphqlExecution::new([
        ScriptedGraphqlResult::Error("temporary disk unavailable"),
        ScriptedGraphqlResult::Error("ambiguous commit acknowledgement"),
        ScriptedGraphqlResult::Success,
    ]);

    let response = execute_graphql_with_terminal_persistence_retry_using(
        &node,
        &executor,
        "mutation { terminal }",
        "persist terminal state",
    )
    .await
    .expect("ambiguous storage failure should recover");

    assert!(!response.has_errors());
    assert_eq!(executor.attempts(), 3);
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn terminal_graphql_exhaustion_returns_the_underlying_storage_error() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let executor = ScriptedGraphqlExecution::new([
        ScriptedGraphqlResult::Error("storage failure 1"),
        ScriptedGraphqlResult::Error("storage failure 2"),
        ScriptedGraphqlResult::Error("storage failure 3"),
        ScriptedGraphqlResult::Error("durable repair sentinel"),
    ]);

    let error = execute_graphql_with_terminal_persistence_retry_using(
        &node,
        &executor,
        "mutation { terminal }",
        "persist terminal state",
    )
    .await
    .expect_err("terminal persistence must leave durable repair pending");

    assert!(error.to_string().contains("durable repair sentinel"));
    assert_eq!(executor.attempts(), 4);
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn terminal_backoff_releases_the_node_mutation_gate() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let terminal_executor = Arc::new(ScriptedGraphqlExecution::new([
        ScriptedGraphqlResult::Conflict("retry terminal mutation"),
        ScriptedGraphqlResult::Success,
    ]));
    let terminal_task = {
        let node = Arc::clone(&node);
        let executor = Arc::clone(&terminal_executor);
        tokio::spawn(async move {
            execute_graphql_with_terminal_persistence_retry_using(
                &node,
                executor.as_ref(),
                "mutation { terminal }",
                "persist terminal state",
            )
            .await
        })
    };

    while terminal_executor.attempts() == 0 {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;

    let unrelated_executor = ScriptedGraphqlExecution::new([ScriptedGraphqlResult::Success]);
    let before = tokio::time::Instant::now();
    crate::graphql::graphql_mutation_once_with_executor(
        &node,
        &unrelated_executor,
        "mutation { unrelated }",
        "unrelated mutation",
    )
    .await
    .expect("unrelated mutation should enter the gate during terminal backoff");

    assert_eq!(
        tokio::time::Instant::now(),
        before,
        "the unrelated mutation must not wait for terminal backoff"
    );
    assert_eq!(unrelated_executor.attempts(), 1);
    tokio::time::advance(Duration::from_millis(
        TERMINAL_PERSISTENCE_INITIAL_BACKOFF_MS,
    ))
    .await;
    terminal_task
        .await
        .expect("terminal task panicked")
        .expect("terminal retry should recover");
    node.shutdown().await;
}

#[tokio::test]
async fn ordinary_mutations_keep_defradb_conflict_retry_policy() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let executor = ScriptedGraphqlExecution::new([ScriptedGraphqlResult::Success]);

    crate::graphql::graphql_mutation_with_transaction_retry_using(
        &node,
        &executor,
        "mutation { ordinary }",
        "ordinary mutation",
    )
    .await
    .unwrap();

    let policies = executor.policies();
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].max_retries, DEFRA_DB_CONFLICT_MAX_RETRIES);
    assert_eq!(
        policies[0].initial_backoff,
        Duration::from_millis(DEFRA_DB_CONFLICT_INITIAL_BACKOFF_MS)
    );
    node.shutdown().await;
}
