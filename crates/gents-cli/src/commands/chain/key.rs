use anyhow::{bail, Context, Result};
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
use crate::{print_json, resolve_agent_did, resolve_config_access};

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
    let init = read_init_config(&home_dir)?
        .ok_or_else(|| anyhow::anyhow!("no initialized home at {}", home_dir.display()))?;
    let identity = load_initialized_home_identity(&home_dir, &init)?;
    let agent_did = identity.did().to_string();
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;

    let binding_id = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if load_binding(&access, &agent_did, &binding_id)
        .await?
        .is_some()
    {
        bail!(
            "chain key binding {:?} already exists; choose another --name",
            binding_id
        );
    }

    let mut secret = generate_secp256k1_secret();
    let address = address_from_secret(&secret).context("deriving Ethereum address")?;
    let created_at = chrono::Utc::now().to_rfc3339();
    let payload = attestation_payload(
        &binding_id,
        &agent_did,
        &address,
        KEY_BACKEND_KEYRING,
        &created_at,
    );
    let signature = identity
        .sign(&payload)
        .await
        .context("attesting chain key with principal DID")?;

    let doc = ChainKeyBindingDocument {
        binding_id: binding_id.clone(),
        agent_did: agent_did.clone(),
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
    let storage_key = binding_storage_key(&agent_did, &binding_id);
    if let Err(error) = store.store_new(&storage_key, &secret) {
        secret.fill(0);
        let cleanup = delete_binding(&access, &created_doc_id).await;
        return match cleanup {
            Ok(()) => Err(error).context("storing chain key in OS keyring"),
            Err(cleanup_error) => Err(error).context(format!(
                "storing chain key in OS keyring; cleanup also failed: {cleanup_error:#}"
            )),
        };
    }
    secret.fill(0);

    print_json(&json!({
        "binding_id": binding_id,
        "address": address,
        "key_backend": KEY_BACKEND_KEYRING,
        "agent_did": agent_did,
    }))
}

async fn list(args: ChainKeyAccessArgs) -> Result<()> {
    let principal = resolve_agent_did(args.home.as_deref(), None)?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let docs = load_bindings(&access, &principal).await?;
    print_json(&json!({
        "agent_did": principal,
        "count": docs.len(),
        "bindings": docs.iter().map(public_binding_json).collect::<Vec<_>>(),
    }))
}

async fn show(args: ChainKeyShowArgs) -> Result<()> {
    let principal = resolve_agent_did(args.access.home.as_deref(), None)?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let doc = load_binding(&access, &principal, &args.binding_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("chain key binding {:?} not found", args.binding_id))?;
    if doc.agent_did != principal {
        bail!("chain key binding is not owned by the local principal");
    }
    print_json(&public_binding_json(&doc))
}

