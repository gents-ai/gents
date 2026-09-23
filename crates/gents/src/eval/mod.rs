//! Eval core contract (#1515): the outcome vocabulary, scoring rules and the
//! fact-only run, trial and verdict documents. Execution lives in the runner.

pub mod documents;
pub mod outcome;
pub mod scoring;

pub use documents::{
    already_completed, already_invalidated, append_verdict, complete_trial, create_run,
    create_trial, feedback_off_train, invalidate_run, load_run, load_trials, load_verdicts, Anchor,
    CellSpec, DefinitionRef, Invalidation, RunOrigin, RunRecord, StageCompletion, SubjectRef,
    TrialCompletion, TrialIdentity, TrialRecord, TrialUsage, VerdictDraft, VerdictRecord,
    DENOMINATOR_POLICY_V1,
};

pub use outcome::{
    classify, subject_causable, EvidenceClass, OutcomeKind, ProviderReason, TAXONOMY_VERSION,
};
pub use scoring::{
    case_means_bp, case_trial_score, exposure, headline_bp, latest_verdicts, pair_trials,
    CaseTrialScore, Pair, PairedEvidence, RunHeader, TrialScore, VerdictView, SCORE_BP_MAX,
};
