//! The read-only asset mounts a plugin call receives: the pack tier from
//! materialized home installs and the project tier under `project_roots`.

use std::collections::BTreeMap;
use std::path::Path;

use gents_protocol::job::{AssetMount, JobPolicy, PROJECT_MOUNT_CLASS};

use super::store::InstalledPlugin;
use crate::pack::home_install::recorded_provenance;
use crate::pack::{list_home_installs, InstalledVia};
use crate::plugin_job::origin::trusted_hint;

#[derive(Debug, thiserror::Error)]
pub enum MountsError {
    #[error(transparent)]
    Home(#[from] crate::pack::home_install::HomeInstallError),
    #[error("dependency {coordinate:?} of {record_coordinate:?} records assets dir {assets:?}, whose name is not a 64-hex digest")]
    DependencyDigest {
        coordinate: String,
        record_coordinate: String,
        assets: String,
    },
}

type Result<T, E = MountsError> = std::result::Result<T, E>;

/// Project directories under `cwd` that join the manifold, in order.
const PROJECT_DIRS: [&str; 2] = [".gents/workflows", ".grok/workflows"];

/// Lean `projectPreopen`: some root equals `cwd` or is a whole-path-component
/// prefix of it.
pub fn project_preopen(cwd: &str, project_roots: &[String]) -> bool {
    project_roots.iter().any(|root| {
        cwd == root
            || cwd
                .strip_prefix(root.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// What the manifold grants over a mount: a mount is never read-write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountAccess {
    Read,
}

/// [`resolve_access`]'s decision for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountResolution {
    /// Per mount, in order: `None` when the host preopens nothing for it.
    pub access: Vec<Option<MountAccess>>,
    pub admitted: bool,
}

/// Lean `mountAccess` and `mountCallAdmitted`. Each mount carries Lean's
/// `installed` flag; a project mount is read whether installed or not. A
/// read-write bind with any mount, an uninstalled one included, is refused.
pub fn resolve_access(rw_bind: bool, mounts: &[(AssetMount, bool)]) -> MountResolution {
    MountResolution {
        access: mounts
            .iter()
            .map(|(mount, installed)| {
                (*installed || mount.class == PROJECT_MOUNT_CLASS).then_some(MountAccess::Read)
            })
            .collect(),
        admitted: !(rw_bind && !mounts.is_empty()),
    }
}

/// The mounts one call of `plugin` receives. Pack tier: each class the plugin
/// declares in `asset_mounts`, from every home install whose materialized dir
/// holds `<class>/`, pinned to that record's digest dir. Project tier: only
/// for a job plugin (`policy`) whose `cwd` lies under a `project_roots` entry.
pub fn resolve(
    home: &Path,
    plugin: &InstalledPlugin,
    policy: Option<&JobPolicy>,
    cwd: Option<&Path>,
) -> Result<Vec<AssetMount>> {
    let trusted_packs = policy.map_or(&[][..], |policy| policy.trusted_packs.as_slice());
    let mut mounts = Vec::new();
    if !plugin.declaration.asset_mounts.is_empty() {
        let records = list_home_installs(home)?;
        let installed_via: BTreeMap<&str, InstalledVia> = records
            .iter()
            .map(|record| {
                let via = recorded_provenance(record.kind.clone(), Some(record.installed_via));
                (record.coordinate.as_str(), via)
            })
            .collect();
        for declared in &plugin.declaration.asset_mounts {
            let mount = |pack: &str, digest: String, dir: std::path::PathBuf| AssetMount {
                pack: pack.to_owned(),
                class: declared.class.clone(),
                digest,
                path: dir.to_string_lossy().into_owned(),
                trusted: trusted_hint(pack, &declared.class, &installed_via, trusted_packs),
            };
            for record in records.iter().filter(|record| !record.assets.is_empty()) {
                let dir = home.join(&record.assets).join(&declared.class);
                if dir.is_dir() {
                    mounts.push(mount(&record.coordinate, record.digest.clone(), dir));
                }
            }
            // Own records come first, so a path both name keeps the own entry.
            for record in &records {
                for (coordinate, assets) in &record.dependency_assets {
                    let hex = Path::new(assets)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| {
                            name.len() == 64
                                && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                        })
                        .ok_or_else(|| MountsError::DependencyDigest {
                            coordinate: coordinate.clone(),
                            record_coordinate: record.coordinate.clone(),
                            assets: assets.clone(),
                        })?;
                    let dir = home.join(assets).join(&declared.class);
                    let path = dir.to_string_lossy();
                    if dir.is_dir() && !mounts.iter().any(|mounted| mounted.path == path) {
                        mounts.push(mount(coordinate, format!("sha256:{hex}"), dir));
                    }
                }
            }
        }
    }
    if let (Some(policy), Some(cwd)) = (policy, cwd) {
        mounts.extend(project_mounts(policy, cwd));
    }
    Ok(mounts)
}

fn project_mounts(policy: &JobPolicy, cwd: &Path) -> Vec<AssetMount> {
    let Ok(cwd) = cwd.canonicalize() else {
        return Vec::new();
    };
    let roots: Vec<String> = policy
        .project_roots
        .iter()
        .filter_map(|root| Path::new(root).canonicalize().ok())
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    if !project_preopen(&cwd.to_string_lossy(), &roots) {
        return Vec::new();
    }
    PROJECT_DIRS
        .iter()
        .filter_map(|relative| {
            let real = cwd.join(relative).canonicalize().ok()?;
            if !real.is_dir() {
                return None;
            }
            if !real.starts_with(&cwd) {
                tracing::warn!(dir = %cwd.join(relative).display(), "project workflow dir resolves outside its cwd; not mounted");
                return None;
            }
            Some(AssetMount {
                pack: String::new(),
                class: PROJECT_MOUNT_CLASS.into(),
                digest: String::new(),
                path: real.to_string_lossy().into_owned(),
                trusted: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{write_home_install, HomePackInstall, InstalledVia, PackKind};
    use crate::plugin::store::InstalledPlugin;
    use gents_protocol::job::{AssetMount, AssetMountDeclaration, JobPolicy};
    use std::path::Path;

    fn plugin(classes: &[&str]) -> InstalledPlugin {
        InstalledPlugin {
            namespace: "acme".into(),
            name: "engine".into(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            language: "rust".into(),
            declaration: crate::pack::PackPlugin {
                name: "engine".into(),
                asset_mounts: classes
                    .iter()
                    .map(|class| AssetMountDeclaration {
                        class: (*class).into(),
                    })
                    .collect(),
                ..Default::default()
            },
            granted: None,
            instructions: None,
            owner_pack_coordinate: None,
            owner_pack_digest: None,
            model_binding: None,
            data_unique_keys: Default::default(),
            data_collection_fields: Default::default(),
        }
    }

    /// Writes a materialized dir shipping `classes` and the home record
    /// naming it.
    fn install(home: &Path, coordinate: &str, via: InstalledVia, classes: &[&str]) -> String {
        let hex = coordinate.replace('/', "").repeat(2);
        let assets = format!("packs/.materialized/{coordinate}/{hex}");
        for class in classes {
            let dir = home.join(&assets).join(class);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("a.rhai"), "1").unwrap();
        }
        let digest = format!("sha256:{hex:0<64}");
        write_home_install(
            home,
            &HomePackInstall {
                coordinate: coordinate.into(),
                version: "1.0.0".into(),
                digest: digest.clone(),
                kind: PackKind::Documents,
                assets,
                dependency_assets: vec![],
                plugins: vec![],
                ux: Vec::new(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                installed_via: Some(via),
            },
        )
        .unwrap();
        digest
    }

    fn policy(trusted: &[&str], roots: &[&Path]) -> JobPolicy {
        JobPolicy {
            trusted_packs: trusted.iter().map(|pack| (*pack).to_owned()).collect(),
            project_roots: roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect(),
            ..Default::default()
        }
    }

    fn project(path: &Path) -> AssetMount {
        AssetMount {
            pack: String::new(),
            class: "project".into(),
            digest: String::new(),
            path: path.canonicalize().unwrap().to_string_lossy().into_owned(),
            trusted: false,
        }
    }

    #[test]
    fn resolve_access_is_lean_mount_access_and_mount_call_admitted() {
        let mount = |pack: &str, class: &str, digest: &str| AssetMount {
            pack: pack.into(),
            class: class.into(),
            digest: digest.into(),
            path: "/p".into(),
            trusted: false,
        };
        let pack = mount("acme/content", "workflows", "sha256:aa");
        let project = mount("", PROJECT_MOUNT_CLASS, "");
        let mounts = [
            (pack.clone(), true),
            (project.clone(), false),
            (pack.clone(), false),
        ];
        let read = resolve_access(false, &mounts);
        assert_eq!(
            read.access,
            [Some(MountAccess::Read), Some(MountAccess::Read), None]
        );
        assert!(read.admitted);
        assert!(resolve_access(false, &[]).admitted);
        assert!(resolve_access(true, &[]).admitted);
        for mounts in [&mounts[..1], &mounts[2..]] {
            assert!(!resolve_access(true, mounts).admitted);
        }
    }

    #[test]
    fn resolve_lists_only_declared_classes() {
        let home = tempfile::tempdir().unwrap();
        install(
            home.path(),
            "acme/content",
            InstalledVia::Operator,
            &["workflows", "prompts"],
        );
        let mounts = resolve(home.path(), &plugin(&["workflows"]), None, None).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].class, "workflows");
        assert!(resolve(home.path(), &plugin(&[]), None, None)
            .unwrap()
            .is_empty());
        assert!(resolve(home.path(), &plugin(&["skills"]), None, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn resolve_pins_each_mount_to_its_digest_dir() {
        let home = tempfile::tempdir().unwrap();
        let digest = install(
            home.path(),
            "acme/content",
            InstalledVia::Operator,
            &["workflows"],
        );
        let mounts = resolve(home.path(), &plugin(&["workflows"]), None, None).unwrap();
        let hex = "acmecontent".repeat(2);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].pack, "acme/content");
        assert_eq!(mounts[0].digest, digest);
        assert_eq!(
            Path::new(&mounts[0].path),
            home.path()
                .join(format!("packs/.materialized/acme/content/{hex}/workflows"))
        );
    }

    #[test]
    fn trusted_is_true_only_for_operator_packs_listed_in_trusted_packs() {
        let home = tempfile::tempdir().unwrap();
        install(
            home.path(),
            "acme/listed",
            InstalledVia::Operator,
            &["workflows"],
        );
        install(
            home.path(),
            "acme/unlisted",
            InstalledVia::Operator,
            &["workflows"],
        );
        install(
            home.path(),
            "acme/model",
            InstalledVia::SelfConfig,
            &["workflows"],
        );
        let trusted = policy(&["acme/listed", "acme/model"], &[]);
        let mounts = resolve(home.path(), &plugin(&["workflows"]), Some(&trusted), None).unwrap();
        let hints: Vec<(&str, bool)> = mounts
            .iter()
            .map(|mount| (mount.pack.as_str(), mount.trusted))
            .collect();
        assert_eq!(
            hints,
            [
                ("acme/listed", true),
                ("acme/model", false),
                ("acme/unlisted", false)
            ]
        );
        let none = resolve(home.path(), &plugin(&["workflows"]), None, None).unwrap();
        assert!(none.iter().all(|mount| !mount.trusted));
    }

    #[test]
    fn project_tier_appears_only_under_project_roots() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cwd = repo.path().join("repo/sub");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(cwd.join(".gents/workflows")).unwrap();
        std::fs::create_dir_all(cwd.join(".grok/workflows")).unwrap();
        let plugin = plugin(&[]);
        let under = policy(&[], &[&repo.path().join("repo")]);
        let mounts = resolve(home.path(), &plugin, Some(&under), Some(&cwd)).unwrap();
        assert_eq!(
            mounts,
            [
                project(&cwd.join(".gents/workflows")),
                project(&cwd.join(".grok/workflows"))
            ]
        );
        for roots in [vec![], vec![repo.path().join("other")]] {
            let roots: Vec<&Path> = roots.iter().map(|root| root.as_path()).collect();
            let denied = policy(&[], &roots);
            assert!(resolve(home.path(), &plugin, Some(&denied), Some(&cwd))
                .unwrap()
                .is_empty());
        }
        let bare = repo.path().join("repo/bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(resolve(home.path(), &plugin, Some(&under), Some(&bare))
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn project_symlink_escape_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".gents")).unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join(".gents/workflows")).unwrap();
        std::fs::create_dir_all(repo.path().join(".grok/workflows")).unwrap();
        let roots = policy(&[], &[repo.path()]);
        let mounts = resolve(home.path(), &plugin(&[]), Some(&roots), Some(repo.path())).unwrap();
        assert_eq!(mounts, [project(&repo.path().join(".grok/workflows"))]);
    }

    #[test]
    fn non_job_plugins_get_no_project_tier() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".gents/workflows")).unwrap();
        assert!(resolve(home.path(), &plugin(&[]), None, Some(repo.path()))
            .unwrap()
            .is_empty());
    }

