use anyhow::Result;
use defra_node::EmbeddedNode;
use serde_json::{json, Value};

use super::*;

pub async fn lookup_oauth_credential_by_id(
    node: &EmbeddedNode,
    credential_id: &str,
) -> Result<Option<OAuthCredential>> {
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &oauth_credential_by_id_query(credential_id),
        "lookup_oauth_credential_by_id",
    )
    .await?;
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub async fn list_oauth_credentials(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &oauth_credentials_for_node_query(node_did),
        "list_oauth_credentials",
    )
    .await?;
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .collect()
}

pub async fn list_oauth_credentials_on(
    access: &crate::config_client::ConfigAccess,
    node_did: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = access
        .execute(&oauth_credentials_for_node_query(node_did))
        .await?;
    let mut rows = oauth_credentials_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(resolver_order);
    Ok(rows)
}

pub async fn lookup_oauth_credential_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<OAuthCredential>> {
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &oauth_credential_by_doc_id_query(doc_id),
        "lookup_oauth_credential_by_doc_id",
    )
    .await?;
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub async fn upsert_oauth_credential(
    node: &std::sync::Arc<EmbeddedNode>,
    credential: &OAuthCredential,
) -> Result<String> {
    let mutation = oauth_credential_upsert_mutation(credential);
    let response =
        crate::config_client::ConfigAccess::write_local(node, "oauth_credential.upsert", &mutation)
            .await?;
    Ok(gents_protocol::graphql::extract_mutation_doc_id(
        &response,
        "OAuthCredential",
    )?)
}

pub async fn upsert_oauth_credential_on(
    access: &crate::config_client::ConfigAccess,
    credential: &OAuthCredential,
) -> Result<String> {
    let mutation = oauth_credential_upsert_mutation(credential);
    let response = access.write("oauth_credential.upsert", &mutation).await?;
    Ok(gents_protocol::graphql::extract_mutation_doc_id(
        &response,
        "OAuthCredential",
    )?)
}

pub(super) async fn provider_rows_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = txn
        .execute(&oauth_credentials_of_provider_query(node_did, provider))
        .await?;
    let mut rows = oauth_credentials_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(resolver_order);
    Ok(rows)
}
