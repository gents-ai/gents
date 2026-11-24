use crate::conflict::RedactedKey;
use crate::io_queue::WouldBlock;
use crate::io_queue::wait::{copy_would_block, take_would_block};

/// Errors returned by regolith operations.
///
/// No message and no `Debug` output prints the bytes of a key or a value,
/// since either can hold user data. A variant that names a key carries it as
/// data and shows its length and a short hash.
#[non_exhaustive]
#[derive(thiserror::Error)]
pub enum Error {
    /// A caller supplied an invalid option, key, value, range, or
    /// other API argument.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// A size read exceeded its per-SST-data-block frame plus decoded-buffer limit.
    #[error("SST data block exceeds allocation limit of {max_data_block_bytes} bytes")]
    DataBlockLimitExceeded {
        /// The caller's limit, not the size of the block or value.
        max_data_block_bytes: usize,
    },
    /// On-disk database, WAL, SSTable, manifest, or backup data was
    /// malformed or truncated.
    #[error("corruption: {0}")]
    Corruption(#[source] std::io::Error),
    /// A mutating operation was attempted through a read-only handle.
    #[error("database was opened read-only")]
    ReadOnly,
    /// An operation was attempted after the database handle was closed.
    #[error("database is closed")]
    Closed,
    /// A column-family handle or id is stale, dropped, or belongs to a
    /// different database handle.
    #[error("invalid column family: {0}")]
    InvalidColumnFamily(String),
    /// A write stall refused the write, and no wait would end it. Returned
    /// when [`crate::WriteOptions::no_slowdown`] is set and the engine is
    /// stalling writes (too many L0 files, too many unflushed memtables, or
    /// pending compaction bytes over a limit), and when inline compaction
    /// ([`crate::Options::inline_compaction`]) ran its bound of steps
    /// without relieving a stop. The included string names the stall
    /// condition for diagnostics.
    #[error("engine busy: {0}")]
    Busy(&'static str),
    /// The configured [`crate::MergeOperator`] returned `None` when
    /// asked to combine a value with one or more merge operands,
    /// indicating that the operands were corrupt or the merge
    /// semantics failed. The payload is the key, as the caller wrote it;
    /// neither the message nor the `Debug` output prints its bytes, only
    /// its length and a short hash, since a key can hold user data.
    #[error("merge operator failed for a {}", RedactedKey(.0))]
    MergeFailed(Vec<u8>),
    /// A merge was written to a database that has no
    /// [`crate::MergeOperator`] configured. Set [`crate::Options::merge_operator`]
    /// before opening, or write the value with a put.
    #[error("no merge operator is configured; set Options::merge_operator to write merges")]
    NoMergeOperator,
    /// A transaction put a [`crate::KeyClass::ContentAddressed`] key with
    /// bytes that differ from the bytes a commit made after the transaction
    /// began left under it. Such a key must always hold the same bytes, so the
    /// commit applied nothing; name the key by the bytes it holds.
    #[error("a content-addressed key already holds different bytes, so the commit applied nothing")]
    ContentMismatch,
    /// Code the caller supplied panicked while a commit ran it: an
    /// implementation of the trait named by `callback`, such as
    /// [`crate::KeyClassifier`], or a `before_commit` callback. The panic was
    /// caught and the commit applied nothing.
    ///
    /// When `latched` is true the panic was in the commit's ordered step,
    /// whose shared state can no longer be vouched for: the database is
    /// read-only until it is reopened, and every later write fails with this
    /// error too. When it is false (a [`crate::Transaction::before_commit`]
    /// callback or a [`crate::TransactionHooks::before_commit`]) the panic
    /// failed that transaction only, the database stays writable, and the
    /// transaction can only be rolled back. A panic outside a commit unwinds
    /// into the call that ran it and nothing else.
    #[error(
        "the {callback} you provided panicked while committing{}",
        if *.latched { "; the database is read-only until it is reopened" } else { "" }
    )]
    CallbackPanicked {
        /// The trait the panicking code implements, such as `KeyClassifier`,
        /// or `before_commit`.
        callback: &'static str,
        /// Whether the panic left the database read-only until it is
        /// reopened.
        latched: bool,
    },
    /// A write was stalled behind background work (a flush or a
    /// compaction) whose most recent attempt failed, so waiting would not
    /// end. `source` carries the failure, OS error code included, so a
    /// caller can tell a full disk (ENOSPC) or an exhausted descriptor
    /// table (EMFILE) from anything else. The engine keeps retrying; a
    /// later write succeeds once the job does.
    #[error("background {job} is failing ({hazard}), so writes cannot proceed: {source}")]
    BackgroundFailed {
        /// The job the write was waiting on: `"flush"` or `"compaction"`.
        job: &'static str,
        /// The class of the failure, such as `"disk full"`.
        hazard: &'static str,
        /// The failure itself.
        #[source]
        source: std::io::Error,
    },
    /// A transaction put, deleted or merged a key that its installed
    /// [`crate::KeyClassifier`] declares [`crate::KeyClass::Log`]. Only
    /// [`crate::Transaction::append`] writes the keys of a commit-ordered log.
    #[error("this key belongs to a commit-ordered log; write it with append")]
    LogKeyWrite,
    /// The call returned instead of waiting. A read through a
    /// [`crate::ReadMode::CacheOnly`] handle needed a block the cache does
    /// not hold ([`WouldBlock::Io`]), or a write met a write stall
    /// ([`WouldBlock::Stall`]): poll the queue the wait names, or await the
    /// wait, then run the call again. The call left nothing half done; a
    /// scan resumes where it stopped, and a stalled write applied nothing.
    #[error("{0}")]
    WouldBlock(WouldBlock),
    /// The database is encrypted at rest and was opened without a key
    /// provider. Set [`crate::Options::key_provider`] to the provider it was
    /// written with.
    #[error("this database is encrypted; open it with Options::key_provider")]
    KeyProviderRequired,
    /// A file names a key the [`crate::KeyProvider`] does not provide, or the
    /// provider's current key is one it does not provide. Nothing sealed
    /// under that key can be read, and no file can be sealed, until the
    /// provider provides it.
    #[error("the key provider does not provide key id {id}, which this database needs")]
    UnknownKey {
        /// The key id the provider returned no key for.
        id: crate::KeyId,
    },
    /// An underlying I/O error from the filesystem or operating system.
    #[error("I/O error: {0}")]
    Io(#[source] std::io::Error),
}

// A hand-written `Debug`, because the derived one would print a `MergeFailed`
// key's bytes into a log or a panic message. Every other variant prints what
// the derive would.
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArgument(message) => {
                f.debug_tuple("InvalidArgument").field(message).finish()
            }
            Self::DataBlockLimitExceeded {
                max_data_block_bytes,
            } => f
                .debug_struct("DataBlockLimitExceeded")
                .field("max_data_block_bytes", max_data_block_bytes)
                .finish(),
            Self::Corruption(source) => f.debug_tuple("Corruption").field(source).finish(),
            Self::ReadOnly => f.write_str("ReadOnly"),
            Self::Closed => f.write_str("Closed"),
            Self::InvalidColumnFamily(message) => {
                f.debug_tuple("InvalidColumnFamily").field(message).finish()
            }
            Self::Busy(reason) => f.debug_tuple("Busy").field(reason).finish(),
            Self::MergeFailed(key) => f
                .debug_tuple("MergeFailed")
                .field(&format_args!("{}", RedactedKey(key)))
                .finish(),
            Self::NoMergeOperator => f.write_str("NoMergeOperator"),
            Self::ContentMismatch => f.write_str("ContentMismatch"),
            Self::CallbackPanicked { callback, latched } => f
                .debug_struct("CallbackPanicked")
                .field("callback", callback)
                .field("latched", latched)
                .finish(),
            Self::BackgroundFailed {
                job,
                hazard,
                source,
            } => f
                .debug_struct("BackgroundFailed")
                .field("job", job)
                .field("hazard", hazard)
                .field("source", source)
                .finish(),
            Self::LogKeyWrite => f.write_str("LogKeyWrite"),
            Self::WouldBlock(would_block) => {
                f.debug_tuple("WouldBlock").field(would_block).finish()
            }
            Self::KeyProviderRequired => f.write_str("KeyProviderRequired"),
            Self::UnknownKey { id } => f.debug_struct("UnknownKey").field("id", id).finish(),
            Self::Io(source) => f.debug_tuple("Io").field(source).finish(),
        }
    }
}

