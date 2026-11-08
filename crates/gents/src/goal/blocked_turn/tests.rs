use std::sync::OnceLock;

use chrono::{Duration, TimeZone};
use gents_loop::provider_limit::{persisted_failure_reason, ProviderLimitHeaders};
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};

use super::*;
use crate::inference::auth::credential::{
    account_failure, account_failure_state, XAI_OAUTH_PRODUCT,
};
use crate::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::store::identity::NodeIdentity;
use crate::Collection;

struct ModelIdentities {
    _directory: tempfile::TempDir,
    _runtime: crate::KeyIdentity,
    node_did: String,
    agents: std::collections::BTreeMap<
        &'static str,
        gents_protocol::agent_identity::AgentIdentityBinding,
    >,
}

fn model_identities() -> &'static ModelIdentities {
    static IDENTITIES: OnceLock<ModelIdentities> = OnceLock::new();
    IDENTITIES.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        let runtime =
            crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
        let node_did = runtime.did().to_owned();
        let agents = ["x", "y", "z", "pack-w"]
            .into_iter()
            .map(|name| {
                (
                    name,
                    crate::store::identity::provision_agent_identity(&runtime).unwrap(),
                )
            })
            .collect();
        ModelIdentities {
            _directory: directory,
            _runtime: runtime,
            node_did,
            agents,
        }
    })
}

fn agent(name: &str) -> &gents_protocol::agent_identity::AgentIdentityBinding {
    &model_identities().agents[name]
}

fn agent_ids(names: &[&str]) -> Vec<String> {
    let mut ids = names
        .iter()
        .map(|name| agent(name).agent_did.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

const A: &str = "claude";
const B: &str = "claude-subscription-acct-b";
const RIG_429: &str = "HttpError: Invalid status code 429 Too Many Requests with message: ";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 25, 16, 0, 0).unwrap()
}

fn backend_for(
    node_did: &str,
    backend_id: &str,
    account_ref: Option<&str>,
    name: &str,
) -> (Collection, Value) {
    let auth = match account_ref {
        Some(account_ref) => json!({"kind": "node_oauth", "account_ref": account_ref}),
        None => json!({"kind": "node_oauth"}),
    };
    (
        Collection::InferenceBackend,
        json!({
            "node_did": node_did, "backend_id": backend_id, "name": name,
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": auth,
        }),
    )
}

fn profile_for(node_did: &str, profile_id: &str, backend_id: &str) -> (Collection, Value) {
    (
        Collection::InferenceProfile,
        json!({"node_did": node_did, "profile_id": profile_id,
        "backend_id": backend_id, "model_name": "model-x"}),
    )
}

fn behavior_for(
    node_did: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    profile: &str,
    compaction: Option<&str>,
) -> Vec<(Collection, Value)> {
    let agent_did = &binding.agent_did;
    let Some(compaction) = compaction else {
        return vec![(
            Collection::Agent,
            json!({"node_did": node_did, "agent_did": agent_did,
            "creation_nonce": binding.creation_nonce, "inference_profile_id": profile}),
        )];
    };
    let context = format!("context-{agent_did}");
    let compaction_id = format!("compaction-{agent_did}");
    vec![
        (
            Collection::Compaction,
            json!({"node_did": node_did, "compaction_id": compaction_id, "inference_profile_id": compaction}),
        ),
        (
            Collection::AgentContext,
            json!({"node_did": node_did, "context_id": context, "compaction_id": compaction_id}),
        ),
        (
            Collection::Agent,
            json!({"node_did": node_did, "agent_did": agent_did,
            "creation_nonce": binding.creation_nonce, "context_id": context, "inference_profile_id": profile}),
        ),
    ]
}

fn behavior(name: &str, profile: &str, compaction: Option<&str>) -> Vec<(Collection, Value)> {
    behavior_for(
        &model_identities().node_did,
        agent(name),
        profile,
        compaction,
    )
}

fn documents_for(
    node_did: &str,
    primary: &gents_protocol::agent_identity::AgentIdentityBinding,
    sibling: &gents_protocol::agent_identity::AgentIdentityBinding,
    main_backend: &str,
    summ_backend: &str,
) -> Vec<(Collection, Value)> {
    let mut documents = vec![
        backend_for(node_did, A, None, "Claude"),
        backend_for(node_did, B, Some("acct-b"), "label-b"),
        profile_for(node_did, "main", main_backend),
        profile_for(node_did, "summ", summ_backend),
    ];
    documents.extend(behavior_for(node_did, primary, "main", Some("summ")));
    documents.extend(behavior_for(node_did, sibling, "main", None));
    documents
}

