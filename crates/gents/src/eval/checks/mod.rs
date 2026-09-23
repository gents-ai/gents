//! The builtin check registry.
//!
//! An eval definition names a check; it never carries code. A [`Check`] is a
//! pure function from a check ref's params and one stage's evidence to a
//! [`CheckVerdict`], so grading a trial is reproducible from its evidence
//! alone.

pub mod captured_rows_count;

use std::collections::BTreeMap;

use serde_json::Value;

use crate::eval::checks::captured_rows_count::CapturedRowsCount;
use crate::eval::runner::executor::StageEvidence;
use crate::eval::OutcomeKind;

/// Bumped when the builtin set changes in a way that could move a score.
/// Frozen into every run's origin.
pub const CHECK_REGISTRY_VERSION: &str = "1";

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
        registry.register(Box::new(CapturedRowsCount));
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
    fn the_builtin_registry_holds_the_seed_check_under_its_own_name() {
        let registry = CheckRegistry::builtin();
        assert_eq!(registry.names(), vec!["captured_rows_count"]);
        let check = registry.get("captured_rows_count").expect("the seed check");
        assert_eq!(
            (check.name(), check.version()),
            ("captured_rows_count", "1")
        );
        assert!(registry.get("no_such_check").is_none());
    }
}
