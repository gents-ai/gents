use super::*;

#[tokio::test]
async fn a_chatgpt_account_matches_across_key_forms() {
    let access = access().await;
    let did = "did:key:z6MkTestKeyForms";
    let id = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" }, "chatgpt_user_id": "user-a" });
    let fallback_only = chatgpt_with(
        did,
        json!({ "chatgpt_account_id": "acct-ws" }),
        id.clone(),
        "refresh-1",
    );
    assert_eq!(
        fallback_only.provider_account_key.as_deref(),
        Some("acct-ws:user-a")
    );
    store(&access, fallback_only).await;

    let both = chatgpt_with(
        did,
        json!({ "chatgpt_account_id": "acct-ws", "chatgpt_account_user_id": "member-a" }),
        id,
        "refresh-2",
    );
    let signed = store(&access, both).await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    let stored = rows(&access, did, CHATGPT_CODEX_PROVIDER).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].refresh_token, "refresh-2");
    assert_eq!(
        stored[0].provider_account_key.as_deref(),
        Some("acct-ws:user-a")
    );
}

#[tokio::test]
async fn a_key_form_the_sign_in_does_not_compute_is_rewritten() {
    let access = access().await;
    let did = "did:key:z6MkTestKeyRewrite";
    let id = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" }, "chatgpt_user_id": "user-a" });
    let both = chatgpt_with(
        did,
        json!({ "chatgpt_account_id": "acct-ws", "chatgpt_account_user_id": "member-a" }),
        id.clone(),
        "refresh-1",
    );
    assert_eq!(both.provider_account_key.as_deref(), Some("member-a"));
    store(&access, both).await;

    let fallback_only = chatgpt_with(
        did,
        json!({ "chatgpt_account_id": "acct-ws" }),
        id,
        "refresh-2",
    );
    let signed = store(&access, fallback_only).await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    let stored = rows(&access, did, CHATGPT_CODEX_PROVIDER).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].provider_account_key.as_deref(),
        Some("acct-ws:user-a")
    );
}

#[tokio::test]
async fn an_upgraded_claude_row_with_its_own_account_id_is_filled() {
    let access = access().await;
    let did = "did:key:z6MkTestUpgradedClaude";
    let mut input = raw_row(did, CLAUDE_OAUTH_PROVIDER);
    input["account_id"] = json!("label-a");
    seed_raw(&access, input).await;

    let signed = store(
        &access,
        claude(did, "label-a", Some("account-a"), "refresh-1"),
    )
    .await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    assert!(signed.identity_matched);
    let stored = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].credential_id,
        oauth_credential_id(did, CLAUDE_OAUTH_PROVIDER)
    );
    assert_eq!(
        stored[0].provider_account_key.as_deref(),
        Some("org-1:account-a")
    );
}

#[tokio::test]
async fn an_upgraded_claude_row_with_another_account_id_is_not_overwritten() {
    let access = access().await;
    let did = "did:key:z6MkTestUpgradedOther";
    let mut input = raw_row(did, CLAUDE_OAUTH_PROVIDER);
    input["account_id"] = json!("label-a");
    seed_raw(&access, input).await;
    let before = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.remove(0);

    let signed = store(
        &access,
        claude(did, "label-b", Some("account-b"), "refresh-1"),
    )
    .await;
    assert_eq!(signed.result, SignInResult::Added);
    let stored = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await;
    assert_eq!(stored.len(), 2);
    assert!(stored.contains(&before));
}

#[tokio::test]
async fn a_row_with_no_identity_is_filled() {
    let access = access().await;
    let did = "did:key:z6MkTestNoIdentity";
    seed_raw(&access, raw_row(did, CHATGPT_CODEX_PROVIDER)).await;

    let signed = store(&access, Product::ChatGpt.sign_in(did, "a", "refresh-1")).await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    assert!(!signed.identity_matched);
    let stored = rows(&access, did, CHATGPT_CODEX_PROVIDER).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].provider_account_key.as_deref(), Some("member-a"));
}

#[tokio::test]
async fn a_keyless_sign_in_adds_beside_a_keyed_row() {
    let access = access().await;
    let did = "did:key:z6MkTestKeyless";
    store(
        &access,
        claude(did, "label-a", Some("account-a"), "refresh-1"),
    )
    .await;
    let before = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.remove(0);

    let signed = store(&access, claude(did, "label-a", None, "refresh-2")).await;
    assert_eq!(signed.result, SignInResult::Added);
    let stored = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await;
    assert_eq!(stored.len(), 2);
    assert!(stored.contains(&before));
}

