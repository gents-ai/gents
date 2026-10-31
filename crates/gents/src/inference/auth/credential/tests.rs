use serde_json::json;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::bearer::get_or_insert_arc;
use super::document::oauth_credential_from_value;
use super::*;

use std::sync::atomic::AtomicUsize;

fn sample_credential() -> OAuthCredential {
    OAuthCredential {
        doc_id: Some("doc-1".to_string()),
        credential_id: oauth_credential_id("did:key:zAgent", "chatgpt-codex"),
        node_did: "did:key:zAgent".to_string(),
        provider: "chatgpt-codex".to_string(),
        access_token: "access-tok".to_string(),
        refresh_token: "refresh-tok".to_string(),
        id_token: Some("id-tok".to_string()),
        account_id: Some("acct-1".to_string()),
        chatgpt_plan_type: Some("pro".to_string()),
        is_fedramp: false,
        access_token_expires_at: DateTime::<Utc>::from_timestamp(1_900_000_000, 0).unwrap(),
        last_refresh: Some(DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap()),
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
    }
}

#[test]
fn debug_redacts_every_token() {
    let mut credential = sample_credential();
    credential.provider_account_key = Some("pak-sentinel".to_string());
    let refreshed = RefreshedTokens {
        access_token: credential.access_token.clone(),
        refresh_token: credential.refresh_token.clone(),
        id_token: credential.id_token.clone(),
        account_id: credential.account_id.clone(),
        is_fedramp: false,
        plan_type: None,
        access_token_expires_at: credential.access_token_expires_at,
    };
    let debug = format!("{credential:?} {credential:#?} {refreshed:?} {refreshed:#?}");
    for token in ["access-tok", "refresh-tok", "id-tok", "pak-sentinel"] {
        assert!(!debug.contains(token), "{token} leaked: {debug}");
    }
    assert!(debug.contains("acct-1"), "{debug}");
    for field in [
        "account_ref",
        "connected_at",
        "provider_account_key",
        "label",
    ] {
        assert!(debug.contains(field), "{field} missing: {debug}");
    }
}

#[test]
fn account_label_is_display_only_and_prefers_email() {
    use base64::Engine;
    let mut credential = sample_credential();
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(r#"{"email":"person@example.test","sub":"provider-user"}"#);
    credential.id_token = Some(format!("header.{payload}.signature"));
    assert_eq!(
        account_display_label(&credential).as_deref(),
        Some("person@example.test")
    );
    assert_eq!(credential.account_id.as_deref(), Some("acct-1"));
    credential.id_token = None;
    assert_eq!(
        account_display_label(&credential).as_deref(),
        Some("acct-1")
    );
    credential.account_id = None;
    assert_eq!(account_display_label(&credential), None);
}

#[test]
fn shared_registry_returns_one_arc_per_key_and_constructs_once() {
    use rapidhash::{HashMapExt, RapidHashMap};
    use std::sync::Mutex as StdMutex;

    let registry: StdMutex<RapidHashMap<String, Arc<u32>>> = StdMutex::new(RapidHashMap::new());
    let calls = AtomicUsize::new(0);

    let a = get_or_insert_arc(&registry, "k1", || {
        calls.fetch_add(1, Ordering::SeqCst);
        7u32
    });
    let b = get_or_insert_arc(&registry, "k1", || {
        calls.fetch_add(1, Ordering::SeqCst);
        99u32
    });
    let c = get_or_insert_arc(&registry, "k2", || {
        calls.fetch_add(1, Ordering::SeqCst);
        7u32
    });

    assert!(Arc::ptr_eq(&a, &b), "same key must share one Arc");
    assert!(!Arc::ptr_eq(&a, &c), "different keys must be distinct Arcs");
    assert_eq!(*a, 7, "first construction wins for a key");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "construct exactly once per distinct key"
    );
}

#[test]
fn apply_refreshed_tokens_preserves_omitted_optionals_and_ors_fedramp() {
    let mut credential = sample_credential();
    credential.is_fedramp = true;
    let prior_id = credential.id_token.clone();

    apply_refreshed_tokens(
        &mut credential,
        RefreshedTokens {
            access_token: "new-access".to_string(),
            refresh_token: "new-refresh".to_string(),
            id_token: None,
            account_id: None,
            is_fedramp: false,
            plan_type: None,
            access_token_expires_at: DateTime::<Utc>::from_timestamp(2_000_000_000, 0).unwrap(),
        },
    );

    assert_eq!(credential.access_token, "new-access");
    assert_eq!(credential.refresh_token, "new-refresh");
    assert_eq!(credential.id_token, prior_id, "omitted id_token preserved");
    assert_eq!(
        credential.account_id.as_deref(),
        Some("acct-1"),
        "omitted account_id preserved"
    );
    assert_eq!(
        credential.chatgpt_plan_type.as_deref(),
        Some("pro"),
        "omitted plan preserved"
    );
    assert!(credential.is_fedramp, "is_fedramp stays true via OR");
    assert_eq!(
        credential.access_token_expires_at.timestamp(),
        2_000_000_000
    );
    assert!(credential.last_refresh.is_some());
}

