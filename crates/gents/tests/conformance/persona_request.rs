//! Conformance fence for the PersonaRequest lifecycle model
//! (`Proofs/PeerRegistryDiscovery/PersonaRequest.lean`).
//!
//! Shared-table style (mirrors `bearer_claim.rs`): each Rust test is tagged
//! with the Lean theorem it fences and exercises the two pure cores the model
//! abstracts — `decide_persona_request` (Lean `admits`) and
//! `apply_persona_request` (Lean `applyStep`). No JSON snapshot / lean-contract
//! loader is pulled in, so this module carries no `lake` dependency.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use gents::agent::persona_ops::{
    apply_persona_request, decide_persona_request, BehaviorRef, PersonaCatalogView, PersonaOp,
    PersonaRequestDoc, PersonaVerdict,
};
use gents::agent::persona_presets;
use gents::defra_node::EmbeddedNode;
use gents::{
    ensure_runtime_schemas, list_agent_behaviors, load_agent_behavior, load_tool_selection,
    upsert_agent_behavior, upsert_tool_selection, AgentBehaviorDocument, ToolSelectionDocument,
};

fn catalog_with(
    models: &[&str],
    roots: &[&str],
    profiles: &[&str],
    behaviors: &[(&str, bool, &str)],
) -> PersonaCatalogView {
    PersonaCatalogView {
        available_models: models
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        allowed_roots: roots.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        available_profile_ids: profiles
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        known_agent_dids: BTreeSet::from(["did:key:agent".to_string()]),
        behaviors: behaviors
            .iter()
            .map(|(id, enabled, selection_id)| {
                (
                    id.to_string(),
                    BehaviorRef {
                        enabled: *enabled,
                        tool_selection_id: selection_id.to_string(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    }
}

fn base_catalog() -> PersonaCatalogView {
    catalog_with(
        &["openai|gpt-5"],
        &["/workspace/root"],
        &["profile-1"],
        &[
            ("existing-enabled", true, "sel-existing-enabled"),
            ("existing-disabled", false, "sel-existing-disabled"),
            ("existing-selectionless", true, ""),
        ],
    )
}

fn create_doc(op: PersonaOp) -> PersonaRequestDoc {
    PersonaRequestDoc {
        request_key: "req-1".to_string(),
        requester_did: "did:key:requester".to_string(),
        agent_did: "did:key:agent".to_string(),
        authority_kind: gents_protocol::persona::PERSONA_AUTHORITY_ENROLLMENT.to_string(),
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
        current_enrollment_authorized: true,
        op_raw: "create".to_string(),
        op: Some(op),
        persona_name: Some("Research Assistant".to_string()),
        backend_model: Some("openai|gpt-5".to_string()),
        root: None,
        preset: Some(persona_presets::PRESET_WRITE.to_string()),
        profile_id: Some("profile-1".to_string()),
        ..Default::default()
    }
}

async fn build_node(tempdir: &tempfile::TempDir) -> Arc<EmbeddedNode> {
    let node = EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("embedded node boots");
    ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas register");
    Arc::new(node)
}

/// Mirrors Lean `admits`, `pending_request_grants_nothing`, and
/// `rejected_changes_nothing`: every admission conjunct in the model gates the
/// same request the Rust `decide_persona_request` gate does. The Admit rows
/// witness the `admits`-true branch; the Reject rows witness that an
/// inadmissible request never reaches `applyStep` (its state is left
/// unchanged) — the Rust contract only calls `apply_persona_request` on Admit.
#[test]
fn admission_matrix_mirrors_lean_admits() {
    let cat = base_catalog();

    // Admit branch (Lean `admits` = true): create/clone/edit/disable happy paths.
    let happy_create = create_doc(PersonaOp::Create { clone_from: None });
    assert_eq!(
        decide_persona_request(&happy_create, &cat),
        PersonaVerdict::Admit
    );

    let mut happy_clone = create_doc(PersonaOp::Create {
        clone_from: Some("existing-enabled".to_string()),
    });
    happy_clone.preset = None;
    assert_eq!(
        decide_persona_request(&happy_clone, &cat),
        PersonaVerdict::Admit
    );

    let mut happy_edit = create_doc(PersonaOp::Edit);
    happy_edit.op_raw = "edit".to_string();
    happy_edit.behavior_id = Some("existing-enabled".to_string());
    assert_eq!(
        decide_persona_request(&happy_edit, &cat),
        PersonaVerdict::Admit
    );

    let happy_disable = PersonaRequestDoc {
        agent_did: "did:key:agent".to_string(),
        authority_kind: gents_protocol::persona::PERSONA_AUTHORITY_ENROLLMENT.to_string(),
        op_raw: "disable".to_string(),
        op: Some(PersonaOp::Disable),
        behavior_id: Some("existing-enabled".to_string()),
        current_enrollment_authorized: true,
        ..Default::default()
    };
    assert_eq!(
        decide_persona_request(&happy_disable, &cat),
        PersonaVerdict::Admit
    );

    // Reject branch (Lean `admits` = false → `applyStep` is a no-op): one row
    // per failing conjunct.
    let mut rejects: Vec<PersonaRequestDoc> = Vec::new();

    // Malformed op (Lean: no matching `Op`).
    rejects.push(PersonaRequestDoc {
        op_raw: "yeet".to_string(),
        op: None,
        ..Default::default()
    });
    // agentOk: the request's agent_did must name a known enabled principal
    // (mirrors Lean `unknown_agent_changes_nothing`) — for every op.
    let mut phantom_create = create_doc(PersonaOp::Create { clone_from: None });
    phantom_create.agent_did = "did:key:phantom".to_string();
    rejects.push(phantom_create);
    let mut phantom_disable = create_doc(PersonaOp::Disable);
    phantom_disable.op_raw = "disable".to_string();
    phantom_disable.agent_did = "did:key:phantom".to_string();
    phantom_disable.behavior_id = Some("existing-enabled".to_string());
    rejects.push(phantom_disable);
    // modelOk.
    let mut bad_model = create_doc(PersonaOp::Create { clone_from: None });
    bad_model.backend_model = Some("nope|nope".to_string());
    rejects.push(bad_model);
    // rootOk.
    let mut bad_root = create_doc(PersonaOp::Create { clone_from: None });
    bad_root.root = Some("/not/allowed".to_string());
    rejects.push(bad_root);
    // profileOk.
    let mut bad_profile = create_doc(PersonaOp::Create { clone_from: None });
    bad_profile.profile_id = Some("no-such-profile".to_string());
    rejects.push(bad_profile);
    // nameOk.
    let mut bad_name = create_doc(PersonaOp::Create { clone_from: None });
    bad_name.persona_name = Some(String::new());
    rejects.push(bad_name);
    // createModeOk: clone must omit preset.
    let mut clone_with_preset = create_doc(PersonaOp::Create {
        clone_from: Some("existing-enabled".to_string()),
    });
    clone_with_preset.preset = Some(persona_presets::PRESET_WRITE.to_string());
    rejects.push(clone_with_preset);
    // cloneOk: unknown clone source.
    let mut unknown_clone = create_doc(PersonaOp::Create {
        clone_from: Some("no-such-behavior".to_string()),
    });
    unknown_clone.preset = None;
    rejects.push(unknown_clone);
    // cloneOk: source must be ENABLED (folded conjunct).
    let mut disabled_clone = create_doc(PersonaOp::Create {
        clone_from: Some("existing-disabled".to_string()),
    });
    disabled_clone.preset = None;
    rejects.push(disabled_clone);
    // cloneOk (folded below the model's abstraction): the source must carry
    // a tool selection to copy, else an admitted clone would wedge in apply.
    let mut selectionless_clone = create_doc(PersonaOp::Create {
        clone_from: Some("existing-selectionless".to_string()),
    });
    selectionless_clone.preset = None;
    rejects.push(selectionless_clone);
    // editPresetOk (folded below the model's abstraction): keeping the
    // current selection requires the target to actually have one.
    let mut selectionless_edit = create_doc(PersonaOp::Edit);
    selectionless_edit.op_raw = "edit".to_string();
    selectionless_edit.behavior_id = Some("existing-selectionless".to_string());
    selectionless_edit.preset = None;
    rejects.push(selectionless_edit);
    // presetCreateOk: unknown preset name (folded conjunct).
    let mut unknown_preset = create_doc(PersonaOp::Create { clone_from: None });
    unknown_preset.preset = Some("bogus".to_string());
    rejects.push(unknown_preset);
    // behaviorPresent (edit).
    let mut edit_missing = create_doc(PersonaOp::Edit);
    edit_missing.op_raw = "edit".to_string();
    edit_missing.behavior_id = Some("no-such-behavior".to_string());
    rejects.push(edit_missing);
    // behaviorPresent (disable).
    rejects.push(PersonaRequestDoc {
        agent_did: "did:key:agent".to_string(),
        op_raw: "disable".to_string(),
        op: Some(PersonaOp::Disable),
        behavior_id: Some("no-such-behavior".to_string()),
        ..Default::default()
    });

    for mut doc in rejects {
        // Isolate each legacy admission conjunct from the new Lean-modeled
        // enrollment authorization conjunct.
        doc.current_enrollment_authorized = true;
        assert!(
            matches!(
                decide_persona_request(&doc, &cat),
                PersonaVerdict::Reject(_)
            ),
            "expected Reject for {:?}",
            doc.op_raw
        );
    }

    let mut stale_authorization = create_doc(PersonaOp::Create { clone_from: None });
    stale_authorization.current_enrollment_authorized = false;
    assert_eq!(
        decide_persona_request(&stale_authorization, &cat),
        PersonaVerdict::Reject(
            "persona request has no exact current enrollment authorization".to_string()
        )
    );

    let mut valid_local = create_doc(PersonaOp::Create { clone_from: None });
    valid_local.authority_kind = gents_protocol::persona::PERSONA_AUTHORITY_LOCAL_SELF.to_string();
    valid_local.current_enrollment_authorized = false;
    valid_local.requester_did = valid_local.agent_did.clone();
    valid_local.local_signer_did = valid_local.agent_did.clone();
    valid_local.local_signature_valid = true;
    assert_eq!(
        decide_persona_request(&valid_local, &cat),
        PersonaVerdict::Admit
    );

    let mut unsigned_local = valid_local.clone();
    unsigned_local.local_signature_valid = false;
    assert!(matches!(
        decide_persona_request(&unsigned_local, &cat),
        PersonaVerdict::Reject(_)
    ));

    let mut cross_branch = valid_local;
    cross_branch.authority_kind = gents_protocol::persona::PERSONA_AUTHORITY_ENROLLMENT.to_string();
    assert!(matches!(
        decide_persona_request(&cross_branch, &cat),
        PersonaVerdict::Reject(_)
    ));
}

/// Mirrors Lean `admitted_create_mints_wellformed`: an admitted create mints an
/// ENABLED behavior, backed by the fresh `sel-{request_key}` selection, with
/// the admission-validated inference profile stamped (never null).
#[tokio::test]
async fn create_mints_wellformed_mirrors_lean() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let node = build_node(&tempdir).await;

    let doc = PersonaRequestDoc {
        request_key: "req-create-1".to_string(),
        agent_did: "did:key:create-agent".to_string(),
        op_raw: "create".to_string(),
        op: Some(PersonaOp::Create { clone_from: None }),
        persona_name: Some("Research Assistant".to_string()),
        backend_model: Some("openai|gpt-5".to_string()),
        root: Some(String::new()),
        preset: Some(persona_presets::PRESET_WRITE.to_string()),
        profile_id: Some("profile-1".to_string()),
        ..Default::default()
    };

    let outcome = apply_persona_request(&node, &doc, &PersonaCatalogView::default()).await?;
    assert!(!outcome.repaired);

    // (mintedBehaviorId, true) ∈ behaviors.
    let behavior = load_agent_behavior(&node, &outcome.behavior_id)
        .await?
        .expect("minted behavior present");
    assert!(behavior.enabled, "minted behavior must be enabled");
    // selId ∈ selections.
    assert_eq!(
        behavior.tool_selection_id,
        Some("sel-req-create-1".to_string()),
        "behavior points at the freshly minted sel-{{request_key}}"
    );
    assert!(
        load_tool_selection(&node, "sel-req-create-1")
            .await?
            .is_some(),
        "minted selection present"
    );
    // profile validated by admission is stamped, never null.
    assert_eq!(behavior.inference_profile_id, Some("profile-1".to_string()));
    Ok(())
}

/// Mirrors Lean `admitted_clone_copies_selection`: the clone's minted selection
/// is distinct from the source's, and the source selection is left present.
#[tokio::test]
async fn clone_copies_selection_mirrors_lean() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let node = build_node(&tempdir).await;

    let source_selection = ToolSelectionDocument {
        selection_id: "sel-source".to_string(),
        agent_did: "did:key:clone-agent".to_string(),
        enable_bash: Some(true),
        bash_mode: Some("ReadOnly".to_string()),
        ..Default::default()
    };
    upsert_tool_selection(&node, &source_selection).await?;
    let source_behavior = AgentBehaviorDocument {
        behavior_id: "source-behavior".to_string(),
        agent_did: "did:key:clone-agent".to_string(),
        display_name: Some("Source Persona".to_string()),
        description: None,
        summary: None,
        system_prompt: None,
        request_context_template: None,
        backend_id: None,
        model_name: None,
        tool_selection_id: Some("sel-source".to_string()),
        inference_profile_id: None,
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        created_at: None,
    };
    upsert_agent_behavior(&node, &source_behavior).await?;

    let mut catalog = PersonaCatalogView::default();
    catalog.behaviors.insert(
        "source-behavior".to_string(),
        BehaviorRef {
            enabled: true,
            tool_selection_id: "sel-source".to_string(),
        },
    );

    let doc = PersonaRequestDoc {
        request_key: "req-clone-1".to_string(),
        agent_did: "did:key:clone-agent".to_string(),
        op_raw: "create".to_string(),
        op: Some(PersonaOp::Create {
            clone_from: Some("source-behavior".to_string()),
        }),
        persona_name: Some("Cloned Persona".to_string()),
        backend_model: Some("openai|gpt-5".to_string()),
        root: None,
        preset: None,
        profile_id: Some("profile-1".to_string()),
        ..Default::default()
    };

    let outcome = apply_persona_request(&node, &doc, &catalog).await?;
    assert!(!outcome.repaired);

    let cloned = load_tool_selection(&node, "sel-req-clone-1")
        .await?
        .expect("cloned selection minted");
    let source = load_tool_selection(&node, "sel-source")
        .await?
        .expect("source selection still present");
    // clone's selection distinct from source's.
    assert_ne!(cloned.selection_id, source.selection_id);
    // source still present, unchanged.
    assert_eq!(source.bash_mode, Some("ReadOnly".to_string()));
    Ok(())
}

/// Mirrors Lean `admitted_edit_preserves_behavior_set`: an admitted edit never
/// adds or removes a behavior id — the behavior set is preserved.
#[tokio::test]
async fn edit_preserves_behavior_set_mirrors_lean() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let node = build_node(&tempdir).await;

    let selection = ToolSelectionDocument {
        selection_id: "sel-edit".to_string(),
        agent_did: "did:key:edit-agent".to_string(),
        enable_file_tools: Some(true),
        file_tools_mode: Some("ReadOnly".to_string()),
        ..Default::default()
    };
    upsert_tool_selection(&node, &selection).await?;
    let behavior = AgentBehaviorDocument {
        behavior_id: "edit-behavior".to_string(),
        agent_did: "did:key:edit-agent".to_string(),
        display_name: Some("Old Name".to_string()),
        description: None,
        summary: None,
        system_prompt: None,
        request_context_template: None,
        backend_id: Some("openai".to_string()),
        model_name: Some("gpt-5".to_string()),
        tool_selection_id: Some("sel-edit".to_string()),
        inference_profile_id: Some("profile-1".to_string()),
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        created_at: None,
    };
    upsert_agent_behavior(&node, &behavior).await?;

    let before = list_agent_behaviors(&node, "did:key:edit-agent").await?;
    assert_eq!(before.len(), 1);

    let doc = PersonaRequestDoc {
        request_key: "req-edit-1".to_string(),
        agent_did: "did:key:edit-agent".to_string(),
        op_raw: "edit".to_string(),
        op: Some(PersonaOp::Edit),
        behavior_id: Some("edit-behavior".to_string()),
        persona_name: Some("Renamed".to_string()),
        backend_model: Some("openai|gpt-5".to_string()),
        root: None,
        preset: Some(String::new()),
        profile_id: Some("profile-1".to_string()),
        ..Default::default()
    };

    apply_persona_request(&node, &doc, &PersonaCatalogView::default()).await?;

    let after = list_agent_behaviors(&node, "did:key:edit-agent").await?;
    assert_eq!(after.len(), 1, "edit must not add/remove a behavior id");
    assert_eq!(
        after[0].behavior_id, "edit-behavior",
        "the same behavior id is preserved across an edit"
    );
    Ok(())
}

/// Mirrors Lean `disable_only_flips_enabled`: disable flips only the enabled
/// flag (`(target, true)` → `(target, false)`) and leaves every other field
/// untouched.
#[tokio::test]
async fn disable_only_flips_enabled_mirrors_lean() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let node = build_node(&tempdir).await;

    let behavior = AgentBehaviorDocument {
        behavior_id: "disable-behavior".to_string(),
        agent_did: "did:key:disable-agent".to_string(),
        display_name: Some("Disable Persona".to_string()),
        description: Some("keep me".to_string()),
        summary: None,
        system_prompt: None,
        request_context_template: None,
        backend_id: Some("openai".to_string()),
        model_name: Some("gpt-5".to_string()),
        tool_selection_id: Some("sel-disable".to_string()),
        inference_profile_id: Some("profile-1".to_string()),
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        created_at: None,
    };
    upsert_agent_behavior(&node, &behavior).await?;

    let doc = PersonaRequestDoc {
        request_key: "req-disable-1".to_string(),
        agent_did: "did:key:disable-agent".to_string(),
        op_raw: "disable".to_string(),
        op: Some(PersonaOp::Disable),
        behavior_id: Some("disable-behavior".to_string()),
        ..Default::default()
    };

    apply_persona_request(&node, &doc, &PersonaCatalogView::default()).await?;

    let after = load_agent_behavior(&node, "disable-behavior")
        .await?
        .expect("behavior still present");
    // flag flipped:
    assert!(!after.enabled);
    // only the flag: every other field preserved.
    assert_eq!(after.display_name, Some("Disable Persona".to_string()));
    assert_eq!(after.description, Some("keep me".to_string()));
    assert_eq!(after.tool_selection_id, Some("sel-disable".to_string()));
    assert_eq!(after.backend_id, Some("openai".to_string()));
    assert_eq!(after.model_name, Some("gpt-5".to_string()));
    assert_eq!(after.inference_profile_id, Some("profile-1".to_string()));
    Ok(())
}

/// Mirrors Lean `applyStep_idempotent`: replaying an already-applied request
/// converges — the second apply is a repair no-op and mints no duplicate.
#[tokio::test]
async fn reprocessing_is_idempotent_mirrors_lean() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let node = build_node(&tempdir).await;

    let doc = PersonaRequestDoc {
        request_key: "req-idem-1".to_string(),
        agent_did: "did:key:idem-agent".to_string(),
        op_raw: "create".to_string(),
        op: Some(PersonaOp::Create { clone_from: None }),
        persona_name: Some("Idempotent Persona".to_string()),
        backend_model: Some("openai|gpt-5".to_string()),
        root: None,
        preset: Some(persona_presets::PRESET_READONLY.to_string()),
        profile_id: Some("profile-1".to_string()),
        ..Default::default()
    };

    let first = apply_persona_request(&node, &doc, &PersonaCatalogView::default()).await?;
    assert!(!first.repaired);

    // A reconciler retry reloads the catalog reflecting the just-created row.
    let behaviors = list_agent_behaviors(&node, &doc.agent_did).await?;
    let mut catalog_after = PersonaCatalogView::default();
    for behavior in &behaviors {
        catalog_after.behaviors.insert(
            behavior.behavior_id.clone(),
            BehaviorRef {
                enabled: behavior.enabled,
                tool_selection_id: behavior.tool_selection_id.clone().unwrap_or_default(),
            },
        );
    }

    let second = apply_persona_request(&node, &doc, &catalog_after).await?;
    assert!(second.repaired, "second apply converges as a repair no-op");
    assert_eq!(second.behavior_id, first.behavior_id);

    let after = list_agent_behaviors(&node, &doc.agent_did).await?;
    assert_eq!(after.len(), 1, "idempotent reprocessing mints no duplicate");
    Ok(())
}

