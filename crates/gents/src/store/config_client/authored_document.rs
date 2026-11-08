use serde_json::Value;

use super::{ConfigAccess, SchemaInstallError};
use crate::store::graphql::GraphqlResponseError;

/// The provenance of SDL supplied to a client-side signing operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentSchemaSource {
    /// The caller explicitly selected its own authored schema bytes.
    UserAuthored,
    /// A schema observed from a remote server cannot authorize local signatures.
    RemoteObservation,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthoredDocumentCreateError {
    #[error("provide a locally authored schema for exactly one matching application collection")]
    InvalidSchema,
    #[error("canonical and runtime collections require their dedicated publication owner")]
    ProtectedCollection,
    #[error("document fields must be a nonempty object and cannot contain empty lists")]
    InvalidFields,
    #[error("authored document publication requires a held User and a running HTTP endpoint")]
    ClientSignerRequired,
    #[error("the authored schema does not match the live collection")]
    Schema(#[source] SchemaInstallError),
    #[error("client-signed document publication failed")]
    Publication(#[source] GraphqlResponseError),
    #[error("document publication outcome is unconfirmed; inspect the collection before retrying")]
    Unconfirmed(#[source] GraphqlResponseError),
}

pub(super) fn document_create_allowed(
    source: DocumentSchemaSource,
    single_collection: bool,
    application_collection: bool,
    matches_live: bool,
    held_signer: bool,
    http: bool,
) -> bool {
    source == DocumentSchemaSource::UserAuthored
        && single_collection
        && application_collection
        && matches_live
        && held_signer
        && http
}

fn contains_empty_list(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.is_empty() || values.iter().any(contains_empty_list),
        Value::Object(values) => values.values().any(contains_empty_list),
        _ => false,
    }
}

/// Create one application document using the held client's native signing key.
/// The caller supplies locally authored SDL; observed server SDL is not signing authority.
/// The live collection is checked before signing, and an ambiguous commit is not replayed.
pub async fn create_authored_document(
    access: &ConfigAccess,
    collection: &str,
    fields: &Value,
    source: DocumentSchemaSource,
    sdl: &str,
) -> Result<String, AuthoredDocumentCreateError> {
    if source != DocumentSchemaSource::UserAuthored {
        return Err(AuthoredDocumentCreateError::InvalidSchema);
    }
    let application = !gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&collection)
        && !gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.contains(&collection);
    if !application {
        return Err(AuthoredDocumentCreateError::ProtectedCollection);
    }
    if !fields.as_object().is_some_and(|fields| !fields.is_empty()) || contains_empty_list(fields) {
        return Err(AuthoredDocumentCreateError::InvalidFields);
    }
    // Offline access publishes through its owned node's signed loopback
    // endpoint, so the signing route, not the read transport, decides.
    let (held_signer, http) = match access.write_route() {
        super::WriteRoute::Http(endpoint) => (
            endpoint.pack_replay.is_none() && endpoint.held_client_signing_capability().is_ok(),
            true,
        ),
        super::WriteRoute::Local(_) => (false, false),
    };
    if !held_signer || !http {
        return Err(AuthoredDocumentCreateError::ClientSignerRequired);
    }
    let plan = super::preview_schema_install(access, sdl)
        .await
        .map_err(AuthoredDocumentCreateError::Schema)?;
    let single =
        plan.collection_contracts.len() == 1 && plan.collection_contracts.contains_key(collection);
    let matches_live = !plan.requires_publication;
    if !document_create_allowed(source, single, application, matches_live, held_signer, http) {
        return Err(AuthoredDocumentCreateError::InvalidSchema);
    }
    let mutation = format!(
        "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
    );
    let variables = serde_json::json!({"input": fields});
    access
        .transact_write_with_finality("document.create_authored", |txn| {
            let (mutation, variables) = (&mutation, &variables);
            Box::pin(async move {
                let response = txn
                    .execute_client_signed_create(mutation, variables, sdl)
                    .await?;
                crate::store::graphql::created_doc_id(&response, collection)
            })
        })
        .await
        .map_err(|failure| {
            if failure.unconfirmed {
                AuthoredDocumentCreateError::Unconfirmed(failure.error)
            } else {
                AuthoredDocumentCreateError::Publication(failure.error)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeIdentity;
    use serde_json::json;

    #[tokio::test]
    async fn offline_access_publishes_through_its_signed_loopback_endpoint() -> anyhow::Result<()> {
        const SDL: &str = "type OfflineAuthoredFixture { name: String }";
        let directory = tempfile::tempdir()?;
        let root = crate::KeyIdentity::load_or_create(directory.path().join("node"), None)?;
        let user = crate::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/api/v0/graphql", listener.local_addr()?);
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(root.did())
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await?,
        );
        node.add_collection(SDL).await?;
        let offline = || ConfigAccess::local_as(node.clone(), identity::Did::new(user.did()).unwrap());
        assert!(matches!(
            create_authored_document(
                &offline(),
                "OfflineAuthoredFixture",
                &json!({"name": "unrouted"}),
                DocumentSchemaSource::UserAuthored,
                SDL,
            )
            .await,
            Err(AuthoredDocumentCreateError::ClientSignerRequired)
        ));
        let routed = offline().with_client_signed_writes_through(
            super::super::GraphqlEndpoint::as_principal(&url, user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?,
        )?;
        let doc = create_authored_document(
            &routed,
            "OfflineAuthoredFixture",
            &json!({"name": "offline"}),
            DocumentSchemaSource::UserAuthored,
            SDL,
        )
        .await?;
        let read = routed
            .execute(&format!(
                r#"{{ OfflineAuthoredFixture(docID: "{}") {{ name }} }}"#,
                crate::store::graphql::escape_graphql_string(&doc)
            ))
            .await?;
        assert_eq!(read["data"]["OfflineAuthoredFixture"][0]["name"], "offline");
        node.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn authored_application_document_is_signed_by_the_held_user() -> anyhow::Result<()> {
        const SDL: &str = "type AuthoredDocumentFixture { name: String }";
        const WRONG: &str = "type AuthoredDocumentFixture { name: Int }";
        let (db, user, access, _) =
            crate::test_support::signed_test_node("authored-document").await;
        // Schema installation is node administration; the held User authors documents only.
        let admin = ConfigAccess::local_as(
            db.node.clone(),
            identity::Did::new(db.node_identity.did()).unwrap(),
        );
        let plan = super::super::preview_schema_install(&admin, SDL).await?;
        super::super::apply_schema_install(&admin, SDL, &plan.artifact_digest).await?;
        let doc = create_authored_document(
            &access,
            "AuthoredDocumentFixture",
            &json!({"name": "created"}),
            DocumentSchemaSource::UserAuthored,
            SDL,
        )
        .await?;
        let escaped = crate::store::graphql::escape_graphql_string(&doc);
        let response = access.execute(&format!(
            r#"{{ _commits(docID: "{escaped}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#,
        )).await?;
        let cid = response["data"]["_commits"][0]["cid"]
            .as_str()
            .expect("genesis CID");
        let (block, signature) = db
            .node
            .authorized_signed_block_bytes(cid, Some(user.did()))
            .await?;
        let block = defra_core::block::Block::from_dag_cbor(&block)?;
        let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
        let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(signer, user.did());
        assert_ne!(user.did(), db.node_identity.did());
        let cases = &crate::lean_vocab_test::lean_contract_snapshot()
            .identity_client_document_creation_cases;
        for (case_name, source, schema) in [
            ("remote", DocumentSchemaSource::RemoteObservation, SDL),
            ("schema-mismatch", DocumentSchemaSource::UserAuthored, WRONG),
        ] {
            let model = cases.iter().find(|case| case["name"] == case_name).unwrap();
            assert_eq!(model["expected_route"], "refuse");
            assert!(
                create_authored_document(
                    &access,
                    "AuthoredDocumentFixture",
                    &json!({"name": "denied"}),
                    source,
                    schema,
                )
                .await
                .is_err(),
                "{case_name}"
            );
        }
        let response = access
            .execute("{ AuthoredDocumentFixture { name } }")
            .await?;
        assert_eq!(
            response["data"]["AuthoredDocumentFixture"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        Ok(())
    }
}
