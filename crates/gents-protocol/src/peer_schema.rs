//! Compatibility fingerprint over the collection schemas two peers replicate.
//!
//! DefraDB admits a peer connection regardless of schema, then rejects every
//! pushed block whose collection version the receiver does not know. Gents
//! has no schema upgrade path between builds, so a peer whose replicated
//! schema set differs is incompatible and must be reported instead of being
//! allowed to author documents that never merge.

use std::fmt;

use sha2::{Digest as _, Sha256};

/// Top-level `/status` field carrying the runtime's replicated schema
/// fingerprint for the client route.
pub const STATUS_REPLICATED_SCHEMA_FINGERPRINT_FIELD: &str = "replicated_schema_fingerprint";

const REPLICATED_SCHEMA_DOMAIN: &str = "gents-replicated-schema-v1";

/// Fingerprint `(collection, sdl)` pairs independent of input order.
///
/// SDL comments and whitespace do not change DefraDB collection versions, so
/// they are removed before hashing; any other text change is a new schema.
pub fn replicated_schema_fingerprint<'a>(
    collections: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> String {
    let mut collections = collections
        .into_iter()
        .map(|(name, sdl)| (name, canonical_sdl(sdl)))
        .collect::<Vec<_>>();
    collections.sort();
    collections.dedup();
    let mut hash = Sha256::new();
    hash.update(REPLICATED_SCHEMA_DOMAIN.as_bytes());
    for (name, sdl) in &collections {
        for field in [name.as_bytes(), sdl.as_bytes()] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field);
        }
    }
    let digest = hash.finalize();
    let mut rendered = String::from("sha256:");
    for byte in digest {
        rendered.push_str(&format!("{byte:02x}"));
    }
    rendered
}

fn canonical_sdl(sdl: &str) -> String {
    sdl.lines()
        .map(|line| line.split_once('#').map_or(line, |(code, _)| code))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A peer whose replicated schema set differs from this build's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicatedSchemaSkew {
    pub local: String,
    /// `None` when the peer advertises no fingerprint, which only builds
    /// older than this check do.
    pub remote: Option<String>,
}

impl fmt::Display for ReplicatedSchemaSkew {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remote = self.remote.as_deref().unwrap_or("not advertised");
        write!(
            f,
            "this app and the agent runtime are different Gents versions, so their synced \
             data is incompatible and writes would never reach the runtime; update the app \
             and the runtime to the same version (app schema {}, runtime schema {remote})",
            self.local
        )
    }
}

impl std::error::Error for ReplicatedSchemaSkew {}

/// Compare this build's fingerprint with the one a peer advertises.
pub fn check_replicated_schema(
    local: &str,
    remote: Option<&str>,
) -> Result<(), ReplicatedSchemaSkew> {
    let remote = remote.map(str::trim).filter(|remote| !remote.is_empty());
    if remote == Some(local) {
        return Ok(());
    }
    Err(ReplicatedSchemaSkew {
        local: local.to_string(),
        remote: remote.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "type AgentSession {\n    session_id: String\n}\n";

    #[test]
    fn identical_schema_sets_are_compatible_in_any_order() {
        let a =
            replicated_schema_fingerprint([("AgentSession", SESSION), ("Task", "type Task {}")]);
        let b =
            replicated_schema_fingerprint([("Task", "type Task {}"), ("AgentSession", SESSION)]);
        assert_eq!(a, b);
        assert_eq!(check_replicated_schema(&a, Some(&b)), Ok(()));
    }

    #[test]
    fn a_changed_replicated_collection_is_incompatible() {
        let local = replicated_schema_fingerprint([("AgentSession", SESSION)]);
        let remote = replicated_schema_fingerprint([(
            "AgentSession",
            "type AgentSession {\n    session_id: String\n    title: String\n}\n",
        )]);
        assert_ne!(local, remote);
        let skew = check_replicated_schema(&local, Some(&remote)).unwrap_err();
        assert_eq!(skew.remote.as_deref(), Some(remote.as_str()));
        assert!(skew.to_string().contains("update the app and the runtime"));
    }

    #[test]
    fn an_added_or_renamed_collection_is_incompatible() {
        let local = replicated_schema_fingerprint([("AgentSession", SESSION)]);
        let added =
            replicated_schema_fingerprint([("AgentSession", SESSION), ("Task", "type Task {}")]);
        let renamed = replicated_schema_fingerprint([("Session", SESSION)]);
        assert_ne!(local, added);
        assert_ne!(local, renamed);
    }

    #[test]
    fn comments_and_whitespace_do_not_change_the_fingerprint() {
        let reformatted = "# session\ntype   AgentSession {\n\tsession_id: String # id\n}\n\n";
        assert_eq!(
            replicated_schema_fingerprint([("AgentSession", SESSION)]),
            replicated_schema_fingerprint([("AgentSession", reformatted)])
        );
    }

    #[test]
    fn a_peer_without_a_fingerprint_is_incompatible() {
        let local = replicated_schema_fingerprint([("AgentSession", SESSION)]);
        for remote in [None, Some(""), Some("  ")] {
            let skew = check_replicated_schema(&local, remote).unwrap_err();
            assert_eq!(skew.remote, None);
            assert!(skew.to_string().contains("not advertised"));
        }
    }
}
