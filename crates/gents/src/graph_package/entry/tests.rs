use super::*;
use sha2::{Digest, Sha256};

#[test]
fn review_cannot_escape_ceiling_through_enclosing_repository() {
    let directory = tempfile::tempdir().unwrap();
    git_output(directory.path(), &["init", "--quiet"]).unwrap();
    let permitted = directory.path().join("permitted");
    std::fs::create_dir(&permitted).unwrap();
    let permitted = std::fs::canonicalize(permitted).unwrap();
    let error = resolve_repository(&permitted, "HEAD", "HEAD", Some(&permitted)).unwrap_err();
    assert!(
        error.to_string().contains("escapes operator tool root"),
        "{error:#}"
    );
}

fn init_repo() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    git_output(directory.path(), &["init", "--quiet"]).unwrap();
    git_output(
        directory.path(),
        &["config", "user.email", "test@example.com"],
    )
    .unwrap();
    git_output(directory.path(), &["config", "user.name", "Test"]).unwrap();
    directory
}

fn commit(repo: &Path, message: &str) -> String {
    git_output(repo, &["add", "-A"]).unwrap();
    git_output(repo, &["commit", "--quiet", "-m", message]).unwrap();
    git_output(repo, &["rev-parse", "HEAD"]).unwrap()
}

#[test]
fn git_diff_host_step_runs_the_declared_diff() {
    let directory = init_repo();
    let repo = directory.path();
    std::fs::write(repo.join("a.txt"), "line one\n").unwrap();
    let base = commit(repo, "base");
    std::fs::write(repo.join("a.txt"), "line one changed\n").unwrap();
    let head = commit(repo, "head");

    let facts = collect_git_diff(repo, &base, &head, 3, 40, None).unwrap();
    let expected_name_status =
        git_output(repo, &["diff", "--name-status", &base, &head, "--"]).unwrap();
    let expected_stat = git_output(repo, &["diff", "--stat", &base, &head, "--"]).unwrap();
    let expected_patch = git_output_exact(
        repo,
        &[
            "-c",
            "core.quotepath=true",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--find-renames=40%",
            "--unified=3",
            &base,
            &head,
            "--",
        ],
    )
    .unwrap();
    assert_eq!(facts.base_sha, base);
    assert_eq!(facts.head_sha, head);
    assert_eq!(facts.name_status, expected_name_status);
    assert_eq!(facts.stat, expected_stat);
    assert_eq!(facts.patch, expected_patch);
}

use crate::plugin::tests::constant_output_wat;

/// Installs a constant-output plugin under a fresh home, qualified
/// `fixture/prepare_fixture`.
fn install_prepare_fixture_plugin(output: &Value) -> (tempfile::TempDir, String) {
    let wat_source = constant_output_wat(&serde_json::to_vec(output).unwrap());
    let (plugin, afb) =
        crate::plugin::tests::build_plugin_pack("prepare_fixture_pack", &wat_source, None);
    let home = tempfile::tempdir().unwrap();
    let hex = format!("{:x}", Sha256::digest(&afb));
    crate::plugin::store::store_bytes(home.path(), &hex, &afb).unwrap();
    let digest = format!("sha256:{hex}");
    let record = crate::plugin::store::InstalledPlugin {
        namespace: "fixture".into(),
        name: plugin.name.clone(),
        version: "0.1.0".into(),
        digest: digest.clone(),
        language: "rust".into(),
        declaration: plugin,
        granted: None,
        instructions: None,
        owner_pack_coordinate: None,
        owner_pack_digest: None,
        model_binding: None,
    };
    crate::plugin::store::write_record(home.path(), &record).unwrap();
    (home, digest)
}