impl Error {
    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::InvalidArgument(message.into())
    }

    pub(crate) fn invalid_column_family(message: impl Into<String>) -> Self {
        Self::InvalidColumnFamily(message.into())
    }

    pub(crate) fn corruption(message: impl Into<String>) -> Self {
        Self::Corruption(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.into(),
        ))
    }

    /// Render this engine error as the `io::Error` a caller on an
    /// `io::Result` boundary sees, preserving the kind and message a
    /// direct `io::Error` path (a closed handle, a read-only handle)
    /// already uses for the same condition.
    ///
    /// The variants that carry no payload beyond what the error itself says
    /// travel inside the `io::Error` as its source, and `From<io::Error>`
    /// takes them back out, so a caller on the other side matches the
    /// variant and never the message. Every `io::Result` path in the engine
    /// goes through this one function, so none drifts from the others on what
    /// a given `Error` variant reads as.
    pub(crate) fn into_io_error(self) -> std::io::Error {
        match self {
            Self::Io(io) | Self::Corruption(io) => io,
            // Carried typed, under the failure's own kind: a commit told
            // that the relief of its stop is failing reaches the caller as
            // this variant, as a plain write does.
            Self::BackgroundFailed {
                job,
                hazard,
                source,
            } => std::io::Error::new(
                source.kind(),
                Self::BackgroundFailed {
                    job,
                    hazard,
                    source,
                },
            ),
            Self::InvalidArgument(message) => {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
            }
            // Carried typed, so a write refused for a dropped family when
            // it is decided reaches the caller as this variant.
            typed @ Self::InvalidColumnFamily(_) => {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, typed)
            }
            Self::ReadOnly => {
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, Self::ReadOnly)
            }
            Self::Closed => std::io::Error::new(std::io::ErrorKind::NotConnected, Self::Closed),
            Self::WouldBlock(would_block) => {
                std::io::Error::new(std::io::ErrorKind::WouldBlock, would_block)
            }
            typed @ (Self::DataBlockLimitExceeded { .. }
            | Self::Busy(_)
            | Self::ContentMismatch
            | Self::CallbackPanicked { .. }
            | Self::KeyProviderRequired
            | Self::UnknownKey { .. }) => std::io::Error::other(typed),
            other => std::io::Error::other(other.to_string()),
        }
    }
}

