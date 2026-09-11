use crate::support::*;

use std::fs;

use anyhow::Result;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_export_writes_one_canonical_bundle() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("export");
    fs::create_dir_all(&home)?;
    let model = format!("export-model-{}", Uuid::new_v4().simple());
    let endpoint = MockModelEndpoint::start(&model)?;
    let init = run_init_json(
        &home,
        &[
            "--agent-name",
            "export-agent",
            "--model-name",
            &model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let owner = agent_did_from_init(&init)?;

    run_cli_text(
        &home,
        &["config", "export", "--root", root.to_str().unwrap()],
    )?;
    let config = read_json_file(&root.join("pack_config.json"))?;
    assert_eq!(config["agent_principal"]["agent_did"], owner);
    assert_eq!(config["contexts"].as_array().map(Vec::len), Some(1));
    assert_eq!(config["tools"].as_array().map(Vec::len), Some(2));
    assert!(config.get("format").is_none());
    assert!(config.get("exported_at").is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_export_apply_round_trips_canonical_context_documents() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("export");
    fs::create_dir_all(&home)?;
    run_init_json(&home, &["--agent-name", "roundtrip-agent"])?;
    run_cli_text(
        &home,
        &["config", "export", "--root", root.to_str().unwrap()],
    )?;

    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    config["contexts"][0]["system_prompt"] = serde_json::json!("Round-trip canonical context.");
    write_json_file(&path, &config)?;

    let applied = run_cli_json(
        &home,
        &["config", "apply", "--root", root.to_str().unwrap()],
    )?;
    assert_eq!(applied["applied"]["contexts"], 1);

    let second = tempdir.path().join("reexport");
    run_cli_text(
        &home,
        &["config", "export", "--root", second.to_str().unwrap()],
    )?;
    let exported = read_json_file(&second.join("pack_config.json"))?;
    let prompt_path = exported["contexts"][0]["system_prompt"]
        .as_str()
        .expect("exported context prompt path");
    assert_eq!(
        fs::read_to_string(second.join(prompt_path.trim_start_matches("./")))?,
        "Round-trip canonical context."
    );
    Ok(())
}
