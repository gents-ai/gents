//! Canonical descendant-root admission
//! (`proofs/Proofs/PeerRegistryDiscovery/RootAdmission.lean`, emitted through
//! `Conformance.RootAdmissionContracts`).
//!
//! Lean `rootSelectionOk` is the only policy owner. Each emitted case
//! materializes its abstract canonical paths as a sandbox filesystem, projects
//! the published roots through `project_workspace_root_policy`, and replays the
//! authored root through `admit_authored_root`, the owner shared by Agent
//! creation and the canonical `Tools.host.root` writer.

use std::collections::BTreeSet;

use super::lean_contract_snapshot;

/// Register every emitted root case and reject drift in the serialized
/// operation/text observations.
#[test]
fn generated_root_admission_case_inventory_is_complete() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().root_admission_cases;
    assert!(!cases.is_empty());
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(
            names.insert(case.name.as_str()),
            "duplicate case {}",
            case.name
        );
        assert_eq!(case.blank, case.authored.trim().is_empty(), "{}", case.name);
        assert_eq!(
            case.operation, "create",
            "unknown operation {} in {}",
            case.operation, case.name
        );
        assert!(
            !case.observation.trim().is_empty(),
            "{} omitted its filesystem observation",
            case.name
        );
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
            "blank" => None,
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

        let authored_root = if case.blank {
            case.authored.clone()
        } else {
            candidate
                .expect("nonblank case has a filesystem candidate")
                .to_string_lossy()
                .into_owned()
        };
        let admitted = gents::tool_surface::admit_authored_root(&policy, &authored_root).is_ok();
        assert_eq!(
            admitted, case.expected,
            "production root admission disagrees with generated case {} ({})",
            case.name, case.observation
        );
    }
}
