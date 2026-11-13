use super::*;

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
