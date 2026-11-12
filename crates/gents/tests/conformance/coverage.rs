use super::*;

#[test]
fn lean_boundary_metadata_is_typed_and_reviewable() {
    let snapshot = lean_contract_snapshot();
    let expected_boundary_ids = [
        "boundary.request.dead-preclaim-only",
        "boundary.request.recovery-sweep-reachable",
        "boundary.tool-call.permanent-without-retry-evidence",
        "boundary.mcp.call-tool-dispatch-retry-evidence",
        "boundary.inference-slots.running-row-derived",
        "boundary.fleet-slot-accounting.derived-view",
        "boundary.command-policy.host-execution-assumptions",
        "boundary.compaction.unique-call-ids-checked",
        "boundary.trigger.dispatch-source-delivery",
        "boundary.persistence.abstract-lifecycle",
        "boundary.storage.hook-failure-policy",
        "boundary.storage.observation-daemon-visible",
        "boundary.storage.minimum-visibility-path",
        "boundary.backend-health.admission-freshness",
        "boundary.session-recovery.client-retry-surface",
        "boundary.coverage-ledger.review-discipline",
        "boundary.event-delivery.fair-substrate",
        "boundary.prompt-assembly.provider-input-sanitization",
        "boundary.model.nat-typed-ids-time",
        "boundary.p2p-backpressure.obligation-model",
        "boundary.rendered-capture.assembled-request-artifact",
        "boundary.rendered-capture.key-encoding-injectivity",
        "boundary.plugin-job.guest-determinism",
        "boundary.plugin-job.single-step-driver",
        "boundary.plugin-data.transaction",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();

    let mut actual_boundary_ids = BTreeSet::new();
    let mut boundary_subjects = BTreeSet::new();
    for boundary in &snapshot.boundaries {
        assert!(
            !boundary.id.trim().is_empty(),
            "boundary id must be non-empty: {:?}",
            boundary
        );
        assert!(
            !boundary.domain.trim().is_empty(),
            "boundary domain must be non-empty: {:?}",
            boundary
        );
        assert!(
            !boundary.subject.trim().is_empty(),
            "boundary subject must be non-empty: {:?}",
            boundary
        );
        assert!(
            !boundary.statement.trim().is_empty(),
            "boundary statement must be non-empty: {:?}",
            boundary
        );
        assert!(
            boundary
                .accepted_failure_mode
                .as_deref()
                .is_none_or(|text| !text.trim().is_empty()),
            "boundary accepted_failure_mode must be omitted or non-empty: {:?}",
            boundary
        );
        assert!(
            boundary
                .accepted_follow_up
                .as_deref()
                .is_none_or(|text| !text.trim().is_empty()),
            "boundary accepted_follow_up must be omitted or non-empty: {:?}",
            boundary
        );
        assert!(
            actual_boundary_ids.insert(boundary.id.clone()),
            "duplicate boundary id: {:?}",
            boundary
        );
        assert!(
            boundary_subjects.insert((boundary.domain.clone(), boundary.subject.clone())),
            "duplicate boundary subject in domain {:?}: {:?}",
            boundary.domain,
            boundary
        );
    }

    assert_eq!(
        actual_boundary_ids, expected_boundary_ids,
        "Lean boundary metadata ids changed; update this review-discipline list with the boundary data"
    );
}

#[test]
fn lean_deviation_metadata_is_empty_or_explicitly_classified() {
    let snapshot = lean_contract_snapshot();
    let mut deviation_ids = BTreeSet::new();
    let mut deviation_subjects = BTreeSet::new();

    for deviation in &snapshot.deviations {
        assert!(
            !deviation.id.trim().is_empty(),
            "deviation id must be non-empty: {:?}",
            deviation
        );
        assert!(
            !deviation.domain.trim().is_empty(),
            "deviation domain must be non-empty: {:?}",
            deviation
        );
        assert!(
            !deviation.subject.trim().is_empty(),
            "deviation subject must be non-empty: {:?}",
            deviation
        );
        assert!(
            !deviation.statement.trim().is_empty(),
            "deviation statement must be non-empty: {:?}",
            deviation
        );
        assert!(
            deviation
                .accepted_failure_mode
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
                || deviation
                    .accepted_follow_up
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty()),
            "active deviations must carry accepted_failure_mode or accepted_follow_up text: {:?}",
            deviation
        );
        assert!(
            deviation_ids.insert(deviation.id.clone()),
            "duplicate deviation id: {:?}",
            deviation
        );
        assert!(
            deviation_subjects.insert((deviation.domain.clone(), deviation.subject.clone())),
            "duplicate deviation subject in domain {:?}: {:?}",
            deviation.domain,
            deviation
        );
    }
}

const REQUIRE_FEATURE_TAG_FOR_ALL_ROWS: bool = true;

#[test]
fn lean_feature_matrix_covers_every_declared_required_surface() {
    let snapshot = lean_contract_snapshot();
    let valid_features: BTreeSet<&str> = snapshot
        .feature_surface_requirements
        .iter()
        .map(|req| req.feature.as_str())
        .collect();

    for entry in &snapshot.coverage_ledger {
        if REQUIRE_FEATURE_TAG_FOR_ALL_ROWS {
            assert!(
                !entry.feature.is_empty(),
                "coverage ledger row is untagged: {:?}",
                entry
            );
        }
        if !entry.feature.is_empty() {
            assert!(
                valid_features.contains(entry.feature.as_str()),
                "coverage ledger row tags unknown feature: {:?}",
                entry
            );
            if entry.category != "follow_up_hook" {
                assert!(
                    !entry.surfaces.is_empty(),
                    "coverage ledger row carries feature {:?} but no surfaces; \
                     each tagged non-follow-up hook row must declare at least one surface: {:?}",
                    entry.feature,
                    entry
                );
            }
        }
    }

    for req in &snapshot.feature_surface_requirements {
        for surface in &req.required {
            let covered = snapshot.coverage_ledger.iter().any(|entry| {
                entry.feature == req.feature
                    && entry.surfaces.iter().any(|candidate| candidate == surface)
            });
            assert!(
                covered,
                "feature {:?} declares required surface {:?} but no \
                 ledger row tags this (feature, surface). Either add a \
                 ledger row, or move this surface to `deferred` with a \
                 follow-up note.",
                req.feature, surface
            );
        }
    }

    for req in &snapshot.feature_surface_requirements {
        for surface in &req.required {
            let cell = snapshot
                .feature_matrix
                .get(&req.feature)
                .and_then(|surfaces| surfaces.get(surface.as_str()));
            let strength = cell
                .map(|cell| cell.coverage_strength.as_str())
                .unwrap_or("missing");
            assert!(
                strength != "missing",
                "feature_matrix[{}][{:?}] is `missing` but required",
                req.feature,
                surface
            );
        }
    }
}
