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
}
