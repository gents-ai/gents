use std::sync::Arc;

use gents_inference::llm::ToolCallHookAction;
use gents_store::identity::NodeIdentity;
use serde_json::json;

use gents_toolcalls::call::hook::fixtures::*;
use gents_toolcalls::call::hook::*;

use gents_loop::SessionHook;
use gents_toolcalls::test_support::script_composition::*;
/// A script whose `agent_new` effect started a background child: the
/// fixture, the child's opening scope, the starter's scope, the receipt and
/// the effect's and the script's documents.
struct ComposedAgentNew {
    fixture: gents_toolcalls::test_support::background_hook::BudgetHookFixture,
    target: gents_protocol::session::SessionScope,
    parent: gents_protocol::session::SessionScope,
    receipt: serde_json::Value,
    /// The effect's native tool call ID.
    child: String,
    effect_doc: String,
    script_doc: String,
}

async fn composed_agent_new(name: &str) -> ComposedAgentNew {
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        name,
        Default::default(),
        Default::default(),
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    gents_toolcalls::test_support::enable_agents_tools_with_access(
        &fixture.owner,
        fixture.db.node_identity.did(),
        fixture.agent.did(),
    )
    .await;
    let hook = &fixture.hook;
    accept_hook_tool_call(hook, "script", "plugin", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("plugin", None, "script", "{}").await,
        ToolCallHookAction::Continue
    ));
    let name = gents_protocol::tool_names::AGENT_NEW_TOOL_NAME;
    let args = json!({"agent":"general", "prompt":"work"}).to_string();
    let child = hook
        .admit_tool_effect("script", 1, name, &args)
        .await
        .unwrap();
    let ToolCallHookAction::Skip { reason } = hook.on_tool_call(name, None, &child, &args).await
    else {
        panic!("agent_new did not return a receipt")
    };
    let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
    assert_eq!(receipt["ok"], true, "{reason}");
    hook.finish_tool_effect(&child, None).await.unwrap();
    let row = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        &child,
    )
    .await;
    assert_eq!(row["lifecycle_state"], "running");
    assert_eq!(row["await_mode"], "background");
    let result = gents_session::tool_call::query::result::load_tool_call_result(
        &fixture.access,
        row["_docID"].as_str().unwrap(),
        fixture.db.node_identity.did(),
        &fixture.session_id,
        Some(fixture.user.did()),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &gents_session::tool_call::query::result::render_tool_result(&result).unwrap()
        )
        .unwrap(),
        receipt
    );
    let target_session = receipt["session_id"].as_str().unwrap();
    let request_doc_id = receipt["request_doc_id"].as_str().unwrap();
    let opening = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &fixture.access,
        request_doc_id,
    )
    .await
    .unwrap()
    .expect("child opening request visible");
    let parent = gents_protocol::session::SessionScope {
        node_did: fixture.db.node_identity.did().to_owned(),
        session_id: fixture.session_id.clone(),
        requester_did: Some(fixture.user.did().to_owned()),
    };
    let target = gents_protocol::session::SessionScope::of_request(&opening);
    assert_eq!(target.session_id, target_session);
    let verified = gents_session::session::origin::verified_agent_new_creation(
        &fixture.access,
        &target,
        &parent,
    )
    .await
    .unwrap();
    assert_eq!(
        verified,
        Ok(gents_session::session::origin::VerifiedCreation::CallResult),
        "effect receipt resolves, but signed starter verification failed: receipt={receipt}"
    );
    // The same valid receipt, with the effect's parent (the script call)
    // withheld by the loader: missing, then unreadable. Each is denied by name.
    let script_doc = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        "script",
    )
    .await["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    ComposedAgentNew {
        effect_doc: row["_docID"].as_str().unwrap().to_owned(),
        child,
        fixture,
        target,
        parent,
        receipt,
        script_doc,
    }
}

