//! What this build is, so two parties can tell whether they are the same one.
//!
//! A pack install is prepared twice — once here and once by the runtime — and
//! the two plans must agree exactly. When they do not, the first question is
//! whether the two parties are even the same build, and until this existed a
//! stale `gents` on `PATH` was indistinguishable from a real disagreement.

use serde::{Deserialize, Serialize};

/// The running build's identity: the crate version, the commit it was built
/// from when the build knew one, and the core packs compiled into it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub version: String,
    /// The commit this binary was built from, from the build script that
    /// recorded it. `None` only for a build made outside a git checkout.
    pub git_sha: Option<String>,
    /// Whether the checkout was dirty at build time. A dirty build has no
    /// stable identity -- the same sha can mean different code -- so two dirty
    /// builds are not the same build.
    pub git_dirty: Option<bool>,
    /// The core packs compiled in, which decide what a pack install plans.
    pub core_packs: Vec<CorePackBuild>,
}

/// One core pack this build carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorePackBuild {
    /// The pack's coordinate, `namespace/name`.
    pub coordinate: String,
    /// The digest of the archive bytes compiled in. Two builds carrying a
    /// different core pack differ here, and that is what makes their pack
    /// installations differ.
    pub digest: String,
}

impl BuildInfo {
    /// This process's build from the crate version and the core packs a caller
    /// supplies. The git facts are left unset, because `gents-protocol` has no
    /// build script -- a caller with one states them through
    /// [`Self::from_parts`]. A build from here is therefore unverifiable, which
    /// is what [`Self::UNVERIFIABLE`] states explicitly.
    pub fn current(core_packs: Vec<CorePackBuild>) -> Self {
        Self::from_parts(env!("CARGO_PKG_VERSION").to_owned(), None, None, core_packs)
    }

    /// A build whose facts are unknown. `current` needs a build script, and
    /// `gents-protocol` has none, so this is what a caller that never recorded
    /// its build gets -- never a quiet guess from an always-empty env var.
    pub const UNVERIFIABLE: Self = Self {
        version: String::new(),
        git_sha: None,
        git_dirty: None,
        core_packs: Vec::new(),
    };

    /// [`Self::current`] with the facts stated, so a test can build the pairs a
    /// comparison is about without a build script.
    pub fn from_parts(
        version: String,
        git_sha: Option<String>,
        git_dirty: Option<bool>,
        core_packs: Vec<CorePackBuild>,
    ) -> Self {
        Self {
            version,
            git_sha,
            git_dirty,
            core_packs,
        }
    }

    /// One line to print when two builds agree only because both are dirty, so
    /// an operator knows the install was accepted on a weaker guarantee.
    pub fn dirty_warning(&self, other: &Self) -> Option<String> {
        let both_dirty = self.git_dirty == Some(true) && other.git_dirty == Some(true);
        if both_dirty && self.git_sha.is_some() && self.git_sha == other.git_sha {
            Some(format!(
                "both this client and the runtime are dirty builds of {}, so they were accepted as \
                 the same build on the same commit and core packs; a clean install of each is the \
                 stronger check",
                self.git_sha.as_deref().unwrap_or("an unrecorded commit")
            ))
        } else {
            None
        }
    }

    /// Whether this build's identity can be compared at all. A build made
    /// outside a git checkout has no sha, and two of those must never be taken
    /// for the same build: `None == None` is not a match.
    pub fn is_verifiable(&self) -> bool {
        self.git_sha.is_some()
    }

    /// One line for a prompt, or for a handshake's log.
    pub fn summary(&self) -> String {
        let mut revision = self
            .git_sha
            .clone()
            .unwrap_or_else(|| "unrecorded commit".to_owned());
        if self.git_dirty == Some(true) {
            revision.push_str("-dirty");
        }
        format!("gents {} ({revision})", self.version)
    }

