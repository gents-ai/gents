//! Eval execution support shared by the runtime and the test harness.
pub mod embedded;
pub mod executor;
pub mod grade;
pub mod plan;
pub mod scripted;

pub use executor::{
    Capture, CaptureResult, FileRef, FixtureDocument, FixtureFile, InferenceBinding, Isolation,
    StageEvidence, StageSpec, TrialEvidence, TrialExecutor, TrialFixtures, TrialLocator, TrialSpec,
};
pub use grade::{grade, VerdictRow};
pub use plan::{plan, trial_id_for, PlannedTrial};
pub use scripted::{ScriptKey, ScriptedExecutor};
