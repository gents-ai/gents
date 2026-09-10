//! Conformance fence for the authenticated persona command boundary
//! (`proofs/Proofs/PeerRegistryDiscovery/PersonaRequest.lean`).
//!
//! Persona requests are command DTOs, not installed configuration. The Lean
//! model admits the command against a published catalog and then resolves the
//! shared authoring loader's canonical behavior candidate through
//! `Configuration.resolveBehavior` (`materializedSession_iff`); publication
//! and idempotence belong to ApplyReconcile, not to a behavior/tools
//! materializer here. The fences below therefore drive the pure admission
//! core (`decide_persona_request`, Lean `admits`) and the preset vocabulary
//! and capability templates the composer choices validate against. The
//! candidate-resolution half of `materializedSession_iff` has no production
//! owner yet (see the conformance report) and is deliberately not faked with
//! a test-local resolver.

use std::collections::{BTreeMap, BTreeSet};

use gents::agent::persona_ops::{
    decide_persona_request, BehaviorRef, PersonaCatalogView, PersonaOp, PersonaRequestDoc,
    PersonaVerdict,
};
use gents::agent::persona_presets;

fn catalog_with(
    roots: &[&str],
    profiles: &[&str],
    behaviors: &[(&str, bool, &str)],
) -> PersonaCatalogView {
    PersonaCatalogView {
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
        ..Default::default()
    }
}

fn base_catalog() -> PersonaCatalogView {
    catalog_with(
        &["/workspace/root"],
        &["profile-1"],
        &[
            ("existing-enabled", true, "sel-existing-enabled"),
            ("existing-disabled", false, "sel-existing-disabled"),
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
        root: None,
        preset: Some(persona_presets::PRESET_WRITE.to_string()),
        profile_id: Some("profile-1".to_string()),
        ..Default::default()
    }
}

/// Mirrors Lean `admits` conjunct-for-conjunct, plus the boundary theorems
/// `unauthorized_command_resolves_nothing`,
/// `unknown_agent_resolves_nothing`, `unsigned_local_command_denied`, and
/// `cross_principal_local_command_denied`: every admission conjunct gates the
/// same request the Rust `decide_persona_request` gate does. The Admit rows
/// witness the `admits`-true branch (including disable, which the model
/// admits but never materializes a session for,
/// `disable_has_no_materialized_session`); the Reject rows witness that an
/// inadmissible command never reaches candidate resolution.
#[test]
fn admission_matrix_mirrors_lean_admits() {
    let cat = base_catalog();

    // Admit branch (Lean `admits` = true): create/clone/edit/disable happy
    // paths through enrollment authority.
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
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
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

    // Reject branch (Lean `admits` = false → no candidate resolution): one
    // row per failing conjunct.
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
    // rootOk.
    let mut bad_root = create_doc(PersonaOp::Create { clone_from: None });
    bad_root.root = Some("/not/allowed".to_string());
    rejects.push(bad_root);
    // Inference is selected once, through the required profile binding.
    let mut missing_profile = create_doc(PersonaOp::Create { clone_from: None });
    missing_profile.profile_id = None;
    rejects.push(missing_profile);
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
    // cloneOk: the source must be an ENABLED behavior — `(cloneFrom, true)`
    // membership in the behavior catalog.
    let mut disabled_clone = create_doc(PersonaOp::Create {
        clone_from: Some("existing-disabled".to_string()),
    });
    disabled_clone.preset = None;
    rejects.push(disabled_clone);
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
        authority_kind: gents_protocol::persona::PERSONA_AUTHORITY_ENROLLMENT.to_string(),
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
        op_raw: "disable".to_string(),
        op: Some(PersonaOp::Disable),
        behavior_id: Some("no-such-behavior".to_string()),
        current_enrollment_authorized: true,
        ..Default::default()
    });

    for mut doc in rejects {
        // Isolate each legacy admission conjunct from the Lean-modeled
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

    // authorizationOk (enrollment branch): the single durable authority
    // owner's exact current authorization is required; staleness fails
    // closed.
    let mut stale_authorization = create_doc(PersonaOp::Create { clone_from: None });
    stale_authorization.current_enrollment_authorized = false;
    assert_eq!(
        decide_persona_request(&stale_authorization, &cat),
        PersonaVerdict::Reject(
            "persona request has no exact current enrollment authorization".to_string()
        )
    );

    // authorizationOk (localSelf branch): the request must be signed by the
    // same local principal it requests and targets.
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

    // unsigned_local_command_denied.
    let mut unsigned_local = valid_local.clone();
    unsigned_local.local_signature_valid = false;
    assert!(matches!(
        decide_persona_request(&unsigned_local, &cat),
        PersonaVerdict::Reject(_)
    ));

    // cross_principal_local_command_denied.
    let mut cross_principal_local = valid_local;
    cross_principal_local.requester_did = "did:key:other".to_string();
    cross_principal_local.local_signer_did = "did:key:other".to_string();
    assert!(matches!(
        decide_persona_request(&cross_principal_local, &cat),
        PersonaVerdict::Reject(_)
    ));

    // authorizationOk is a closed two-branch disjunction: an unknown
    // authority kind satisfies neither branch.
    let mut unknown_authority = create_doc(PersonaOp::Create { clone_from: None });
    unknown_authority.authority_kind = "carrier-pigeon".to_string();
    assert!(matches!(
        decide_persona_request(&unknown_authority, &cat),
        PersonaVerdict::Reject(_)
    ));
}

