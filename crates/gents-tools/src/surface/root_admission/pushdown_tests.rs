#[cfg(unix)]
#[test]
fn generated_root_admission_cases_drive_production_root_policy() {
    use gents_lean_vocab::families::root_admission_contracts_admission::{
        AdmissionOutput, CanonicalPath,
    };
    for case in gents_lean_vocab::families::root_admission_contracts_admission::cases() {
        let input = &case.input;
        let blank = input.authored.trim().is_empty();
        let sandbox = tempfile::tempdir().expect("case sandbox");
        let canonical_sandbox = std::fs::canonicalize(sandbox.path()).expect("canonical sandbox");
        let modeled = |path: &std::path::Path| {
            let mut parts = path
                .strip_prefix(&canonical_sandbox)
                .expect("production root stays in the case sandbox")
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned());
            CanonicalPath {
                anchor: parts.next().expect("modeled anchor"),
                components: parts.collect(),
            }
        };
        let materialize = |anchor: &str, components: &[String]| {
            let mut path = sandbox.path().join(anchor);
            path.extend(components);
            path
        };
        let ceiling = input
            .ceiling
            .as_ref()
            .map(|path| materialize(&path.anchor, &path.components));
        if let Some(ceiling) = &ceiling {
            std::fs::create_dir_all(ceiling).expect("policy ceiling");
        }
        let invalid_ceiling = if input.observation == "invalid_ceiling" {
            let link = sandbox.path().join("invalid-ceiling");
            std::os::unix::fs::symlink(sandbox.path().join("absent-ceiling-target"), &link)
                .expect("broken ceiling symlink");
            Some(link)
        } else {
            None
        };
        let enabled = input
            .enabled
            .iter()
            .map(|path| materialize(&path.anchor, &path.components))
            .collect::<Vec<_>>();
        for root in &enabled {
            std::fs::create_dir_all(root).expect("enabled root");
        }
        let mut documents = enabled
            .iter()
            .map(|root| crate::surface::WorkspaceRootDocument {
                root_path: Some(root.to_string_lossy().into_owned()),
                enabled: Some(true),
            })
            .collect::<Vec<_>>();
        if input.configured && documents.is_empty() && input.observation != "invalid_ceiling" {
            documents.push(crate::surface::WorkspaceRootDocument {
                root_path: ceiling
                    .as_ref()
                    .map(|root| root.to_string_lossy().into_owned()),
                enabled: Some(false),
            });
        }
        let policy = crate::surface::project_workspace_root_policy(
            documents,
            invalid_ceiling.as_deref().or(ceiling.as_deref()),
        );
        assert_eq!(policy.configured, input.configured, "{}", input.name);
        let roots = &policy.published;
        let mut published = roots.iter().map(|root| modeled(root)).collect::<Vec<_>>();
        published.sort_by(|left, right| {
            (&left.anchor, &left.components).cmp(&(&right.anchor, &right.components))
        });
        let resolved_candidate = input
            .candidate
            .as_ref()
            .map(|path| materialize(&path.anchor, &path.components));
        let candidate = match input.observation.as_str() {
            "blank" => None,
            "existing" | "explicit_restriction" | "invalid_ceiling" => {
                let path = resolved_candidate.clone().expect("modeled candidate");
                std::fs::create_dir_all(&path).expect("existing candidate");
                Some(path)
            }
            "nonexistent" => Some(resolved_candidate.clone().expect("modeled candidate")),
            "unresolved" => {
                let link = roots[0].join(format!("broken-{}", input.name));
                std::os::unix::fs::symlink(roots[0].join("absent-target"), &link)
                    .expect("broken symlink");
                Some(link)
            }
            "symlink" => {
                let target = resolved_candidate.clone().expect("modeled target");
                std::fs::create_dir_all(&target).expect("symlink target");
                let link = if input.name == "symlink_target_inside" {
                    sandbox.path().join("links").join(&input.name)
                } else {
                    roots[0].join(format!("link-{}", input.name))
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
        let authored_root = if blank {
            input.authored.clone()
        } else {
            candidate
                .clone()
                .expect("nonblank case has a filesystem candidate")
                .to_string_lossy()
                .into_owned()
        };
        let (expected, effective) = match input.operation.as_str() {
            "create" => (
                crate::surface::admit_authored_root(&policy, &authored_root, input.stored).is_ok(),
                None,
            ),
            "meet" => {
                let ceiling = ceiling
                    .as_deref()
                    .expect("meet case models a process ceiling");
                let meet = match candidate.as_deref() {
                    Some(selection) => crate::surface::resolve_effective_tool_root(
                        "conformance",
                        Some(selection),
                        Some(ceiling),
                    ),
                    None => {
                        let mut tools = gents_documents::Tools {
                            host: Some(gents_documents::HostTools {
                                root: Some(input.authored.clone()),
                                files: Some(gents_documents::FileTools {
                                    mode: gents_documents::FileToolMode::ReadOnly,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            ..Default::default()
                        };
                        crate::surface::canonicalize_tools_root(&mut tools, &policy, None).and_then(
                            |()| {
                                let configured = tools
                                    .host
                                    .and_then(|host| host.root)
                                    .filter(|root| !root.trim().is_empty());
                                crate::surface::resolve_effective_tool_root(
                                    "conformance",
                                    configured.as_deref().map(std::path::Path::new),
                                    Some(ceiling),
                                )
                            },
                        )
                    }
                };
                match meet {
                    Ok(effective) => (
                        true,
                        effective.map(|root| {
                            modeled(&std::fs::canonicalize(root).expect("canonical effective root"))
                        }),
                    ),
                    Err(_) => (false, None),
                }
            }
            other => panic!("unhandled Lean root operation {other}"),
        };
        case.check(AdmissionOutput {
            blank,
            published,
            expected,
            effective,
        });
    }
}
