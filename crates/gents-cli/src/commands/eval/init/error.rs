use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub(crate) enum EvalInitError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("eval output must name a directory")]
    OutputUnnamed,
    #[error("definition_id {definition_id:?} does not name a pack: with `-` read as `_`, it must be a lowercase letter followed by lowercase letters, digits and underscores")]
    NotPackName { definition_id: String },
    #[error("creating {}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("eval output lock must be a regular file: {}", path.display())]
    LockNotRegular { path: PathBuf },
    #[error("opening eval output lock {}", path.display())]
    OpenLock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("locking {}; another eval init may be publishing or recovering it; retry after it finishes", out.display())]
    LockBusy {
        out: PathBuf,
        #[source]
        source: std::fs::TryLockError,
    },
    #[error("creating a directory beside {}", path.display())]
    PrivateSibling {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("restoring {} after {why}; the old pack is kept at {}", existing.display(), kept.display())]
    Restore {
        existing: PathBuf,
        why: &'static str,
        kept: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("resolving the subject directory {}", path.display())]
    ResolveSubject {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("resolving --out {}", path.display())]
    ResolveOut {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("--out {} overlaps the subject pack at {}; write the eval pack somewhere outside the subject", out.display(), subject.display())]
    SubjectOverlap { out: PathBuf, subject: PathBuf },
    #[error("reading the pack manifest")]
    ManifestRead(#[source] std::io::Error),
    #[error("parsing the pack manifest")]
    ManifestParse(#[source] serde_json::Error),
    #[error("listing {}", path.display())]
    ListPath {
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
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{} already exists; pass --force to replace it", out.display())]
    OutputExists { out: PathBuf },
    #[error("start `gents server` for this home and retry: the author runs on a served home")]
    ServedHomeRequired,
    #[error("the author's draft did not validate after {rounds} rounds; nothing was written; last messages:\n{listed}")]
    DraftRejected { rounds: usize, listed: String },
    #[error("nothing was written to {to}")]
    NothingWritten { to: &'static str },
    #[error("the pilot was declined; the pack stays as written at {}", out.display())]
    PilotDeclined { out: PathBuf },
    #[error("pilot run {run_id} stopped before it finished; the pack stays as written; `gents eval resume {run_id}` continues it")]
    PilotStopped { run_id: String },
    #[error("eval author {agent:?} did not become ready within {seconds} seconds ({last}); inspect `gents status` for this home and retry")]
    AuthorNotReady {
        agent: String,
        seconds: f64,
        last: String,
    },
    #[error(
        "the {agent} turn ended without a reply; `gents response show {request_id}` shows why"
    )]
    TurnEmpty { agent: String, request_id: String },
    #[error("the pack has no agent {agent:?}; it declares: {declared}")]
    AgentUnknown { agent: String, declared: String },
    #[error("the pack declares no agent to evaluate")]
    NoAgentToEvaluate,
    #[error("the pack declares several agents ({declared}); choose one with --agent")]
    SeveralAgents { declared: String },
    #[error("the subject dossier is {bytes} bytes, over the {limit}-byte limit; its largest asset is {largest} ({largest_size} bytes)")]
    DossierTooLarge {
        bytes: usize,
        limit: usize,
        largest: String,
        largest_size: usize,
    },
    #[error("cases {case_id:?} and another share the sidecar {path}; case ids must differ after `-` is read as `_`")]
    SidecarCollision { case_id: String, path: String },
    #[error("writing {path}")]
    WriteAsset {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("refusing to write {path:?}: it is not a pack asset path")]
    NotAssetPath { path: String },
    #[error("refusing to replace --out {}: it stopped being a definition pack gents eval init wrote while replacing it", out.display())]
    ReplaceNotPack { out: PathBuf },
    #[error("refusing to replace --out {}: {} appeared while replacing it", out.display(), extra.display())]
    ReplaceAppeared { out: PathBuf, extra: PathBuf },
    #[error("refusing to replace --out {}: it is a symbolic link", out.display())]
    OutSymlink { out: PathBuf },
    #[error("{} does not exist", out.display())]
    OutMissing { out: PathBuf },
    #[error("refusing to replace --out {}: it is or contains a Gents home", out.display())]
    OutHome { out: PathBuf },
    #[error("refusing to replace --out {}: it is or contains the working directory", out.display())]
    OutCwd { out: PathBuf },
    #[error("refusing to replace --out {}: it is not a definition pack gents eval init wrote; choose another --out or remove it yourself", out.display())]
    OutNotGenerated { out: PathBuf },
    #[error("refusing to replace --out {}: {} is not part of the pack gents eval init wrote; move it out or remove --out yourself", out.display(), extra.display())]
    OutExtraEntry { out: PathBuf, extra: PathBuf },
    #[error("refusing to recover eval output {}: it contains a protected home or working directory", out.display())]
    OutProtected { out: PathBuf },
    #[error("several interrupted eval replacements exist for {}; restore the intended pack from {backups:?} before retrying", out.display())]
    SeveralBackups { out: PathBuf, backups: Vec<PathBuf> },
    #[error("the pack loads {count} eval definitions, not one")]
    LoadedCount { count: usize },
    #[error("the loaded definition differs from the validated draft")]
    DraftMismatch,
    #[error("the installed definition's cases differ from the pack's")]
    InstalledCasesDiffer,
    #[error(transparent)]
    HomeState(#[from] crate::home_state::HomeStateError),
    #[error("copying the definition pack to {}", path.display())]
    CopyPack {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("moving {} aside", path.display())]
    MoveAside {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("moving the definition pack from {} to {}", from.display(), to.display())]
    LandPack {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("removing the replaced pack at {}", path.display())]
    RemoveReplaced {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("inspecting eval output")]
    InspectOutput(#[source] std::io::Error),
    #[error("inspecting eval output before recovery")]
    InspectOutputBeforeRecovery(#[source] std::io::Error),
    #[error("resolving the working directory")]
    WorkingDirectory(#[source] std::io::Error),
    #[error("resolving the user home for eval recovery")]
    RecoveryUserHome,
    #[error("eval output has no parent")]
    OutputWithoutParent,
    #[error("restoring interrupted eval output {} from {}", out.display(), backup.display())]
    RestoreInterrupted {
        out: PathBuf,
        backup: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("eval backup has no holder")]
    BackupWithoutHolder,
    #[error("reading runtime readiness for the eval author")]
    ReadAuthorReadiness(#[source] gents::config_client::ConfigClientError),
    #[error("decoding runtime readiness for the eval author")]
    DecodeAuthorReadiness(#[source] serde_json::Error),
    #[error("parsing pack config JSON")]
    ParsePackConfig(#[source] serde_json::Error),
    #[error("refusing to replace --out: {error}")]
    OutRefused { error: String },
}
