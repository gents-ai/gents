//! The conformance rule: every family the Lean snapshot emits is replayed by
//! exactly one native test. The scan finds that test from the family's
//! snapshot key, in a Rust file some target compiles (`module_tree`) or a
//! TypeScript file, so a consumed family needs no ledger entry. A family no test
//! replays is refused unless the coverage ledger
//! (`Proofs/Conformance/CoverageLedger.lean`) lists it as a boundary or as
//! open; a ledger entry naming a consumed or unemitted family is refused.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;

const CONTRACT_METADATA: &[&str] = &["coverage_ledger", "feature_surface_requirements"];

const DERIVED_FAMILIES: &str = "families";
const DERIVED_VOCABULARIES: &str = "family_vocabularies";

const SOURCE_ROOTS: &[&str] = &["crates", "apps", "packages"];
const SKIPPED_DIRECTORIES: &[&str] = &["target", "node_modules", "proofs", "dist", "gen"];

fn is_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn identifiers(source: &str) -> impl Iterator<Item = (u8, &str, u8)> {
    let bytes = source.as_bytes();
    let mut start = 0;
    std::iter::from_fn(move || {
        while start < bytes.len() && !is_identifier(bytes[start]) {
            start += 1;
        }
        if start == bytes.len() {
            return None;
        }
        let mut end = start;
        while end < bytes.len() && is_identifier(bytes[end]) {
            end += 1;
        }
        let before = if start == 0 { b' ' } else { bytes[start - 1] };
        let after = bytes.get(end).copied().unwrap_or(b' ');
        let identifier = &source[start..end];
        start = end;
        Some((before, identifier, after))
    })
}

fn families_read(
    source: &str,
    families: &BTreeSet<String>,
    loaders: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut read = BTreeSet::new();
    for (before, identifier, after) in identifiers(source) {
        let is_quoted = before == b'"' && after == b'"';
        if (before == b'.' || is_quoted) && families.contains(identifier) {
            read.insert(identifier.to_owned());
        }
        if after == b'(' {
            if let Some(keys) = loaders.get(identifier) {
                read.extend(keys.iter().cloned());
            }
        }
    }
    read
}

fn loaders(
    vocab_sources: &[String],
    families: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut direct = BTreeMap::new();
    let mut calls: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for source in vocab_sources {
        for function in source.split("\npub fn ").skip(1) {
            let body = function.split("\n}\n").next().unwrap_or(function);
            let name: String = body
                .bytes()
                .take_while(|byte| is_identifier(*byte))
                .map(char::from)
                .collect();
            if !name.starts_with("lean_") && !name.starts_with("assert_") {
                continue;
            }
            direct.insert(
                name.clone(),
                families_read(body, families, &BTreeMap::new()),
            );
            calls.insert(
                name.clone(),
                identifiers(&body[name.len()..])
                    .filter(|(_, callee, after)| *after == b'(' && *callee != name)
                    .map(|(_, callee, _)| callee.to_owned())
                    .collect(),
            );
        }
    }
    let mut resolved = direct.clone();
    loop {
        let mut changed = false;
        for (name, callees) in &calls {
            for callee in callees {
                let Some(inherited) = resolved.get(callee).cloned() else {
                    continue;
                };
                let own = resolved.get_mut(name).expect("loader is recorded");
                let before = own.len();
                own.extend(inherited);
                changed |= own.len() != before;
            }
        }
        if !changed {
            return resolved;
        }
    }
}

fn split_at_tests(source: &str) -> (&str, &str) {
    let first = [
        "#[cfg(test)]\nmod tests {",
        "#[cfg(test)]\nuse ",
        "#[test]",
        "#[tokio::test]",
    ]
    .iter()
    .filter_map(|marker| source.find(marker))
    .min();
    source.split_at(first.unwrap_or(source.len()))
}

fn source_files(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("source directory entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if !SKIPPED_DIRECTORIES.contains(&name) && !name.starts_with('.') {
                source_files(&path, files);
            }
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rs" | "ts" | "tsx")
        ) {
            files.push(path);
        }
    }
}

