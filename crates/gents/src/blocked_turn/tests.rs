use std::sync::Arc;

use chrono::{Duration, TimeZone};
use defra_node::EmbeddedNode;
use gents_loop::provider_limit::{persisted_failure_reason, ProviderLimitHeaders};
use gents_protocol::behavior_readiness::BehaviorReadinessUnavailableReason;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};

use super::*;
use crate::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::oauth_credential::{account_failure, account_failure_state, XAI_OAUTH_PRODUCT};
use crate::Collection;

const DID: &str = "did:key:z6MkTestBlockedTurn";
const A: &str = "claude";
const B: &str = "claude-subscription-acct-b";
const RIG_429: &str = "HttpError: Invalid status code 429 Too Many Requests with message: ";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 25, 16, 0, 0).unwrap()
}

fn backend(backend_id: &str, account_ref: Option<&str>, name: &str) -> (Collection, Value) {
    let auth = match account_ref {
        Some(account_ref) => json!({"kind": "principal_oauth", "account_ref": account_ref}),
        None => json!({"kind": "principal_oauth"}),
    };
    (
        Collection::InferenceBackend,
        json!({
            "agent_did": DID, "backend_id": backend_id, "name": name,
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": auth,
        }),
    )
}

fn profile(profile_id: &str, backend_id: &str) -> (Collection, Value) {
    (
        Collection::InferenceProfile,
        json!({"agent_did": DID, "profile_id": profile_id, "backend_id": backend_id, "model_name": "model-x"}),
    )
}

/// A behavior on `profile`; with `compaction`, its context compacts with
/// that profile.
fn behavior(
    behavior_id: &str,
    profile: &str,
    compaction: Option<&str>,
) -> Vec<(Collection, Value)> {
    let Some(compaction) = compaction else {
        return vec![(
            Collection::AgentBehavior,
            json!({"agent_did": DID, "behavior_id": behavior_id, "inference_profile_id": profile}),
        )];
    };
    let context = format!("context-{behavior_id}");
    let compaction_id = format!("compaction-{behavior_id}");
    vec![
        (
            Collection::Compaction,
            json!({"agent_did": DID, "compaction_id": compaction_id, "inference_profile_id": compaction}),
        ),
        (
            Collection::AgentContext,
            json!({"agent_did": DID, "context_id": context, "compaction_id": compaction_id}),
        ),
        (
            Collection::AgentBehavior,
            json!({"agent_did": DID, "behavior_id": behavior_id, "context_id": context, "inference_profile_id": profile}),
        ),
    ]
}

/// Accounts A (original) and B (`label-b`); profile `main` on `main_backend`
/// used by `x` and `y`; `x` compacts with `summ` on `summ_backend`.
fn documents(main_backend: &str, summ_backend: &str) -> Vec<(Collection, Value)> {
    let mut documents = vec![
        backend(A, None, "Claude"),
        backend(B, Some("acct-b"), "label-b"),
        profile("main", main_backend),
        profile("summ", summ_backend),
    ];
    documents.extend(behavior("x", "main", Some("summ")));
    documents.extend(behavior("y", "main", None));
    documents
}

fn references(documents: Vec<(Collection, Value)>) -> ConfigReferences {
    ConfigReferences::from_documents(DID, documents).unwrap()
}

fn account(account_ref: Option<&str>, label: &str) -> AccountSummary {
    AccountSummary {
        credential_id: format!("credential-KEY-SENTINEL-{label}"),
        provider: crate::claude_oauth::CLAUDE_OAUTH_PROVIDER.into(),
        account_ref: account_ref.map(str::to_owned),
        label: label.into(),
        identity: Some("IDENTITY".into()),
        plan: None,
        enabled: true,
        default: account_ref.is_none(),
        access_token_expires_at: now() + Duration::hours(1),
        connected_at: Some(now() - Duration::days(1)),
    }
}

fn accounts() -> Vec<AccountSummary> {
    vec![account(None, "Claude"), account(Some("acct-b"), "label-b")]
}

fn failed_request(failure_reason: &str) -> AgentRequestRow {
    AgentRequestRow {
        request_id: "request-1".into(),
        agent_did: Some(DID.into()),
        behavior_id: Some("x".into()),
        lifecycle_state: Some(RequestLifecycleState::Failed),
        failure_reason: Some(failure_reason.into()),
        ..Default::default()
    }
}