    /// Whether two builds plan the same pack install. A build with no commit is
    /// never "the same build" as another.
    pub fn plans_the_same_installs(&self, other: &Self) -> bool {
        self.is_verifiable() && other.is_verifiable() && self.differences(other).is_empty()
    }

    /// What differs between two builds, in one line each: version, then commit,
    /// then the core packs. Empty only when both are verifiable and agree.
    ///
    /// Two **dirty** builds of the same commit with the same core packs agree,
    /// and [`Self::dirty_warning`] says so out loud: a developer must be able to
    /// install from their own binary into the runtime they just built, and
    /// `build.rs` counts only tracked modifications (`--untracked-files=no`), so
    /// two dirty builds of one commit are usually the same code. A dirty build
    /// against a *clean* one of the same commit differs, because one of them
    /// certainly carries changes the other does not.
    pub fn differences(&self, other: &Self) -> Vec<String> {
        let mut differences = Vec::new();
        if !self.is_verifiable() && !other.is_verifiable() {
            differences.push(
                "neither build records a commit, so the two cannot be verified as the same build"
                    .to_owned(),
            );
            return differences;
        }
        if self.version != other.version {
            differences.push(format!(
                "version: this is {}, the other is {}",
                self.version, other.version
            ));
        }
        if self.git_sha != other.git_sha {
            differences.push(format!(
                "commit: this is {}, the other is {}",
                self.git_sha.as_deref().unwrap_or("unrecorded"),
                other.git_sha.as_deref().unwrap_or("unrecorded")
            ));
        }
        // Dirty beyond the sha and the packs: a dirty build against a clean one
        // of the same commit means one carries changes the other does not.
        let both_dirty = self.git_dirty == Some(true) && other.git_dirty == Some(true);
        if self.git_dirty != other.git_dirty && !both_dirty {
            differences.push(format!(
                "working tree: this was built {}, the other {}",
                describe_dirty(self.git_dirty),
                describe_dirty(other.git_dirty)
            ));
        }
        if self.core_packs != other.core_packs {
            differences.push(format!(
                "core packs: this has {}, the other has {}",
                describe_packs(&self.core_packs),
                describe_packs(&other.core_packs)
            ));
        }
        differences
    }
}

/// `clean`, `dirty`, or `with an unrecorded tree state`, for a comparison's text.
fn describe_dirty(dirty: Option<bool>) -> &'static str {
    match dirty {
        Some(true) => "dirty",
        Some(false) => "clean",
        None => "with an unrecorded tree state",
    }
}

