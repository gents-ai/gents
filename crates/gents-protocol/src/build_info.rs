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
    /// The commit this binary was built from, when the build recorded one.
    pub commit: Option<String>,
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
    /// This process's build, from the crate version, the commit the build
    /// recorded when it knew one, and the core packs it was given.
    ///
    /// The packs are passed in rather than read here, so this type stays the
    /// wire's: a caller supplies what its own build carries.
    pub fn current(core_packs: Vec<CorePackBuild>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            commit: option_env!("GENTS_BUILD_COMMIT").map(str::to_owned),
            core_packs,
        }
    }

    /// One line for a prompt, or for a handshake's log.
    pub fn summary(&self) -> String {
        format!(
            "gents {} ({})",
            self.version,
            self.commit.as_deref().unwrap_or("unknown commit")
        )
    }

    /// Whether two builds plan the same pack install: the same core packs decide
    /// what an install contains, so a differing set is a differing plan before
    /// anything is read.
    pub fn plans_the_same_installs(&self, other: &Self) -> bool {
        self.core_packs == other.core_packs
    }

    /// What differs between two builds, in one line each. Empty when they
    /// agree, so a caller can use it as the answer to "are we the same build?".
    pub fn differences(&self, other: &Self) -> Vec<String> {
        let mut differences = Vec::new();
        if self.version != other.version {
            differences.push(format!(
                "version: this is {}, the other is {}",
                self.version, other.version
            ));
        }
        if self.commit != other.commit {
            differences.push(format!(
                "commit: this is {}, the other is {}",
                self.commit.as_deref().unwrap_or("unknown"),
                other.commit.as_deref().unwrap_or("unknown")
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
