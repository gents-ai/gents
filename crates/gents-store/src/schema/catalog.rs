use std::sync::LazyLock;

use defra_core::RequestContext;
use defra_node::Node as EmbeddedNode;

mod compatibility;
mod error;
mod indexes;
mod install;
mod node_policies;
mod shared_transport;
pub(crate) use shared_transport::{ensure_admission_schema, SharedTransportHome};
#[cfg(test)]
mod tests;
#[cfg(any(test, feature = "vera"))]
pub mod vera_seed;

pub use compatibility::{check_lineages, validate_existing_schema};
pub use error::SchemaError;
pub use indexes::ORDINARY_INDEXES;
pub use install::{ensure, ensure_runtime, ensure_runtime_owned, RuntimeSchemaError};

#[derive(Debug, Clone, Copy)]
pub struct BaselineCollection<'a> {
    pub name: &'a str,
    pub sdl: &'a str,
    pub expected_version: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct OrdinaryIndex<'a> {
    pub collection: &'a str,
    pub name: &'a str,
    pub fields: &'a [OrdinaryIndexField<'a>],
}

#[derive(Debug, Clone, Copy)]
pub struct OrdinaryIndexField<'a> {
    pub name: &'a str,
    pub descending: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Catalog<'a> {
    pub baseline: &'a [BaselineCollection<'a>],
    pub ordinary_indexes: &'a [OrdinaryIndex<'a>],
}

impl<'a> Catalog<'a> {
    pub fn managed_names(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.baseline.iter().map(|entry| entry.name)
    }

    pub fn manages(&self, name: &str) -> bool {
        self.managed_names().any(|managed| managed == name)
    }
}

static BASELINE_SCHEMAS: LazyLock<Vec<(&'static str, &'static str)>> = LazyLock::new(|| {
    use gents_protocol::schemas::{
        ALL, ALL_COLLECTION_NAMES, RUNTIME_ALL, RUNTIME_COLLECTION_NAMES,
    };

    assert_eq!(RUNTIME_COLLECTION_NAMES.len(), RUNTIME_ALL.len());
    assert_eq!(ALL_COLLECTION_NAMES.len(), ALL.len());
    RUNTIME_COLLECTION_NAMES
        .iter()
        .copied()
        .zip(RUNTIME_ALL.iter().copied())
        .chain(
            ALL_COLLECTION_NAMES
                .iter()
                .copied()
                .zip(ALL.iter().copied()),
        )
        .collect()
});

// The native parser derives version IDs from the complete bundled SDL, including
// policy bindings and relation identities, so each set of policy IDs has its own
// version IDs. Keep the parsed roots alive for the static baseline instead of
// maintaining a second list of version IDs.
fn build_catalog(ids: &gents_protocol::schemas::HomePolicyIds) -> Catalog<'static> {
    let sdl = BASELINE_SCHEMAS
        .iter()
        .map(|(_, sdl)| *sdl)
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = query::parse_sdl(&ids.render(&sdl)).expect("bundled baseline SDL must parse");
    assert_eq!(
        parsed.len(),
        BASELINE_SCHEMAS.len(),
        "bundled baseline collection count"
    );
    let baseline: Vec<BaselineCollection<'static>> = BASELINE_SCHEMAS
        .iter()
        .map(|(name, sdl)| {
            let mut matches = parsed.iter().filter(|schema| schema.name == *name);
            let schema = matches
                .next()
                .expect("bundled baseline collection must exist");
            assert!(
                matches.next().is_none(),
                "duplicate bundled baseline collection"
            );
            let root: &'static str = Box::leak(schema.version_id.clone().into_boxed_str());
            BaselineCollection {
                name,
                sdl,
                expected_version: Some(root),
            }
        })
        .collect();
    Catalog {
        baseline: Box::leak(baseline.into_boxed_slice()),
        ordinary_indexes: ORDINARY_INDEXES,
    }
}

static NODE_STORE_CATALOG: LazyLock<Catalog<'static>> =
    LazyLock::new(|| build_catalog(gents_protocol::schemas::HomePolicyIds::node_store()));

static EMBEDDED_VERA_CATALOG: LazyLock<Catalog<'static>> =
    LazyLock::new(|| build_catalog(gents_protocol::schemas::HomePolicyIds::embedded_vera()));

static EMBEDDED_VERA_TRANSPORT_V2_CATALOG: LazyLock<Catalog<'static>> = LazyLock::new(|| {
    build_catalog(gents_protocol::schemas::HomePolicyIds::embedded_vera_transport_v2())
});

/// The runtime's catalog for a home whose policies have `ids`, built once per
/// set of IDs.
pub fn runtime_catalog(ids: &gents_protocol::schemas::HomePolicyIds) -> &'static Catalog<'static> {
    if ids == gents_protocol::schemas::HomePolicyIds::embedded_vera_transport_v2() {
        &EMBEDDED_VERA_TRANSPORT_V2_CATALOG
    } else if ids == gents_protocol::schemas::HomePolicyIds::embedded_vera() {
        &EMBEDDED_VERA_CATALOG
    } else {
        assert_eq!(ids, gents_protocol::schemas::HomePolicyIds::node_store());
        &NODE_STORE_CATALOG
    }
}

/// Whether `name` is a collection the runtime manages, whatever its home's
/// policy IDs.
pub fn is_runtime_collection(name: &str) -> bool {
    BASELINE_SCHEMAS.iter().any(|(managed, _)| *managed == name)
}

/// Every collection the runtime manages, whatever its home's policy IDs.
pub fn runtime_collection_names() -> impl Iterator<Item = &'static str> {
    BASELINE_SCHEMAS.iter().map(|(name, _)| *name)
}

/// [`runtime_catalog`]'s collections.
pub fn baseline(
    ids: &gents_protocol::schemas::HomePolicyIds,
) -> &'static [BaselineCollection<'static>] {
    runtime_catalog(ids).baseline
}

const FILE_COLLECTION_POLICIES: &[(&str, bool)] = &[
    (gents_protocol::schemas::TOOL_FILE_CHANGE_NAME, false),
    (gents_protocol::schemas::TOOL_FILE_CONTENT_NAME, false),
    (gents_protocol::schemas::TOOL_FILE_INVENTORY_NAME, false),
    (gents_protocol::schemas::FILE_INVENTORY_REPLY_NAME, false),
    (gents_protocol::schemas::FILE_INVENTORY_PROGRESS_NAME, false),
];

fn manages_identity(catalog: &Catalog<'_>) -> bool {
    catalog.managed_names().any(|name| {
        gents_protocol::schemas::IDENTITY_POLICY_BINDINGS
            .iter()
            .any(|(collection, _)| *collection == name)
    })
}

fn manages_client_journal(catalog: &Catalog<'_>) -> bool {
    catalog.manages(gents_protocol::schemas::PENDING_CHAT_SUBMISSION_NAME)
}

fn manages_mailbox(catalog: &Catalog<'_>) -> bool {
    catalog.manages(gents_protocol::schemas::MAILBOX_ITEM_NAME)
}

fn manages_file_images(catalog: &Catalog<'_>) -> bool {
    catalog.managed_names().any(|name| {
        FILE_COLLECTION_POLICIES
            .iter()
            .any(|(collection, _)| *collection == name)
    })
}

fn node_context(node: &EmbeddedNode) -> RequestContext {
    RequestContext::default().with_identity(node.node_identity_did().map(str::to_owned))
}
