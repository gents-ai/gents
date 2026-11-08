use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use serde_json::{json, Value};

use super::*;

pub(crate) fn runtime_credential_access(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<crate::store::config_client::ConfigAccess> {
    anyhow::ensure!(
        node.node_identity_did() == Some(node_did),
        "credential access requires the configured runtime identity"
    );
    Ok(crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node_did)?,
    ))
}

pub(super) async fn lookup_oauth_credential_by_id_on(
    access: &crate::store::config_client::ConfigAccess,
    credential_id: &str,
) -> Result<Option<OAuthCredential>> {
    let response = access
        .execute(&oauth_credential_by_id_query(credential_id))
        .await?;
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub async fn list_oauth_credentials(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<Vec<OAuthCredential>> {
    let access = runtime_credential_access(node, node_did)?;
    list_oauth_credentials_on(&access, node_did).await
}

pub async fn list_oauth_credentials_on(
    access: &crate::store::config_client::ConfigAccess,
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
    node: &std::sync::Arc<EmbeddedNode>,
    doc_id: &str,
) -> Result<Option<OAuthCredential>> {
    let node_did = node
        .node_identity_did()
        .context("credential lookup requires a runtime identity")?;
    let access = runtime_credential_access(node, node_did)?;
    let response = access
        .execute(&oauth_credential_by_doc_id_query(doc_id))
        .await?;
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub async fn upsert_oauth_credential(
    node: &std::sync::Arc<EmbeddedNode>,
    credential: &OAuthCredential,
) -> Result<String> {
    upsert_oauth_credential_on(
        &runtime_credential_access(node, &credential.node_did)?,
        credential,
    )
    .await
}

pub(super) fn credential_input(credential: &OAuthCredential, omitted: &[&str]) -> Result<Value> {
    let mut input = serde_json::to_value(credential)?;
    let object = input
        .as_object_mut()
        .context("credential input is not an object")?;
    object.remove("doc_id");
    for field in omitted {
        object.remove(*field);
    }
    Ok(input)
}

pub(super) async fn create_credential_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
) -> Result<String> {
    let input = credential_input(credential, &[])?;
    let response = txn.execute_client_signed_create(
        "mutation($input: OAuthCredentialMutationInputArg!) { create_OAuthCredential(input: $input) { _docID } }",
        &json!({"input": input}),
        gents_protocol::schemas::OAUTH_CREDENTIAL,
    ).await?;
    let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;
    crate::store::config_client::publish_secret_configuration_in_txn(
        txn,
        "OAuthCredential",
        &doc,
        &credential.node_did,
    )
    .await?;
    Ok(doc)
}

pub(super) async fn update_credential_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
    doc: &str,
    set_once: &[&str],
) -> Result<()> {
    let mut omitted = set_once.to_vec();
    omitted.push("credential_id");
    let input = credential_input(credential, &omitted)?;
    update_credential_patch_in_txn(txn, doc, input).await
}

pub(super) async fn update_credential_patch_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    doc: &str,
    input: Value,
) -> Result<()> {
    let document = crate::store::graphql::escape_graphql_string(doc);
    let mutation = format!(
        r#"mutation($input: OAuthCredentialMutationInputArg!) {{ update_OAuthCredential(filter: {{_docID: {{_eq: "{document}"}}}}, input: $input) {{_docID}} }}"#
    );
    let response = txn
        .execute_client_signed_update(
            &mutation,
            &json!({"input": input}),
            gents_protocol::schemas::OAUTH_CREDENTIAL,
        )
        .await?;
    let changed = gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;
    anyhow::ensure!(
        changed == doc,
        "credential update affected a different document"
    );
    Ok(())
}

pub async fn upsert_oauth_credential_on(
    access: &crate::store::config_client::ConfigAccess,
    credential: &OAuthCredential,
) -> Result<String> {
    access.transact_write("oauth_credential.upsert", |txn| {
        Box::pin(async move {
            let id = crate::store::graphql::escape_graphql_string(&credential.credential_id);
            let existing = txn.execute(&format!(
                r#"{{ OAuthCredential(filter: {{credential_id: {{_eq: "{id}"}}}}, limit: 2) {{_docID node_did}} }}"#
            )).await?;
            let rows = gents_protocol::graphql::graphql_rows_from_response(&existing, "OAuthCredential");
            anyhow::ensure!(rows.len() <= 1, "ambiguous OAuthCredential identity");
            if let Some(row) = rows.first() {
                anyhow::ensure!(row["node_did"].as_str() == Some(credential.node_did.as_str()),
                    "OAuthCredential belongs to a different Node");
                let doc = row["_docID"].as_str().context("OAuthCredential has no document ID")?;
                update_credential_in_txn(txn, credential, doc, SET_ONCE_FIELDS).await?;
                Ok(doc.to_owned())
            } else {
                create_credential_in_txn(txn, credential).await
            }
        })
    }).await
}

pub(super) async fn provider_rows_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