/// Mirrors Lean `applyStep_ownership_safe` and
/// `admitted_edit_with_preset_mints_selection`: a named-preset edit mints the
/// fresh `sel-{request_key}` selection and repoints the behavior to it, and
/// request processing never touches an operator-authored selection — the
/// operator's original selection stays byte-for-byte intact.
#[tokio::test]
async fn ownership_safe_operator_selection_untouched_mirrors_lean() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let node = build_node(&tempdir).await;

    // An operator-authored selection the behavior currently points at.
    let operator_selection = ToolSelectionDocument {
        selection_id: "operator-sel".to_string(),
        agent_did: "did:key:own-agent".to_string(),
        enable_file_tools: Some(true),
        file_tools_mode: Some("ReadWrite".to_string()),
        enable_bash: Some(true),
        bash_mode: Some("Unrestricted".to_string()),
        enable_memory: Some(true),
        ..Default::default()
    };
    upsert_tool_selection(&node, &operator_selection).await?;
    let behavior = AgentBehaviorDocument {
        behavior_id: "own-behavior".to_string(),
        agent_did: "did:key:own-agent".to_string(),
        display_name: Some("Owner Persona".to_string()),
        description: None,
        summary: None,
        system_prompt: None,
        request_context_template: None,
        backend_id: Some("openai".to_string()),
        model_name: Some("gpt-5".to_string()),
        tool_selection_id: Some("operator-sel".to_string()),
        inference_profile_id: Some("profile-1".to_string()),
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        created_at: None,
    };
    upsert_agent_behavior(&node, &behavior).await?;

    // A request-driven edit with a NAMED preset mints a NEW selection and
    // repoints the behavior to it.
    let doc = PersonaRequestDoc {
        request_key: "req-own-1".to_string(),
        agent_did: "did:key:own-agent".to_string(),
        op_raw: "edit".to_string(),
        op: Some(PersonaOp::Edit),
        behavior_id: Some("own-behavior".to_string()),
        persona_name: Some("Owner Persona".to_string()),
        backend_model: Some("openai|gpt-5".to_string()),
        root: None,
        preset: Some(persona_presets::PRESET_READONLY.to_string()),
        profile_id: Some("profile-1".to_string()),
        ..Default::default()
    };

    apply_persona_request(&node, &doc, &PersonaCatalogView::default()).await?;

    // The behavior repoints to the freshly minted selection...
    let repointed = load_agent_behavior(&node, "own-behavior")
        .await?
        .expect("behavior present");
    assert_eq!(
        repointed.tool_selection_id,
        Some("sel-req-own-1".to_string())
    );

    // ...but the operator-authored selection is untouched.
    let operator_after = load_tool_selection(&node, "operator-sel")
        .await?
        .expect("operator selection still present");
    assert_eq!(
        operator_after.file_tools_mode,
        Some("ReadWrite".to_string())
    );
    assert_eq!(operator_after.bash_mode, Some("Unrestricted".to_string()));
    assert_eq!(operator_after.enable_memory, Some(true));
    Ok(())
}
