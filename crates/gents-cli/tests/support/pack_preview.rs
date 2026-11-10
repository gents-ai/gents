//! Fixtures for asserting that a pack install preview names install
//! variables without revealing their values.

use anyhow::{Context, Result};

use super::{copy_dir_all, fixture_pack_dir};

/// `documents_fixture` copied to `dir` with its Tools display name set by the
/// install variable `PACK_PREVIEW_SECRET`.
pub fn documents_fixture_with_secret_variable(dir: &std::path::Path) -> Result<std::path::PathBuf> {
    let pack = dir.join("documents_fixture");
    copy_dir_all(&fixture_pack_dir("documents_fixture"), &pack)?;
    let config_path = pack.join("pack_config.json");
    let mut config: serde_json::Value = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    config["tools"][0]["display_name"] = serde_json::json!("${PACK_PREVIEW_SECRET}");
    std::fs::write(&config_path, serde_json::to_vec(&config)?)?;
    Ok(pack)
}

/// A preview names each install variable, where its value comes from and
/// whether one is present, and shows neither the value nor a field it sets.
pub fn assert_preview_hides_the_secret(preview: &serde_json::Value, secret: &str) -> Result<()> {
    anyhow::ensure!(
        !preview.to_string().contains(secret),
        "the preview reveals the variable's value: {preview}"
    );
    let uses = preview["install_variables"][0]["uses"]
        .as_array()
        .context("the preview names no install variables")?;
    let tools = uses
        .iter()
        .find(|item| item["path"] == "/tools/0/display_name")
        .with_context(|| format!("no use of the secret variable: {preview}"))?;
    anyhow::ensure!(
        tools["variables"]
            == serde_json::json!([{"name": "PACK_PREVIEW_SECRET", "source": "environment", "present": true}])
            && tools.get("resolved").is_none(),
        "the variable is not named with its source and presence: {tools}"
    );
    let shown = preview["documents"]
        .as_array()
        .context("the preview lists no documents")?
        .iter()
        .find(|document| document["collection"] == "Tools")
        .with_context(|| format!("no Tools document: {preview}"))?;
    anyhow::ensure!(
        shown["content"]["display_name"] == "[redacted]" && shown["name"] == "[redacted]",
        "the field the variable sets is shown: {shown}"
    );
    Ok(())
}