pub fn emitted_families(snapshot: &Value) -> BTreeSet<String> {
    let snapshot = snapshot.as_object().expect("snapshot object");
    let mut families: BTreeSet<String> = snapshot
        .iter()
        .filter(|(key, value)| {
            (value.is_array() || value.is_object())
                && !CONTRACT_METADATA.contains(&key.as_str())
                && ![DERIVED_FAMILIES, DERIVED_VOCABULARIES].contains(&key.as_str())
        })
        .map(|(key, _)| key.clone())
        .collect();
    for derived in [DERIVED_FAMILIES, DERIVED_VOCABULARIES] {
        let derived = snapshot[derived].as_object().expect("derived families");
        families.extend(
            derived
                .keys()
                .map(|key| format!("{DERIVED_FAMILIES}/{key}")),
        );
    }
    families
}

/// The emitted family a ledger name denotes: a derived family by its key, or
/// a top-level snapshot key.
pub fn ledger_family(name: &str, families: &BTreeSet<String>) -> Option<String> {
    [format!("{DERIVED_FAMILIES}/{name}"), name.to_owned()]
        .into_iter()
        .find(|family| families.contains(family))
}

pub fn consumer_sites(
    workspace: &Path,
    families: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let vocab = workspace.join("crates/gents-lean-vocab/src");
    let mut vocab_files = Vec::new();
    source_files(&vocab, &mut vocab_files);
    let vocab_sources: Vec<String> = vocab_files
        .iter()
        .map(|path| fs::read_to_string(path).expect("vocab source"))
        .collect();
    let loader_sources: Vec<String> = vocab_sources
        .iter()
        .map(|source| split_at_tests(source).0.to_owned())
        .collect();
    let loaders = loaders(&loader_sources, families);

    let mut sites: BTreeMap<String, BTreeSet<String>> = families
        .iter()
        .map(|family| (family.clone(), BTreeSet::new()))
        .collect();
    let compiled = crate::module_tree::module_tree(workspace).compiled;
    let mut files = Vec::new();
    for root in SOURCE_ROOTS {
        source_files(&workspace.join(root), &mut files);
    }
    let runs = |path: &Path| {
        path.extension().is_none_or(|extension| extension != "rs") || compiled.contains(path)
    };
    for path in files.into_iter().filter(|path| runs(path)) {
        let relative = path
            .strip_prefix(workspace)
            .expect("workspace file")
            .display()
            .to_string();
        if relative.starts_with("crates/gents-lean-contract/src") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("workspace source");
        let source = if relative.starts_with("crates/gents-lean-vocab/src") {
            if relative.ends_with("consumer_scan.rs") {
                continue;
            }
            if relative.ends_with("/tests.rs") {
                source
            } else {
                split_at_tests(&source).1.to_owned()
            }
        } else {
            source
        };
        for family in families_read(&source, families, &loaders) {
            sites
                .get_mut(&family)
                .expect("known family")
                .insert(relative.clone());
        }
        if !source.contains("families::") {
            continue;
        }
        let prefix = format!("{DERIVED_FAMILIES}/");
        for (family, found) in sites.range_mut(prefix.clone()..) {
            let Some(key) = family.strip_prefix(&prefix) else {
                break;
            };
            let calls: usize = [
                "::replay(",
                "::replay::<",
                "::replay)",
                "::cases(",
                "::cases::<",
                "::cases)",
                " as family;",
            ]
            .iter()
            .map(|site| source.matches(&format!("families::{key}{site}")).count())
            .sum();
            found.extend((1..=calls).map(|call| format!("{relative}#{call}")));
        }
    }
    sites
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    const PREMISE_PREFIX: &str = "boundary.";

    /// Each name is an emitted family that no native test replays. The list
    /// only shrinks: a family leaves it when its consumer is written or Lean
    /// stops emitting it, and a new family arrives with its consumer.
    const OPEN: &[&str] = &[
        "apply_reconcile_contract_cases_publish_cases",
        "client_window_scope_bound_delivers",
        "contracts_atomic_admission_unreached",
        "contracts_background_wake_rows_unreached",
        "contracts_edit_anchor_fork_unreached",
        "contracts_fork_publication_unreached",
        "contracts_pack_installation_finality_unreached",
        "contracts_pack_installation_hosted_plugin_unreached",
        "contracts_queued_steering_guard_unreached",
        "contracts_runtime_reconcile_unreached",
        "contracts_self_config_job_guard_unreached",
        "contracts_session_advance_unreached",
        "contracts_session_refresh_unreached",
        "contracts_title_admission_fresh_verifier_unreached",
        "contracts_workspace_transition_unreached",
        "cross_actor_authorization_unreached",
        "enrollment_auto_renewal_offer_unreached",
        "eval_records_published_unreached",
        "event_delivery_group_capture_unreached",
        "event_delivery_group_clock_unreached",
        "fold_queue_contracts_management_unreached",
        "goal_claimed_publication_contracts_identity_staged_unreached",
        "household_callback_module_claim_unreached",
        "household_tool_grant_rebinding_unreached",
        "household_unattended_receipt_unreached",
        "identity_operations_deferred_unreached",
        "identity_operations_enrollment_policy_unreached",
        "identity_operations_reader_policy_unreached",
        "identity_operations_readiness_observation_unreached",
        "identity_operations_readiness_recovery_unreached",
        "identity_operations_registry_writer_unreached",
        "identity_operations_replica_grant_unreached",
        "mailbox_handoff_contracts_linked_unreached",
        "native_completion_trace_unreached",
        "native_refinement_claim_unreached",
        "node_provisioning_agent_creator_unreached",
        "node_provisioning_bootstrap_grant_unreached",
        "node_provisioning_runtime_nac_unreached",
        "optimization_promotion_held_transition_unreached",
        "remote_delivery_claim_unreached",
        "remote_delivery_delivery_unreached",
        "remote_delivery_recovery_unreached",
        "remote_delivery_response_unreached",
        "session_child_child_scope_binding_unreached",
        "session_child_child_scope_repair_unreached",
        "session_child_client_request_recovery_unreached",
        "session_documents_retry_unreached",
        "title_admission_join_unreached",
        "title_usage_budget_unreached",
        "title_usage_catalog_unreached",
        "title_usage_goal_unreached",
        "watcher_actors_scanned_unreached",
    ];

    struct Scan {
        families: BTreeSet<String>,
        sites: BTreeMap<String, BTreeSet<String>>,
    }

    impl Scan {
        fn consumed(&self, family: &str) -> bool {
            self.sites
                .get(family)
                .is_some_and(|found| !found.is_empty())
        }
    }

    fn scan() -> &'static Scan {
        static SCAN: OnceLock<Scan> = OnceLock::new();
        SCAN.get_or_init(|| {
            let json = gents_lean_contract::load_contract_json().expect("Lean contract JSON");
            let snapshot: Value = serde_json::from_str(&json).expect("snapshot JSON");
            let families = emitted_families(&snapshot);
            let sites = consumer_sites(&workspace(), &families);
            Scan { families, sites }
        })
    }

    fn workspace() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("runtime CARGO_MANIFEST_DIR"),
        )
        .join("../..")
        .canonicalize()
        .expect("workspace root")
    }

    fn is_unreached(family: &str) -> bool {
        family.ends_with("_unreached") || family.ends_with("Unreached")
    }

    #[test]
    fn every_emitted_family_has_one_consumer_or_a_ledger_entry() {
        let scan = scan();
        let ledger = &crate::lean_contract_snapshot().coverage_ledger;
        let names = ledger
            .boundaries
            .iter()
            .map(|boundary| boundary.name.as_str())
            .filter(|name| !name.starts_with(PREMISE_PREFIX))
            .chain(ledger.open.iter().map(String::as_str));

        let mut listed = BTreeSet::new();
        let mut refused = Vec::new();
        for name in names {
            match ledger_family(name, &scan.families) {
                Some(family) if !scan.consumed(&family) => {
                    listed.insert(family);
                }
                _ => refused.push(name),
            }
        }
        assert!(
            refused.is_empty(),
            "coverage ledger entries name families that a test replays or the snapshot \
             does not emit; delete them from Proofs/Conformance/CoverageLedger.lean: {refused:?}"
        );

        let premises_naming_families: Vec<&str> = ledger
            .boundaries
            .iter()
            .map(|boundary| boundary.name.as_str())
            .filter(|name| name.starts_with(PREMISE_PREFIX))
            .filter(|name| ledger_family(name, &scan.families).is_some())
            .collect();
        assert!(
            premises_naming_families.is_empty(),
            "a boundary named `{PREMISE_PREFIX}*` is a premise, not a family: {premises_naming_families:?}"
        );

        let unlisted: Vec<&str> = scan
            .families
            .iter()
            .filter(|family| !scan.consumed(family) && !listed.contains(*family))
            .map(String::as_str)
            .collect();
        assert!(
            unlisted.is_empty(),
            "no native test replays these emitted families; write each family's consumer \
             in the same change as the family: {unlisted:?}"
        );

        let prefix = format!("{DERIVED_FAMILIES}/");
        let replayed_twice: Vec<_> = scan
            .sites
            .iter()
            .filter(|(family, found)| family.starts_with(&prefix) && found.len() > 1)
            .collect();
        assert!(
            replayed_twice.is_empty(),
            "a derived family is replayed by exactly one test: {replayed_twice:?}"
        );
    }

    #[test]
    fn open_only_shrinks() {
        let ledger = &crate::lean_contract_snapshot().coverage_ledger;
        let open: BTreeSet<&str> = ledger.open.iter().map(String::as_str).collect();
        let pinned: BTreeSet<&str> = OPEN.iter().copied().collect();
        let added: Vec<_> = open.difference(&pinned).collect();
        assert!(
            added.is_empty(),
            "the coverage ledger's open list only shrinks; write these families' consumers \
             in the same change as the families: {added:?}"
        );
        let removed: Vec<_> = pinned.difference(&open).collect();
        assert!(
            removed.is_empty(),
            "these families left the coverage ledger's open list; delete them from OPEN: {removed:?}"
        );
    }

    #[test]
    fn no_new_family_splits_off_its_unreached_cases() {
        let scan = scan();
        let ledger = &crate::lean_contract_snapshot().coverage_ledger;
        let allowed: BTreeSet<String> = OPEN
            .iter()
            .copied()
            .chain(
                ledger
                    .boundaries
                    .iter()
                    .map(|boundary| boundary.name.as_str()),
            )
            .filter_map(|name| ledger_family(name, &scan.families))
            .collect();
        let prefix = format!("{DERIVED_FAMILIES}/");
        let split: Vec<&str> = scan
            .families
            .iter()
            .filter(|family| is_unreached(family.strip_prefix(&prefix).unwrap_or(family.as_str())))
            .filter(|family| !allowed.contains(*family))
            .map(String::as_str)
            .collect();
        assert!(
            split.is_empty(),
            "an owner emits one family over the cases production reaches, never an \
             `_unreached` complement; fold these cases into the replayed family or keep \
             them as Lean theorems: {split:?}"
        );
    }

    #[test]
    fn feature_surfaces_name_consumed_families() {
        let scan = scan();
        let unproven: Vec<_> = crate::lean_feature_surface_requirements()
            .iter()
            .flat_map(|requirement| {
                requirement
                    .required
                    .iter()
                    .map(move |evidence| (requirement.feature.as_str(), evidence))
            })
            .filter(|(_, evidence)| {
                !ledger_family(&evidence.family, &scan.families)
                    .is_some_and(|family| scan.consumed(&family))
            })
            .map(|(feature, evidence)| (feature, evidence.surface, evidence.family.as_str()))
            .collect();
        assert!(
            unproven.is_empty(),
            "a required feature surface names a family a native test replays: {unproven:?}"
        );
    }
}