/// `gents/web 1.1.2, gents/script 1.0.4`, in the order it was built.
fn describe_packs(packs: &[CorePackBuild]) -> String {
    if packs.is_empty() {
        return "none".to_owned();
    }
    packs
        .iter()
        .map(|pack| format!("{} {}", pack.coordinate, pack.digest))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(coordinate: &str, digest: &str) -> CorePackBuild {
        CorePackBuild {
            coordinate: coordinate.to_owned(),
            digest: digest.to_owned(),
        }
    }

    /// A refusal from a deliberately mismatched build names both builds. A
    /// stale `gents` on `PATH` is otherwise indistinguishable from a real plan
    /// difference, which is what made the install refusal so hard to read.
    #[test]
    fn a_mismatched_build_names_both_builds_and_what_differs() {
        let client = BuildInfo {
            version: "1.0.0-rc2".to_owned(),
            git_sha: Some("aaaaaaa".to_owned()),
            git_dirty: Some(false),
            core_packs: vec![pack("gents/web", "sha256:1111")],
        };
        let runtime = BuildInfo {
            version: "1.0.0-rc2".to_owned(),
            git_sha: Some("bbbbbbb".to_owned()),
            git_dirty: Some(false),
            core_packs: vec![pack("gents/web", "sha256:2222")],
        };
        let differences = client.differences(&runtime);
        assert_eq!(differences.len(), 2, "{differences:?}");
        assert!(
            differences[0].contains("aaaaaaa") && differences[0].contains("bbbbbbb"),
            "the commit difference names both: {}",
            differences[0]
        );
        assert!(
            differences[1].contains("sha256:1111") && differences[1].contains("sha256:2222"),
            "the pack difference names both digests: {}",
            differences[1]
        );
        // Both summaries are available for the message.
        assert!(
            client.summary().contains("1.0.0-rc2"),
            "{}",
            client.summary()
        );
        assert!(runtime.summary().contains("1.0.0-rc2"));
        // And they do not plan the same installs.
        assert!(!client.plans_the_same_installs(&runtime));
    }

    /// A developer must be able to install from their own (dirty) binary into
    /// the (dirty) runtime they just built: same commit and same core packs is
    /// accepted, and said out loud.
    #[test]
    fn two_dirty_builds_of_one_commit_agree_and_say_so() {
        let web = pack("gents/web", "sha256:1111");
        let client = BuildInfo::from_parts(
            "1.0.0-rc2".to_owned(),
            Some("aaaaaaa".to_owned()),
            Some(true),
            vec![web.clone()],
        );
        let runtime = client.clone();
        assert!(
            client.differences(&runtime).is_empty(),
            "two dirty builds of one commit agree: {:?}",
            client.differences(&runtime)
        );
        assert!(client.plans_the_same_installs(&runtime));
        let warning = client
            .dirty_warning(&runtime)
            .expect("the weaker guarantee is stated");
        assert!(warning.contains("dirty"), "{warning}");
        assert!(warning.contains("aaaaaaa"), "{warning}");

        // Same commit, but one is clean: that is a real difference.
        let clean = BuildInfo::from_parts(
            "1.0.0-rc2".to_owned(),
            Some("aaaaaaa".to_owned()),
            Some(false),
            vec![web.clone()],
        );
        assert!(!client.differences(&clean).is_empty());
        assert!(clean.dirty_warning(&client).is_none());

        // Dirty but different core packs: still a difference.
        let other_packs = BuildInfo::from_parts(
            "1.0.0-rc2".to_owned(),
            Some("aaaaaaa".to_owned()),
            Some(true),
            vec![pack("gents/web", "sha256:2222")],
        );
        assert!(!client.differences(&other_packs).is_empty());

        // Two clean builds of one commit say nothing extra.
        assert!(clean.dirty_warning(&clean).is_none());
    }

    /// The same build says nothing, so a plain plan difference is not buried
    /// under build detail.
    #[test]
    fn the_same_build_reports_no_difference() {
        let build = BuildInfo {
            version: "1.0.0-rc2".to_owned(),
            git_sha: Some("aaaaaaa".to_owned()),
            git_dirty: Some(false),
            core_packs: vec![pack("gents/web", "sha256:1111")],
        };
        assert!(build.differences(&build.clone()).is_empty());
        assert!(build.plans_the_same_installs(&build.clone()));
    }

    /// A build that recorded no commit says so rather than guessing, and a
    /// difference from it is still named.
    #[test]
    fn an_unknown_commit_is_said_not_guessed() {
        let unknown = BuildInfo {
            version: "1.0.0-rc2".to_owned(),
            git_sha: None,
            git_dirty: Some(false),
            core_packs: Vec::new(),
        };
        assert!(
            unknown.summary().contains("unrecorded commit"),
            "{}",
            unknown.summary()
        );
        // A build that recorded a commit differs from it by that commit, named
        // on both sides including the unknown one.
        let recorded = BuildInfo {
            version: "1.0.0-rc2".to_owned(),
            git_sha: Some("aaaaaaa".to_owned()),
            git_dirty: Some(false),
            core_packs: Vec::new(),
        };
        let differences = unknown.differences(&recorded);
        assert!(
            differences
                .iter()
                .any(|difference| difference.contains("commit")
                    && difference.contains("unrecorded")
                    && difference.contains("aaaaaaa")),
            "a commit difference from a build that recorded none is still named: {differences:?}"
        );
    }
}
