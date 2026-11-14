use gents_loop::tool::ToolDyn;
use gents_toolcalls::test_support::self_config::{config, SelfConfigFixture};
use gents_tools::builtin::self_config::ops::ApplyRequest;
use gents_tools::builtin::self_config::*;
use serde_json::json;
use std::sync::Arc;

pub async fn account_choice_core(name: &str) -> (SelfConfigFixture, SelfConfigCore) {
    let fixture = crate::test_support::self_config_fixture(name).await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    fixture.grant_user_relation_delete().await;
    fixture.grant_agent_relation_delete().await;
    let owner = fixture.node_did().to_owned();
    let agent = fixture.agent_did().to_owned();
    let access = fixture.user_access();
    for (id, kind, auth) in [
        (
            "chat-original",
            "ChatGptCodex",
            json!({"kind":"node_oauth"}),
        ),
        (
            "chat-b",
            "ChatGptCodex",
            json!({"kind":"node_oauth","account_ref":"acct-b"}),
        ),
        (
            "grok-original",
            "XaiGrokOAuth",
            json!({"kind":"node_oauth"}),
        ),
    ] {
        let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner, "backend_id": id, "name": id, "provider_kind": kind,
            "endpoint": "http://127.0.0.1:1/v1", "auth": auth,
        }))
        .unwrap();
        gents_documents::write_inference_backend_document(
            access,
            &backend,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }
    for (id, backend) in [
        (format!("{agent}:inference"), "chat-original"),
        ("p-original".into(), "chat-original"),
        ("p-chat-b".into(), "chat-b"),
        ("p-grok".into(), "grok-original"),
    ] {
        let profile: gents_documents::InferenceProfile = serde_json::from_value(json!({
            "node_did": owner, "profile_id": id, "backend_id": backend,
            "model_name": "test-model",
        }))
        .unwrap();
        gents_documents::write_inference_profile_document(
            access,
            &profile,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }
    let compaction = format!("{agent}:compaction");
    let plan = gents_documents::DesiredStateApplyPlan::new(
        [
            (
                gents_store::collection::Collection::Compaction,
                json!({"node_did": owner, "compaction_id": compaction}),
            ),
            (
                gents_store::collection::Collection::AgentContext,
                json!({"node_did": owner, "context_id": format!("{agent}:context"),
                    "tools_id": format!("{agent}:tools"), "compaction_id": compaction}),
            ),
        ]
        .into_iter()
        .chain(["original", "chat-b"].into_iter().flat_map(|account| {
            let compaction = format!("c-{account}");
            [
                (
                    gents_store::collection::Collection::Compaction,
                    json!({"node_did": owner, "compaction_id": compaction,
                        "inference_profile_id": format!("p-{account}")}),
                ),
                (
                    gents_store::collection::Collection::AgentContext,
                    json!({"node_did": owner, "context_id": format!("ctx-{account}"),
                        "compaction_id": compaction}),
                ),
            ]
        }))
        .map(
            |(collection, value)| gents_documents::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )
    .unwrap();
    access
        .transact_write("test.compaction", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    fixture.grant_selected_configuration().await;
    let mut selections = vec![(
        gents_store::collection::Collection::Compaction,
        compaction,
        true,
    )];
    selections.push((
        gents_store::collection::Collection::AgentContext,
        format!("{agent}:context"),
        false,
    ));
    selections.extend(["p-original", "p-chat-b", "p-grok"].into_iter().map(|id| {
        (
            gents_store::collection::Collection::InferenceProfile,
            id.to_owned(),
            false,
        )
    }));
    selections.extend(["c-original", "c-chat-b"].into_iter().map(|id| {
        (
            gents_store::collection::Collection::Compaction,
            id.to_owned(),
            false,
        )
    }));
    selections.extend(["ctx-original", "ctx-chat-b"].into_iter().map(|id| {
        (
            gents_store::collection::Collection::AgentContext,
            id.to_owned(),
            false,
        )
    }));
    selections.extend(
        ["chat-original", "chat-b", "grok-original"]
            .into_iter()
            .map(|id| {
                (
                    gents_store::collection::Collection::InferenceBackend,
                    id.to_owned(),
                    false,
                )
            }),
    );
    fixture.grant_account_fixture_selections(&selections).await;
    let core = fixture.core();
    (fixture, core)
}

pub async fn assert_account_choice_refused(
    core: &SelfConfigCore,
    request: impl Fn() -> ApplyRequest<'static>,
) {
    for result in [core.preview(request()).await, core.apply(request()).await] {
        let error = result.expect_err("switching to another account must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
}

pub async fn grok_accounts_core(
    agent: &str,
    original: bool,
    g2: bool,
) -> (SelfConfigFixture, SelfConfigCore) {
    let (fixture, core) = account_choice_core(agent).await;
    let owner = core.node_did().to_owned();
    let access = fixture.user_access();
    let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
        "node_did": owner, "backend_id": "grok-g2", "name": "grok-g2",
        "provider_kind": "XaiGrokOAuth", "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind":"node_oauth","account_ref":"g2"},
    }))
    .unwrap();
    gents_documents::write_inference_backend_document(
        access,
        &backend,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let profile: gents_documents::InferenceProfile = serde_json::from_value(json!({
        "node_did": owner, "profile_id": "p-grok-g2", "backend_id": "grok-g2",
        "model_name": "test-model",
    }))
    .unwrap();
    gents_documents::write_inference_profile_document(
        access,
        &profile,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    fixture
        .grant_account_fixture_selections(&[
            (
                gents_store::collection::Collection::InferenceBackend,
                "grok-g2".into(),
                false,
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                "p-grok-g2".into(),
                false,
            ),
        ])
        .await;
    let provider = gents_inference::clients::xai_grok::XAI_OAUTH_PROVIDER;
    for (account_ref, enabled) in [(None, original), (Some("g2"), g2)] {
        let original_id = gents_inference::auth::credential::oauth_credential_id(&owner, provider);
        let credential = gents_inference::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: match account_ref {
                Some(account_ref) => format!("{original_id}:{account_ref}"),
                None => original_id,
            },
            node_did: owner.clone(),
            provider: provider.to_string(),
            access_token: "access-TEST".into(),
            refresh_token: "refresh-TEST".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled,
            account_ref: account_ref.map(str::to_string),
            connected_at: account_ref.map(|_| chrono::Utc::now()),
            provider_account_key: None,
            label: None,
        };
        gents_inference::auth::credential::upsert_oauth_credential_on(access, &credential)
            .await
            .unwrap();
    }
    (fixture, core)
}

pub async fn signed_pack_tool(
    label: &str,
    plugins: Arc<gents_toolcalls::plugin::executor::PluginExecutor>,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = crate::test_support::self_config_fixture(label).await;
    fixture
        .user_access()
        .transact_write("test.self_config.engineer_init_grants", |txn| {
            let node = fixture.node_did();
            let agent = fixture.agent_did();
            Box::pin(async move { gents_documents::provision_engineer(txn, node, agent).await })
        })
        .await
        .expect("User grants the default Agent what init grants the Engineer");
    fixture.grant_pack_install().await;
    fixture.grant_agent_schema().await;
    let mut tool_config = config(&[]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = true;
    let tools = fixture.tools_with_plugins(&tool_config, plugins);
    (fixture, tools)
}
