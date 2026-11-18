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

/// A command Task's host command (gents-ai/gents#3951); existing rows read null.
const TASK_COMMAND_PATCH: &str =
    r#"[{"op":"add","path":"/Task/Fields/-","value":{"Name":"command","Kind":"JSON"}}]"#;

/// The last run of a command Task a Trigger fired; existing rows read null.
const TRIGGER_LAST_RUN_PATCH: &str =
    r#"[{"op":"add","path":"/Trigger/Fields/-","value":{"Name":"last_run","Kind":"JSON"}}]"#;

/// A collection, its source and destination versions, the step id, its patch and the
/// fields the destination must have.
type Upgrade = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
);

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
    let upgrades: &[Upgrade] = &[
        (
            "Task",
            "bafyreieh67b32auhf33bkdltep2gh6gmrb5wgs2sd7vxexmqxno45jnmdm",
            "bafyreifpyagnw5qkrsko323nlukdzoixgmzalbzcf5nn3urrmxtouopr6e",
            "task-add-declared-outcome-readers",
            r#"[{"op":"add","path":"/Task/Fields/-","value":{"Name":"outcome_readers","Kind":"JSON"}}]"#,
            &["outcome_readers"][..],
        ),
        (
            "Task",
            "bafyreifpyagnw5qkrsko323nlukdzoixgmzalbzcf5nn3urrmxtouopr6e",
            "bafyreibmskcylyd6nphbjfn4ecfw67c64tcajn6t7yjtwwacphbpnd3svi",
            "task-add-command",
            TASK_COMMAND_PATCH,
            &["command"][..],
        ),
        (
            "Task",
            "bafyreifoxyabazkzdh6rvkvhx4524dq4z2nyewnro2dfjrtnemcjyizl5a",
            "bafyreidkk5cwi6ckjusgnkgqddcqs273kkyjfqm34eje32e2srsxdgj6iq",
            "task-add-command",
            TASK_COMMAND_PATCH,
            &["command"][..],
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
        (
            "PackInstallation",
            "bafyreia5f6iex3vrzfjzzlpm7jqkqehmnis3vqnbcdyuno2jlafyyrx3vu",
            "bafyreif63c7rk5p7u66z6wvhab5gjh2544wt6uauq7ndmfszizfixsmwmi",
            "pack-installation-add-source-and-variables",
            r#"[{"op":"add","path":"/PackInstallation/Fields/-","value":{"Name":"source","Kind":"JSON"}},{"op":"add","path":"/PackInstallation/Fields/-","value":{"Name":"source_history","Kind":"JSON"}},{"op":"add","path":"/PackInstallation/Fields/-","value":{"Name":"variables","Kind":"JSON"}}]"#,
            &["source", "source_history", "variables"][..],
        ),
        (
            "Trigger",
            "bafyreiez56udcss6hqfibh4j5fxfhbac7wp63y7pfz77vwvqz2fqrt5f74",
            "bafyreicvfje3ivdjbxuxd6gbne5skoruiws3hm2vz5pngehfhsr4so4nuq",
            "trigger-add-session-creation-and-fire-budget",
            r#"[{"op":"add","path":"/Trigger/Fields/-","value":{"Name":"create_session","Kind":"Boolean"}},{"op":"add","path":"/Trigger/Fields/-","value":{"Name":"max_fires_per_hour","Kind":"Int"}}]"#,
            &["create_session", "max_fires_per_hour"][..],
        ),
        (
            "Trigger",
            "bafyreicvfje3ivdjbxuxd6gbne5skoruiws3hm2vz5pngehfhsr4so4nuq",
            "bafyreiafysif5uf5kpq6cilcsgk6on4gp7l5nvgun2ip2fovvb5c2nrbui",
            "trigger-add-last-run",
            TRIGGER_LAST_RUN_PATCH,
            &["last_run"][..],
        ),
        (
            "Trigger",
            "bafyreiaof6tzfe5u3qt3sqc7in6isxl5b4nztukemi7yhmqa7kbgsdrwju",
            "bafyreib42wjbg2ofdj6t4z3hamptdligfx7greiyrckxhhzadj44qtnjgi",
            "trigger-add-last-run",
            TRIGGER_LAST_RUN_PATCH,
            &["last_run"][..],
        ),
    ];
    let mut seen = Vec::new();
    for &(name, ..) in upgrades {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        let Some(current) = get_version(name)? else {
            continue;
        };
        let entries = upgrades
            .iter()
            .filter(|entry| entry.0 == name)
            .collect::<Vec<_>>();
        let Some(at) = entries
            .iter()
            .position(|entry| current == entry.1 || current == entry.2)
        else {
            continue;
        };
        // A store takes the whole chain of patches its version is on, each
        // destination the next step's source, so every version it passed
        // through stays a known pin.
        let mut start = at;
        while start > 0 && entries[start - 1].2 == entries[start].1 {
            start -= 1;
        }
        let mut end = at;
        while end + 1 < entries.len() && entries[end + 1].1 == entries[end].2 {
            end += 1;
        }
        if let Some(entry) = baseline.iter_mut().find(|entry| entry.name == name) {
            entry.expected_version = Some(entries[start].1);
            for &&(_, _, target, id, patch, fields) in &entries[start..=end] {
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
    }
    Ok(())
}