/// The payload-free variant `err` carries as its source, if
/// [`Error::into_io_error`] built it from one. A wait it carries comes back
/// as a clone, which waits on the same read.
fn carried(err: &std::io::Error) -> Option<Error> {
    if let Some(would_block) = copy_would_block(err) {
        return Some(Error::WouldBlock(would_block));
    }
    match err.get_ref()?.downcast_ref::<Error>()? {
        Error::DataBlockLimitExceeded {
            max_data_block_bytes,
        } => Some(Error::DataBlockLimitExceeded {
            max_data_block_bytes: *max_data_block_bytes,
        }),
        Error::Closed => Some(Error::Closed),
        Error::InvalidColumnFamily(message) => Some(Error::InvalidColumnFamily(message.clone())),
        Error::ReadOnly => Some(Error::ReadOnly),
        Error::Busy(reason) => Some(Error::Busy(reason)),
        Error::ContentMismatch => Some(Error::ContentMismatch),
        Error::CallbackPanicked { callback, latched } => Some(Error::CallbackPanicked {
            callback,
            latched: *latched,
        }),
        Error::KeyProviderRequired => Some(Error::KeyProviderRequired),
        Error::UnknownKey { id } => Some(Error::UnknownKey { id: *id }),
        Error::BackgroundFailed {
            job,
            hazard,
            source,
        } => Some(Error::BackgroundFailed {
            job,
            hazard,
            source: Error::clone_io(source),
        }),
        _ => None,
    }
}

impl Error {
    /// Whether `err` carries a typed variant, which context added around it
    /// must not flatten into a plain message.
    pub(crate) fn is_typed(err: &std::io::Error) -> bool {
        carried(err).is_some()
    }

