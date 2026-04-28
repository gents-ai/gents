use super::*;

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
