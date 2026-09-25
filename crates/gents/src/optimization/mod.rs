//! Configuration optimization (#1455), a consumer of the eval core contract.
//! `policy` is the pure promotion decision, `target` freezes the owner's
//! configuration closure and builds the guarded single-field patch, and `job`
//! is the `OptimizationJob` document: a frozen origin and a length-guarded,
//! append-only journal from which the job's state is derived. `subject`
//! materializes the baseline subject pack and each candidate derived from it.
//! `show` projects a job's journal and recomputes each decision from its runs.
//! `promote` holds the operator's two verbs, `promote` and `revert`.

pub mod driver;
pub mod evidence;
pub mod gate;
pub mod job;
pub mod policy;
pub mod promote;
pub mod proposer;
pub mod show;
pub mod subject;
pub mod target;

pub use driver::{
    baseline_dir, candidate_dir, job_dir, job_refused, run_cost, run_job, spend_so_far,
    split_case_count, JobOutcome, JobRefused, JobRequest, Spend,
};
pub use evidence::{
    decision_evidence, decision_seed, token_totals, train_feedback, BASELINE_CELL, CANDIDATE_CELL,
};
pub use gate::{structural_gate, text_gate, StructuralRejection};
pub use job::{
    append, checkpoint, create_job, derive_state, journal_conflict, load_job, rounds_used, Budgets,
    Checkpoint, DecisionSummary, DriftedRef, JobOrigin, JobRecord, JobState, JournalConflict,
    JournalEntry,
};
pub use policy::{
    alpha_effective_ppm, decide, decide_gates, evidence_from_pairs, permutation_p_ppm,
    CaseEvidence, Decision, DecisionReport, Evidence, Gates, InconclusiveReason, Mode, PolicyV2,
    RejectReason, TokenTotals, POLICY_VERSION,
};
pub use promote::{promote, promote_refused, revert, PromoteRefused, Promotion};
pub use proposer::{CheckFeedback, Proposal, ProposalInput, Proposer, Rejection, ScriptedProposer};
pub use show::{show, DecisionView, JobView};
pub use subject::{baseline_text, materialize_candidate, materialize_pack, MaterializedPack};
pub use target::{
    apply_text, baseline_equivalence, capture_closure, closure_digests, current_text, expectations,
    target_digest, target_plan, BaselineMismatch, Closure, FrozenDocument, Target, TargetField,
};
