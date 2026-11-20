use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

const PIN_COLLECTION: &str = gents_protocol::schemas::AGENT_REQUEST_RETRY_BUDGET_NAME;
const REQUEST_RESOURCE: &str = "delivered_requests";
const PAYLOAD_DOMAIN: &str = "gents/request-retry-budget/v1";

#[derive(Debug, thiserror::Error)]
pub enum RetryBudgetError {
    #[error("retry parent max_retries must not be negative")]
    NegativeParentPin,
    #[error("pin writer {writer} is not the request's Node {node_did}")]
    NotTheRequestNode { writer: String, node_did: String },
    #[error("signing the retry budget pin: {0}")]
    Sign(#[source] anyhow::Error),
    #[error("writing the retry budget pin: {0}")]
    Write(#[source] anyhow::Error),
    #[error("reading the retry budget pin: {0}")]
    Read(#[source] anyhow::Error),
}

/// The successor budget a request's first claim pins (Lean
/// `Request.PinnedRetryBudget.claimPin`): a retry successor of a pinned
/// parent inherits that pin; otherwise a normal request takes the claiming
/// Agent's configured `InferenceExecution.max_request_retries` and an
/// auxiliary one takes zero. Only the first claim calls it, so a running
/// request keeps the budget it started with.
pub fn claim_pin(
    normal: bool,
    configured: u64,
    retry_successor: bool,
    parent_pin: Option<i64>,
) -> Result<u64, RetryBudgetError> {
    match (retry_successor, parent_pin) {
        (true, Some(pin)) => u64::try_from(pin).map_err(|_| RetryBudgetError::NegativeParentPin),
        _ if normal => Ok(configured),
        _ => Ok(0),
    }
}

/// Whether a terminal request's lineage may take one more successor (Lean
/// `Request.PinnedRetryBudget.successorBudgetRemains`). A request without a
/// pin never reached a claim: an operator reissue may follow it, automated
/// background redrive does not.
pub fn successor_budget_remains(
    automated: bool,
    retry_count: i64,
    max_retries: Option<i64>,
) -> bool {
    match max_retries {
        None => !automated,
        Some(max_retries) => (0..max_retries).contains(&retry_count),
    }
}

/// The pin a reader accepts among a request's stored pins, each given as
/// whether its Node signature verifies and its value (Lean
/// `Request.PinnedRetryBudget.readPin`): the smallest Node-signed value.
pub fn read_pins(pins: impl IntoIterator<Item = (bool, i64)>) -> Option<i64> {
    pins.into_iter()
        .filter_map(|(signed, value)| signed.then_some(value))
        .min()
}

fn payload(node_did: &str, request_doc_id: &str, max_retries: i64) -> Vec<u8> {
    format!("{PAYLOAD_DOMAIN}\n{node_did}\n{request_doc_id}\n{max_retries}").into_bytes()
}

fn encode_signature(signature: Vec<u8>) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(signature)
}

fn signed_by_node(node_did: &str, request_doc_id: &str, max_retries: i64, signature: &str) -> bool {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(signature)
        .is_ok_and(|signature| {
            gents_store::identity::verify_did_signature(
                node_did,
                &payload(node_did, request_doc_id, max_retries),
                &signature,
            )
            .unwrap_or(false)
        })
}

/// Writes the pin of a claimed request as its Node (Lean
/// `Identity.RequestRetryBudget`): the document's owner is the Node, its
/// resource grants no one else `update` or `delete`, and its fields are
/// `@immutable`, so the working Agent cannot change the budget it runs under.
pub async fn write_pin(
    node: std::sync::Arc<defra_node::Node>,
    runtime: std::sync::Arc<dyn NodeIdentity>,
    node_did: &str,
    request_doc_id: &str,
    max_retries: u64,
) -> Result<(), RetryBudgetError> {
    if runtime.did() != node_did {
        return Err(RetryBudgetError::NotTheRequestNode {
            writer: runtime.did().to_owned(),
            node_did: node_did.to_owned(),
        });
    }
    let max_retries = i64::try_from(max_retries)
        .map_err(|error| RetryBudgetError::Write(anyhow::Error::from(error)))?;
    let signature = encode_signature(
        runtime
            .sign(&payload(node_did, request_doc_id, max_retries))
            .await
            .map_err(|error| RetryBudgetError::Sign(anyhow::Error::from(error)))?,
    );
    let access = ConfigAccess::local_runtime(node, runtime)
        .map_err(|error| RetryBudgetError::Write(anyhow::Error::from(error)))?;
    let mutation = format!(
        r#"mutation {{ create_{PIN_COLLECTION}(input: {{ request_doc_id: "{}", node_did: "{}", max_retries: {max_retries}, signature: "{signature}" }}) {{ _docID }} }}"#,
        escape_graphql_string(request_doc_id),
        escape_graphql_string(node_did),
    );
    let request_doc_id = request_doc_id.to_owned();
    access
        .transact_write("request.retry_budget.pin", |txn| {
            let mutation = &mutation;
            let request_doc_id = &request_doc_id;
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc = gents_store::graphql::created_doc_id(&response, PIN_COLLECTION)?;
                txn.add_resource_relationship(
                    PIN_COLLECTION,
                    &doc,
                    "parent",
                    REQUEST_RESOURCE,
                    request_doc_id,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .map_err(RetryBudgetError::Write)
}

fn pins_query(node_did: &str, request_doc_ids: &[&str]) -> String {
    let ids = request_doc_ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{ {PIN_COLLECTION}(filter: {{ request_doc_id: {{ _in: [{ids}] }}, node_did: {{ _eq: "{}" }} }}) {{ request_doc_id max_retries signature }} }}"#,
        escape_graphql_string(node_did),
    )
}

/// Each request's accepted pin among the stored rows (`read_pins`).
fn accepted_pins(
    node_did: &str,
    response: &serde_json::Value,
) -> std::collections::BTreeMap<String, i64> {
    let mut stored = std::collections::BTreeMap::<String, Vec<(bool, i64)>>::new();
    for row in response
        .pointer(&format!("/data/{PIN_COLLECTION}"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(request_doc_id), Some(value), Some(signature)) = (
            row["request_doc_id"].as_str(),
            row["max_retries"].as_i64(),
            row["signature"].as_str(),
        ) else {
            continue;
        };
        stored.entry(request_doc_id.to_owned()).or_default().push((
            signed_by_node(node_did, request_doc_id, value, signature),
            value,
        ));
    }
    stored
        .into_iter()
        .filter_map(|(request_doc_id, pins)| read_pins(pins).map(|pin| (request_doc_id, pin)))
        .collect()
}

/// The pin of `request_doc_id` that `access` can read and its Node signed
/// (`read_pins`); `None` when the request was never pinned.
pub async fn read_pin(
    access: &ConfigAccess,
    node_did: &str,
    request_doc_id: &str,
) -> Result<Option<i64>, RetryBudgetError> {
    let response = access
        .execute(&pins_query(node_did, &[request_doc_id]))
        .await
        .map_err(|error| RetryBudgetError::Read(anyhow::Error::from(error)))?;
    Ok(accepted_pins(node_did, &response).remove(request_doc_id))
}

/// The accepted pins of `request_doc_ids` within one transaction.
pub async fn read_pins_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    request_doc_ids: &[&str],
) -> Result<std::collections::BTreeMap<String, i64>, RetryBudgetError> {
    if request_doc_ids.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    let response = txn
        .execute(&pins_query(node_did, request_doc_ids))
        .await
        .map_err(|error| RetryBudgetError::Read(anyhow::Error::from(error)))?;
    Ok(accepted_pins(node_did, &response))
}

#[cfg(test)]
#[path = "retry_budget_tests.rs"]
mod tests;