fn documents(main_backend: &str, summ_backend: &str) -> Vec<(Collection, Value)> {
    documents_for(
        &model_identities().node_did,
        agent("x"),
        agent("y"),
        main_backend,
        summ_backend,
    )
}

fn references(documents: Vec<(Collection, Value)>) -> ConfigReferences {
    ConfigReferences::from_documents(&model_identities().node_did, documents).unwrap()
}

fn account(account_ref: Option<&str>, label: &str) -> AccountSummary {
    AccountSummary {
        credential_id: format!("credential-KEY-SENTINEL-{label}"),
        provider: crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER.into(),
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
        node_did: Some(model_identities().node_did.clone()),
        agent_did: Some(agent("x").agent_did.clone()),
        lifecycle_state: Some(RequestLifecycleState::Failed),
        failure_reason: Some(failure_reason.into()),
        ..Default::default()
    }
}

fn call(backend_id: &str, call_kind: &str, failure_reason: String) -> FailedCall {
    let at = (now() - Duration::minutes(5)).to_rfc3339();
    FailedCall {
        backend_id: Some(backend_id.into()),
        agent_did: Some(agent("x").agent_did.clone()),
        call_kind: Some(call_kind.into()),
        failure_reason: Some(failure_reason),
        queued_at: Some(at.clone()),
        started_at: Some(at),
    }
}

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
        provider: crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER.into(),
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
            agents_on_profile: agent_ids(&["x", "y"]),
            resets_at: Utc.timestamp_opt(1_790_354_400, 0).single(),
            switch_command: Some("gents config profile set-account main".into()),
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
    assert_eq!(turn.agents_on_profile, agent_ids(&["x"]));
    assert_eq!(
        turn.switch_command.as_deref(),
        Some("gents config profile set-account summ")
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
    assert_eq!(compaction.agents_on_profile, agent_ids(&["x"]));
    assert_eq!(
        compaction.switch_command.as_deref(),
        Some("gents config profile set-account summ")
    );
    assert_eq!(stopped("inference").profile.as_deref(), Some("main"));
}

#[test]
fn a_moved_profile_is_not_replaced_by_its_sibling() {
    let stopped = |main_backend, summ_backend, call_kind| {
        let call = call(A, call_kind, anthropic_rejected());
        blocked(
            &references(documents(main_backend, summ_backend)),
            &accounts(),
            &failed_request("limit"),
            Some(&call),
        )
        .expect("blocked")
        .profile
    };
    assert_eq!(stopped(B, A, "inference"), None, "main moved off A");
    assert_eq!(stopped(A, B, "compaction"), None, "summ moved off A");
}

#[test]
fn agents_on_profile_counts_direct_and_compaction_users() {
    let mut documents = documents(B, A);
    documents.extend(behavior("z", "summ", Some("main")));
    documents.extend(behavior("pack-w", "main", None));
    let references = references(documents);
    assert_eq!(
        references.agents_on_profile("main"),
        agent_ids(&["pack-w", "x", "y", "z"])
    );
    assert_eq!(
        references.agent_profiles(&agent("x").agent_did),
        ["main", "summ"]
    );
    assert_eq!(
        references.agent_profiles(&agent("z").agent_did),
        ["summ", "main"]
    );
    assert_eq!(references.agent_profiles(&agent("y").agent_did), ["main"]);
}

