use gents_protocol::error_chain::ErrorChain;
use std::path::PathBuf;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::{
    address_from_secret, attestation_payload, binding_storage_key, chain_key_binding_by_id_query,
    create_chain_key_binding_mutation, delete_chain_key_binding_mutation, encode_attestation,
    generate_secp256k1_secret, list_chain_key_bindings_query, upsert_chain_key_binding_mutation,
    ChainKeyBindingDocument, ChainKeyMaterialStore, KeyringChainKeyStore, KEY_BACKEND_KEYRING,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::cli::args::{
    ChainKeyAccessArgs, ChainKeyCommand, ChainKeyGenerateArgs, ChainKeyShowArgs,
};
use crate::home_state::{load_initialized_home_identity, read_init_config, resolve_home_dir};
use crate::{print_json, resolve_config_access, resolve_node_did};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChainKeyError {
    #[error("no initialized home at {}", home.display())]
    NoInitializedHome { home: PathBuf },
    #[error("chain key binding {binding_id:?} already exists; choose another --name")]
    BindingExists { binding_id: String },
    #[error("chain key binding {binding_id:?} not found")]
    BindingNotFound { binding_id: String },
    #[error("chain key binding is not owned by the local node")]
    NotOwned,
    #[error("chain key binding already exists for this node")]
    BindingAlreadyExists,
    #[error("duplicate chain key binding within node scope")]
    DuplicateBinding,
    #[error("chain key lookup returned mismatched scoped identity")]
    MismatchedLookup,
    #[error("invalid or duplicate chain key identity in node list")]
    InvalidNodeList,
    #[error("response missing ChainKeyBinding rows")]
    MissingRows,
    #[error("ChainKeyBinding rows must be an array")]
    RowsNotArray,
    #[error("ChainKeyBinding row must be an object")]
    RowNotObject,
    #[error("decoding ChainKeyBinding rows")]
    DecodeRows(#[source] serde_json::Error),
}

pub(crate) async fn dispatch(command: ChainKeyCommand) -> Result<()> {
    match command {
        ChainKeyCommand::Generate(args) => generate(args).await,
        ChainKeyCommand::List(args) => list(args).await,
        ChainKeyCommand::Show(args) => show(args).await,
        ChainKeyCommand::Revoke(args) => revoke(args).await,
    }
}

async fn generate(args: ChainKeyGenerateArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.access.home.as_deref());
    let init = read_init_config(&home_dir)?.ok_or_else(|| ChainKeyError::NoInitializedHome {
        home: home_dir.clone(),
    })?;
    let identity = load_initialized_home_identity(&home_dir, &init)?;
    let node_did = identity.did().to_string();
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;

    let binding_id = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if load_binding(&access, &node_did, &binding_id)
        .await?
        .is_some()
    {
        return Err(ChainKeyError::BindingExists { binding_id }.into());
    }

    let mut secret = generate_secp256k1_secret();
    let address = address_from_secret(&secret).context("deriving Ethereum address")?;
    let created_at = chrono::Utc::now().to_rfc3339();
    let payload = attestation_payload(
        &binding_id,
        &node_did,
        &address,
        KEY_BACKEND_KEYRING,
        &created_at,
    );
    let signature = identity
        .sign(&payload)
        .await
        .context("attesting chain key with node DID")?;

    let doc = ChainKeyBindingDocument {
        binding_id: binding_id.clone(),
        node_did: node_did.clone(),
        address: address.clone(),
        key_backend: Some(KEY_BACKEND_KEYRING.to_string()),
        attestation: Some(encode_attestation(&signature)),
        created_at: Some(created_at),
        revoked_at: None,
        tags: Vec::new(),
    };
    let created_doc_id = match create_binding(&access, &doc).await {
        Ok(doc_id) => doc_id,
        Err(error) => {
            secret.fill(0);
            return Err(error);
        }
    };

    let store = KeyringChainKeyStore;
    let storage_key = binding_storage_key(&node_did, &binding_id);
    if let Err(error) = store.store_new(&storage_key, &secret) {
        secret.fill(0);
        let cleanup = delete_binding(&access, &created_doc_id).await;
        return match cleanup {
            Ok(()) => Err(error).context("storing chain key in OS keyring"),
            Err(cleanup_error) => Err(error).context(format!(
                "storing chain key in OS keyring; cleanup also failed: {}",
                ErrorChain(cleanup_error.as_ref())
            )),
        };
    }
    secret.fill(0);

    Ok(print_json(&json!({
        "binding_id": binding_id,
        "address": address,
        "key_backend": KEY_BACKEND_KEYRING,
        "node_did": node_did,
    }))?)
}

async fn list(args: ChainKeyAccessArgs) -> Result<()> {
    let principal = resolve_node_did(args.home.as_deref(), None)?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let docs = load_bindings(&access, &principal).await?;
    Ok(print_json(&json!({
        "node_did": principal,
        "count": docs.len(),
        "bindings": docs.iter().map(public_binding_json).collect::<Vec<_>>(),
    }))?)
}

async fn show(args: ChainKeyShowArgs) -> Result<()> {
    let principal = resolve_node_did(args.access.home.as_deref(), None)?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let doc = load_binding(&access, &principal, &args.binding_id)
        .await?
        .ok_or_else(|| ChainKeyError::BindingNotFound {
            binding_id: args.binding_id.clone(),
        })?;
    if doc.node_did != principal {
        return Err(ChainKeyError::NotOwned.into());
    }
    Ok(print_json(&public_binding_json(&doc))?)
}

async fn revoke(args: ChainKeyShowArgs) -> Result<()> {
    let principal = resolve_node_did(args.access.home.as_deref(), None)?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let mut doc = load_binding(&access, &principal, &args.binding_id)
        .await?
        .ok_or_else(|| ChainKeyError::BindingNotFound {
            binding_id: args.binding_id.clone(),
        })?;
    if doc.node_did != principal {
        return Err(ChainKeyError::NotOwned.into());
    }
    let already_revoked = doc
        .revoked_at
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if !already_revoked {
        doc.revoked_at = Some(chrono::Utc::now().to_rfc3339());
        write_binding(&access, &doc).await?;
    }
    KeyringChainKeyStore
        .delete(&binding_storage_key(&doc.node_did, &args.binding_id))
        .context("deleting revoked chain key from OS keyring")?;
    Ok(print_json(&json!({
        "binding_id": doc.binding_id,
        "address": doc.address,
        "revoked_at": doc.revoked_at,
        "tags": doc.tags,
    }))?)
}

fn public_binding_json(doc: &ChainKeyBindingDocument) -> Value {
    json!({
        "binding_id": doc.binding_id,
        "node_did": doc.node_did,
        "address": doc.address,
        "key_backend": doc.key_backend,
        "created_at": doc.created_at,
        "revoked_at": doc.revoked_at,
        "tags": doc.tags,
    })
}

async fn write_binding(access: &ConfigAccess, doc: &ChainKeyBindingDocument) -> Result<()> {
    access
        .transact_write("cli.chain_key.binding.upsert", |txn| {
            Box::pin(async move {
                let rows = decode_binding_rows(
                    &txn.execute(&chain_key_binding_by_id_query(
                        &doc.node_did,
                        &doc.binding_id,
                    )?)
                    .await?,
                )?;
                if rows.len() > 1 {
                    return Err(ChainKeyError::DuplicateBinding.into());
                }
                txn.execute(&upsert_chain_key_binding_mutation(doc)?)
                    .await?;
                Ok(())
            })
        })
        .await
}

async fn create_binding(access: &ConfigAccess, doc: &ChainKeyBindingDocument) -> Result<String> {
    access
        .transact_write("cli.chain_key.binding.create", |txn| {
            Box::pin(async move {
                let rows = decode_binding_rows(
                    &txn.execute(&chain_key_binding_by_id_query(
                        &doc.node_did,
                        &doc.binding_id,
                    )?)
                    .await?,
                )?;
                if !rows.is_empty() {
                    return Err(ChainKeyError::BindingAlreadyExists.into());
                }
                let response = txn
                    .execute(&create_chain_key_binding_mutation(doc)?)
                    .await?;
                Ok(gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "ChainKeyBinding",
                )?)
            })
        })
        .await
}

