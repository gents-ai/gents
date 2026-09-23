//! The builtin check registry.
//!
//! An eval definition names a check; it never carries code. A [`Check`] is a
//! pure function from a check ref's params and one stage's evidence to a
//! [`CheckVerdict`], so grading a trial is reproducible from its evidence
//! alone.

pub mod captured_rows_count;
pub mod finding_count;
pub mod finding_pairs_unique;
pub mod finding_state_counts;
pub mod finding_text_excludes;
pub mod findings_match;
pub(crate) mod mailbox;
pub mod mailbox_item_count;
pub mod mailbox_open_row;
pub mod mailbox_text_excludes;
pub mod payload_well_formed;

use std::collections::BTreeMap;

use serde_json::Value;

use crate::eval::checks::captured_rows_count::CapturedRowsCount;
use crate::eval::checks::finding_count::FindingCount;
use crate::eval::checks::finding_pairs_unique::FindingPairsUnique;
use crate::eval::checks::finding_state_counts::FindingStateCounts;
use crate::eval::checks::finding_text_excludes::FindingTextExcludes;
use crate::eval::checks::findings_match::FindingsMatch;
use crate::eval::checks::mailbox_item_count::MailboxItemCount;
use crate::eval::checks::mailbox_open_row::MailboxOpenRow;
use crate::eval::checks::mailbox_text_excludes::MailboxTextExcludes;
use crate::eval::checks::payload_well_formed::PayloadWellFormed;
use crate::eval::runner::executor::StageEvidence;
use crate::eval::OutcomeKind;

/// Bumped when the builtin set changes in a way that could move a score.
/// Frozen into every run's origin.
/// "2": the nine mailbox checks of M3 (spec 3a).
pub const CHECK_REGISTRY_VERSION: &str = "2";

/// What one check concluded about one stage. `score_bp` is `None` when the
/// verdict is not evidence about the subject, such as a grader fault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckVerdict {
    pub kind: OutcomeKind,
    pub score_bp: Option<u32>,
    /// An object carrying at least a `reason_code`.
    pub raw: Value,
    pub feedback: Option<String>,
}

pub trait Check: Send + Sync {
    fn name(&self) -> &'static str;

    /// Bumped when the same params would yield a different verdict.
    fn version(&self) -> &'static str;

    /// Pure. `params` is the check ref's params; `stage` is the stage's
    /// evidence. Never panics on either: malformed params are a grader
    /// outcome.
    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict;
}

/// The checks a definition may name, by name.
pub struct CheckRegistry {
    checks: BTreeMap<&'static str, Box<dyn Check>>,
}

impl CheckRegistry {
    pub fn builtin() -> Self {
        let mut registry = Self {
            checks: BTreeMap::new(),
        };
        let checks: [Box<dyn Check>; 10] = [
            Box::new(CapturedRowsCount),
            Box::new(FindingCount),
            Box::new(FindingPairsUnique),
            Box::new(FindingStateCounts),
            Box::new(FindingTextExcludes),
            Box::new(FindingsMatch),
            Box::new(MailboxItemCount),
            Box::new(MailboxOpenRow),
            Box::new(MailboxTextExcludes),
            Box::new(PayloadWellFormed),
        ];
        for check in checks {
            registry.register(check);
        }
        registry
    }

    pub fn get(&self, name: &str) -> Option<&dyn Check> {
        self.checks.get(name).map(AsRef::as_ref)
    }

    /// Every registered name, sorted.
    pub fn names(&self) -> Vec<&'static str> {
        self.checks.keys().copied().collect()
    }

    /// Registers `check` over the builtin set. Test-only: the shipped
    /// registry is the builtin one, and a definition may only name a check
    /// that ships with it.
    #[cfg(test)]
    pub(crate) fn with(mut self, check: Box<dyn Check>) -> Self {
        self.register(check);
        self
    }

    fn register(&mut self, check: Box<dyn Check>) {
        self.checks.insert(check.name(), check);
    }
}

impl Default for CheckRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_registry_holds_every_check_under_its_own_name_at_version_one() {
        let registry = CheckRegistry::builtin();
        assert_eq!(
            registry.names(),
            vec![
                "captured_rows_count",
                "finding_count",
                "finding_pairs_unique",
                "finding_state_counts",
                "finding_text_excludes",
                "findings_match",
                "mailbox_item_count",
                "mailbox_open_row",
                "mailbox_text_excludes",
                "payload_well_formed",
            ]
        );
        for name in registry.names() {
            let check = registry.get(name).expect("a registered check");
            assert_eq!((check.name(), check.version()), (name, "1"));
        }
        assert!(
            registry.get("no_findings_for_correlation").is_none(),
            "not until a case needs it"
        );
        assert_eq!(CHECK_REGISTRY_VERSION, "2");
    }
}
