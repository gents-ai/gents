use super::*;

#[tokio::test]
async fn simple_identity_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amy-general.key");
    let identity = SimpleIdentity::new("amy-general", &path, None);
    let payload = b"hello world";

    let signature = identity.sign(payload).await.unwrap();
    assert!(identity
        .verify(identity.did(), payload, &signature)
        .await
        .unwrap());

    let second = SimpleIdentity::new("amy-general", path, None);
    assert_eq!(identity.did(), second.did());
    assert!(second
        .verify(identity.did(), payload, &signature)
        .await
        .unwrap());
}