    /// The trait named by the [`Error::CallbackPanicked`] that `err` carries,
    /// if it carries one.
    pub(crate) fn callback_panic_of(err: &std::io::Error) -> Option<&'static str> {
        match carried(err)? {
            Error::CallbackPanicked { callback, .. } => Some(callback),
            _ => None,
        }
    }

    /// A copy of this error, for an outcome several readers are handed: a
    /// ticket's callbacks and the task that awaits it. An I/O error keeps its
    /// kind, message and OS code (see [`Error::clone_io`]); every other
    /// variant stays itself.
    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::InvalidArgument(message) => Self::InvalidArgument(message.clone()),
            Self::DataBlockLimitExceeded {
                max_data_block_bytes,
            } => Self::DataBlockLimitExceeded {
                max_data_block_bytes: *max_data_block_bytes,
            },
            Self::Corruption(err) => Self::Corruption(Self::clone_io(err)),
            Self::ReadOnly => Self::ReadOnly,
            Self::Closed => Self::Closed,
            Self::InvalidColumnFamily(message) => Self::InvalidColumnFamily(message.clone()),
            Self::Busy(reason) => Self::Busy(reason),
            Self::MergeFailed(key) => Self::MergeFailed(key.clone()),
            Self::NoMergeOperator => Self::NoMergeOperator,
            Self::ContentMismatch => Self::ContentMismatch,
            Self::CallbackPanicked { callback, latched } => Self::CallbackPanicked {
                callback,
                latched: *latched,
            },
            Self::BackgroundFailed {
                job,
                hazard,
                source,
            } => Self::BackgroundFailed {
                job,
                hazard,
                source: Self::clone_io(source),
            },
            Self::LogKeyWrite => Self::LogKeyWrite,
            Self::WouldBlock(would_block) => Self::WouldBlock(would_block.clone()),
            Self::KeyProviderRequired => Self::KeyProviderRequired,
            Self::UnknownKey { id } => Self::UnknownKey { id: *id },
            Self::Io(err) => Self::Io(Self::clone_io(err)),
        }
    }

    /// A copy of `err`, for a failure handed to several callers:
    /// `io::Error` is not `Clone`. An OS error keeps its code, a typed
    /// variant the error carries stays that variant, and anything else keeps
    /// its kind and message.
    pub(crate) fn clone_io(err: &std::io::Error) -> std::io::Error {
        if let Some(code) = err.raw_os_error() {
            return std::io::Error::from_raw_os_error(code);
        }
        match carried(err) {
            Some(typed) => typed.into_io_error(),
            None => std::io::Error::new(err.kind(), err.to_string()),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        // A wait is moved out, not copied: the read it names is this one.
        let err = match take_would_block(err) {
            Ok(would_block) => return Self::WouldBlock(would_block),
            Err(err) => err,
        };
        if let Some(typed) = carried(&err) {
            return typed;
        }
        match err.kind() {
            std::io::ErrorKind::InvalidInput => Self::InvalidArgument(err.to_string()),
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
                Self::Corruption(err)
            }
            _ => Self::Io(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_converts_via_from_when_filesystem_related() {
        let ioe = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        let e: Error = ioe.into();
        assert!(matches!(e, Error::Io(_)));
    }

    #[test]
    fn invalid_input_converts_to_invalid_argument() {
        let ioe = std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad key");
        let e: Error = ioe.into();
        assert!(matches!(e, Error::InvalidArgument(msg) if msg.contains("bad key")));
    }

    #[test]
    fn corruption_kinds_convert_to_corruption() {
        for kind in [
            std::io::ErrorKind::InvalidData,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            let ioe = std::io::Error::new(kind, "bad bytes");
            let e: Error = ioe.into();
            assert!(matches!(e, Error::Corruption(source) if source.kind() == kind));
        }
    }

    #[test]
    fn typed_variants_survive_a_round_trip_through_io_error() {
        for typed in [
            Error::Closed,
            Error::ReadOnly,
            Error::Busy("too many L0 files"),
            Error::ContentMismatch,
            Error::CallbackPanicked {
                callback: "KeyClassifier",
                latched: true,
            },
            Error::CallbackPanicked {
                callback: "before_commit",
                latched: false,
            },
            Error::DataBlockLimitExceeded {
                max_data_block_bytes: 4096,
            },
            Error::KeyProviderRequired,
            Error::UnknownKey {
                id: crate::KeyId(9),
            },
            Error::BackgroundFailed {
                job: "flush",
                hazard: "disk full",
                source: std::io::Error::new(std::io::ErrorKind::StorageFull, "no space"),
            },
        ] {
            let text = typed.to_string();
            let io = typed.into_io_error();
            assert_eq!(io.to_string(), text);
            let back = Error::from(io);
            assert_eq!(back.to_string(), text);
            assert!(!matches!(back, Error::Io(_)), "{back:?} lost its variant");
        }
    }

    /// A background failure keeps its job, its class and the failure's own
    /// kind across the I/O boundary, and its `Debug` output there and back
    /// is its three fields.
    #[test]
    fn a_background_failure_crosses_an_io_error_with_its_fields() {
        let source = || std::io::Error::new(std::io::ErrorKind::StorageFull, "no space");
        let failed = || Error::BackgroundFailed {
            job: "compaction",
            hazard: "disk full",
            source: source(),
        };
        let shown = format!(
            "BackgroundFailed {{ job: \"compaction\", hazard: \"disk full\", source: {:?} }}",
            source()
        );
        assert_eq!(format!("{:?}", failed()), shown);

        let io = failed().into_io_error();
        assert_eq!(io.kind(), std::io::ErrorKind::StorageFull);
        let back = Error::from(io);
        assert_eq!(format!("{back:?}"), shown);
        assert!(matches!(
            back,
            Error::BackgroundFailed { job: "compaction", hazard: "disk full", source }
                if source.kind() == std::io::ErrorKind::StorageFull
        ));
    }

    #[test]
    fn a_cloned_io_error_keeps_its_variant() {
        for typed in [
            Error::Closed,
            Error::ReadOnly,
            Error::Busy("stalled"),
            Error::ContentMismatch,
            Error::CallbackPanicked {
                callback: "EventListener",
                latched: true,
            },
            Error::CallbackPanicked {
                callback: "before_commit",
                latched: false,
            },
        ] {
            let text = typed.to_string();
            let copy = Error::clone_io(&typed.into_io_error());
            assert_eq!(copy.to_string(), text);
            assert!(!matches!(Error::from(copy), Error::Io(_)));
        }
    }

    #[test]
    fn a_callback_panic_is_named_through_an_io_error() {
        let err = Error::CallbackPanicked {
            callback: "RateLimiter",
            latched: true,
        }
        .into_io_error();
        assert_eq!(Error::callback_panic_of(&err), Some("RateLimiter"));
        assert_eq!(
            Error::callback_panic_of(&Error::Closed.into_io_error()),
            None
        );
        let plain = std::io::Error::other("not typed");
        assert_eq!(Error::callback_panic_of(&plain), None);
    }

    #[test]
    fn only_a_panic_that_latched_the_database_says_it_is_read_only() {
        let latched = Error::CallbackPanicked {
            callback: "KeyClassifier",
            latched: true,
        };
        assert_eq!(
            latched.to_string(),
            "the KeyClassifier you provided panicked while committing; the database is \
             read-only until it is reopened"
        );
        let loose = Error::CallbackPanicked {
            callback: "before_commit",
            latched: false,
        };
        assert_eq!(
            loose.to_string(),
            "the before_commit you provided panicked while committing"
        );
    }

    #[test]
    fn a_panic_keeps_whether_it_latched_through_an_io_error() {
        for latched in [true, false] {
            let io = Error::CallbackPanicked {
                callback: "before_commit",
                latched,
            }
            .into_io_error();
            let copy = Error::clone_io(&io);
            assert!(matches!(
                Error::from(copy),
                Error::CallbackPanicked { latched: l, .. } if l == latched
            ));
            assert!(matches!(
                Error::from(io),
                Error::CallbackPanicked { latched: l, .. } if l == latched
            ));
        }
    }

    fn a_wait() -> WouldBlock {
        use crate::engine::io::shared::{QueueShared, WaitSlot, test_id};
        let queue = QueueShared::new(test_id(), 1);
        WouldBlock::Io(crate::IoWait::new(
            &queue,
            None,
            std::sync::Arc::new(WaitSlot::new()),
        ))
    }

    #[test]
    fn a_wait_moves_through_an_io_error_and_back() {
        let io = Error::WouldBlock(a_wait()).into_io_error();
        assert_eq!(io.kind(), std::io::ErrorKind::WouldBlock);
        // A copy waits on the same read, and the original moves out whole.
        let copy = Error::clone_io(&io);
        assert!(matches!(
            Error::from(copy),
            Error::WouldBlock(WouldBlock::Io(_))
        ));
        let Error::WouldBlock(WouldBlock::Io(wait)) = Error::from(io) else {
            panic!("the wait was lost on the way out");
        };
        assert_eq!(wait.queue(), crate::engine::io::shared::test_id());
        assert!(wait.unit().is_none());
        assert!(!wait.is_ready());
    }

    #[test]
    fn a_wait_is_a_transaction_wait_never_an_engine_failure() {
        let from_error = crate::TransactionError::from(Error::WouldBlock(a_wait()));
        assert!(matches!(
            from_error,
            crate::TransactionError::WouldBlock(WouldBlock::Io(_))
        ));
        let from_io = crate::TransactionError::from(Error::WouldBlock(a_wait()).into_io_error());
        assert!(matches!(
            from_io,
            crate::TransactionError::WouldBlock(WouldBlock::Io(_))
        ));
        let message = Error::WouldBlock(a_wait()).to_string();
        assert!(message.contains("poll its I/O queue"), "{message}");
    }

    #[test]
    fn closed_keeps_its_io_kind_and_message() {
        let io = Error::Closed.into_io_error();
        assert_eq!(io.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(io.to_string(), "database is closed");
    }

    #[test]
    fn log_key_write_says_what_to_do() {
        let msg = Error::LogKeyWrite.to_string();
        assert!(msg.contains("append"), "{msg}");
    }

    #[test]
    fn busy_display_contains_reason() {
        let e = Error::Busy("too many L0 files");
        let msg = format!("{e}");
        assert!(msg.contains("too many L0 files"));
        assert!(msg.contains("busy"));
    }

    #[test]
    fn a_failed_merge_shows_its_key_by_length_and_hash_never_by_bytes() {
        let e = Error::MergeFailed(b"SECRET".to_vec());
        let shown = RedactedKey(b"SECRET").to_string();
        assert_eq!(
            e.to_string(),
            format!("merge operator failed for a {shown}")
        );
        assert_eq!(format!("{e:?}"), format!("MergeFailed({shown})"));
        // The text a caller on an `io::Result` boundary reads is the same one.
        let through_io = e.duplicate().into_io_error().to_string();
        for text in [
            e.to_string(),
            format!("{e:?}"),
            format!("{e:#?}"),
            through_io,
        ] {
            assert!(text.contains("6-byte key (hash "), "{text}");
            let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            for leaked in ["SECRET", "83,69,67"] {
                assert!(!squeezed.contains(leaked), "{leaked} leaked into {text}");
            }
        }
        assert!(matches!(e, Error::MergeFailed(key) if key == b"SECRET"));
    }

    /// The hand-written `Debug` prints every other variant as a derive would:
    /// a unit variant by name, a tuple variant with its field, a struct
    /// variant with its named fields.
    #[test]
    fn the_other_variants_debug_as_a_derive_prints_them() {
        assert_eq!(format!("{:?}", Error::Closed), "Closed");
        assert_eq!(format!("{:?}", Error::NoMergeOperator), "NoMergeOperator");
        assert_eq!(format!("{:?}", Error::Busy("stalled")), "Busy(\"stalled\")");
        assert_eq!(
            format!("{:?}", Error::invalid_argument("bad")),
            "InvalidArgument(\"bad\")"
        );
        assert_eq!(
            format!(
                "{:?}",
                Error::DataBlockLimitExceeded {
                    max_data_block_bytes: 4096
                }
            ),
            "DataBlockLimitExceeded { max_data_block_bytes: 4096 }"
        );
        assert_eq!(
            format!(
                "{:?}",
                Error::CallbackPanicked {
                    callback: "KeyClassifier",
                    latched: true
                }
            ),
            "CallbackPanicked { callback: \"KeyClassifier\", latched: true }"
        );
        let io = || std::io::Error::other("disk");
        assert_eq!(format!("{:?}", Error::Io(io())), format!("Io({:?})", io()));
        assert_eq!(
            format!("{:?}", Error::Corruption(io())),
            format!("Corruption({:?})", io())
        );
    }
}