#[tokio::test]
async fn a_stored_key_beside_other_tokens_is_not_trusted() {
    let access = access().await;
    let did = "did:key:z6MkTestStaleKey";
    let mut stale = grok(did, "principal-2", "refresh-1");
    stale.provider_account_key = Some("user:principal-1".into());
    seed_raw(
        &access,
        json!({
            "credential_id": oauth_credential_id(did, XAI_OAUTH_PROVIDER),
            "node_did": did,
            "provider": XAI_OAUTH_PROVIDER,
            "access_token": stale.access_token,
            "refresh_token": "refresh-1",
            "is_fedramp": false,
            "access_token_expires_at": "2030-01-01T00:00:00Z",
            "enabled": true,
            "provider_account_key": "user:principal-1",
        }),
    )
    .await;
    let signed = store(&access, grok(did, "principal-2", "refresh-2")).await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    assert_eq!(rows(&access, did, XAI_OAUTH_PROVIDER).await.len(), 1);

    let member_b = Product::ChatGpt.sign_in(did, "b", "refresh-1");
    seed_raw(
        &access,
        json!({
            "credential_id": oauth_credential_id(did, CHATGPT_CODEX_PROVIDER),
            "node_did": did,
            "provider": CHATGPT_CODEX_PROVIDER,
            "access_token": member_b.access_token,
            "refresh_token": "refresh-1",
            "id_token": member_b.id_token,
            "account_id": "acct-ws",
            "is_fedramp": false,
            "access_token_expires_at": "2030-01-01T00:00:00Z",
            "enabled": true,
            "provider_account_key": "member-a",
        }),
    )
    .await;
    let signed = store(&access, Product::ChatGpt.sign_in(did, "b", "refresh-2")).await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    assert_eq!(rows(&access, did, CHATGPT_CODEX_PROVIDER).await.len(), 1);
}

#[tokio::test]
async fn a_claude_row_with_a_stale_key_is_not_overwritten() {
    let access = access().await;
    let did = "did:key:z6MkTestClaudeSkew";
    store(
        &access,
        claude(did, "label-b", Some("account-1"), "refresh-1"),
    )
    .await;
    let before = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.remove(0);

    let signed = store(
        &access,
        claude(did, "label-a", Some("account-1"), "refresh-2"),
    )
    .await;
    assert_eq!(signed.result, SignInResult::Added);
    let signed = store(
        &access,
        claude(did, "label-b", Some("account-2"), "refresh-3"),
    )
    .await;
    assert_eq!(signed.result, SignInResult::Added);
    assert!(rows(&access, did, CLAUDE_OAUTH_PROVIDER)
        .await
        .contains(&before));
}

#[tokio::test]
async fn a_claude_row_matches_on_its_stored_key() {
    let access = access().await;
    let did = "did:key:z6MkTestClaudeKeys";
    store(
        &access,
        claude(did, "label-1", Some("account-1"), "refresh-1"),
    )
    .await;
    let second = store(
        &access,
        claude(did, "label-2", Some("account-2"), "refresh-2"),
    )
    .await;

    let signed = store(
        &access,
        claude(did, "label-2", Some("account-2"), "refresh-3"),
    )
    .await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    assert_eq!(signed.doc_id, second.doc_id);
    assert_eq!(rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.len(), 2);
}

#[tokio::test]
async fn padded_values_compare_trimmed() {
    let access = access().await;
    let did = "did:key:z6MkTestPadded";
    store(&access, grok(did, "principal-1", "refresh-1")).await;
    let padded = grok(did, "principal-1 ", "refresh-2");
    assert_eq!(
        padded.provider_account_key.as_deref(),
        Some("user:principal-1 ")
    );
    let signed = store(&access, padded).await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    assert_eq!(rows(&access, did, XAI_OAUTH_PROVIDER).await.len(), 1);

    let mut input = raw_row(did, CLAUDE_OAUTH_PROVIDER);
    input["account_id"] = json!("label-a");
    input["provider_account_key"] = json!(" org-1:account-1 ");
    seed_raw(&access, input).await;
    let signed = store(
        &access,
        claude(did, "label-a", Some("account-1"), "refresh-3"),
    )
    .await;
    assert_eq!(signed.result, SignInResult::Refreshed);
    assert_eq!(rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.len(), 1);
}