fn call(backend_id: &str, call_kind: &str, failure_reason: String) -> FailedCall {
    let at = (now() - Duration::minutes(5)).to_rfc3339();
    FailedCall {
        backend_id: Some(backend_id.into()),
        behavior_id: Some("x".into()),
        call_kind: Some(call_kind.into()),
        failure_reason: Some(failure_reason),
        queued_at: Some(at.clone()),
        started_at: Some(at),
    }
}

/// The text a usage-limited call records.
fn recorded(text: &str) -> String {
    persisted_failure_reason(text, now())
}

fn anthropic_rejected() -> String {
    let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."}}"#;
    let annotated = ProviderLimitHeaders::from_headers(
        [
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", "1790354400"),
        ],
        now(),
    )
    .annotate(body);
    recorded(&format!(
        "Claude Messages HTTP 429 Too Many Requests body={annotated}"
    ))
}

fn blocked(
    references: &ConfigReferences,
    accounts: &[AccountSummary],
    request: &AgentRequestRow,
    call: Option<&FailedCall>,
) -> Option<BlockedTurn> {
    blocked_turn_from(references, accounts, request, call, now())
}

fn limit_on(main_backend: &str, failure_reason: String) -> Option<BlockedTurn> {
    let call = call(main_backend, "inference", failure_reason.clone());
    blocked(
        &references(documents(main_backend, A)),
        &accounts(),
        &failed_request(&failure_reason),
        Some(&call),
    )
}

fn label_b() -> Option<BlockedAccount> {
    Some(BlockedAccount {
        label: "label-b".into(),
        provider: crate::claude_oauth::CLAUDE_OAUTH_PROVIDER.into(),
    })
}

#[test]
fn rejected_headers_limit() {
    assert_eq!(
        limit_on(B, anthropic_rejected()),
        Some(BlockedTurn {
            reason: BlockedReason::UsageLimit,
            account: label_b(),
            profile: Some("main".into()),
            behaviors_on_profile: vec!["x".into(), "y".into()],
            resets_at: Utc.timestamp_opt(1_790_354_400, 0).single(),
            switch_command: Some("gents config profile set-account main <account>".into()),
        })
    );
}

#[test]
fn body_only_limit() {
    let body = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_at":1790362800}}"#;
    let turn = limit_on(B, recorded(&format!("{RIG_429}{body}"))).expect("blocked");
    assert_eq!(turn.reason, BlockedReason::UsageLimit);
    assert_eq!(turn.resets_at, Utc.timestamp_opt(1_790_362_800, 0).single());
    assert_eq!(turn.account, label_b());
}

#[test]
fn limit_without_reset() {
    let body =
        r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached"}}"#;
    let turn = limit_on(B, recorded(&format!("{RIG_429}{body}"))).expect("blocked");
    assert_eq!(turn.reason, BlockedReason::UsageLimit);
    assert_eq!(turn.resets_at, None);
    assert_eq!(turn.profile.as_deref(), Some("main"));
}

#[test]
fn compaction_call_names_the_compaction_profile() {
    let references = references(documents(A, B));
    let call = call(B, "compaction", anthropic_rejected());
    let turn = blocked(
        &references,
        &accounts(),
        &failed_request("per-turn provider-input compaction failed: limit"),
        Some(&call),
    )
    .expect("blocked");
    assert_eq!(turn.account, label_b());
    assert_eq!(turn.profile.as_deref(), Some("summ"));
    assert_eq!(turn.behaviors_on_profile, ["x"]);
    assert_eq!(
        turn.switch_command.as_deref(),
        Some("gents config profile set-account summ <account>")
    );
}

#[test]
fn call_kind_picks_the_profile_when_both_share_the_backend() {
    let references = references(documents(B, B));
    let stopped = |call_kind| {
        let call = call(B, call_kind, anthropic_rejected());
        blocked(
            &references,
            &accounts(),
            &failed_request("limit"),
            Some(&call),
        )
        .expect("blocked")
    };
    let compaction = stopped("compaction");
    assert_eq!(compaction.profile.as_deref(), Some("summ"));
    assert_eq!(compaction.behaviors_on_profile, ["x"]);
    assert_eq!(
        compaction.switch_command.as_deref(),
        Some("gents config profile set-account summ <account>")
    );
    assert_eq!(stopped("inference").profile.as_deref(), Some("main"));
}

