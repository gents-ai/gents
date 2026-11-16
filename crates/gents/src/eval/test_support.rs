use crate::eval::documents::*;
use crate::eval::{OutcomeKind, ProviderReason, TAXONOMY_VERSION};
use gents_documents::{EvalSplit, EvalTier};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use std::sync::Arc;

use serde_json::json;

use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_store::identity::NodeIdentity;

pub(crate) struct HeldEvalFixture {
    pub(crate) access: ConfigAccess,
    pub(crate) node_did: String,
    pub(crate) user_did: String,
    pub(crate) other_did: String,
    _node: Arc<crate::defra_node::Node>,
    _user: gents_store::identity::KeyIdentity,
    _other: gents_store::identity::KeyIdentity,
    _runtime: gents_store::identity::KeyIdentity,
    _keys: tempfile::TempDir,
}

impl HeldEvalFixture {
    pub(crate) async fn new() -> anyhow::Result<Self> {
        let keys = tempfile::tempdir()?;
        let user =
            gents_store::identity::KeyIdentity::load_or_create(keys.path().join("user.key"), None)?;
        let runtime = gents_store::identity::KeyIdentity::load_or_create(
            keys.path().join("runtime.key"),
            None,
        )?;
        let other = gents_store::identity::KeyIdentity::load_or_create(
            keys.path().join("other.key"),
            None,
        )?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let node = Arc::new(
            crate::defra_node::Node::builder()
                .enable_signing()
                .with_node_identity_did(runtime.did())
                .with_http(crate::defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await?,
        );
        gents_store::schema::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        );
        let input = json!({"input": {"node_did": runtime.did()}});
        access
            .transact_write("test.eval.held_user_node", |txn| {
                let input = &input;
                Box::pin(async move {
                    txn.execute_client_signed_create(
                        "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                        input,
                        gents_protocol::schemas::NODE,
                    )
                    .await?;
                    Ok::<(), anyhow::Error>(())
                })
            })
            .await?;
        Ok(Self {
            access,
            node_did: runtime.did().to_owned(),
            user_did: user.did().to_owned(),
            other_did: other.did().to_owned(),
            _node: node,
            _user: user,
            _other: other,
            _runtime: runtime,
            _keys: keys,
        })
    }
}

pub(crate) fn origin(split: EvalSplit) -> RunOrigin {
    RunOrigin {
        definition: DefinitionRef {
            definition_id: "monitor-findings".into(),
            comparability_version: 1,
            digest: "sha256:def".into(),
        },
        split,
        case_ids: vec!["disk-warning".into()],
        cells: vec![CellSpec {
            cell_id: "baseline".into(),
            label: "baseline".into(),
            subject: SubjectRef {
                pack_digest: "sha256:pack".into(),
                agent_slot: "monitor".into(),
            },
            inference_profile_id: "local".into(),
        }],
        trials_per_case: 2,
        seed_base: 1000,
        deadline_secs: Some(600),
        concurrency: 1,
        denominator_policy: DENOMINATOR_POLICY_V1.into(),
        taxonomy_version: TAXONOMY_VERSION.into(),
        max_infra_retries: 1,
        check_registry_version: "checks-1".into(),
        source_commit: "0deb7659c".into(),
        source_dirty: false,
        purpose: "eval".into(),
        breaker_threshold: default_breaker_threshold(),
    }
}

pub(crate) fn identity(run_id: &str, trial_id: &str) -> TrialIdentity {
    TrialIdentity {
        trial_id: trial_id.into(),
        run_id: run_id.into(),
        cell_id: "baseline".into(),
        case_id: "disk-warning".into(),
        trial_index: 0,
        attempt: 0,
        trial_node_did: "did:key:trial".into(),
        session_id: format!("session-{trial_id}"),
        seed: 1000,
        home_hint: Some("/tmp/trial".into()),
    }
}

pub(crate) fn completion() -> TrialCompletion {
    TrialCompletion {
        ended_at: "2026-09-21T00:00:00Z".into(),
        stages: vec![StageCompletion {
            stage_id: "check".into(),
            request_id: Some("req-1".into()),
            terminal_state: Some(RequestLifecycleState::Completed),
            failure_kind: None,
            provider_reason: None,
        }],
        usage: TrialUsage {
            input_tokens: Some(10),
            output_tokens: None,
        },
        anchor: Anchor {
            terminal_states: vec![RequestLifecycleState::Completed],
            requests: 1,
            inference_calls: 3,
        },
        evidence_digest: None,
    }
}

pub(crate) fn rival_completion() -> TrialCompletion {
    TrialCompletion {
        ended_at: "2026-09-22T11:22:33Z".into(),
        stages: vec![StageCompletion {
            stage_id: "other".into(),
            request_id: Some("req-2".into()),
            terminal_state: Some(RequestLifecycleState::Failed),
            failure_kind: Some(OutcomeKind::Provider),
            provider_reason: Some(ProviderReason::Unavailable),
        }],
        usage: TrialUsage {
            input_tokens: Some(99),
            output_tokens: Some(7),
        },
        anchor: Anchor {
            terminal_states: vec![RequestLifecycleState::Failed],
            requests: 4,
            inference_calls: 5,
        },
        evidence_digest: Some("sha256:rival".into()),
    }
}

pub(crate) fn draft(
    run_id: &str,
    trial_id: &str,
    verdict_id: &str,
    feedback: Option<&str>,
) -> VerdictDraft {
    VerdictDraft {
        verdict_id: verdict_id.into(),
        run_id: run_id.into(),
        trial_id: trial_id.into(),
        stage_id: "check".into(),
        check: "mailbox_findings".into(),
        check_version: "1".into(),
        tier: EvalTier::Acceptance,
        kind: OutcomeKind::Passed,
        provider_reason: None,
        score_bp: Some(10_000),
        weight: 1,
        raw: serde_json::json!({"detail": "ok"}),
        feedback: feedback.map(str::to_owned),
        regrade_of: None,
    }
}
