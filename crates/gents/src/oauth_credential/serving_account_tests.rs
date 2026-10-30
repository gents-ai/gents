use serde_json::json;

use super::*;
use crate::document_config::BackendAuth;

fn account(account_ref: Option<&str>, label: &str, enabled: bool) -> AccountSummary {
    AccountSummary {
        credential_id: format!(
            "claude-subscription:did:key:z6MkTest{}",
            account_ref.unwrap_or("")
        ),
        provider: crate::claude_oauth::CLAUDE_OAUTH_PROVIDER.into(),
        account_ref: account_ref.map(str::to_owned),
        label: label.into(),
        identity: Some("IDENTITY".into()),
        plan: None,
        enabled,
        default: account_ref.is_none(),
        access_token_expires_at: Utc::now(),
        connected_at: None,
    }
}

fn backend(
    name: &str,
    provider_kind: &str,
    auth: BackendAuth,
    enabled: bool,
) -> crate::InferenceBackend {
    serde_json::from_value(json!({
        "node_did": "did:key:z6MkTest", "backend_id": name, "name": name,
        "provider_kind": provider_kind, "endpoint": "http://127.0.0.1:1/v1",
        "auth": auth, "enabled": enabled,
    }))
    .unwrap()
}

fn oauth(account_ref: Option<&str>) -> BackendAuth {
    BackendAuth::NodeOAuth {
        account_ref: account_ref.map(str::to_owned),
    }
}

fn accounts() -> Vec<AccountSummary> {
    vec![
        account(None, "Claude", true),
        account(Some("acct-b"), "label-b", false),
    ]
}

#[test]
fn an_account_backend_names_its_account() {
    let original = backend("claude", "ClaudeCliSubscription", oauth(None), true);
    assert_eq!(
        serving_account(&original, &accounts()),
        ServingAccount {
            label: "Claude".into(),
            state: AccountState::Enabled,
            provider: Some(crate::claude_oauth::CLAUDE_OAUTH_PROVIDER),
        }
    );
    let b = backend(
        "label-b",
        "ClaudeCliSubscription",
        oauth(Some("acct-b")),
        true,
    );
    assert_eq!(
        serving_account(&b, &accounts()),
        ServingAccount {
            label: "label-b".into(),
            state: AccountState::Disabled,
            provider: Some(crate::claude_oauth::CLAUDE_OAUTH_PROVIDER),
        }
    );
}

#[test]
fn a_backend_whose_account_is_gone_is_missing_by_its_name() {
    let gone = backend(
        "label-gone",
        "ClaudeCliSubscription",
        oauth(Some("acct-other")),
        true,
    );
    assert_eq!(
        serving_account(&gone, &accounts()),
        ServingAccount {
            label: "label-gone".into(),
            state: AccountState::Missing,
            provider: Some(crate::claude_oauth::CLAUDE_OAUTH_PROVIDER),
        }
    );
}

#[test]
fn an_api_key_backend_is_its_own_account() {
    let key = BackendAuth::ApiKey {
        key: "key-SECRET".into(),
    };
    for (enabled, state) in [
        (true, AccountState::Enabled),
        (false, AccountState::Disabled),
    ] {
        let api = backend("OpenAI", "OpenAiCompatible", key.clone(), enabled);
        assert_eq!(
            serving_account(&api, &accounts()),
            ServingAccount {
                label: "OpenAI".into(),
                state,
                provider: None,
            }
        );
    }
}

#[test]
fn the_serving_account_holds_no_identity() {
    for backend in [
        backend("claude", "ClaudeCliSubscription", oauth(None), true),
        backend(
            "label-gone",
            "ClaudeCliSubscription",
            oauth(Some("acct-other")),
            true,
        ),
    ] {
        let json = serde_json::to_value(serving_account(&backend, &accounts())).unwrap();
        let text = json.to_string();
        assert!(
            !text.contains("IDENTITY") && !text.contains("SECRET"),
            "{text}"
        );
        assert_eq!(
            json["state"],
            serving_account(&backend, &accounts()).state.as_str()
        );
    }
}
