use gents_protocol::error_chain::ErrorChain;
use std::collections::BTreeSet;

use super::lean_contract_snapshot;

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

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
        assert!(
            matches!(case.operation.as_str(), "create" | "meet"),
            "unknown operation {} in {}",
            case.operation,
            case.name
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
                .clone()
                .expect("nonblank case has a filesystem candidate")
                .to_string_lossy()
                .into_owned()
        };
        match case.operation.as_str() {
            "create" => {
                let admitted =
                    gents::tool_surface::admit_authored_root(&policy, &authored_root, case.stored)
                        .is_ok();
                assert_eq!(
                    admitted, case.expected,
                    "production root admission disagrees with generated case {} ({})",
                    case.name, case.observation
                );
            }
            "meet" => {
                let ceiling = ceiling
                    .as_deref()
                    .expect("meet case models a process ceiling");
                let meet = match candidate.as_deref() {
                    Some(selection) => gents::tool_surface::resolve_effective_tool_root(
                        "conformance",
                        Some(selection),
                        Some(ceiling),
                    )
                    .map_err(BoxedError::from),
                    None => {
                        let mut tools = gents::document_config::Tools {
                            host: Some(gents::document_config::HostTools {
                                root: Some(case.authored.clone()),
                                files: Some(gents::document_config::FileTools {
                                    mode: gents::tool_surface::FileToolMode::ReadOnly,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            ..Default::default()
                        };
                        gents::tool_surface::canonicalize_tools_root(&mut tools, &policy, None)
                            .map_err(BoxedError::from)
                            .and_then(|()| {
                                let configured = tools
                                    .host
                                    .and_then(|host| host.root)
                                    .filter(|root| !root.trim().is_empty());
                                gents::tool_surface::resolve_effective_tool_root(
                                    "conformance",
                                    configured.as_deref().map(std::path::Path::new),
                                    Some(ceiling),
                                )
                                .map_err(BoxedError::from)
                            })
                    }
                };
                let modeled_effective = case.effective.as_ref().map(|path| {
                    std::fs::canonicalize(materialize(&path.anchor, &path.components))
                        .expect("canonical modeled effective root")
                });
                match (meet, modeled_effective) {
                    (Ok(effective), Some(modeled)) => assert_eq!(
                        effective,
                        Some(modeled),
                        "production meet disagrees with the modeled effective root for {}",
                        case.name
                    ),
                    (Ok(effective), None) => panic!(
                        "meet case {} produced an effective root {effective:?} the model refuses",
                        case.name
                    ),
                    (Err(error), Some(modeled)) => panic!(
                        "meet case {} refused a selection the model admits at {}: {}",
                        case.name,
                        modeled.display(),
                        ErrorChain(error.as_ref())
                    ),
                    (Err(_), None) => {}
                }
            }
            other => panic!("unhandled Lean root operation {other}"),
        }
    }
}