/// The effect's lifecycle, loaded to settle it as its executor would.
async fn effect_lifecycle(
    composed: &ComposedAgentNew,
) -> gents_toolcalls::call::lifecycle::ToolCallLifecycle {
    let fixture = &composed.fixture;
    fixture
        .access
        .transact_readonly("test.effect.load", |txn| {
            let node = fixture.db.node.clone();
            let row_id = composed.effect_doc.clone();
            let node_did = fixture.db.node_identity.did().to_owned();
            let session = fixture.session_id.clone();
            let requester = fixture.user.did().to_owned();
            Box::pin(async move {
                let mut lifecycle =
                    gents_toolcalls::call::lifecycle::ToolCallLifecycle::load_by_doc_id_in_txn(
                        txn,
                        node,
                        &row_id,
                        &node_did,
                        &session,
                        Some(&requester),
                    )
                    .await?
                    .unwrap();
                lifecycle.bind_agent_signer(txn.managed_agent_signer())?;
                Ok::<_, anyhow::Error>(lifecycle)
            })
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn composed_agent_new_keeps_its_asynchronous_receipt() {
    let composed = composed_agent_new("script-agent-new").await;
    let ComposedAgentNew {
        fixture,
        target,
        parent,
        receipt,
        script_doc,
        child,
        ..
    } = &composed;
    let target_session = receipt["session_id"].as_str().unwrap();
    assert_withheld_parent_is_denied(fixture, target, parent, script_doc, &composed.effect_doc)
        .await;
    let mut lifecycle = effect_lifecycle(&composed).await;
    lifecycle
        .cancel_during_run(gents_session::session::tool_call_state::CancelCause::Interrupted)
        .await
        .unwrap();
    let cancelled = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        child,
    )
    .await;
    assert_eq!(cancelled["lifecycle_state"], "cancelled");
    assert_eq!(
        gents_session::session::origin::verified_agent_new_creation(
            &fixture.access,
            target,
            parent
        )
        .await
        .unwrap(),
        Ok(gents_session::session::origin::VerifiedCreation::EffectAcceptance),
        "starter authorization lost after effect cancellation"
    );
    // A terminal effect is denied the same way when its parent is withheld.
    assert_withheld_parent_is_denied(fixture, target, parent, script_doc, &composed.effect_doc)
        .await;
    let listed = control(
        fixture,
        "list-before-message",
        "agent_session_list",
        json!({}),
    )
    .await;
    let entry = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["session_id"] == target_session)
        .expect("child in session list");
    assert_eq!(entry["can_interrupt"], true, "list denied starter: {entry}");
    let message = control(
        fixture,
        "queue-worker",
        "agent_message",
        json!({"session_id": target_session, "message": "continue", "delivery": "queue"}),
    )
    .await;
    assert_eq!(message["ok"], true, "queued message failed: {message}");
    assert_eq!(
        message["delivery"], "request",
        "expected a new message-delivered request: {message}"
    );
    assert_ne!(message["request_id"], receipt["request_id"]);
    assert!(
        gents_session::session::origin::verified_agent_new_creation(
            &fixture.access,
            target,
            parent
        )
        .await
        .unwrap()
        .is_ok(),
        "starter verification changed after message delivery"
    );
    let listed = control(
        fixture,
        "list-after-message",
        "agent_session_list",
        json!({}),
    )
    .await;
    let entry = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["session_id"] == target_session)
        .expect("child in session list after message");
    assert_eq!(
        entry["can_interrupt"], true,
        "list denied starter after message: {entry}"
    );
    // A second signed, claimed session of the same Agent on this node owns a
    // script effect, but it did not start the target and gains no authority.
    let other_session = format!("other-{}", uuid::Uuid::new_v4());
    let other_request = format!("other-request-{}", uuid::Uuid::new_v4());
    let create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        &other_request,
        fixture.db.node_identity.did(),
        fixture.user.did(),
        fixture.agent.did(),
        &other_session,
        "other",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(fixture.user.did()),
    );
    let mut other_lifecycle =
        gents_toolcalls::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &fixture.db.node,
            &fixture.owner,
            fixture.db.node_identity.clone(),
            &fixture.user,
            fixture.agent.as_ref(),
            create,
        )
        .await
        .unwrap();
    assert_eq!(
        other_lifecycle.claim_with_identity().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    let other_hook = gents_toolcalls::call::hook::DefraSessionHook::resume_with_actor_policy(
        fixture.db.node.clone(),
        &other_session,
        fixture.db.node_identity.did(),
        Some(fixture.user.did()),
        fixture.agent.did().parse().unwrap(),
        Default::default(),
    )
    .await
    .unwrap()
    .with_managed_identity(fixture.agent.as_ref())
    .unwrap();
    gents_toolcalls::call::hook::fixtures::bind_claimed_hook_request(
        &other_hook,
        other_lifecycle,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    gents_toolcalls::call::hook::fixtures::accept_hook_tool_call(
        &other_hook,
        "other-script",
        "plugin",
        "{}",
        None,
    )
    .await;
    assert!(matches!(
        other_hook
            .on_tool_call("plugin", None, "other-script", "{}")
            .await,
        ToolCallHookAction::Continue
    ));
    other_hook
        .admit_tool_effect("other-script", 1, "agent_interrupt", "{}")
        .await
        .unwrap();
    async fn other_control(
        hook: &gents_toolcalls::call::hook::DefraSessionHook,
        id: &str,
        name: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let args = args.to_string();
        gents_toolcalls::call::hook::fixtures::accept_hook_tool_call(hook, id, name, &args, None)
            .await;
        let ToolCallHookAction::Skip { reason } = hook.on_tool_call(name, None, id, &args).await
        else {
            panic!("{name} did not return");
        };
        serde_json::from_str(&reason).unwrap()
    }
    let other_list =
        other_control(&other_hook, "other-list", "agent_session_list", json!({})).await;
    let other_entry = other_list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["session_id"] == target_session)
        .expect("visible target");
    assert_eq!(other_entry["can_interrupt"], false, "{other_entry}");
    for (id, name) in [
        ("other-interrupt", "agent_interrupt"),
        ("other-stop", "agent_stop"),
    ] {
        let denied =
            other_control(&other_hook, id, name, json!({"session_id": target_session})).await;
        assert_eq!(denied["ok"], false, "{denied}");
        assert_eq!(denied["code"], "interrupt_not_permitted", "{denied}");
    }

    let interrupted = control(
        fixture,
        "interrupt-worker",
        "agent_interrupt",
        json!({"session_id": target_session}),
    )
    .await;
    assert_eq!(interrupted["ok"], true, "interrupt rejected: {interrupted}");
    let stop = control(
        fixture,
        "stop-worker",
        "agent_stop",
        json!({"session_id":target_session}),
    )
    .await;
    assert_eq!(stop["ok"], true, "stop failed: {stop}");
    let unrelated = gents_protocol::session::SessionScope {
        session_id: "different-session".into(),
        ..parent.clone()
    };
    assert_eq!(
        gents_session::session::origin::verified_agent_new_creation(
            &fixture.access,
            target,
            &unrelated
        )
        .await
        .unwrap(),
        Err(gents_session::session::origin::CreationDenial::CallerSessionNotFound)
    );
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_a_background_script_settles_its_in_flight_child() {
    use gents_toolcalls::test_support::host_call_plugin_wat;
    let wat = host_call_plugin_wat(&json!({"tool_name":"stall", "arguments":{}}), 1);
    let (_home, tool, digest) = host_call_tool(&wat);
    let registry = BackgroundToolRegistry::from_tools(vec![tool], &["plugin".into()]);
    let executions = BackgroundExecutionRegistry::default();
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-cancel-child",
        registry,
        executions.clone(),
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let started = Arc::new(tokio::sync::Notify::new());
    fixture
        .hook
        .bind_tool_surface(Arc::new(vec![Box::new(Stall(started.clone()))]))
        .await;
    let receipt = control(
        &fixture,
        "start-script",
        "spawn_process",
        json!({"tool_name":"plugin", "args":{}}),
    )
    .await;
    let id = receipt["tool_call_id"].as_str().unwrap().to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(30), started.notified())
        .await
        .expect("the script reached its child");
    let rows = session_tool_rows(&fixture).await;
    let child = rows.iter().find(|row| row["tool_name"] == "stall").unwrap();
    assert_eq!(child["lifecycle_state"], "running", "{rows:?}");

    let cancelled = control(
        &fixture,
        "cancel-script",
        "cancel_process",
        json!({"tool_call_id": id}),
    )
    .await;
    assert_eq!(cancelled["ok"], true, "{cancelled}");
    executions.wait_for_completion(&id).await;
    guest_stopped(&digest, std::time::Duration::from_secs(2)).await;

    let rows = session_tool_rows(&fixture).await;
    let child = rows.iter().find(|row| row["tool_name"] == "stall").unwrap();
    assert_eq!(child["lifecycle_state"], "cancelled", "{rows:?}");
    assert!(
        rows.iter()
            .all(|row| !matches!(row["lifecycle_state"].as_str(), Some("pending" | "running"))),
        "every row of the stopped script is terminal: {rows:?}"
    );
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_a_background_script_stops_a_guest_that_ignores_the_host() {
    // The guest keeps calling the host and never checks the answer, so only
    // the store's revoked allowance ends it.
    let (fixture, executions, id, digest, _home) = spawn_spinner(
        "script-cancel-guest",
        true,
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let cancelled = control(
        &fixture,
        "cancel-script",
        "cancel_process",
        json!({"tool_call_id": id}),
    )
    .await;
    assert_eq!(cancelled["ok"], true, "{cancelled}");
    executions.wait_for_completion(&id).await;
    guest_stopped(&digest, std::time::Duration::from_secs(2)).await;
    let row = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        &id,
    )
    .await;
    assert_eq!(row["lifecycle_state"], "cancelled");
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_computing_background_guest_stops_at_its_declared_allowance() {
    // No host call ever reaches the store, so the declared wall clock (the
    // five second default here), not the admitted background lifetime of
    // hours, is what ends `while (true) {}`.
    let (fixture, executions, id, digest, _home) = spawn_spinner(
        "script-spin-guest",
        false,
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    assert_eq!(
        guest_allowances(&digest).await,
        [std::time::Duration::from_secs(5)],
        "the guest computes under its declared allowance"
    );
    let cancelled = control(
        &fixture,
        "cancel-script",
        "cancel_process",
        json!({"tool_call_id": id}),
    )
    .await;
    // Nothing orders the cancel before the allowance (TLA
    // `MCGuestAllowanceCancelLate`): a late cancel reports the ended process.
    assert!(
        cancelled["ok"] == true
            || matches!(cancelled["status"].as_str(), Some(status) if status != "running" && status != "pending"),
        "{cancelled}"
    );
    executions.wait_for_completion(&id).await;
    // The allowance is counted in wake-ups of the WASM host's epoch ticker,
    // which a loaded host delays without bound (Lean `allowanceTicks`), so
    // the stop is awaited, not timed.
    guest_released(&digest).await;
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_background_request_on_a_script_child_is_refused() {
    let (fixture, [_completed, _pending, running, _cancelled]) = script_with_children(
        "script-child-background-control",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let refusal = gents_toolcalls::background::status::request_foreground_backgrounding(
        &fixture.access,
        fixture.db.node_identity.did(),
        &fixture.session_id,
        &fixture.request_id,
        &running,
    )
    .await
    .expect_err("only the model's own Bash call can move to the background");
    assert!(
        format!("{refusal:#}").contains("only a direct native Bash command can be backgrounded"),
        "{refusal:#}"
    );
    let row = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        &running,
    )
    .await;
    assert_eq!(row["await_mode"], "foreground");
    assert_eq!(row["lifecycle_state"], "running");
    fixture.db.node.shutdown().await.unwrap();
}

/// What a test loader does when asked for the effect's parent.
enum Fault {
    /// No such row.
    Missing,
    /// The row was read but does not decode.
    Unreadable,
    /// The database did not answer.
    QueryFailed,
    /// The store refused the reader: permanent, not a failed query.
    PermissionDenied,
    /// Answer with another accepted call (this document) instead.
    Substitute(String),
}

/// A loader that faults on one tool call (the effect's parent) and loads every
/// other through the production loader, the parent walk included.
struct FaultyParent {
    parent_doc: String,
    fault: Fault,
    asked: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl gents_session::tool_call::query::result::ToolCallLoader for FaultyParent {
    async fn load(
        &self,
        source: gents_session::tool_call::query::result::ReadSource<'_, '_>,
        tool_call_doc_id: &str,
        scope: gents_session::tool_call::query::result::ToolCallScope<'_>,
        ancestry: &mut gents_session::tool_call::query::result::ToolCallAncestry,
    ) -> Result<
        gents_session::tool_call::query::result::AcceptedToolCall,
        gents_session::tool_call::query::result::ToolCallLoadError,
    > {
        use gents_session::tool_call::query::result::{
            load_accepted_tool_call_with, ProductionToolCallLoader, ToolCallAncestry,
            ToolCallLoadError,
        };
        if tool_call_doc_id != self.parent_doc {
            return load_accepted_tool_call_with(self, source, tool_call_doc_id, scope, ancestry)
                .await;
        }
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match &self.fault {
            Fault::Missing => Err(ToolCallLoadError::Missing(tool_call_doc_id.to_owned())),
            Fault::Unreadable => Err(ToolCallLoadError::Unreadable(anyhow::anyhow!(
                "the parent row does not decode"
            ))),
            Fault::QueryFailed => Err(ToolCallLoadError::QueryFailed(anyhow::anyhow!(
                "the database did not answer"
            ))),
            // Classified by the production row-read classifier, as the
            // identity read classifies a refusal from the store.
            Fault::PermissionDenied => Err(ToolCallLoadError::of_row_read(anyhow::Error::new(
                gents_store::config_client::ConfigClientError::Query(Box::new(
                    query::error::QueryError::PermissionDenied("not the reader's".into()),
                )),
            ))),
            Fault::Substitute(other) => {
                load_accepted_tool_call_with(
                    &ProductionToolCallLoader,
                    source,
                    other,
                    scope,
                    &mut ToolCallAncestry::default(),
                )
                .await
            }
        }
    }
}

/// With a valid creation receipt, a fault at the effect's parent decides the
/// starter check as the Lean `creationVerdict` does:
/// - a missing parent is `ParentMissing`; a corrupt one, or one the store
///   refuses the reader (permanent, so not a retry), `ParentUnreadable`;
/// - a loader answering with another call than the parent it was asked for is
///   refused (`CauseCallInvalid`);
/// - a query the database could not answer is not a denial: the check fails
///   with a retryable `CreationCheckUnavailable`.
///
/// Each denial refuses the interrupt with its reason, so the session list
/// reports `can_interrupt=false` (it is `interrupt_refusal(..).is_none()`).
async fn assert_withheld_parent_is_denied(
    fixture: &gents_toolcalls::test_support::background_hook::BudgetHookFixture,
    target: &gents_protocol::session::SessionScope,
    parent: &gents_protocol::session::SessionScope,
    parent_doc: &str,
    effect_doc: &str,
) {
    use gents_session::session::origin::CreationDenial;
    let loader = |fault| FaultyParent {
        parent_doc: parent_doc.to_owned(),
        fault,
        asked: Default::default(),
    };
    for (fault, denial) in [
        (Fault::Missing, CreationDenial::ParentMissing),
        (Fault::Unreadable, CreationDenial::ParentUnreadable),
        (Fault::PermissionDenied, CreationDenial::ParentUnreadable),
        (
            Fault::Substitute(effect_doc.to_owned()),
            CreationDenial::CauseCallInvalid,
        ),
    ] {
        let loader = loader(fault);
        let verdict = gents_session::session::origin::verified_agent_new_creation_with(
            &loader,
            &fixture.access,
            target,
            parent,
        )
        .await
        .unwrap();
        assert_eq!(verdict, Err(denial));
        let refusal = gents_session::session::message::interrupt_refusal_with(
            &loader,
            &fixture.db.node,
            fixture.agent.did(),
            parent,
            target,
        )
        .await
        .unwrap()
        .expect("interrupt refused without an accepted effect parent");
        assert!(refusal.contains(&denial.to_string()), "{refusal}");
        assert!(
            loader.asked.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "the parent was never asked for"
        );
    }
    // The substitution is refused by the walk's own identity check, not by
    // another invalid-row path: loading the effect through the substituting
    // loader names the parent asked for and the call returned.
    let substituting = loader(Fault::Substitute(effect_doc.to_owned()));
    let scope = gents_session::tool_call::query::result::ToolCallScope {
        node_did: &parent.node_did,
        session_id: &parent.session_id,
        requester_did: parent.requester_did.as_deref(),
    };
    let Err(error) = gents_session::tool_call::query::result::load_accepted_tool_call_with(
        &substituting,
        gents_session::tool_call::query::result::ReadSource::Access(&fixture.access),
        effect_doc,
        scope,
        &mut Default::default(),
    )
    .await
    else {
        panic!("a substituted parent is refused");
    };
    let invalid = matches!(
        error,
        gents_session::tool_call::query::result::ToolCallLoadError::Invalid(_)
    );
    // The whole `source()` chain: the variant's message, then the check's.
    let message = format!("{:#}", anyhow::Error::new(error));
    assert!(invalid, "{message}");
    assert!(
        message.contains(&format!(
            "the loader returned tool call {effect_doc} for parent {parent_doc}"
        )),
        "{message}"
    );
    // A failed query is a retryable error, not "you are not the starter".
    let loader = loader(Fault::QueryFailed);
    let error = gents_session::session::origin::verified_agent_new_creation_with(
        &loader,
        &fixture.access,
        target,
        parent,
    )
    .await
    .expect_err("a failed query is not a verdict");
    assert!(
        error.to_string().contains("a database read failed; retry"),
        "{error:#}"
    );
    let error = gents_session::session::message::interrupt_refusal_with(
        &loader,
        &fixture.db.node,
        fixture.agent.did(),
        parent,
        target,
    )
    .await
    .expect_err("a failed query does not refuse the interrupt");
    assert!(format!("{error:#}").contains("retry"), "{error:#}");
    // The production loader still verifies the same receipt.
    assert!(gents_session::session::message::interrupt_refusal(
        &fixture.db.node,
        fixture.agent.did(),
        parent,
        target,
    )
    .await
    .unwrap()
    .is_none());
}

/// The common live case: a background `agent_new` effect ends completed, with
/// the child's output as its result. The call's result is no longer the
/// receipt, so the effect's immutable acceptance receipt verifies the starter,
/// which may still list, interrupt and stop the child.
#[tokio::test]
async fn a_completed_agent_new_effect_keeps_its_starter() {
    let composed = composed_agent_new("script-agent-new-completed").await;
    let ComposedAgentNew {
        fixture,
        target,
        parent,
        receipt,
        child,
        effect_doc,
        script_doc,
    } = &composed;
    let target_session = receipt["session_id"].as_str().unwrap();
    let mut lifecycle = effect_lifecycle(&composed).await;
    lifecycle
        .complete("the child finished: all work done")
        .await
        .unwrap();
    let completed = gents_toolcalls::test_support::background_hook::tool_row(
        &fixture.access,
        &fixture.session_id,
        child,
    )
    .await;
    assert_eq!(completed["lifecycle_state"], "completed", "{completed}");
    assert_eq!(
        gents_session::session::origin::verified_agent_new_creation(
            &fixture.access,
            target,
            parent
        )
        .await
        .unwrap(),
        Ok(gents_session::session::origin::VerifiedCreation::EffectAcceptance),
        "starter authorization lost after the effect completed"
    );
    assert_withheld_parent_is_denied(fixture, target, parent, script_doc, effect_doc).await;
    let listed = control(fixture, "list-completed", "agent_session_list", json!({})).await;
    let entry = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["session_id"] == target_session)
        .expect("child in session list");
    assert_eq!(entry["can_interrupt"], true, "list denied starter: {entry}");
    let interrupted = control(
        fixture,
        "interrupt-completed",
        "agent_interrupt",
        json!({"session_id": target_session}),
    )
    .await;
    assert_eq!(interrupted["ok"], true, "interrupt rejected: {interrupted}");
    fixture.db.node.shutdown().await.unwrap();
}
