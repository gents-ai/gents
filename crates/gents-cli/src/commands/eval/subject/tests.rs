use serde_json::Value;

use super::*;
use crate::commands::init::default_packs::tests::{
    borrowed, explained_engineer, fixture_archives, init_args,
};
use crate::commands::init::default_packs::InitEntryPoint;
use gents_eval::runner::embedded::executor::install_subject_for_tests;
use gents_eval::runner::Isolation;
use gents_pack::pack::core_packs::CorePack;

/// The tools a trial gives its Engineer and those it hides equal what
/// `gents init --engineer` gives the Engineer of a fresh home from the same
/// core pack archives, with host bash and background processes where command
/// execution is allowed, and without exactly those where it is off. The
/// Engineer's own Context and Tools documents match too, but for the ids,
/// node and root the trial binds.
#[tokio::test]
async fn the_trial_engineer_is_the_engineer_init_makes() {
    let archives = fixture_archives().await;
    let archives = borrowed(&archives);
    let product = tempfile::tempdir().unwrap();
    let initialized = crate::commands::init::init_home_with_core_packs(
        init_args(product.path(), &[]),
        &archives,
        InitEntryPoint::Cli,
    )
    .await
    .unwrap();
    assert_eq!(
        initialized["default_packs"]["not_installed"],
        serde_json::json!([]),
        "{initialized}"
    );
    let (default, default_context, default_tools) = explained_engineer(product.path()).await;
    let command_tools: Vec<&str> = default
        .included
        .get("background_process")
        .into_iter()
        .flatten()
        .chain(
            default
                .included
                .get("host")
                .into_iter()
                .flatten()
                .filter(|name| matches!(name.as_str(), "bash" | "bash_unrestricted")),
        )
        .map(String::as_str)
        .collect();
    assert!(
        command_tools.contains(&"bash_unrestricted") && command_tools.len() > 1,
        "a fresh Engineer runs commands and background processes: {:?}",
        default.tool_names
    );

    let core_packs: Vec<String> = CorePack::ALL
        .iter()
        .map(|pack| pack.coordinate().to_owned())
        .collect();
    for (mode, host_bash, isolation) in [
        (
            CommandExecutionArg::Unrestricted,
            true,
            Isolation::Contained,
        ),
        (CommandExecutionArg::Off, false, Isolation::Embedded),
    ] {
        let dir = tempfile::tempdir().unwrap();
        for (path, bytes) in engineer_subject(mode).unwrap() {
            let destination = dir.path().join(path);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::write(destination, bytes).unwrap();
        }
        let trial = install_subject_for_tests(
            dir.path(),
            SLOT,
            &core_packs,
            &[],
            &archives,
            host_bash,
            isolation,
        )
        .await
        .unwrap();
        let access = trial.home.client_access(&trial.evaluator_did).unwrap();
        let owner = trial.home.did().to_owned();
        let explained = crate::commands::tools::explain_agent(
            &access,
            &owner,
            &trial.agent_did,
            &trial.tool_ceiling,
        )
        .await
        .unwrap();
        let (context, tools) = access
            .transact_readonly("test.subject.documents", |txn| {
                let (owner, agent) = (&owner, &trial.agent_did);
                Box::pin(async move {
                    gents_documents::load_agent_context_and_tools_in_txn(txn, owner, agent).await
                })
            })
            .await
            .unwrap();
        trial.home.node.shutdown().await.unwrap();

        let expected: Vec<&str> = default
            .tool_names
            .iter()
            .map(String::as_str)
            .filter(|name| host_bash || !command_tools.contains(name))
            .collect();
        assert_eq!(
            explained
                .tool_names
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            expected,
            "{mode:?}: the trial Engineer's tools"
        );
        assert_eq!(
            explained.hidden_tool_names, default.hidden_tool_names,
            "{mode:?}: the tools the trial Engineer's Tools hide"
        );
        let mut expected_tools = default_tools.clone();
        if !host_bash {
            if let Some(bash) = expected_tools
                .host
                .as_mut()
                .and_then(|host| host.bash.as_mut())
            {
                bash.mode = gents_documents::BashMode::Off;
            }
        }
        let expected_tools = bound(&expected_tools);
        same_fields(
            &installed_as_init(bound(&tools.unwrap())),
            &expected_tools,
            &format!("{mode:?}: the Engineer's Tools"),
        );
        same_fields(
            &installed_as_init(bound(&context.unwrap())),
            &bound(&default_context),
            &format!("{mode:?}: the Engineer's Context"),
        );
    }
}

fn suites_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents/tests/fixtures/configurator_evals")
}

/// Every suite's fixtures only add to the Engineer init makes, in either
/// command execution mode, and the subject they make loads as a pack.
#[test]
fn every_suite_layers_its_fixtures_on_the_engineer_init_makes() {
    let mut suites = Vec::new();
    let mut pending = vec![suites_root()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.ends_with("suite.json") {
                suites.push(dir.clone());
            }
        }
    }
    assert!(suites.len() > 1, "{suites:?}");
    for mode in [CommandExecutionArg::Off, CommandExecutionArg::Unrestricted] {
        let default = engineer_subject(mode).unwrap();
        for suite in &suites {
            let mut files = default.clone();
            for fixture in suite_fixtures(suite).unwrap() {
                apply_fixture(&mut files, suite, &fixture)
                    .unwrap_or_else(|error| panic!("{}: {error:#}", suite.display()));
            }
            let manifest: gents_pack::pack::PackManifest =
                serde_json::from_slice(&files[MANIFEST]).unwrap();
            let authored = gents_pack::pack::load_pack_authoring_config(
                &manifest,
                &gents_pack::pack::PackInstallOptions {
                    node_did: "did:key:subject-owner".into(),
                },
                &|path| files.get(path).cloned().context("missing asset"),
                &|_| None,
            )
            .unwrap_or_else(|error| panic!("{}: {error:#}", suite.display()));
            assert!(authored.agent(SLOT).is_some(), "{}", suite.display());
        }
    }
}

/// A trial installs its subject as a pack, which stamps the pack's origin
/// on each document it installs; init installs none.
fn installed_as_init(mut document: Value) -> Value {
    let fields = document.as_object_mut().unwrap();
    if let Some(Value::Array(tags)) = fields.get_mut("tags") {
        tags.retain(|tag| {
            gents_pack::pack::pack_origin_from_tags(&[tag.as_str().unwrap().to_owned()])
                .unwrap()
                .is_none()
        });
        if tags.is_empty() {
            fields.remove("tags");
        }
    }
    document
}

fn same_fields(trial: &Value, init: &Value, what: &str) {
    let keys: std::collections::BTreeSet<&String> = trial
        .as_object()
        .unwrap()
        .keys()
        .chain(init.as_object().unwrap().keys())
        .collect();
    for key in keys {
        assert_eq!(trial.get(key), init.get(key), "{what}: {key}");
    }
}

/// A document without what its home binds: its own and its node's ids, the
/// host root and the store's document id.
fn bound<T>(document: &T) -> Value
where
    T: Serialize,
{
    let mut value = serde_json::to_value(document).unwrap();
    let fields = value.as_object_mut().unwrap();
    for id in ["_docID", "node_did", "tools_id", "context_id"] {
        fields.remove(id);
    }
    if let Some(host) = fields.get_mut("host").and_then(Value::as_object_mut) {
        host.remove("root");
    }
    value
}