async fn revoke(args: ChainKeyShowArgs) -> Result<()> {
    let principal = resolve_agent_did(args.access.home.as_deref(), None)?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let mut doc = load_binding(&access, &principal, &args.binding_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("chain key binding {:?} not found", args.binding_id))?;
    if doc.agent_did != principal {
        bail!("chain key binding is not owned by the local principal");
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
        .delete(&binding_storage_key(&doc.agent_did, &args.binding_id))
        .context("deleting revoked chain key from OS keyring")?;
    print_json(&json!({
        "binding_id": doc.binding_id,
        "address": doc.address,
        "revoked_at": doc.revoked_at,
        "tags": doc.tags,
    }))
}

fn public_binding_json(doc: &ChainKeyBindingDocument) -> Value {
    json!({
        "binding_id": doc.binding_id,
        "agent_did": doc.agent_did,
        "address": doc.address,
        "key_backend": doc.key_backend,
        "created_at": doc.created_at,
        "revoked_at": doc.revoked_at,
        "tags": doc.tags,
    })
}

async fn write_binding(access: &ConfigAccess, doc: &ChainKeyBindingDocument) -> Result<()> {
    access
        .transact("cli.chain_key.binding.upsert", |txn| {
            Box::pin(async move {
                let rows = decode_binding_rows(
                    &txn.execute(&chain_key_binding_by_id_query(
                        &doc.agent_did,
                        &doc.binding_id,
                    )?)
                    .await?,
                )?;
                anyhow::ensure!(
                    rows.len() <= 1,
                    "duplicate chain key binding within principal"
                );
                txn.execute(&upsert_chain_key_binding_mutation(doc)?)
                    .await?;
                Ok(())
            })
        })
        .await
}

async fn create_binding(access: &ConfigAccess, doc: &ChainKeyBindingDocument) -> Result<String> {
    access
        .transact("cli.chain_key.binding.create", |txn| {
            Box::pin(async move {
                let rows = decode_binding_rows(
                    &txn.execute(&chain_key_binding_by_id_query(
                        &doc.agent_did,
                        &doc.binding_id,
                    )?)
                    .await?,
                )?;
                anyhow::ensure!(
                    rows.is_empty(),
                    "chain key binding already exists for this principal"
                );
                let response = txn
                    .execute(&create_chain_key_binding_mutation(doc)?)
                    .await?;
                gents_protocol::graphql::extract_mutation_doc_id(&response, "ChainKeyBinding")
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
    agent_did: &str,
) -> Result<Vec<ChainKeyBindingDocument>> {
    let rows = decode_binding_rows(
        &access
            .execute(&list_chain_key_bindings_query(agent_did)?)
            .await?,
    )?;
    let mut ids = std::collections::HashSet::new();
    for row in &rows {
        anyhow::ensure!(
            row.agent_did == agent_did && ids.insert(row.binding_id.clone()),
            "invalid or duplicate chain key identity in principal list"
        );
    }
    Ok(rows)
}

async fn load_binding(
    access: &ConfigAccess,
    agent_did: &str,
    binding_id: &str,
) -> Result<Option<ChainKeyBindingDocument>> {
    let mut rows = decode_binding_rows(
        &access
            .execute(&chain_key_binding_by_id_query(agent_did, binding_id)?)
            .await?,
    )?;
    anyhow::ensure!(
        rows.len() <= 1,
        "duplicate chain key binding within principal"
    );
    anyhow::ensure!(
        rows.iter()
            .all(|row| row.agent_did == agent_did && row.binding_id == binding_id),
        "chain key lookup returned mismatched scoped identity"
    );
    Ok(rows.pop())
}

fn decode_binding_rows(value: &Value) -> Result<Vec<ChainKeyBindingDocument>> {
    let rows = value
        .pointer("/data/ChainKeyBinding")
        .or_else(|| value.get("ChainKeyBinding"))
        .cloned()
        .context("response missing ChainKeyBinding rows")?;
    if rows.is_null() {
        return Ok(Vec::new());
    }
    let mut rows = rows
        .as_array()
        .context("ChainKeyBinding rows must be an array")?
        .clone();
    for row in &mut rows {
        row.as_object_mut()
            .context("ChainKeyBinding row must be an object")?
            .remove("_docID");
    }
    serde_json::from_value(Value::Array(rows)).context("decoding ChainKeyBinding rows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_binding_json_omits_attestation_and_key_material() {
        let doc = ChainKeyBindingDocument {
            binding_id: "bind-1".to_string(),
            agent_did: "did:key:zAlice".to_string(),
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

    #[test]
    fn decode_binding_rows_ignores_doc_id_and_null() {
        let value = json!({
            "data": {
                "ChainKeyBinding": [{
                    "_docID": "bae-1",
                    "binding_id": "bind-1",
                    "agent_did": "did:key:zAlice",
                    "address": "0xabc",
                    "key_backend": "keyring",
                    "attestation": "0xsig",
                    "created_at": "t0",
                    "revoked_at": null
                }]
            }
        });
        let rows = decode_binding_rows(&value).expect("decode");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].binding_id, "bind-1");
        let empty = json!({ "data": { "ChainKeyBinding": null } });
        assert!(decode_binding_rows(&empty).expect("null").is_empty());
    }
}