async fn delete_binding(access: &ConfigAccess, doc_id: &str) -> Result<()> {
    access
        .write(
            "cli.chain_key.binding.delete_incomplete",
            &delete_chain_key_binding_mutation(doc_id),
        )
        .await?;
    Ok(())
}

async fn load_bindings(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Vec<ChainKeyBindingDocument>> {
    let rows = decode_binding_rows(
        &access
            .execute(&list_chain_key_bindings_query(node_did)?)
            .await?,
    )?;
    let mut ids = rapidhash::RapidHashSet::default();
    for row in &rows {
        if row.node_did != node_did || !ids.insert(row.binding_id.clone()) {
            return Err(ChainKeyError::InvalidNodeList.into());
        }
    }
    Ok(rows)
}

async fn load_binding(
    access: &ConfigAccess,
    node_did: &str,
    binding_id: &str,
) -> Result<Option<ChainKeyBindingDocument>> {
    let mut rows = decode_binding_rows(
        &access
            .execute(&chain_key_binding_by_id_query(node_did, binding_id)?)
            .await?,
    )?;
    if rows.len() > 1 {
        return Err(ChainKeyError::DuplicateBinding.into());
    }
    if !rows
        .iter()
        .all(|row| row.node_did == node_did && row.binding_id == binding_id)
    {
        return Err(ChainKeyError::MismatchedLookup.into());
    }
    Ok(rows.pop())
}

fn decode_binding_rows(value: &Value) -> Result<Vec<ChainKeyBindingDocument>, ChainKeyError> {
    let rows = value
        .pointer("/data/ChainKeyBinding")
        .or_else(|| value.get("ChainKeyBinding"))
        .cloned()
        .ok_or(ChainKeyError::MissingRows)?;
    if rows.is_null() {
        return Ok(Vec::new());
    }
    let mut rows = rows.as_array().ok_or(ChainKeyError::RowsNotArray)?.clone();
    for row in &mut rows {
        row.as_object_mut()
            .ok_or(ChainKeyError::RowNotObject)?
            .remove("_docID");
    }
    serde_json::from_value(Value::Array(rows)).map_err(ChainKeyError::DecodeRows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_binding_json_omits_attestation_and_key_material() {
        let doc = ChainKeyBindingDocument {
            binding_id: "bind-1".to_string(),
            node_did: "did:key:zAlice".to_string(),
            address: "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266".to_string(),
            key_backend: Some(KEY_BACKEND_KEYRING.to_string()),
            attestation: Some("0xdeadbeef".to_string()),
            created_at: Some("2026-08-28T00:00:00Z".to_string()),
            revoked_at: None,
            tags: Vec::new(),
        };
        let json = public_binding_json(&doc);
        let text = json.to_string();
        assert!(!text.contains("attestation"));
        assert!(!text.contains("deadbeef"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("private"));
        assert_eq!(json["binding_id"], "bind-1");
        assert_eq!(
            json["address"],
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
    }
}
