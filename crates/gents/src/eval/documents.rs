use std::path::PathBuf;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::outcome::{classify, EvidenceClass, OutcomeKind, ProviderReason};
use crate::config_client::{
    ConfigAccess, ConfigClientError, DesiredStateError, TransactionFailure,
};
use crate::document_config::{EvalSplit, EvalTier};
use crate::graphql::escape_graphql_string;

/// Display text is shown to operators and the model; keep it stable.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error(transparent)]
    ReportRefused(#[from] super::report::ReportRefused),
    #[error(transparent)]
    FreezeRefused(#[from] super::runner::freeze::FreezeRefused),
    #[error(transparent)]
    ProviderDown(#[from] super::runner::ProviderDown),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error("decoding {kind} {id:?}")]
    DecodeDocument {
        kind: String,
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("create_{name}")]
    CreateRecord {
        name: &'static str,
        #[source]
        source: ConfigClientError,
    },
    #[error("eval trial completion is write-once and is already set")]
    AlreadyCompleted,
    #[error("eval run invalidation is write-once and is already set")]
    AlreadyInvalidated,
    #[error("eval verdict feedback is only permitted on the train split")]
    FeedbackOffTrain,
    #[error("no eval run {run_id:?}")]
    MissingRun { run_id: String },
    #[error("no eval trial {trial_id:?}")]
    MissingTrial { trial_id: String },
    #[error("{field}")]
    MissingField { field: &'static str },
    #[error("unknown outcome_kind")]
    UnknownOutcomeKind,
    #[error("unknown provider_reason {value:?}")]
    UnknownProviderReason { value: String },
    #[error("decoding run origin")]
    DecodeOrigin(#[source] serde_json::Error),
    #[error("decoding invalidation")]
    DecodeInvalidation(#[source] serde_json::Error),
    #[error("decoding trial completion")]
    DecodeCompletion(#[source] serde_json::Error),
    #[error("decoding verdict tier")]
    DecodeTier(#[source] serde_json::Error),
    #[error("eval verdict {verdict_id:?} scored {score_bp} basis points, above the maximum {max}")]
    ScoreAboveMaximum {
        verdict_id: String,
        score_bp: u32,
        max: u32,
    },
    #[error("{} has no parent directory", path.display())]
    NoParentDirectory { path: PathBuf },
    #[error("{} has no cell directory", path.display())]
    NoCellDirectory { path: PathBuf },
    #[error("creating {}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading {}", path.display())]
    ReadPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {}", path.display())]
    ParsePath {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("writing {}", path.display())]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {}", path.display())]
    WriteRecord {
        path: PathBuf,
        #[source]
        source: Box<EvalError>,
    },
    #[error("removing {}", path.display())]
    RemovePath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("renaming {} to {}", from.display(), to.display())]
    Rename {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("staging {}", path.display())]
    StageSnapshot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("encoding {}", path.display())]
    EncodeSnapshot {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("replacing {}", path.display())]
    ReplaceSnapshot {
        path: PathBuf,
        #[source]
        source: tempfile::PersistError,
    },
    #[error("no eval run {run_id:?} for {owner}")]
    NoRun { run_id: String, owner: String },
    #[error("trial budget closed")]
    TrialBudgetClosed {
        #[source]
        source: tokio::sync::AcquireError,
    },
    #[error("run {run_id} froze no case {case_id:?}")]
    UnfrozenCase { run_id: String, case_id: String },
    #[error("run {run_id} froze no cell {cell_id:?}")]
    UnfrozenCell { run_id: String, cell_id: String },
    #[error("review checks unavailable")]
    ReviewChecksUnavailable,
    #[error("review check failed: {check}")]
    ReviewCheckFailed { check: serde_json::Value },
    #[error("trial concurrency must be between 1 and {max}")]
    TrialConcurrency { max: usize },
    #[error("fixture asset {asset:?} must be a path inside the pack")]
    FixtureAssetInvalid { asset: String },
    #[error("reading fixture asset {}", path.display())]
    ReadFixture {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading fixture asset directory {}", path.display())]
    ReadFixtureDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is not UTF-8", path.display())]
    NotUtf8 { path: PathBuf },
    #[error("fixture asset outside the pack")]
    FixtureOutsidePack {
        #[source]
        source: std::path::StripPrefixError,
    },
    #[error("reading pack directory {}", path.display())]
    ReadPackDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("pack entry {} is a symlink; a pack carries its own bytes", path.display())]
    PackSymlink { path: PathBuf },
    #[error("{} is outside the pack", path.display())]
    PackPathOutside {
        path: PathBuf,
        #[source]
        source: std::path::StripPrefixError,
    },
    #[error("pack path {} is not UTF-8", path.display())]
    PackPathNotUtf8 { path: PathBuf },
}

pub const DENOMINATOR_POLICY_V1: &str = "exclude_not_evidence_v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionRef {
    pub definition_id: String,
    pub comparability_version: i64,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectRef {
    pub pack_digest: String,
    pub agent_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellSpec {
    pub cell_id: String,
    /// Operator-authored display name, never model output.
    pub label: String,
    pub subject: SubjectRef,
    pub inference_profile_id: String,
}

/// Replication safety: `EvalRun` carries no message bodies, only ids, enum wire
/// strings, counts, digests and timestamps; model output, transcripts, prompts
/// and check detail belong to the local-audit `EvalVerdict` collection. The
/// only free-text fields are operator-authored: [`CellSpec::label`] and
/// [`Invalidation::reason`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOrigin {
    pub definition: DefinitionRef,
    pub split: EvalSplit,
    pub case_ids: Vec<String>,
    pub cells: Vec<CellSpec>,
    pub trials_per_case: u32,
    /// A trial's seed is `seed_base + trial_index`.
    pub seed_base: i64,
    pub deadline_secs: Option<u64>,
    pub concurrency: u32,
    pub denominator_policy: String,
    pub taxonomy_version: String,
    pub max_infra_retries: u32,
    pub check_registry_version: String,
    pub source_commit: String,
    pub source_dirty: bool,
    /// Machine-authored (`"eval"`, `"pilot"` or `"optimization:<job_id>"`),
    /// never operator prose or model output.
    pub purpose: String,
    /// Consecutive infrastructure failures that stop the run.
    #[serde(default = "default_breaker_threshold")]
    pub breaker_threshold: u32,
}

pub fn default_breaker_threshold() -> u32 {
    5
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invalidation {
    pub at: String,
    pub by: String,
    /// One free-text value an eval run may replicate.
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecord {
    pub run_id: String,
    pub owner: String,
    pub evaluator_did: String,
    pub origin: RunOrigin,
    pub created_at: String,
    pub invalidated: Option<Invalidation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialIdentity {
    pub trial_id: String,
    pub run_id: String,
    pub cell_id: String,
    pub case_id: String,
    pub trial_index: u32,
    pub attempt: u32,
    pub trial_node_did: String,
    /// With `trial_node_did`, the durable evidence reference.
    pub session_id: String,
    pub seed: i64,
    pub home_hint: Option<String>,
}

/// Every field but the ids is a closed vocabulary, so no provider, tool or model
/// text can reach a replicating row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageCompletion {
    pub stage_id: String,
    pub request_id: Option<String>,
    pub terminal_state: Option<RequestLifecycleState>,
    pub failure_kind: Option<OutcomeKind>,
    /// Required alongside a `Provider` [`Self::failure_kind`]; a candidate
    /// prompt can cause a rejection but not an outage.
    pub provider_reason: Option<ProviderReason>,
}

/// Lets a reader tell a deleted trial home from a mismatched one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub terminal_states: Vec<RequestLifecycleState>,
    pub requests: u32,
    pub inference_calls: u32,
}

/// A missing total is `None`, never zero.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialCompletion {
    pub ended_at: String,
    pub stages: Vec<StageCompletion>,
    pub usage: TrialUsage,
    pub anchor: Anchor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialRecord {
    pub identity: TrialIdentity,
    pub created_at: String,
    pub completion: Option<TrialCompletion>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VerdictDraft {
    pub verdict_id: String,
    pub run_id: String,
    pub trial_id: String,
    pub stage_id: String,
    pub check: String,
    pub check_version: String,
    pub tier: EvalTier,
    pub kind: OutcomeKind,
    pub provider_reason: Option<ProviderReason>,
    pub score_bp: Option<u32>,
    pub weight: u32,
    pub raw: Value,
    pub feedback: Option<String>,
    pub regrade_of: Option<String>,
}

pub type VerdictRecord = VerdictDraft;

impl TransactionFailure for EvalError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            _ => None,
        }
    }
}

type Result<T, E = EvalError> = std::result::Result<T, E>;

/// Every eval error in a chain, outermost first, so a sentinel nested under a
/// wrapper variant (for example a rejected write) still matches.
fn eval_error_in<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> impl Iterator<Item = &'a EvalError> {
    std::iter::successors(Some(error), |cause| cause.source())
        .filter_map(|cause| cause.downcast_ref::<EvalError>())
}

pub fn already_completed(error: &(dyn std::error::Error + 'static)) -> bool {
    eval_error_in(error).any(|error| matches!(error, EvalError::AlreadyCompleted))
}

pub fn already_invalidated<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a EvalError> {
    eval_error_in(error).find(|error| matches!(error, EvalError::AlreadyInvalidated))
}

pub fn feedback_off_train(error: &(dyn std::error::Error + 'static)) -> bool {
    eval_error_in(error).any(|error| matches!(error, EvalError::FeedbackOffTrain))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

async fn create(
    access: &ConfigAccess,
    operation: &'static str,
    name: &'static str,
    input: Value,
) -> Result<()> {
    let variables = json!({ "input": input });
    let mutation = format!(
        "mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID }} }}"
    );
    access
        .transact_write(operation, |txn| {
            let (variables, mutation) = (&variables, &mutation);
            Box::pin(async move {
                txn.execute_with_variables(mutation, variables)
                    .await
                    .map(|_| ())
            })
        })
        .await
        .map_err(|source| EvalError::CreateRecord { name, source })
}

async fn rows(
    access: &ConfigAccess,
    operation: &'static str,
    query: String,
    name: &'static str,
) -> Result<Vec<Value>> {
    let response = access
        .transact_readonly(operation, |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        })
        .await?;
    Ok(response["data"][name]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

pub async fn create_run(
    access: &ConfigAccess,
    run_id: &str,
    owner: &str,
    evaluator_did: &str,
    origin: &RunOrigin,
) -> Result<RunRecord> {
    let created_at = now();
    create(
        access,
        "eval.create_run",
        "EvalRun",
        json!({
            "run_id": run_id,
            "owner_node_did": owner,
            "evaluator_did": evaluator_did,
            "origin": serde_json::to_value(origin)?,
            "created_at": created_at,
        }),
    )
    .await?;
    tracing::info!(run_id, owner, purpose = %origin.purpose, "eval run frozen");
    Ok(RunRecord {
        run_id: run_id.into(),
        owner: owner.into(),
        evaluator_did: evaluator_did.into(),
        origin: origin.clone(),
        created_at,
        invalidated: None,
    })
}

pub async fn load_run(
    access: &ConfigAccess,
    owner: &str,
    run_id: &str,
) -> Result<Option<RunRecord>> {
    let found = rows(
        access,
        "eval.load_run",
        format!(
            r#"{{ EvalRun(filter: {{ owner_node_did: {{ _eq: "{owner}" }}, run_id: {{ _eq: "{run_id}" }} }}) {{ run_id owner_node_did evaluator_did origin created_at invalidated }} }}"#,
            owner = escape_graphql_string(owner),
            run_id = escape_graphql_string(run_id),
        ),
        "EvalRun",
    )
    .await?;
    found
        .first()
        .map(|row| {
            Ok(RunRecord {
                run_id: row["run_id"]
                    .as_str()
                    .ok_or(EvalError::MissingField { field: "run_id" })?
                    .into(),
                owner: row["owner_node_did"]
                    .as_str()
                    .ok_or(EvalError::MissingField {
                        field: "owner_node_did",
                    })?
                    .into(),
                evaluator_did: row["evaluator_did"]
                    .as_str()
                    .ok_or(EvalError::MissingField {
                        field: "evaluator_did",
                    })?
                    .into(),
                origin: serde_json::from_value(row["origin"].clone())
                    .map_err(EvalError::DecodeOrigin)?,
                created_at: row["created_at"]
                    .as_str()
                    .ok_or(EvalError::MissingField {
                        field: "created_at",
                    })?
                    .into(),
                invalidated: match &row["invalidated"] {
                    Value::Null => None,
                    value => Some(
                        serde_json::from_value(value.clone())
                            .map_err(EvalError::DecodeInvalidation)?,
                    ),
                },
            })
        })
        .transpose()
}

/// Write-once: read and write share one transaction, so a second invalidation
/// loses to [`AlreadyInvalidated`] and the first `{at, by, reason}` stands.
pub async fn invalidate_run(
    access: &ConfigAccess,
    owner: &str,
    run_id: &str,
    by: &str,
    reason: &str,
) -> Result<()> {
    let filter = format!(
        r#"owner_node_did: {{ _eq: "{owner}" }}, run_id: {{ _eq: "{run_id}" }}"#,
        owner = escape_graphql_string(owner),
        run_id = escape_graphql_string(run_id),
    );
    let variables = json!({
        "input": { "invalidated": { "at": now(), "by": by, "reason": reason } },
    });
    let run_id_owned = run_id.to_owned();
    access
        .transact_write("eval.invalidate_run", |txn| {
            let (filter, variables, run_id) = (&filter, &variables, &run_id_owned);
            Box::pin(async move {
                let current = txn
                    .execute(&format!(
                        "{{ EvalRun(filter: {{ {filter} }}) {{ invalidated }} }}"
                    ))
                    .await?;
                let row = current["data"]["EvalRun"]
                    .as_array()
                    .and_then(|rows| rows.first())
                    .ok_or_else(|| EvalError::MissingRun {
                        run_id: run_id.clone(),
                    })?;
                if !row["invalidated"].is_null() {
                    return Err(EvalError::AlreadyInvalidated);
                }
                txn.execute_with_variables(
                    &format!(
                        r#"mutation($input: EvalRunMutationInputArg!) {{ update_EvalRun(filter: {{ {filter} }}, input: $input) {{ _docID }} }}"#
                    ),
                    variables,
                )
                .await
                .map(|_| ())
                .map_err(Into::into)
            })
        })
        .await?;
    tracing::warn!(run_id, by, reason, "eval run invalidated");
    Ok(())
}

pub async fn create_trial(
    access: &ConfigAccess,
    owner: &str,
    identity: &TrialIdentity,
) -> Result<()> {
    create(
        access,
        "eval.create_trial",
        "EvalTrial",
        json!({
            "trial_id": identity.trial_id,
            "owner_node_did": owner,
            "run_id": identity.run_id,
            "cell_id": identity.cell_id,
            "case_id": identity.case_id,
            "trial_index": identity.trial_index,
            "attempt": identity.attempt,
            "trial_node_did": identity.trial_node_did,
            "session_id": identity.session_id,
            "seed": identity.seed,
            "home_hint": identity.home_hint,
            "created_at": now(),
        }),
    )
    .await
}

/// Write-once: read and write share one transaction, so a second completion
/// loses to [`AlreadyCompleted`] rather than overwriting a fact.
pub async fn complete_trial(
    access: &ConfigAccess,
    owner: &str,
    trial_id: &str,
    completion: &TrialCompletion,
) -> Result<()> {
    let filter = format!(
        r#"owner_node_did: {{ _eq: "{owner}" }}, trial_id: {{ _eq: "{trial_id}" }}"#,
        owner = escape_graphql_string(owner),
        trial_id = escape_graphql_string(trial_id),
    );
    let variables = json!({ "input": { "completion": serde_json::to_value(completion)? } });
    let trial_id = trial_id.to_owned();
    access
        .transact_write("eval.complete_trial", |txn| {
            let (filter, variables, trial_id) = (&filter, &variables, &trial_id);
            Box::pin(async move {
                let current = txn
                    .execute(&format!(
                        "{{ EvalTrial(filter: {{ {filter} }}) {{ completion }} }}"
                    ))
                    .await?;
                let row = current["data"]["EvalTrial"]
                    .as_array()
                    .and_then(|rows| rows.first())
                    .ok_or_else(|| EvalError::MissingTrial {
                        trial_id: trial_id.clone(),
                    })?;
                if !row["completion"].is_null() {
                    return Err(EvalError::AlreadyCompleted);
                }
                txn.execute_with_variables(
                    &format!(
                        "mutation($input: EvalTrialMutationInputArg!) {{ update_EvalTrial(filter: {{ {filter} }}, input: $input) {{ _docID }} }}"
                    ),
                    variables,
                )
                .await
                .map(|_| ())
                .map_err(Into::into)
            })
        })
        .await
}

pub async fn load_trials(
    access: &ConfigAccess,
    owner: &str,
    run_id: &str,
) -> Result<Vec<TrialRecord>> {
    let found = rows(
        access,
        "eval.load_trials",
        format!(
            r#"{{ EvalTrial(filter: {{ owner_node_did: {{ _eq: "{owner}" }}, run_id: {{ _eq: "{run_id}" }} }}) {{ trial_id run_id cell_id case_id trial_index attempt trial_node_did session_id seed home_hint created_at completion }} }}"#,
            owner = escape_graphql_string(owner),
            run_id = escape_graphql_string(run_id),
        ),
        "EvalTrial",
    )
    .await?;
    found
        .iter()
        .map(|row| {
            let text = |field: &str| row[field].as_str().unwrap_or_default().to_owned();
            Ok(TrialRecord {
                identity: TrialIdentity {
                    trial_id: text("trial_id"),
                    run_id: text("run_id"),
                    cell_id: text("cell_id"),
                    case_id: text("case_id"),
                    trial_index: row["trial_index"].as_u64().unwrap_or_default() as u32,
                    attempt: row["attempt"].as_u64().unwrap_or_default() as u32,
                    trial_node_did: text("trial_node_did"),
                    session_id: text("session_id"),
                    seed: row["seed"].as_i64().unwrap_or_default(),
                    home_hint: row["home_hint"].as_str().map(str::to_owned),
                },
                created_at: text("created_at"),
                completion: match &row["completion"] {
                    Value::Null => None,
                    value => Some(
                        serde_json::from_value(value.clone())
                            .map_err(EvalError::DecodeCompletion)?,
                    ),
                },
            })
        })
        .collect()
}

/// Append-only. The runner, not the check, enforces that feedback exists only
/// on train.
pub async fn append_verdict(
    access: &ConfigAccess,
    owner: &str,
    split: EvalSplit,
    draft: &VerdictDraft,
) -> Result<()> {
    if draft.feedback.is_some() && split != EvalSplit::Train {
        return Err(EvalError::FeedbackOffTrain);
    }
    if let Some(bp) = draft.score_bp {
        if bp > super::scoring::SCORE_BP_MAX {
            return Err(EvalError::ScoreAboveMaximum {
                verdict_id: draft.verdict_id.clone(),
                score_bp: bp,
                max: super::scoring::SCORE_BP_MAX,
            });
        }
    }
    let score_bp = match classify(draft.kind, draft.provider_reason) {
        EvidenceClass::NotEvidence | EvidenceClass::Unknown => None,
        _ => draft.score_bp,
    };
    create(
        access,
        "eval.append_verdict",
        "EvalVerdict",
        json!({
            "verdict_id": draft.verdict_id,
            "owner_node_did": owner,
            "run_id": draft.run_id,
            "trial_id": draft.trial_id,
            "stage_id": draft.stage_id,
            "check": draft.check,
            "check_version": draft.check_version,
            "tier": serde_json::to_value(draft.tier)?,
            "outcome_kind": draft.kind.as_str(),
            "provider_reason": draft.provider_reason.map(ProviderReason::as_str),
            "score_bp": score_bp,
            "weight": draft.weight,
            "raw": draft.raw,
            "feedback": draft.feedback,
            "regrade_of": draft.regrade_of,
            "created_at": now(),
        }),
    )
    .await
}

pub async fn load_verdicts(
    access: &ConfigAccess,
    owner: &str,
    run_id: &str,
) -> Result<Vec<VerdictRecord>> {
    let found = rows(
        access,
        "eval.load_verdicts",
        format!(
            r#"{{ EvalVerdict(filter: {{ owner_node_did: {{ _eq: "{owner}" }}, run_id: {{ _eq: "{run_id}" }} }}) {{ verdict_id run_id trial_id stage_id check check_version tier outcome_kind provider_reason score_bp weight raw feedback regrade_of }} }}"#,
            owner = escape_graphql_string(owner),
            run_id = escape_graphql_string(run_id),
        ),
        "EvalVerdict",
    )
    .await?;
    found
        .iter()
        .map(|row| {
            let text = |field: &str| row[field].as_str().unwrap_or_default().to_owned();
            Ok(VerdictRecord {
                verdict_id: text("verdict_id"),
                run_id: text("run_id"),
                trial_id: text("trial_id"),
                stage_id: text("stage_id"),
                check: text("check"),
                check_version: text("check_version"),
                tier: serde_json::from_value(row["tier"].clone()).map_err(EvalError::DecodeTier)?,
                kind: OutcomeKind::parse(&text("outcome_kind"))
                    .ok_or(EvalError::UnknownOutcomeKind)?,
                provider_reason: row["provider_reason"]
                    .as_str()
                    .map(|value| {
                        ProviderReason::parse(value).ok_or_else(|| {
                            EvalError::UnknownProviderReason {
                                value: value.to_owned(),
                            }
                        })
                    })
                    .transpose()?,
                score_bp: row["score_bp"].as_u64().map(|bp| bp as u32),
                weight: row["weight"].as_u64().unwrap_or(1) as u32,
                raw: row["raw"].clone(),
                feedback: row["feedback"].as_str().map(str::to_owned),
                regrade_of: row["regrade_of"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defra_node::EmbeddedNode;
    use crate::eval::TAXONOMY_VERSION;
    use gents_protocol::error_chain::ErrorChain;
    use std::sync::Arc;

    const OWNER: &str = "did:key:eval-owner";

    async fn access() -> ConfigAccess {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        for sdl in [
            gents_protocol::schemas::EVAL_RUN,
            gents_protocol::schemas::EVAL_TRIAL,
            gents_protocol::schemas::EVAL_VERDICT,
        ] {
            node.add_schema(sdl).await.unwrap();
        }
        ConfigAccess::local(node)
    }

    fn origin(split: EvalSplit) -> RunOrigin {
        RunOrigin {
            definition: DefinitionRef {
                definition_id: "monitor-findings".into(),
                comparability_version: 1,
                digest: "sha256:def".into(),
            },
            split,
            case_ids: vec!["disk-warning".into()],
            cells: vec![CellSpec {
                cell_id: "baseline".into(),
                label: "baseline".into(),
                subject: SubjectRef {
                    pack_digest: "sha256:pack".into(),
                    agent_id: "monitor".into(),
                },
                inference_profile_id: "local".into(),
            }],
            trials_per_case: 2,
            seed_base: 1000,
            deadline_secs: Some(600),
            concurrency: 1,
            denominator_policy: DENOMINATOR_POLICY_V1.into(),
            taxonomy_version: TAXONOMY_VERSION.into(),
            max_infra_retries: 1,
            check_registry_version: "checks-1".into(),
            source_commit: "0deb7659c".into(),
            source_dirty: false,
            purpose: "eval".into(),
            breaker_threshold: default_breaker_threshold(),
        }
    }

    fn identity(run_id: &str, trial_id: &str) -> TrialIdentity {
        TrialIdentity {
            trial_id: trial_id.into(),
            run_id: run_id.into(),
            cell_id: "baseline".into(),
            case_id: "disk-warning".into(),
            trial_index: 0,
            attempt: 0,
            trial_node_did: "did:key:trial".into(),
            session_id: format!("session-{trial_id}"),
            seed: 1000,
            home_hint: Some("/tmp/trial".into()),
        }
    }

    fn completion() -> TrialCompletion {
        TrialCompletion {
            ended_at: "2026-09-21T00:00:00Z".into(),
            stages: vec![StageCompletion {
                stage_id: "check".into(),
                request_id: Some("req-1".into()),
                terminal_state: Some(RequestLifecycleState::Completed),
                failure_kind: None,
                provider_reason: None,
            }],
            usage: TrialUsage {
                input_tokens: Some(10),
                output_tokens: None,
            },
            anchor: Anchor {
                terminal_states: vec![RequestLifecycleState::Completed],
                requests: 1,
                inference_calls: 3,
            },
            evidence_digest: None,
        }
    }

    fn rival_completion() -> TrialCompletion {
        TrialCompletion {
            ended_at: "2026-09-22T11:22:33Z".into(),
            stages: vec![StageCompletion {
                stage_id: "other".into(),
                request_id: Some("req-2".into()),
                terminal_state: Some(RequestLifecycleState::Failed),
                failure_kind: Some(OutcomeKind::Provider),
                provider_reason: Some(ProviderReason::Unavailable),
            }],
            usage: TrialUsage {
                input_tokens: Some(99),
                output_tokens: Some(7),
            },
            anchor: Anchor {
                terminal_states: vec![RequestLifecycleState::Failed],
                requests: 4,
                inference_calls: 5,
            },
            evidence_digest: Some("sha256:rival".into()),
        }
    }

    fn draft(
        run_id: &str,
        trial_id: &str,
        verdict_id: &str,
        feedback: Option<&str>,
    ) -> VerdictDraft {
        VerdictDraft {
            verdict_id: verdict_id.into(),
            run_id: run_id.into(),
            trial_id: trial_id.into(),
            stage_id: "check".into(),
            check: "mailbox_findings".into(),
            check_version: "1".into(),
            tier: EvalTier::Acceptance,
            kind: OutcomeKind::Passed,
            provider_reason: None,
            score_bp: Some(10_000),
            weight: 1,
            raw: serde_json::json!({"detail": "ok"}),
            feedback: feedback.map(str::to_owned),
            regrade_of: None,
        }
    }

    #[tokio::test]
    async fn a_run_round_trips_and_can_only_be_invalidated() {
        let access = access().await;
        let run = create_run(
            &access,
            "run-1",
            OWNER,
            "did:key:evaluator",
            &origin(EvalSplit::Validation),
        )
        .await
        .unwrap();
        assert!(run.invalidated.is_none());
        invalidate_run(&access, OWNER, "run-1", OWNER, "grader bug")
            .await
            .unwrap();
        let loaded = load_run(&access, OWNER, "run-1").await.unwrap().unwrap();
        assert_eq!(loaded.origin, run.origin);
        let first = loaded.invalidated.expect("the run is invalidated");
        assert_eq!(first.reason, "grader bug");

        let error = invalidate_run(&access, OWNER, "run-1", "did:key:other", "second thoughts")
            .await
            .unwrap_err();
        assert!(
            already_invalidated(&error).is_some(),
            "{}",
            ErrorChain(&error)
        );
        let again = load_run(&access, OWNER, "run-1").await.unwrap().unwrap();
        assert_eq!(
            again.invalidated.expect("still invalidated"),
            first,
            "the first invalidation is the fact: who, when and why never change"
        );
    }

    #[tokio::test]
    async fn a_trial_completion_is_written_once() {
        let access = access().await;
        create_run(
            &access,
            "run-2",
            OWNER,
            "did:key:evaluator",
            &origin(EvalSplit::Validation),
        )
        .await
        .unwrap();
        create_trial(&access, OWNER, &identity("run-2", "trial-1"))
            .await
            .unwrap();
        let before = load_trials(&access, OWNER, "run-2").await.unwrap();
        assert!(
            before[0].completion.is_none(),
            "a null completion is a fact"
        );
        complete_trial(&access, OWNER, "trial-1", &completion())
            .await
            .unwrap();
        let error = complete_trial(&access, OWNER, "trial-1", &rival_completion())
            .await
            .unwrap_err();
        assert!(already_completed(&error), "{}", ErrorChain(&error));
        let after = load_trials(&access, OWNER, "run-2").await.unwrap();
        let stored = after[0].completion.as_ref().expect("the trial completed");
        assert_eq!(stored.usage.output_tokens, None, "missing usage stays null");
        assert_eq!(
            stored,
            &completion(),
            "the losing completion overwrote nothing"
        );
    }

    #[tokio::test]
    async fn empty_lists_and_absent_values_round_trip() {
        let access = access().await;
        let mut sparse = origin(EvalSplit::HeldOut);
        sparse.case_ids = Vec::new();
        sparse.deadline_secs = None;
        create_run(&access, "run-3", OWNER, "did:key:evaluator", &sparse)
            .await
            .unwrap();
        let loaded = load_run(&access, OWNER, "run-3").await.unwrap().unwrap();
        assert_eq!(loaded.origin, sparse);
        assert!(loaded.origin.case_ids.is_empty());
        assert_eq!(loaded.origin.deadline_secs, None);

        let mut bare = identity("run-3", "trial-3");
        bare.home_hint = None;
        create_trial(&access, OWNER, &bare).await.unwrap();
        let nothing_ran = TrialCompletion {
            ended_at: "2026-09-21T00:00:00Z".into(),
            stages: Vec::new(),
            usage: TrialUsage::default(),
            anchor: Anchor {
                terminal_states: Vec::new(),
                requests: 0,
                inference_calls: 0,
            },
            evidence_digest: None,
        };
        complete_trial(&access, OWNER, "trial-3", &nothing_ran)
            .await
            .unwrap();
        let trials = load_trials(&access, OWNER, "run-3").await.unwrap();
        assert_eq!(trials[0].identity, bare);
        assert_eq!(trials[0].identity.home_hint, None);
        assert_eq!(trials[0].completion.as_ref(), Some(&nothing_ran));
        let stored = trials[0].completion.as_ref().expect("the trial completed");
        assert!(stored.stages.is_empty());
        assert!(stored.anchor.terminal_states.is_empty());
        assert_eq!(stored.usage.input_tokens, None, "no total is not zero");
        assert_eq!(stored.usage.output_tokens, None, "no total is not zero");
    }

    #[test]
    fn breaker_threshold_defaults_to_five_when_absent() {
        let mut value = serde_json::to_value(origin(EvalSplit::Validation)).unwrap();
        assert_eq!(value["breaker_threshold"], 5);
        value.as_object_mut().unwrap().remove("breaker_threshold");
        let parsed: RunOrigin = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.breaker_threshold, 5);
        assert_eq!(default_breaker_threshold(), 5);
    }

    #[tokio::test]
    async fn an_evidence_digest_round_trips_and_is_omitted_when_absent() {
        let bare = serde_json::to_value(completion()).unwrap();
        assert!(bare.get("evidence_digest").is_none(), "None is omitted");
        let parsed: TrialCompletion = serde_json::from_value(bare).unwrap();
        assert_eq!(parsed.evidence_digest, None);

        let access = access().await;
        create_run(
            &access,
            "run-9",
            OWNER,
            "did:key:evaluator",
            &origin(EvalSplit::Validation),
        )
        .await
        .unwrap();
        create_trial(&access, OWNER, &identity("run-9", "trial-9"))
            .await
            .unwrap();
        let mut digested = completion();
        digested.evidence_digest = Some("sha256:evidence".into());
        complete_trial(&access, OWNER, "trial-9", &digested)
            .await
            .unwrap();
        let trials = load_trials(&access, OWNER, "run-9").await.unwrap();
        assert_eq!(trials[0].completion.as_ref(), Some(&digested));
    }

    #[tokio::test]
    async fn feedback_is_refused_off_the_train_split() {
        let access = access().await;
        let error = append_verdict(
            &access,
            OWNER,
            EvalSplit::Validation,
            &draft("r", "t", "v1", Some("hint")),
        )
        .await
        .unwrap_err();
        assert!(feedback_off_train(&error), "{}", ErrorChain(&error));
        append_verdict(
            &access,
            OWNER,
            EvalSplit::Train,
            &draft("r", "t", "v2", Some("hint")),
        )
        .await
        .unwrap();
        append_verdict(
            &access,
            OWNER,
            EvalSplit::Validation,
            &draft("r", "t", "v3", None),
        )
        .await
        .unwrap();
        let verdicts = load_verdicts(&access, OWNER, "r").await.unwrap();
        assert_eq!(verdicts.len(), 2);
    }

    #[tokio::test]
    async fn a_non_evidence_verdict_stores_a_null_score() {
        let access = access().await;
        let mut outage = draft("r2", "t", "v1", None);
        outage.kind = OutcomeKind::Provider;
        outage.provider_reason = Some(ProviderReason::Unavailable);
        outage.score_bp = Some(10_000);
        append_verdict(&access, OWNER, EvalSplit::Validation, &outage)
            .await
            .unwrap();
        let verdicts = load_verdicts(&access, OWNER, "r2").await.unwrap();
        assert_eq!(
            verdicts[0].score_bp, None,
            "nobody can average a fabricated score"
        );
        assert_eq!(
            verdicts[0].provider_reason,
            Some(ProviderReason::Unavailable)
        );
    }

    #[tokio::test]
    async fn a_score_above_the_maximum_is_refused_and_writes_nothing() {
        let access = access().await;
        let mut impossible = draft("r3", "t", "v1", None);
        impossible.score_bp = Some(10_001);
        let error = append_verdict(&access, OWNER, EvalSplit::Validation, &impossible)
            .await
            .unwrap_err();
        assert!(
            format!("{}", ErrorChain(&error)).contains("10001"),
            "the error names the offending score: {}",
            ErrorChain(&error)
        );
        assert!(
            load_verdicts(&access, OWNER, "r3")
                .await
                .unwrap()
                .is_empty(),
            "a refused verdict writes nothing"
        );
        impossible.score_bp = Some(crate::eval::SCORE_BP_MAX);
        append_verdict(&access, OWNER, EvalSplit::Validation, &impossible)
            .await
            .expect("a perfect score is still allowed");
    }

    #[tokio::test]
    async fn an_unknown_persisted_provider_reason_fails_the_read() {
        let access = access().await;
        create(
            &access,
            "eval.test_unknown_reason",
            "EvalVerdict",
            serde_json::json!({
                "verdict_id": "v-bogus",
                "owner_node_did": OWNER,
                "run_id": "r4",
                "trial_id": "t",
                "stage_id": "check",
                "check": "mailbox_findings",
                "check_version": "1",
                "tier": "acceptance",
                "outcome_kind": "provider",
                "provider_reason": "bogus",
                "score_bp": null,
                "weight": 1,
                "raw": {},
                "created_at": "2026-09-21T00:00:00Z",
            }),
        )
        .await
        .unwrap();
        let error = load_verdicts(&access, OWNER, "r4").await.unwrap_err();
        let rendered = format!("{}", ErrorChain(&error));
        assert!(
            rendered.contains("unknown provider_reason") && rendered.contains("bogus"),
            "{rendered}"
        );
    }
}
