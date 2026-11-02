use defra_node::Node as EmbeddedNode;

use crate::error::{Error, Result};
use crate::expectation::CollectionExpectation;
use crate::registry::{BaselineCollection, MigrationStep};

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
            "ToolServiceRegistry",
            "bafyreigckps33q33zvnrodzqlz72ri4osyodxleniwy6txwkfhnq7tgg3q",
            "bafyreibvc5vydfmb6swugrjjff5c7eqfymcw6ckm5kn62wqz6tfmlvir2y",
            "tool-service-registry-add-endpoint-url",
            r#"[{"op":"add","path":"/ToolServiceRegistry/Fields/-","value":{"Name":"endpoint_url","Kind":"String"}}]"#,
            &["endpoint_url"][..],
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