fn fixture_entry(prepare: Option<crate::graph_pipeline::EntryPrepare>) -> PlannedEntry {
    PlannedEntry {
        name: "job".to_owned(),
        collection: "FixtureJob".to_owned(),
        schema: "FixtureJob/v1".to_owned(),
        input_contract: None,
        to: crate::graph_pipeline::PortRef {
            node_id: "worker".to_owned(),
            port: "job".to_owned(),
        },
        target: crate::graph_pipeline::StageTarget::Task {
            task_id: "worker-task".to_owned(),
        },
        correlation_field: "correlation".to_owned(),
        input_schema: None,
        prepare,
    }
}

fn fixture_plan(entry: PlannedEntry) -> GraphPlan {
    GraphPlan {
        compiler_version: crate::graph_pipeline::COMPILER_VERSION.to_owned(),
        graph_id: "fixture-graph".to_owned(),
        digest: format!("sha256:{}", "0".repeat(64)),
        nodes: Vec::new(),
        edges: Vec::new(),
        entries: vec![entry],
        results: Vec::new(),
        capability_manifest: Vec::new(),
        limits: crate::graph_pipeline::GraphLimits {
            max_nodes: 1,
            max_edges: 1,
            max_depth: 1,
            max_fan_out: 1,
            max_total_invocations: 1,
            max_runtime_secs: 60,
        },
        package: None,
    }
}

#[tokio::test]
async fn prepare_entry_run_returns_admitted_input_when_the_entry_has_no_prepare() {
    let plan = fixture_plan(fixture_entry(None));
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    let access = ConfigAccess::Local(std::sync::Arc::new(node));
    let plugins = PluginExecutor::default();
    let prepared = prepare_entry_run(
        &access,
        "did:key:tester",
        EntryRunRequest {
            plan: &plan,
            entry: None,
            input: json!({"note": "hello"}),
            host_root: None,
            plugins: &plugins,
        },
    )
    .await
    .unwrap();
    assert_eq!(prepared.origin, EntryInputOrigin::Operator);
    assert_eq!(prepared.documents, 0);
    assert_eq!(prepared.input, json!({"note": "hello"}));
}

