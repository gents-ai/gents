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

    assert!(!identity.did().starts_with("did:defra-agent:"));
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
    let payload = b"defra-agent registered local signing";
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
    let payload = b"defra-agent registered remote signing";
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
