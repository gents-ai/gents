use super::*;

#[tokio::test]
async fn another_nodes_account_is_not_touched() {
    let (db, _user, access) = access().await;
    let other_db = gents_store::test_support::TestStore::start(
        "credential-other-node",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let mine = db.node_identity.did();
    let theirs = other_db.node_identity.did();
    gents_documents::test_support::bootstrap_signed_test_node(&access, theirs).await;
    let signed = store(&access, Product::Claude.sign_in(theirs, "a", "refresh-a")).await;
    let id = signed.credential.credential_id.as_str();
    let before = rows(&access, theirs, CLAUDE_OAUTH_PROVIDER).await;
    for error in [
        set_account_label(&access, mine, id, "Work")
            .await
            .unwrap_err(),
        set_account_enabled(&access, mine, id, false)
            .await
            .unwrap_err(),
        remove(&access, mine, id).await.unwrap_err(),
    ] {
        assert!(error.to_string().contains("not found"), "{error}");
    }
    assert_eq!(rows(&access, theirs, CLAUDE_OAUTH_PROVIDER).await, before);
}
