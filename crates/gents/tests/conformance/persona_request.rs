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

use super::lean_contract_snapshot;
use gents::agent::persona_ops::{
    decide_persona_request, BehaviorRef, PersonaCatalogView, PersonaOp, PersonaRequestDoc,
    PersonaVerdict,
};
use gents::agent::persona_presets;

/// Register every emitted root case and reject drift in the serialized
/// operation/text observations. The Rust implementation slice strengthens
/// this same consumer by driving the production policy projector and persona
/// gate; this foundation test keeps the generated contract independently
/// reviewable.
#[test]
fn generated_root_admission_case_inventory_is_complete() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().root_admission_cases;
    assert_eq!(cases.len(), 27);
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(
            names.insert(case.name.as_str()),
            "duplicate case {}",
            case.name
        );
        assert_eq!(case.blank, case.authored.trim().is_empty(), "{}", case.name);
        assert!(
            matches!(
                case.operation.as_str(),
                "create" | "edit_set" | "edit_clear" | "edit_omitted"
            ),
            "unknown operation {} in {}",
            case.operation,
            case.name
        );
        assert!(
            !case.observation.trim().is_empty(),
            "{} omitted its filesystem observation",
            case.name
        );
        if matches!(case.operation.as_str(), "edit_clear") {
            assert!(
                case.candidate.is_none(),
                "{} clear carried a candidate",
                case.name
            );
        }
        if case.operation != "edit_omitted" {
            assert!(
                !case.stored_requires_root,
                "{} marks an authored operation as stored-root-dependent",
                case.name
            );
        }
    }
}

/// Lean selectedToolFlag omission/explicit laws, through the production
/// canonical Tools adapter rather than a second test materializer.
#[test]
fn sibling_tool_selection_preserves_or_explicitly_overrides() {
    for existing in [false, true] {
        for requested in [None, Some(false), Some(true)] {
            let mut tools = gents::document_config::Tools::default();
            gents::self_config::apply_tool_grant_selection(
                &mut tools,
                Some(existing),
                Some(existing),
                None,
            );
            gents::self_config::apply_tool_grant_selection(&mut tools, requested, requested, None);
            let expected = requested.unwrap_or(existing);
            assert_eq!(
                tools
                    .integrations
                    .as_ref()
                    .and_then(|v| v.lsp.as_ref())
                    .is_some(),
                expected
            );
            assert_eq!(
                tools
                    .built_ins
                    .as_ref()
                    .and_then(|v| v.enable_graph_tools)
                    .unwrap_or(false),
                expected
            );
            assert!(tools.self_config.is_none());
        }
    }
}

/// Lean `networkSelectionAllowed`, omission preservation, and no-widening
/// laws through the production admission fence and canonical Tools writer.
#[test]
fn sibling_network_selection_only_admits_omission_or_disabled_narrowing() {
    use gents::toolset::CommandNetworkMode::{Disabled, Enabled, Inherit};

    for existing in [Disabled, Inherit, Enabled] {
        for requested in [None, Some(Disabled), Some(Inherit), Some(Enabled)] {
            let admitted = gents::self_config::validate_tool_network_selection(requested).is_ok();
            assert_eq!(
                admitted,
                requested.is_none_or(|mode| mode == Disabled),
                "admission mismatch for existing={existing:?} requested={requested:?}"
            );
            if !admitted {
                continue;
            }

            let mut tools = gents::document_config::Tools::default();
            gents::self_config::apply_tool_grant_selection(&mut tools, None, None, Some(existing));
            gents::self_config::apply_tool_grant_selection(&mut tools, None, None, requested);
            let selected = tools
                .host
                .as_ref()
                .and_then(|host| host.bash.as_ref())
                .and_then(|bash| bash.network_mode)
                .expect("existing network mode is materialized");
            let expected = requested.unwrap_or(existing);
            assert_eq!(selected, expected);
            assert!(selected.meet(existing) == selected, "selection widened");
        }
    }
}

#[test]
fn graph_presentation_is_independent_of_configuration_and_installation() {
    for requested in [false, true] {
        for enabled in [false, true] {
            for install in [false, true] {
                let names = gents::self_config::self_config_tool_names(
                    &gents::tool_surface::SelfConfigToolConfig {
                        enabled,
                        enable_pack_install: install,
                        enable_graph_tools: requested,
                        ..Default::default()
                    },
                );
                assert_eq!(names.iter().any(|v| v == "run_graph"), requested);
                assert_eq!(names.iter().any(|v| v == "config"), enabled);
                assert!(!names.iter().any(|v| v == "install_pack"));
            }
        }
    }
}

