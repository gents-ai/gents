use super::*;

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