#[test]
fn oauth_credential_from_value_applies_defaults_and_cleans_blanks() {
    let row = json!({
        "_docID": "doc-9",
        "credential_id": "chatgpt-codex:did:key:zA",
        "node_did": "did:key:zA",
        "provider": "chatgpt-codex",
        "access_token": "acc",
        "refresh_token": "ref",
        "id_token": null,
        "account_id": "",
        "chatgpt_plan_type": null,
        "access_token_expires_at": "2030-01-01T00:00:00Z",
        "last_refresh": null,
    });

    let credential = oauth_credential_from_value(row).expect("row parses");

    assert_eq!(credential.doc_id.as_deref(), Some("doc-9"));
    assert_eq!(credential.access_token, "acc");
    assert_eq!(credential.id_token, None, "explicit null -> None");
    assert_eq!(credential.account_id, None, "blank string cleaned to None");
    assert!(!credential.is_fedramp, "missing is_fedramp defaults false");
    assert!(credential.enabled, "missing enabled defaults true");
    assert_eq!(credential.last_refresh, None);
    assert_eq!(
        credential.access_token_expires_at,
        DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    );
}

#[test]
fn oauth_credential_from_value_rejects_a_blank_refresh_token() {
    let row = json!({
        "_docID": "doc-9",
        "credential_id": "gents-cloud:did:key:zA",
        "node_did": "did:key:zA",
        "provider": "gents-cloud",
        "access_token": "workspace-token",
        "refresh_token": "  ",
        "access_token_expires_at": "2030-01-01T00:00:00Z",
    });
    let error = oauth_credential_from_value(row).expect_err("blank refresh token");
    assert!(
        error.to_string().contains("missing refresh_token"),
        "{error}"
    );
    let rows = oauth_credentials_from_response(&json!({
        "data": {
            "OAuthCredential": [{
                "_docID": "doc-9",
                "credential_id": "gents-cloud:did:key:zA",
                "node_did": "did:key:zA",
                "provider": "gents-cloud",
                "access_token": "workspace-token",
                "refresh_token": "",
                "access_token_expires_at": "2030-01-01T00:00:00Z",
            }]
        }
    }));
    let listed = rows.into_iter().collect::<Result<Vec<_>, _>>();
    assert!(listed
        .expect_err("one blank row fails the list")
        .to_string()
        .contains("missing refresh_token"));
}

#[test]
fn upsert_update_block_omits_immutable_node_did() {
    let mutation = oauth_credential_upsert_mutation(&sample_credential());

    let update_idx = mutation
        .find("update:")
        .expect("mutation has an update block");
    let (add_block, update_block) = mutation.split_at(update_idx);

    assert!(
        !update_block.contains("node_did:"),
        "update block must omit immutable node_did: {update_block}"
    );
    assert!(
        update_block.contains("access_token:") && update_block.contains("refresh_token:"),
        "update block must still rotate token fields: {update_block}"
    );
    assert!(
        add_block.contains("node_did:") && add_block.contains("credential_id:"),
        "add block must set node_did and credential_id: {add_block}"
    );
}

#[test]
fn account_fields_are_only_in_the_add_block() {
    let mut credential = sample_credential();
    credential.account_ref = Some("acct-b".to_string());
    credential.connected_at = Some(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap());
    let mutation = oauth_credential_upsert_mutation(&credential);
    let (add_block, update_block) = mutation.split_at(mutation.find("update:").unwrap());

    assert!(
        add_block.contains(r#"account_ref: "acct-b""#)
            && add_block.contains(r#"connected_at: "2023-11-14T22:13:20Z""#),
        "add block must set the account fields: {add_block}"
    );
    assert!(
        !update_block.contains("account_ref") && !update_block.contains("connected_at"),
        "update block must never rewrite the account fields: {update_block}"
    );
}

#[tokio::test]
async fn account_fields_are_kept_when_a_row_is_stored_again() {
    let node = super::test_support::test_node().await;
    let connected_at = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
    let mut credential = sample_credential();
    credential.doc_id = None;
    credential.account_ref = Some("acct-b".to_string());
    credential.connected_at = Some(connected_at);
    upsert_oauth_credential(&node, &credential).await.unwrap();

    let stored = lookup_oauth_credential_by_id(&node, &credential.credential_id)
        .await
        .unwrap()
        .expect("stored row");
    assert_eq!(stored.account_ref.as_deref(), Some("acct-b"));
    assert_eq!(stored.connected_at, Some(connected_at));

    credential.account_ref = None;
    credential.connected_at = None;
    credential.access_token = "access-rotated".to_string();
    upsert_oauth_credential(&node, &credential).await.unwrap();

    let stored = lookup_oauth_credential_by_id(&node, &credential.credential_id)
        .await
        .unwrap()
        .expect("stored row");
    assert_eq!(stored.access_token, "access-rotated");
    assert_eq!(stored.account_ref.as_deref(), Some("acct-b"));
    assert_eq!(stored.connected_at, Some(connected_at));
}

#[test]
fn not_entitled_copy_is_product_specific() {
    let msg = classify_oauth_auth_error(
        &CHATGPT_OAUTH_PRODUCT,
        "did:key:zAgent",
        "chatgpt-codex",
        &OAuthAuthProblem::NotEntitled,
    );
    assert!(
        !msg.contains("api.x.ai") && !msg.contains("SuperGrok"),
        "ChatGPT tier-gate copy must not carry xAI guidance: {msg}"
    );
    assert!(msg.contains("ChatGPT"), "{msg}");
}

#[test]
fn classifies_not_entitled_without_relogin_guidance() {
    let msg = classify_oauth_auth_error(
        &XAI_OAUTH_PRODUCT,
        "did:key:zAgent",
        "xai-oauth",
        &OAuthAuthProblem::NotEntitled,
    );
    assert!(msg.contains("not entitled"), "{msg}");
    assert!(
        !msg.contains("gents grok-login"),
        "tier gate should not push re-login as the fix: {msg}"
    );
    assert!(msg.contains("api.x.ai"), "{msg}");
}