fn catalog_with(
    roots: &[&str],
    profiles: &[&str],
    behaviors: &[(&str, bool)],
) -> PersonaCatalogView {
    PersonaCatalogView {
        allowed_roots: roots.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        root_policy_configured: !roots.is_empty(),
        operator_ceiling: None,
        available_profile_ids: profiles
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        known_agent_dids: BTreeSet::from(["did:key:agent".to_string()]),
        behaviors: behaviors
            .iter()
            .map(|(id, enabled)| {
                (
                    id.to_string(),
                    BehaviorRef {
                        enabled: *enabled,
                        protected: false,
                        ..Default::default()
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
        &[("existing-enabled", true), ("existing-disabled", false)],
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
        description: Some("Researches a focused question".to_string()),
        system_prompt: Some("Research the question and cite evidence.".to_string()),
        root: Some("/workspace/root".to_string()),
        preset: Some(persona_presets::PRESET_WRITE.to_string()),
        profile_id: Some("profile-1".to_string()),
        ..Default::default()
    }
}

#[cfg(unix)]
#[test]
fn generated_root_admission_cases_drive_production_root_policy() {
    let mut names = BTreeSet::new();
    for case in &lean_contract_snapshot().root_admission_cases {
        assert!(
            names.insert(case.name.as_str()),
            "duplicate case {}",
            case.name
        );
        assert_eq!(
            case.authored.trim().is_empty(),
            case.blank,
            "Lean blank observation drifted for {}",
            case.name
        );
        let sandbox = tempfile::tempdir().expect("case sandbox");
        let materialize = |anchor: &str, components: &[String]| {
            let mut path = sandbox.path().join(anchor);
            path.extend(components);
            path
        };
        let ceiling = case
            .ceiling
            .as_ref()
            .map(|path| materialize(&path.anchor, &path.components));
        if let Some(ceiling) = &ceiling {
            std::fs::create_dir_all(ceiling).expect("policy ceiling");
        }
        let invalid_ceiling = if case.observation == "invalid_ceiling" {
            let link = sandbox.path().join("invalid-ceiling");
            std::os::unix::fs::symlink(sandbox.path().join("absent-ceiling-target"), &link)
                .expect("broken ceiling symlink");
            Some(link)
        } else {
            None
        };
        let enabled = case
            .enabled
            .iter()
            .map(|path| materialize(&path.anchor, &path.components))
            .collect::<Vec<_>>();
        for root in &enabled {
            std::fs::create_dir_all(root).expect("enabled root");
        }
        let mut documents = enabled
            .iter()
            .map(|root| gents::tool_surface::WorkspaceRootDocument {
                root_path: Some(root.to_string_lossy().into_owned()),
                enabled: Some(true),
            })
            .collect::<Vec<_>>();
        if case.configured && documents.is_empty() && case.observation != "invalid_ceiling" {
            documents.push(gents::tool_surface::WorkspaceRootDocument {
                root_path: ceiling
                    .as_ref()
                    .map(|root| root.to_string_lossy().into_owned()),
                enabled: Some(false),
            });
        }
        let policy = gents::tool_surface::project_workspace_root_policy(
            documents,
            invalid_ceiling.as_deref().or(ceiling.as_deref()),
        );
        assert_eq!(policy.configured, case.configured, "{}", case.name);
        let roots = case
            .published
            .iter()
            .map(|path| materialize(&path.anchor, &path.components))
            .collect::<Vec<_>>();
        for root in &roots {
            std::fs::create_dir_all(root).expect("published root");
        }
        let roots = roots
            .into_iter()
            .map(|root| std::fs::canonicalize(root).expect("canonical published root"))
            .collect::<Vec<_>>();
        assert_eq!(
            policy.published.iter().cloned().collect::<BTreeSet<_>>(),
            roots.iter().cloned().collect::<BTreeSet<_>>(),
            "production publication disagrees with modeled publication for {}",
            case.name
        );
        let resolved_candidate = case
            .candidate
            .as_ref()
            .map(|path| materialize(&path.anchor, &path.components));
        // Lean anchors are abstract volume identities. On Unix the refinement
        // materializes them as disjoint sandbox subtrees; the native Windows
        // volume-prefix rule is separately fenced in root_admission unit tests.
        let candidate = match case.observation.as_str() {
            "blank" | "clear" | "inactive" => None,
            "existing" | "explicit_restriction" | "invalid_ceiling" => {
                let path = resolved_candidate.clone().expect("modeled candidate");
                std::fs::create_dir_all(&path).expect("existing candidate");
                Some(path)
            }
            "nonexistent" => Some(resolved_candidate.clone().expect("modeled candidate")),
            "unresolved" => {
                let link = roots[0].join(format!("broken-{}", case.name));
                std::os::unix::fs::symlink(roots[0].join("absent-target"), &link)
                    .expect("broken symlink");
                Some(link)
            }
            "symlink" => {
                let target = resolved_candidate.clone().expect("modeled target");
                std::fs::create_dir_all(&target).expect("symlink target");
                let link = if case.expected {
                    sandbox.path().join("links").join(&case.name)
                } else {
                    roots[0].join(format!("link-{}", case.name))
                };
                std::fs::create_dir_all(link.parent().expect("link parent")).expect("link parent");
                std::os::unix::fs::symlink(&target, &link).expect("symlink");
                assert_eq!(
                    std::fs::canonicalize(&link).expect("resolved symlink"),
                    std::fs::canonicalize(&target).expect("canonical target")
                );
                Some(link)
            }
            "traversal" => {
                let target = resolved_candidate.clone().expect("modeled target");
                std::fs::create_dir_all(&target).expect("traversal target");
                let target = std::fs::canonicalize(target).expect("canonical traversal target");
                let root = &roots[0];
                let traversing = if let Ok(relative) = target.strip_prefix(root) {
                    let detour = root.join("detour");
                    std::fs::create_dir_all(&detour).expect("traversal detour");
                    detour.join("..").join(relative)
                } else {
                    let parent = root.parent().expect("published root parent");
                    root.join("..").join(
                        target
                            .strip_prefix(parent)
                            .expect("outside candidate shares modeled parent"),
                    )
                };
                assert_eq!(
                    std::fs::canonicalize(&traversing).expect("resolved traversal"),
                    std::fs::canonicalize(&target).expect("canonical target")
                );
                Some(traversing)
            }
            other => panic!("unhandled Lean filesystem observation {other}"),
        };

        let mut catalog = base_catalog();
        catalog.allowed_roots = roots
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        catalog.root_policy_configured = case.configured;
        catalog.operator_ceiling = ceiling;
        let mut doc = match case.operation.as_str() {
            "create" => create_doc(PersonaOp::Create { clone_from: None }),
            "edit_set" | "edit_clear" | "edit_omitted" => {
                let mut doc = create_doc(PersonaOp::Edit);
                doc.op_raw = "edit".to_string();
                doc.behavior_id = Some("existing-enabled".to_string());
                if case.operation != "edit_omitted" {
                    doc.edit_fields = vec!["root".to_string()];
                }
                doc
            }
            other => panic!("unhandled Lean persona operation {other}"),
        };
        if case.operation == "edit_omitted" {
            let target = catalog
                .behaviors
                .get_mut("existing-enabled")
                .expect("fixture behavior");
            target.root_required = case.stored_requires_root;
            target.root = if case.blank {
                Some(case.authored.clone())
            } else {
                candidate
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
            };
        }
        doc.root = if matches!(case.operation.as_str(), "edit_clear" | "edit_omitted") {
            None
        } else if case.blank {
            Some(case.authored.clone())
        } else {
            Some(
                candidate
                    .expect("nonblank case has a filesystem candidate")
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        let admitted = decide_persona_request(&doc, &catalog) == PersonaVerdict::Admit;
        assert_eq!(
            admitted, case.expected,
            "production gate disagrees with generated case {} ({})",
            case.name, case.observation
        );
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
    happy_edit.persona_name = Some("Renamed".to_string());
    happy_edit.profile_id = None;
    happy_edit.edit_fields = vec!["display_name".to_string()];
    assert_eq!(
        decide_persona_request(&happy_edit, &cat),
        PersonaVerdict::Admit
    );

    let mut clear_profile = happy_edit.clone();
    clear_profile.persona_name = None;
    clear_profile.edit_fields = vec!["profile_id".to_string()];
    assert!(matches!(
        decide_persona_request(&clear_profile, &cat),
        PersonaVerdict::Reject(detail) if detail.contains("unknown profile")
    ));

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

    let mut promoted_disable = happy_disable.clone();
    promoted_disable.make_default = true;
    assert_eq!(
        decide_persona_request(&promoted_disable, &cat),
        PersonaVerdict::Reject("disable must not request make_default".to_string())
    );

    let mut protected_catalog = cat.clone();
    protected_catalog
        .behaviors
        .get_mut("existing-enabled")
        .expect("fixture behavior")
        .protected = true;
    let mut protected_edit = create_doc(PersonaOp::Edit);
    protected_edit.op_raw = "edit".to_string();
    protected_edit.behavior_id = Some("existing-enabled".to_string());
    assert!(matches!(
        decide_persona_request(&protected_edit, &protected_catalog),
        PersonaVerdict::Reject(_)
    ));
    assert!(matches!(
        decide_persona_request(&happy_disable, &protected_catalog),
        PersonaVerdict::Reject(_)
    ));

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
    // createPromptOk: a preset-based create must be useful on its first turn.
    let mut missing_prompt = create_doc(PersonaOp::Create { clone_from: None });
    missing_prompt.system_prompt = None;
    rejects.push(missing_prompt);
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
        &[
            persona_presets::PRESET_READONLY,
            persona_presets::PRESET_WRITE
        ],
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
