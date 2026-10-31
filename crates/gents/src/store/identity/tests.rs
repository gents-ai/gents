use super::*;
use crypto::keys::PrivateKey;
use defra_core::signing::{RemoteSigner, SigningAuthorization, SigningConfig, SigningKeyType};
use std::sync::Arc;

#[tokio::test]
async fn key_identity_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amy-general.key");
    let identity = KeyIdentity::load_or_create(&path, None).unwrap();
    let payload = b"hello world";

    assert!(!identity.did().starts_with("did:test:"));
    assert!(defra_core::signing::get_identity(identity.did()).is_some());
    let signature = identity.sign(payload).await.unwrap();
    assert!(identity
        .verify(identity.did(), payload, &signature)
        .await
        .unwrap());

    let second = KeyIdentity::load_or_create(path, None).unwrap();
    assert_eq!(identity.did(), second.did());
    assert!(second
        .verify(identity.did(), payload, &signature)
        .await
        .unwrap());
}

#[tokio::test]
async fn key_identity_signs_embedded_node_commits() {
    let dir = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(dir.path().join("node.key"), None).unwrap();
    let did = identity.did().to_string();
    let expected_signer = defra_core::signing::get_identity(&did)
        .expect("KeyIdentity registers a DefraDB signer")
        .public_key_hex
        .clone();
    assert_eq!(
        commit_signer_identity_for_did(&did).unwrap(),
        expected_signer
    );
    let node = defra_node::EmbeddedNode::builder()
        .with_node_identity_did(&did)
        .build()
        .await
        .unwrap();
    node.add_collection("type SignedFact { value: String }")
        .await
        .unwrap();

    let response = node
        .execute(
            r#"mutation {
                create_SignedFact(input: {value: "durable"}) { _docID }
            }"#,
        )
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("add_SignedFact"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .expect("created document id");

    let commits =
        crate::store::graphql::composite_commits(&node, doc_id, "load signed fact versions")
            .await
            .unwrap();
    assert!(commits.iter().any(|commit| {
        commit
            .signature
            .as_ref()
            .map(|signature| &signature.identity)
            == Some(&expected_signer)
    }));

    node.shutdown().await;
}

#[tokio::test]
async fn registered_identity_uses_defradb_local_signing_config() {
    let raw_identity = RawIdentity::from_secp256r1(crypto::generate_secp256r1().unwrap()).unwrap();
    let did = raw_identity.did().unwrap().to_string();
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Secp256r1,
            private_key_bytes: SigningConfig::private_key_bytes_from_vec(
                raw_identity.private_key_bytes(),
            ),
            public_key_bytes: raw_identity.public_key_bytes(),
            public_key_hex: String::new(),
            remote_signer: None,
            signing_authorization: None,
        },
    );

    let identity = RegisteredIdentity::from_registered_did(&did, None).unwrap();
    let payload = b"gents registered local signing";
    let signature = identity.sign(payload).await.unwrap();

    assert_eq!(identity.did(), did);
    assert!(identity.verify(&did, payload, &signature).await.unwrap());
}

#[tokio::test]
async fn registered_identity_delegates_to_defradb_remote_signer() {
    let raw_identity = RawIdentity::from_secp256r1(crypto::generate_secp256r1().unwrap()).unwrap();
    let did = raw_identity.did().unwrap().to_string();
    let public_key_bytes = raw_identity.public_key_bytes();
    let private_key_bytes = raw_identity.private_key_bytes();
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Secp256r1,
            private_key_bytes: Vec::new(),
            public_key_bytes,
            public_key_hex: String::new(),
            remote_signer: Some(Arc::new(TestRemoteSigner { private_key_bytes })),
            signing_authorization: None,
        },
    );

    let identity = RegisteredIdentity::from_registered_did(&did, None).unwrap();
    let payload = b"gents registered remote signing";
    let signature = identity.sign(payload).await.unwrap();

    assert_eq!(identity.did(), did);
    assert!(identity.verify(&did, payload, &signature).await.unwrap());
}

struct TestRemoteSigner {
    private_key_bytes: Vec<u8>,
}

impl RemoteSigner for TestRemoteSigner {
    fn sign_sync(
        &self,
        data: &[u8],
        _authorization: Option<&SigningAuthorization>,
    ) -> std::result::Result<Vec<u8>, String> {
        let private_key = crypto::Secp256r1PrivateKey::from_bytes(&self.private_key_bytes)
            .map_err(|error| error.to_string())?;
        private_key.sign(data).map_err(|error| error.to_string())
    }
}

#[tokio::test]
async fn verify_resolves_did_key_without_prior_registration() {
    use crypto::Key as _;
    use identity::FullIdentity as _;
    let raw_private_key = crypto::generate_ed25519().unwrap();
    let raw_public_key_bytes = raw_private_key.to_public_key().raw_owned();
    let raw_did = crypto::create_did_key(crypto::KeyType::Ed25519, &raw_public_key_bytes).unwrap();
    let raw_identity = RawIdentity::from_private_key(raw_private_key).unwrap();
    let payload = b"cross-node invite payload";
    let signature = raw_identity.sign(payload).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path_b = dir.path().join("node-b.key");
    let node_b = KeyIdentity::load_or_create(&path_b, None).unwrap();
    let result = node_b.verify(&raw_did, payload, &signature).await;
    assert!(
        result.is_ok(),
        "verify should succeed for an unregistered did:key, got: {result:?}"
    );
    assert!(
        result.unwrap(),
        "signature over the correct payload should verify as true"
    );
    let tampered = b"tampered payload";
    let tampered_result = node_b.verify(&raw_did, tampered, &signature).await;
    assert!(
        !matches!(tampered_result, Ok(true)),
        "tampered payload must not verify as true, got: {tampered_result:?}"
    );
    let garbage_did = "did:key:zGARBAGE_INVALID_DID";
    let garbage_result = node_b.verify(garbage_did, payload, &signature).await;
    assert!(
        !matches!(garbage_result, Ok(true)),
        "garbage DID must not verify as true, got: {garbage_result:?}"
    );
}