/// Mirrors Lean `presetKnown` ("readonly" ∨ "write") and the admission
/// conjuncts built on it (`presetCreateOk`, `editPresetOk`): the builtin
/// preset vocabulary, and the effective capabilities and named allowed tools
/// the two templates carry. Presets are permission labels, so an admitted
/// persona grants exactly the mode-derived capabilities and no extra named
/// tools.
#[test]
fn preset_vocabulary_and_effective_capabilities_match_lean() {
    assert_eq!(
        persona_presets::builtin_preset_names(),
        (&[
            persona_presets::PRESET_READONLY,
            persona_presets::PRESET_WRITE
        ])[..],
        "Lean presetKnown pins exactly the readonly/write vocabulary"
    );

    let readonly = persona_presets::preset_fields(persona_presets::PRESET_READONLY)
        .expect("readonly must resolve for admission (validate_preset_name)");
    let write = persona_presets::preset_fields(persona_presets::PRESET_WRITE)
        .expect("write must resolve for admission (validate_preset_name)");
    assert!(persona_presets::preset_fields("bogus").is_none());
    assert!(persona_presets::preset_fields("").is_none());

    // Effective capabilities: the templates differ exactly in the permission
    // modes they grant — a readonly persona cannot write files or run
    // unrestricted bash; a write persona can.
    assert!(readonly.enable_file_tools && readonly.enable_bash);
    assert_eq!(readonly.file_tools_mode, "ReadOnly");
    assert_eq!(readonly.bash_mode, "ReadOnly");
    assert!(write.enable_file_tools && write.enable_bash);
    assert_eq!(write.file_tools_mode, "ReadWrite");
    assert_eq!(write.bash_mode, "Unrestricted");

    // Neither preset turns on self-configuration, and neither grants named
    // allowed tools: the argv prefixes, the read-only command allowlist, and
    // the write-tool declarations stay empty, so admission never smuggles in
    // named capabilities beyond the mode-derived ones.
    for fields in [&readonly, &write] {
        assert!(!fields.enable_self_config);
        assert!(fields.command_allowed_argv_prefixes.is_empty());
        assert!(fields.command_forbidden_argv_prefixes.is_empty());
        assert!(fields.read_only_command_allowlist.is_empty());
        assert!(fields.write_tools.is_empty());
    }

    // The name classifies the whole permission bundle: one added allowed
    // argv prefix makes the selection custom instead of readonly, while both
    // templates round-trip through the exact-match classifier.
    let mut customized = readonly.clone();
    customized
        .command_allowed_argv_prefixes
        .push("git status".to_string());
    assert_eq!(persona_presets::preset_name(&customized), None);
    assert_eq!(
        persona_presets::preset_name(&readonly),
        Some(persona_presets::PRESET_READONLY)
    );
    assert_eq!(
        persona_presets::preset_name(&write),
        Some(persona_presets::PRESET_WRITE)
    );
}