#[test]
fn behaviors_on_profile_counts_direct_and_compaction_users() {
    let mut documents = documents(B, A);
    documents.extend(behavior("z", "summ", Some("main")));
    // A pack-installed behavior names its bound profile directly
    // (`bind_pack_install_config` writes the profile id).
    documents.push((
        Collection::AgentBehavior,
        json!({"agent_did": DID, "behavior_id": "pack-w", "inference_profile_id": "main"}),
    ));
    let references = references(documents);
    assert_eq!(
        references.behaviors_on_profile("main"),
        ["pack-w", "x", "y", "z"]
    );
    assert_eq!(references.behavior_profiles("x"), ["main", "summ"]);
    assert_eq!(references.behavior_profiles("z"), ["summ", "main"]);
    assert_eq!(references.behavior_profiles("y"), ["main"]);
}

#[test]
fn routing_refusal_names_the_unavailable_account() {
    let credentials = BehaviorReadinessUnavailableReason::CredentialsRequired.public_message();
    let mut disabled = accounts();
    disabled[1].enabled = false;
    let turn = blocked(
        &references(documents(B, A)),
        &disabled,
        &failed_request(credentials),
        None,
    )
    .expect("blocked");
    assert_eq!(turn.reason, BlockedReason::AccountDisabled);
    assert_eq!(turn.account, label_b());
    assert_eq!(turn.profile.as_deref(), Some("main"));
    assert_eq!(turn.resets_at, None);

    let removed = vec![accounts().remove(0)];
    let turn = blocked(
        &references(documents(B, A)),
        &removed,
        &failed_request(credentials),
        None,
    )
    .expect("blocked");
    assert_eq!(turn.reason, BlockedReason::AccountRemoved);
    assert_eq!(
        turn.account,
        label_b(),
        "the backend keeps the label as its name"
    );

    let tools = BehaviorReadinessUnavailableReason::ToolConfigurationInvalid.public_message();
    let turn = blocked(
        &references(documents(A, B)),
        &disabled,
        &failed_request(tools),
        None,
    )
    .expect("blocked");
    assert_eq!(turn.reason, BlockedReason::AccountDisabled);
    assert_eq!(turn.profile.as_deref(), Some("summ"));
}

