use std::ffi::OsStr;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gents_fs_runner::beneath::decision::{step, Component, Observed, Step, StepInput};
use gents_fs_runner::beneath::{BeneathError, Intent, Located, LookupHook, RootDir};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireComponent {
    Name,
    Parent,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireObserved {
    Directory,
    NonDirectory,
    Missing,
    LinkRelative,
    LinkBeneathRoot,
    LinkOutsideRoot,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum WireStep {
    Ascend,
    Descend,
    Land,
    LandMissing,
    PendingCreate,
    FollowRelative,
    FollowFromRoot,
    RefuseEscape,
    RefuseNotDirectory,
    RefuseNotFound,
    RefuseDanglingLink,
    RefuseLoop,
}

#[test]
fn path_namespace_steps_match_the_lean_model() {
    gents_lean_vocab::families::path_namespace_step::replay::<WireComponent, WireObserved, WireStep>(
        |input| {
            let decided = step(StepInput {
                component: match input.component {
                    WireComponent::Name => Component::Name,
                    WireComponent::Parent => Component::Parent,
                },
                at_root: input.at_root,
                last: input.last,
                create: input.create,
                via_link: input.via_link,
                hops_left: input.hops_left,
                observed: match input.observed {
                    WireObserved::Directory => Observed::Directory,
                    WireObserved::NonDirectory => Observed::NonDirectory,
                    WireObserved::Missing => Observed::Missing,
                    WireObserved::LinkRelative => Observed::LinkRelative,
                    WireObserved::LinkBeneathRoot => Observed::LinkBeneathRoot,
                    WireObserved::LinkOutsideRoot => Observed::LinkOutsideRoot,
                },
            });
            match decided {
                Step::Ascend => WireStep::Ascend,
                Step::Descend => WireStep::Descend,
                Step::Land => WireStep::Land,
                Step::LandMissing => WireStep::LandMissing,
                Step::PendingCreate => WireStep::PendingCreate,
                Step::FollowRelative => WireStep::FollowRelative,
                Step::FollowFromRoot => WireStep::FollowFromRoot,
                Step::RefuseEscape => WireStep::RefuseEscape,
                Step::RefuseNotDirectory => WireStep::RefuseNotDirectory,
                Step::RefuseNotFound => WireStep::RefuseNotFound,
                Step::RefuseDanglingLink => WireStep::RefuseDanglingLink,
                Step::RefuseLoop => WireStep::RefuseLoop,
            }
        },
    );
}

/// The kernel premise the resolver relies on: a lookup of one name relative
/// to a directory handle with `O_NOFOLLOW` does not follow a link at that
/// name, and a held directory handle keeps naming its directory after that
/// directory is renamed and a link to elsewhere takes its place.
#[test]
fn the_kernel_resolves_one_component_beneath_a_held_directory_without_following_it() {
    use rustix::fs::{openat, Mode, OFlags};
    let fixture = Fixture::new();
    let root = openat(
        rustix::fs::CWD,
        fixture.root(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    let held = openat(
        &root,
        "a",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    fixture.swap_a_for_outside_link();

    let refused = openat(
        &root,
        "a",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .expect_err("a link at the name is not followed");
    assert!(
        matches!(refused, rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR),
        "{refused:?}"
    );
    let file = openat(
        &held,
        "f",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::fs::File::from(file), &mut text).unwrap();
    assert_eq!(text, "inside");
}

/// A root with `a/f` inside it and a directory outside it holding `f`.
struct Fixture {
    _root: tempfile::TempDir,
    _outside: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root_dir.path()).unwrap();
        let outside = std::fs::canonicalize(outside_dir.path()).unwrap();
        std::fs::create_dir(root.join("a")).unwrap();
        std::fs::write(root.join("a/f"), "inside").unwrap();
        std::fs::write(outside.join("f"), "secret").unwrap();
        Self {
            _root: root_dir,
            _outside: outside_dir,
            root,
            outside,
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    /// Another process moves `a` aside and links its name out of the root.
    fn swap_a_for_outside_link(&self) {
        std::fs::rename(self.root.join("a"), self.root.join("a.held")).unwrap();
        symlink(&self.outside, self.root.join("a")).unwrap();
    }

    fn outside_untouched(&self) {
        let mut names = std::fs::read_dir(&self.outside)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["f"], "nothing is created outside the root");
        assert_eq!(
            std::fs::read_to_string(self.outside.join("f")).unwrap(),
            "secret"
        );
    }

    /// A resolver that swaps `a` for the outside link just before it looks
    /// up `name` in `dir`.
    fn root_swapping_before(&self, dir: PathBuf, name: &str) -> RootDir {
        let name = name.to_owned();
        let root = self.root.clone();
        let outside = self.outside.clone();
        let fired = Arc::new(Mutex::new(false));
        let hook: LookupHook = Arc::new(move |held: &Path, looked_up: &OsStr| {
            let mut fired = fired.lock().unwrap();
            if !*fired && held == dir && looked_up == OsStr::new(&name) {
                *fired = true;
                std::fs::rename(root.join("a"), root.join("a.held")).unwrap();
                symlink(&outside, root.join("a")).unwrap();
            }
        });
        RootDir::open(&self.root).unwrap().with_lookup_hook(hook)
    }

    /// Each point in the walk of `a/<leaf>` where another process can swap.
    fn swap_points(&self) -> [(PathBuf, &'static str); 2] {
        [(self.root.clone(), "a"), (self.root.join("a"), "f")]
    }

    fn locate(&self, root: &RootDir, path: &str, intent: Intent) -> Result<Located, BeneathError> {
        root.locate_relative(Path::new(path), intent)
    }
}

#[test]
fn a_read_never_reaches_outside_the_root_when_a_component_is_swapped() {
    for point in [0, 1, 2] {
        let fixture = Fixture::new();
        let outcome = if point < 2 {
            let (dir, name) = fixture.swap_points()[point].clone();
            let root = fixture.root_swapping_before(dir, name);
            fixture
                .locate(&root, "a/f", Intent::Existing)
                .and_then(|located| located.read())
        } else {
            let root = RootDir::open(fixture.root()).unwrap();
            let located = fixture.locate(&root, "a/f", Intent::Existing).unwrap();
            fixture.swap_a_for_outside_link();
            located.read()
        };
        match outcome {
            Ok(bytes) => assert_eq!(bytes, b"inside", "swap point {point}"),
            Err(BeneathError::Outside { .. }) => assert_eq!(point, 0),
            Err(error) => panic!("swap point {point}: {error}"),
        }
        fixture.outside_untouched();
    }
}

#[test]
fn a_write_never_reaches_outside_the_root_when_a_component_is_swapped() {
    for target in ["a/f", "a/new.txt", "a/deeper/new.txt"] {
        for point in [0, 1, 2] {
            let fixture = Fixture::new();
            let outcome = if point < 2 {
                let (dir, _) = fixture.swap_points()[point].clone();
                let name = if point == 0 {
                    "a"
                } else {
                    target
                        .strip_prefix("a/")
                        .unwrap()
                        .split('/')
                        .next()
                        .unwrap()
                };
                let root = fixture.root_swapping_before(dir, name);
                fixture
                    .locate(&root, target, Intent::Create)
                    .and_then(|located| located.write(b"written"))
            } else {
                let root = RootDir::open(fixture.root()).unwrap();
                let located = fixture.locate(&root, target, Intent::Create).unwrap();
                fixture.swap_a_for_outside_link();
                located.write(b"written")
            };
            match outcome {
                Ok(()) => {
                    let below = target.strip_prefix("a/").unwrap();
                    assert_eq!(
                        std::fs::read(fixture.root().join("a.held").join(below)).unwrap(),
                        b"written",
                        "{target} at swap point {point} lands in the directory it resolved"
                    );
                }
                Err(BeneathError::Outside { .. }) => assert_eq!(point, 0, "{target}"),
                Err(error) => panic!("{target} at swap point {point}: {error}"),
            }
            fixture.outside_untouched();
        }
    }
}

#[test]
fn a_rename_never_reaches_outside_the_root_when_a_component_is_swapped() {
    for point in [0, 1, 2] {
        let fixture = Fixture::new();
        let outcome = if point < 2 {
            let (dir, name) = fixture.swap_points()[point].clone();
            let root = fixture.root_swapping_before(dir, name);
            fixture
                .locate(&root, "a/f", Intent::Existing)
                .and_then(|from| {
                    let to = fixture.locate(&root, "a/g", Intent::Create)?;
                    from.rename_to(&to)
                })
        } else {
            let root = RootDir::open(fixture.root()).unwrap();
            let from = fixture.locate(&root, "a/f", Intent::Existing).unwrap();
            let to = fixture.locate(&root, "a/g", Intent::Create).unwrap();
            fixture.swap_a_for_outside_link();
            from.rename_to(&to)
        };
        match outcome {
            Ok(()) => assert_eq!(
                std::fs::read(fixture.root().join("a.held/g")).unwrap(),
                b"inside"
            ),
            Err(BeneathError::Outside { .. }) => assert!(point < 2),
            Err(error) => panic!("swap point {point}: {error}"),
        }
        fixture.outside_untouched();
    }
}

#[test]
fn a_target_swapped_for_a_link_after_resolution_is_refused_not_followed() {
    let fixture = Fixture::new();
    let root = RootDir::open(fixture.root()).unwrap();
    let located = fixture.locate(&root, "a/f", Intent::Existing).unwrap();
    std::fs::remove_file(fixture.root().join("a/f")).unwrap();
    symlink(fixture.outside.join("f"), fixture.root().join("a/f")).unwrap();

    assert!(matches!(located.read(), Err(BeneathError::Changed { .. })));
    assert!(matches!(
        located.write(b"written"),
        Err(BeneathError::Changed { .. })
    ));
    let to = fixture.locate(&root, "a/g", Intent::Create).unwrap();
    located.rename_to(&to).unwrap();
    assert_eq!(
        std::fs::read_link(fixture.root().join("a/g")).unwrap(),
        fixture.outside.join("f"),
        "a rename moves the link itself"
    );
    fixture.outside_untouched();
}

#[test]
fn links_inside_the_root_are_followed_and_links_out_of_it_are_refused() {
    let fixture = Fixture::new();
    let root_path = fixture.root().to_path_buf();
    symlink("a", root_path.join("relative")).unwrap();
    symlink(root_path.join("a/f"), root_path.join("absolute")).unwrap();
    symlink(fixture.outside.join("f"), root_path.join("leak")).unwrap();
    symlink("../..", root_path.join("a/up")).unwrap();
    symlink(root_path.join("a/missing"), root_path.join("dangling")).unwrap();
    let root = RootDir::open(&root_path).unwrap();

    let read = |path: &str| fixture.locate(&root, path, Intent::Existing)?.read();
    assert_eq!(read("relative/f").unwrap(), b"inside");
    assert_eq!(read("absolute").unwrap(), b"inside");
    assert_eq!(read("a/../a/f").unwrap(), b"inside");
    assert!(matches!(read("leak"), Err(BeneathError::Outside { .. })));
    assert!(matches!(read("a/up/x"), Err(BeneathError::Outside { .. })));
    assert!(matches!(read("../x"), Err(BeneathError::Outside { .. })));
    assert!(matches!(
        root.locate(&fixture.outside.join("f"), &root_path, Intent::Existing),
        Err(BeneathError::Outside { .. })
    ));
    assert!(matches!(
        fixture.locate(&root, "dangling", Intent::Create),
        Err(BeneathError::DanglingLink { .. })
    ));
    fixture.outside_untouched();
}

#[test]
fn a_grep_walk_never_reads_outside_the_root() {
    let fixture = Fixture::new();
    let root_path = fixture.root().to_path_buf();
    symlink(fixture.outside.join("f"), root_path.join("leak")).unwrap();
    symlink(root_path.join("a/f"), root_path.join("alias")).unwrap();

    let matched = |root: RootDir, pattern: &str| -> Vec<String> {
        let output = gents_fs_runner::execute_request_in_root(
            root,
            gents_fs_runner::protocol::NativeFsRunnerRequest::Grep(
                gents_fs_runner::protocol::GrepArgs {
                    pattern: pattern.to_string(),
                    path: None,
                    case_sensitive: true,
                    max_matches: 100,
                    raw_json: true,
                    max_entries_visited: None,
                    max_bytes_read: None,
                    max_wall_ms: None,
                },
            ),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        value["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap().to_owned())
            .collect()
    };

    assert!(matched(RootDir::open(&root_path).unwrap(), "secret").is_empty());
    assert_eq!(
        matched(RootDir::open(&root_path).unwrap(), "inside"),
        ["a/f", "alias"]
    );
    let swapped = fixture.root_swapping_before(root_path.clone(), "a");
    assert!(matched(swapped, "secret").is_empty());
    fixture.outside_untouched();
}