#[test]
fn routing_refusal_names_the_unavailable_account() {
    let credentials = AgentReadinessUnavailableReason::CredentialsRequired.public_message();
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

    let tools = AgentReadinessUnavailableReason::ToolConfigurationInvalid.public_message();
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

async fn embedded() -> (
    crate::support::TestDb,
    crate::KeyIdentity,
    ConfigAccess,
    Vec<String>,
) {
    use crate::request::lifecycle::{
        ClaimOutcome, ExecutionOrigin, RequestIdentity, RequestLifecycle, RequestSigner,
        RequestSpec,
    };

    let (db, user, access, primary) = crate::test_support::signed_test_node("blocked-turn").await;
    let node_did = db.node_identity.did();
    ConfigAccess::local_as(db.node.clone(), identity::Did::new(node_did).unwrap())
        .grant_node_permission(
            &identity::Did::new(user.did()).unwrap(),
            crate::store::config_client::NodePermission::DacRelationDelete,
        )
        .await
        .unwrap();
    let sibling = crate::store::identity::provision_agent_identity_authorized(
        &db.node,
        db.node_identity.as_ref(),
        &identity::Did::new(user.did()).unwrap(),
    )
    .await
    .unwrap();
    crate::test_support::install_test_agent_binding_with_access(
        node_did,
        "Sibling agent",
        &sibling,
        &access,
    )
    .await;
    let mut agents = vec![primary.agent_did.clone(), sibling.agent_did.clone()];
    agents.sort();
    let sign_in = |who: &str| {
        crate::inference::auth::claude::credential_from_login_tokens(
            node_did,
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &crate::inference::auth::claude::ClaudeLoginTokens {
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
    let (_, original) = backend_for(node_did, A, None, "Claude");
    crate::documents::write_inference_backend_document(
        &access,
        &serde_json::from_value(original).unwrap(),
    )
    .await
    .unwrap();
    crate::inference::auth::credential::store_sign_in(&access, sign_in("a"), None)
        .await
        .unwrap();
    let b =
        crate::inference::auth::credential::store_sign_in(&access, sign_in("b"), Some("label-b"))
            .await
            .unwrap();
    let b_backend = format!(
        "{}-{}",
        crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        b.credential.account_ref.as_deref().unwrap()
    );
    let documents = documents_for(node_did, &primary, &sibling, &b_backend, A)
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
        .transact_write("test.blocked_turn", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan)
                    .await
                    .map(|_| ())
            })
        })
        .await
        .unwrap();

    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &primary)
        .unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let create = crate::request::lifecycle::build_signed_client_request(
        &access,
        RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            RequestIdentity {
                request_id: "request-1".into(),
                node_did: node_did.into(),
                requester_did: Some(user.did().into()),
                agent_did: primary.agent_did.clone(),
                session_id: "session-1".into(),
                content: "run".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: at.clone(),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(&access, &create)
        .await
        .unwrap();
    let response = agent_access
        .execute(&format!(
            r#"{{AgentRequest(filter: {{request_id: {{_eq: "request-1"}}}}, limit: 2) {{{}}}}}"#,
            gents_protocol::row::AGENT_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let [row] = response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("blocked request must be unique");
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone()).unwrap();
    let request: gents_protocol::row::AgentRequest = row.try_into().unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.node_identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(
        admission.is_some(),
        "client claim requires verified admission"
    );
    let mut lifecycle =
        RequestLifecycle::new_with_node_did(db.node.clone(), request, 60);
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        db.node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let failure = anthropic_rejected();
    let request_doc = lifecycle.request().doc_id.clone();
    let input = json!({
        "call_id": "call-1", "request_id": "request-1", "request_doc_id": request_doc,
        "session_id": "session-1", "node_did": node_did, "agent_did": agent.did(),
        "call_seq": 1, "backend_id": b_backend, "call_kind": "inference", "attempt": 1,
        "call_state": "failed", "failure_reason": failure, "queued_at": at,
        "started_at": at, "ended_at": at,
    });
    let session_doc = create.session_doc_id.as_deref().expect("reserved session");
    agent_access.transact_write("test.blocked_turn.call", |txn| {
        let input = &input;
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: InferenceCallMutationInputArg!) { create_InferenceCall(input: $input) { _docID } }",
                &json!({"input": input}),
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "InferenceCall")?;
            txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    lifecycle
        .terminalize_owned(
            crate::request::lifecycle::RequestTerminalOutcome::Failed,
            gents_protocol::output::TerminalOutput::NoMessage,
            Some(&failure),
        )
        .await
        .unwrap();
    drop(lifecycle);
    (db, user, access, agents)
}

#[tokio::test]
async fn wrappers_read_the_stored_turn() {
    let (db, _user, access, agents) = embedded().await;
    let node_did = db.node_identity.did();
    let turn = blocked_turn(&access, node_did, "request-1")
        .await
        .unwrap()
        .expect("blocked request");
    assert_eq!(turn.reason, BlockedReason::UsageLimit);
    assert_eq!(turn.account, label_b());
    assert_eq!(turn.profile.as_deref(), Some("main"));
    assert_eq!(turn.agents_on_profile, agents);
    assert_eq!(turn.resets_at, Utc.timestamp_opt(1_790_354_400, 0).single());

    crate::goal::set_goal_from_access(
        &access,
        node_did,
        "session-1",
        Some("ship it"),
        Some(crate::goal::GoalStatus::UsageLimited),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        blocked_goal_turn(&access, node_did, "session-1")
            .await
            .unwrap(),
        Some(turn)
    );
    assert!(blocked_turn(&access, node_did, "request-unknown")
        .await
        .is_err());
    db.node.shutdown().await.expect("node shutdown");
}
