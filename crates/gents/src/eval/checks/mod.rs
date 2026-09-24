//! The builtin check registry.
//!
//! An eval definition names a check; it never carries code. A [`Check`] is a
//! pure function from a check ref's params and one stage's evidence to a
//! [`CheckVerdict`], so grading a trial is reproducible from its evidence
//! alone.

pub mod captured_rows_count;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
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

/// What a check tells an author about itself. Rendered into the catalog an
/// eval author drafts against; never read by `evaluate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckDescription {
    pub name: String,
    pub version: String,
    pub summary: String,
    /// JSON Schema (draft 2020-12) for the check ref's `params`.
    pub params_schema: Value,
    /// What the check reads: `"capture:documents"`, `"capture:files"`,
    /// `"stage:terminal_state"`, or a field path such as `"rows.payload"`.
    pub reads: Vec<String>,
    /// `(reason_code, one line)` for every code `raw.reason_code` can carry.
    pub reason_codes: Vec<(String, String)>,
}

pub trait Check: Send + Sync {
    fn name(&self) -> &'static str;

    /// Bumped when the same params would yield a different verdict.
    fn version(&self) -> &'static str;

    /// Pure. `params` is the check ref's params; `stage` is the stage's
    /// evidence. Never panics on either: malformed params are a grader
    /// outcome.
    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict;

    /// The check's entry in the author-facing catalog.
    fn describe(&self) -> CheckDescription;
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

    /// Every registered check's description, sorted by name.
    pub fn catalog(&self) -> Vec<CheckDescription> {
        self.checks.values().map(|check| check.describe()).collect()
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

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn every_builtin_check_describes_itself_with_a_schema_and_reason_codes() {
        let catalog = CheckRegistry::builtin().catalog();
        assert_eq!(
            catalog.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            CheckRegistry::builtin().names()
        );
        for check in &catalog {
            assert_eq!(check.params_schema["type"], "object", "{}", check.name);
            assert!(!check.summary.is_empty(), "{}", check.name);
            assert!(!check.reason_codes.is_empty(), "{}", check.name);
        }
    }

    #[test]
    fn captured_rows_count_schema_accepts_its_params_and_rejects_unknown_fields() {
        let schema = CheckRegistry::builtin()
            .get("captured_rows_count")
            .unwrap()
            .describe()
            .params_schema;
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&serde_json::json!({"name": "items", "min": 1})));
        assert!(!validator.is_valid(&serde_json::json!({"name": "items"})));
        assert!(!validator.is_valid(&serde_json::json!({"name": "items", "min": 1, "extra": 1})));
    }
}