#[tokio::test]
async fn prepare_entry_run_runs_host_steps_and_persists_the_plugins_documents() {
    let directory = init_repo();
    let repo = directory.path();
    std::fs::write(repo.join("a.txt"), "line one\n").unwrap();
    let base = commit(repo, "base");
    std::fs::write(repo.join("a.txt"), "line one changed\n").unwrap();
    let head = commit(repo, "head");

    let plugin_output = json!({
        "input": {"summary": "prepared"},
        "documents": [{"collection": "FixtureEvidence", "fields": {"note": "from the plugin"}}],
    });
    let (home, digest) = install_prepare_fixture_plugin(&plugin_output);
    let plugins = PluginExecutor::new(Some(home.path().to_owned()));

    let entry = fixture_entry(Some(crate::graph_pipeline::EntryPrepare {
        host: vec![HostInput::GitDiff {
            repository_field: "repository".to_owned(),
            base_field: "base".to_owned(),
            head_field: "head".to_owned(),
            unified_context_lines: 3,
            rename_similarity_percent: 50,
        }],
        plugin: "fixture/plugin".to_owned(),
        digest: Some(digest),
        writes: vec!["FixtureEvidence".to_owned()],
    }));
    let plan = fixture_plan(entry);

    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    node.add_schema("type FixtureEvidence { note: String }")
        .await
        .unwrap();
    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    let prepared = prepare_entry_run(
        &access,
        "did:key:tester",
        EntryRunRequest {
            plan: &plan,
            entry: None,
            input: json!({
                "repository": repo.to_string_lossy(),
                "base": base,
                "head": head,
            }),
            host_root: None,
            plugins: &plugins,
        },
    )
    .await
    .unwrap();

    assert_eq!(prepared.origin, EntryInputOrigin::Prepared);
    assert_eq!(prepared.documents, 1);
    assert_eq!(prepared.input, json!({"summary": "prepared"}));

    let response = access
        .execute("{ FixtureEvidence { note } }")
        .await
        .unwrap();
    let notes: Vec<&str> = response["data"]["FixtureEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["note"].as_str().unwrap())
        .collect();
    assert_eq!(notes, vec!["from the plugin"]);
}

#[tokio::test]
async fn prepare_entry_run_refuses_a_document_outside_declared_writes() {
    let directory = init_repo();
    let repo = directory.path();
    std::fs::write(repo.join("a.txt"), "x\n").unwrap();
    let base = commit(repo, "base");
    std::fs::write(repo.join("a.txt"), "y\n").unwrap();
    let head = commit(repo, "head");

    let plugin_output = json!({
        "input": {},
        "documents": [{"collection": "NotDeclared", "fields": {}}],
    });
    let (home, digest) = install_prepare_fixture_plugin(&plugin_output);
    let plugins = PluginExecutor::new(Some(home.path().to_owned()));
    let entry = fixture_entry(Some(crate::graph_pipeline::EntryPrepare {
        host: vec![HostInput::GitDiff {
            repository_field: "repository".to_owned(),
            base_field: "base".to_owned(),
            head_field: "head".to_owned(),
            unified_context_lines: 3,
            rename_similarity_percent: 50,
        }],
        plugin: "fixture/plugin".to_owned(),
        digest: Some(digest),
        writes: vec!["FixtureEvidence".to_owned()],
    }));
    let plan = fixture_plan(entry);
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    let access = ConfigAccess::Local(std::sync::Arc::new(node));
    let error = prepare_entry_run(
        &access,
        "did:key:tester",
        EntryRunRequest {
            plan: &plan,
            entry: None,
            input: json!({"repository": repo.to_string_lossy(), "base": base, "head": head}),
            host_root: None,
            plugins: &plugins,
        },
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("does not declare in prepare.writes"),
        "{error:#}"
    );
}

/// The batched aliased-mutation path (D4 step 7): more than
/// [`PREPARE_DOCUMENTS_BATCH_LIMIT`] documents forces the count-based split
/// into multiple batches, and one oversized document forces a byte-based
/// split mid-batch (and must still land, alone, in its own batch rather than
/// being silently dropped). Every document must survive with its exact
/// fields, and none may be dropped, duplicated, or truncated.
#[tokio::test]
async fn persist_prepared_documents_batches_and_never_drops_a_document() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    node.add_schema("type FixtureEvidence { note: String }")
        .await
        .unwrap();
    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    let mut documents: Vec<PreparePluginDocument> = (0..70)
        .map(|i| PreparePluginDocument {
            collection: "FixtureEvidence".to_owned(),
            fields: json!({"note": format!("doc-{i:03}")}),
        })
        .collect();
    let oversized_note = "z".repeat(PREPARE_DOCUMENTS_BATCH_BYTES + 1);
    // Inserted mid-list (not first in its would-be batch), so this document
    // alone tripping the byte budget forces an early batch break rather than
    // only ever being exercised as a batch's first, always-admitted alias.
    documents.insert(
        40,
        PreparePluginDocument {
            collection: "FixtureEvidence".to_owned(),
            fields: json!({"note": oversized_note.clone()}),
        },
    );
    assert!(
        documents.len() > 2 * PREPARE_DOCUMENTS_BATCH_LIMIT,
        "the count-based split must be exercised more than once"
    );

    persist_prepared_documents(&access, &documents)
        .await
        .unwrap();

    let response = access
        .execute("{ FixtureEvidence { note } }")
        .await
        .unwrap();
    let mut notes: Vec<String> = response["data"]["FixtureEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["note"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        notes.len(),
        documents.len(),
        "every document must persist exactly once"
    );
    assert!(
        notes.contains(&oversized_note),
        "the oversized document must not be dropped or truncated"
    );
    notes.retain(|note| note != &oversized_note);
    let mut expected: Vec<String> = (0..70).map(|i| format!("doc-{i:03}")).collect();
    notes.sort();
    expected.sort();
    assert_eq!(notes, expected, "every small document must survive intact");
}