#[test]
fn signed_out_bearer_failure() {
    let text = format!(
        "{}\nSign in again.",
        account_failure(
            &XAI_OAUTH_PRODUCT,
            "label-a",
            "signed out (expired or revoked)"
        )
    );
    assert!(text.starts_with(r#"Grok account "label-a" is signed out (expired or revoked)."#));
    let call = call(B, "inference", text.clone());
    let turn = blocked(
        &references(documents(B, A)),
        &accounts(),
        &failed_request(&text),
        Some(&call),
    )
    .expect("blocked");
    assert_eq!(turn.reason, BlockedReason::AccountSignedOut);
    assert_eq!(turn.resets_at, None);
    assert_eq!(turn.profile.as_deref(), Some("main"));

    for state in [
        "signed out (expired or revoked)",
        "not entitled",
        "unusable",
        "disabled",
        "removed from this node",
    ] {
        let sentence = account_failure(&XAI_OAUTH_PRODUCT, "label-a", state);
        assert_eq!(account_failure_state(&sentence), Some(state), "{sentence}");
    }
    assert_eq!(
        account_failure_state("HttpError: Invalid status code 500"),
        None
    );
}

#[test]
fn slot_reused_since_the_call_names_no_account() {
    let mut reused = accounts();
    reused[1].connected_at = Some(now());
    let failure = anthropic_rejected();
    let call = call(B, "inference", failure.clone());
    let turn = blocked(
        &references(documents(B, A)),
        &reused,
        &failed_request(&failure),
        Some(&call),
    )
    .expect("blocked");
    assert_eq!(turn.reason, BlockedReason::UsageLimit);
    assert_eq!(turn.account, None);
    assert_eq!(turn.profile.as_deref(), Some("main"));
}

#[test]
fn not_blocked() {
    let references = references(documents(B, A));
    let completed = AgentRequestRow {
        lifecycle_state: Some(RequestLifecycleState::Completed),
        failure_reason: None,
        ..failed_request("")
    };
    assert_eq!(blocked(&references, &accounts(), &completed, None), None);
    let server_error = "HttpError: Invalid status code 500".to_string();
    let call = call(B, "inference", server_error.clone());
    assert_eq!(
        blocked(
            &references,
            &accounts(),
            &failed_request(&server_error),
            Some(&call)
        ),
        None
    );
}

fn dead_request(failure_reason: &str) -> AgentRequestRow {
    AgentRequestRow {
        lifecycle_state: Some(RequestLifecycleState::Dead),
        ..failed_request(failure_reason)
    }
}

#[test]
fn dead_request_with_a_limited_call_is_blocked() {
    let call = call(B, "inference", anthropic_rejected());
    let turn = blocked(
        &references(documents(B, A)),
        &accounts(),
        &dead_request("canonical output integrity failure: corrupt"),
        Some(&call),
    );
    assert_eq!(
        turn.map(|turn| turn.reason),
        Some(BlockedReason::UsageLimit)
    );
}

#[test]
fn dead_request_without_a_call_is_not_blocked() {
    assert_eq!(
        blocked(
            &references(documents(B, A)),
            &accounts(),
            &dead_request("canonical output integrity failure: corrupt"),
            None
        ),
        None
    );
}

#[test]
fn the_value_holds_no_secret() {
    let turn = limit_on(B, anthropic_rejected());
    let text = serde_json::to_string(&turn).unwrap();
    for secret in ["SECRET", "IDENTITY", "KEY-SENTINEL", "credential"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

async fn embedded() -> (Arc<EmbeddedNode>, ConfigAccess) {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::ensure_agent_principal(&node, DID).await.unwrap();
    let access = ConfigAccess::Local(node.clone());
    let sign_in = |who: &str| {
        crate::claude_oauth::credential_from_login_tokens(
            DID,
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            &crate::claude_oauth::ClaudeLoginTokens {
                access_token: "access-SECRET".into(),
                refresh_token: format!("refresh-SECRET-{who}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some("IDENTITY".into()),
                organization_uuid: Some("org-KEY".into()),
                account_uuid: Some(format!("account-KEY-{who}")),
            },
            Utc::now() - Duration::hours(1),
        )
    };
    let (_, original) = backend(A, None, "Claude");
    crate::config_client::write_inference_backend_document(
        &access,
        &serde_json::from_value(original).unwrap(),
    )
    .await
    .unwrap();
    crate::oauth_credential::store_sign_in(&access, sign_in("a"), None)
        .await
        .unwrap();
    let b = crate::oauth_credential::store_sign_in(&access, sign_in("b"), Some("label-b"))
        .await
        .unwrap();
    // Seed B's backend id so the fixture's documents name it.
    let b_backend = format!(
        "{}-{}",
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
        b.credential.account_ref.as_deref().unwrap()
    );
    let documents = documents(&b_backend, A)
        .into_iter()
        .filter(|(collection, _)| *collection != Collection::InferenceBackend)
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(documents).unwrap();
    access
        .transact("test.blocked_turn", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::config_client::apply_desired_state_plan(txn, plan)
                    .await
                    .map(|_| ())
            })
        })
        .await
        .unwrap();
    // The failed call ends after B's sign-in connected.
    let at = Utc::now().to_rfc3339();
    let failure = anthropic_rejected();
    let request_failure = escape_graphql_string(&failure);
    access
        .write(
            "test.blocked_turn.rows",
            &format!(
                r#"mutation {{
                    create_AgentRequest(input: {{
                        request_id: "request-1" purpose: "normal" agent_did: "{DID}"
                        behavior_id: "x" session_id: "session-1" content: "run"
                        lifecycle_state: "failed" failure_reason: "{request_failure}"
                        created_at: "{at}"
                    }}) {{ _docID }}
                    create_InferenceCall(input: {{
                        call_id: "call-1" request_id: "request-1" call_seq: 1
                        backend_id: "{b_backend}" behavior_id: "x" agent_did: "{DID}"
                        call_kind: "inference" attempt: 1 call_state: "failed"
                        failure_reason: "{request_failure}"
                        queued_at: "{at}" started_at: "{at}" ended_at: "{at}"
                    }}) {{ _docID }}
                }}"#
            ),
        )
        .await
        .unwrap();
    (node, access)
}

#[tokio::test]
async fn wrappers_read_the_stored_turn() {
    let (node, access) = embedded().await;
    let turn = blocked_turn(&access, DID, "request-1")
        .await
        .unwrap()
        .expect("blocked request");
    assert_eq!(turn.reason, BlockedReason::UsageLimit);
    assert_eq!(turn.account, label_b());
    assert_eq!(turn.profile.as_deref(), Some("main"));
    assert_eq!(turn.behaviors_on_profile, ["x", "y"]);
    assert_eq!(turn.resets_at, Utc.timestamp_opt(1_790_354_400, 0).single());

    crate::goal::set_goal_from_access(
        &access,
        DID,
        "session-1",
        Some("ship it"),
        Some(crate::goal::GoalStatus::UsageLimited),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        blocked_goal_turn(&access, DID, "session-1").await.unwrap(),
        Some(turn)
    );
    assert!(blocked_turn(&access, DID, "request-unknown").await.is_err());
    node.shutdown().await;
}
