use defra_node::Node as EmbeddedNode;

use crate::error::{Error, Result};
use crate::expectation::CollectionExpectation;
use crate::registry::{BaselineCollection, MigrationStep};

/// An rc0 `AgentMessage` after v1.0.0-rc1's `native_request_id` patch.
const MESSAGE_REQUEST_ID_TARGET: &str =
    "bafyreiawf2s2jdgsajh6rff4plix6e4fawkkqu22dy3ade5pnzaaomqtj4";
/// `AgentMessage` as a store created by v1.0.0-rc1 registered it (with `native_request_id`).
const MESSAGE_RC1_FRESH: &str = "bafyreih6qxw3cq3uswastxlatdplwmkpknqkf7csgecoswijez5o2uihhm";

/// Versions earlier builds produced that are off this release's lineage, so a peer
/// advertising one is named as the side to re-create rather than asked to update.
/// `AgentMessage` is client-authored and registers its current SDL directly, so rc1's
/// `native_request_id` column is off the lineage whether a store was created by rc1 or
/// patched by it. A version this release registers is never listed: a fresh store's
/// canonical version would otherwise be reported as off its own lineage.
pub(crate) const KNOWN_OFF_LINEAGE: &[(&str, &str)] = &[
    ("AgentMessage", MESSAGE_REQUEST_ID_TARGET),
    ("AgentMessage", MESSAGE_RC1_FRESH),
];

/// Collections whose fresh store registers an older root and replays the versioned patches
/// an upgraded store takes, so both land on one version ID (DefraDB derives a patched
/// version ID from the version it patches, so a direct registration of the current SDL and
/// a patched store never share one). Empty: every collection's fresh store registers its
/// current SDL, which is the canonical version.
///
/// A collection added here names its root SDL and root version, and its
/// [`crate::DEFAULT_BASELINE`] entry must register the same SDL and root.
pub(crate) const CANONICAL_ROOTS: &[(&str, &str, &str)] = &[];

fn canonical_root(name: &str) -> Option<&'static str> {
    CANONICAL_ROOTS
        .iter()
        .find(|(collection, _, _)| *collection == name)
        .map(|(_, _, root)| *root)
}

/// Native workflow documents carry the handoff route their producer stamps
/// (Lean `Triggers.NativeRoute`); existing rows keep null route columns.
const HANDOFF_ROUTE_FIELDS: [&str; 3] = ["handoff_id", "reply_session_id", "attempt"];

macro_rules! handoff_route_patch {
    ($name:literal) => {
        concat!(
            r#"[{"op":"add","path":"/"#,
            $name,
            r#"/Fields/-","value":{"Name":"handoff_id","Kind":"String","Immutable":true}},{"op":"add","path":"/"#,
            $name,
            r#"/Fields/-","value":{"Name":"reply_session_id","Kind":"String","Immutable":true}},{"op":"add","path":"/"#,
            $name,
            r#"/Fields/-","value":{"Name":"attempt","Kind":"Int","Immutable":true}}]"#
        )
    };
}

/// DefraDB field positions are persistent identifiers, so a store an earlier build of
/// this lineage registered appends fields through versioned patches; a fresh store
/// registers the current SDL. Only these exact source and destination versions qualify.
/// Stores from before the native identity baseline are refused (`UnknownLineage`) by the
/// `Node` check instead of being converted, so no step starts from their versions.
pub(crate) fn include_existing(
    node: &EmbeddedNode,
    baseline: &mut [BaselineCollection<'static>],
    steps: &mut Vec<MigrationStep<'static>>,
) -> Result<()> {
    include_existing_by_version(
        &|name| {
            node.get_collection(name)
                .map(|current| current.map(|current| current.version_id))
                .map_err(|error| Error::Node(error.into()))
        },
        baseline,
        steps,
    )
}

pub(crate) fn include_existing_by_version(
    get_version: &impl Fn(&str) -> Result<Option<String>>,
    baseline: &mut [BaselineCollection<'static>],
    steps: &mut Vec<MigrationStep<'static>>,
) -> Result<()> {
    // A collection the store lacks is registered at its canonical root (the baseline entry),
    // so it takes the same steps a store holding that root takes.
    let get_version = &|name: &str| -> Result<Option<String>> {
        Ok(get_version(name)?.or_else(|| canonical_root(name).map(str::to_owned)))
    };
    for (name, root, target, id, patch, fields) in [
        (
            "Task",
            "bafyreieh67b32auhf33bkdltep2gh6gmrb5wgs2sd7vxexmqxno45jnmdm",
            "bafyreifpyagnw5qkrsko323nlukdzoixgmzalbzcf5nn3urrmxtouopr6e",
            "task-add-declared-outcome-readers",
            r#"[{"op":"add","path":"/Task/Fields/-","value":{"Name":"outcome_readers","Kind":"JSON"}}]"#,
            &["outcome_readers"][..],
        ),
        (
            "RepositoryPlacement",
            "bafyreieks33oeixd4h4sdqfmli3fyuksjo3bkw7povukuak6xvavl7sbxq",
            "bafyreidwzvocwv4nkgwr326qyd4ofkw74dm4zl4wbesbmfzmbkdseqq3zy",
            "repository-placement-add-declared-readers",
            r#"[{"op":"add","path":"/RepositoryPlacement/Fields/-","value":{"Name":"readers","Kind":"JSON"}}]"#,
            &["readers"][..],
        ),
        (
            "CallbackResult",
            "bafyreiay7i7erfmg4o56oqrzbbwrohw253x7yme4plut46t4tgpzujdcq4",
            "bafyreiavdoikuwav2vgp4lxpnulwqlm6tvlqkmw7tugwb7gtjs5twd7kpq",
            "callback-result-add-handoff-route",
            handoff_route_patch!("CallbackResult"),
            &HANDOFF_ROUTE_FIELDS[..],
        ),
        (
            "IsolatedWorkspace",
            "bafyreidyccgfeed6jazx6ssevrlcvucqby5q5nknzpulvu2aiciehqpkpq",
            "bafyreies2644xt5ywgfr2gzjwug2rpmlq6c25fsbcqu7tgwb7u6pfkuvxe",
            "isolated-workspace-add-handoff-route",
            handoff_route_patch!("IsolatedWorkspace"),
            &HANDOFF_ROUTE_FIELDS[..],
        ),
        (
            "WorkspaceReceipt",
            "bafyreiecsdzgs6zctavbmm7hom5saefv3k23qmrfmnbeguriji346ofnnq",
            "bafyreifdy3cjtrymcygl4cjfnoaouaru5gnuccthaw246i4t2lqahy3mky",
            "workspace-receipt-add-handoff-route",
            handoff_route_patch!("WorkspaceReceipt"),
            &HANDOFF_ROUTE_FIELDS[..],
        ),
    ] {
        let Some(current) = get_version(name)? else {
            continue;
        };
        if current != root && current != target {
            continue;
        }
        if let Some(entry) = baseline.iter_mut().find(|entry| entry.name == name) {
            entry.expected_version = Some(root);
            steps.push(MigrationStep::PatchVersioned {
                id,
                collection: name,
                patch,
                lens: None,
                expected_version: Some(target),
                expected_transform: None,
                expected_state: CollectionExpectation::fields(fields),
            });
        }
    }
    Ok(())
}