    /// Writes a record of `acme/dependent` naming `dependency_assets`.
    fn depend(home: &Path, dependency_assets: &[(&str, &str)]) {
        write_home_install(
            home,
            &HomePackInstall {
                coordinate: "acme/dependent".into(),
                version: "1.0.0".into(),
                digest: format!("sha256:{}", "d".repeat(64)),
                kind: PackKind::Documents,
                assets: String::new(),
                dependency_assets: dependency_assets
                    .iter()
                    .map(|(coordinate, dir)| ((*coordinate).into(), (*dir).into()))
                    .collect(),
                plugins: vec![],
                ux: Vec::new(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                installed_via: Some(InstalledVia::Operator),
            },
        )
        .unwrap();
    }

    #[test]
    fn dependent_with_deps_content_then_engine_mounts_the_content_workflows() {
        let home = tempfile::tempdir().unwrap();
        let content = format!("packs/.materialized/acme/content/{}", "c".repeat(64));
        let engine = format!("packs/.materialized/acme/engine/{}", "e".repeat(64));
        let workflows = home.path().join(&content).join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::create_dir_all(home.path().join(&engine)).unwrap();
        depend(
            home.path(),
            &[("acme/content", &content), ("acme/engine", &engine)],
        );
        let mounts = resolve(home.path(), &plugin(&["workflows"]), None, None).unwrap();
        assert_eq!(
            mounts,
            [AssetMount {
                pack: "acme/content".into(),
                class: "workflows".into(),
                digest: format!("sha256:{}", "c".repeat(64)),
                path: workflows.to_string_lossy().into_owned(),
                trusted: false,
            }]
        );
    }

    #[test]
    fn dependency_mount_listed_by_its_own_record_is_not_repeated() {
        let home = tempfile::tempdir().unwrap();
        let hex = "c".repeat(64);
        let content = format!("packs/.materialized/acme/content/{hex}");
        std::fs::create_dir_all(home.path().join(&content).join("workflows")).unwrap();
        write_home_install(
            home.path(),
            &HomePackInstall {
                coordinate: "acme/content".into(),
                version: "1.0.0".into(),
                digest: format!("sha256:{hex}"),
                kind: PackKind::Documents,
                assets: content.clone(),
                dependency_assets: vec![],
                plugins: vec![],
                ux: Vec::new(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                installed_via: Some(InstalledVia::Operator),
            },
        )
        .unwrap();
        depend(home.path(), &[("acme/content", &content)]);
        let mounts = resolve(home.path(), &plugin(&["workflows"]), None, None).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].pack, "acme/content");
        assert!(!mounts[0].trusted);
    }

    #[test]
    fn dependency_dir_whose_name_is_not_64_lowercase_hex_is_refused() {
        for name in [
            "c".repeat(63),
            "C".repeat(64),
            "g".repeat(64),
            "latest".into(),
        ] {
            let home = tempfile::tempdir().unwrap();
            let content = format!("packs/.materialized/acme/content/{name}");
            std::fs::create_dir_all(home.path().join(&content).join("workflows")).unwrap();
            depend(home.path(), &[("acme/content", &content)]);
            let error = resolve(home.path(), &plugin(&["workflows"]), None, None).unwrap_err();
            assert!(error.to_string().contains("64-hex"), "{name}: {error}");
        }
    }

    #[test]
    fn project_preopen_compares_whole_path_components() {
        let roots = vec!["/work/repo".to_owned()];
        assert!(project_preopen("/work/repo", &roots));
        assert!(project_preopen("/work/repo/sub", &roots));
        assert!(!project_preopen("/work/repository", &roots));
        assert!(!project_preopen("/work/other", &roots));
        assert!(!project_preopen("/work/repo", &[]));
    }
}
