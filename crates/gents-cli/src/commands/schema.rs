use anyhow::Result;
use gents_pack::pack::schema_apply::{
    apply_schema_inputs, discover_schema_inputs, SchemaApplyFileResult, SchemaApplyPatchResult,
};
use serde::Serialize;

use crate::cli::args::{SchemaApplyArgs, SchemaCommand};
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access};

#[derive(Debug, Serialize)]
struct SchemaApplySummary {
    status: &'static str,
    home: String,
    mode: &'static str,
    graphql: Option<String>,
    root: String,
    schema_files: Vec<SchemaApplyFileResult>,
    patch_files: Vec<SchemaApplyPatchResult>,
}

pub(crate) async fn dispatch(command: SchemaCommand) -> Result<()> {
    match command {
        SchemaCommand::Apply(args) => schema_apply(args).await,
    }
}

pub(crate) async fn schema_apply(args: SchemaApplyArgs) -> Result<()> {
    let root = args.path;
    let explicit_patches = args.patches;
    let inputs = discover_schema_inputs(&root, &explicit_patches)?;
    if inputs.is_empty() {
        anyhow::bail!(
            "no schema inputs found under {}; expected *.graphql, *.gql, *.patch.json, or *.json-patch",
            root.display()
        );
    }

    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let graphql = match &access {
        ConfigAccess::Graphql(endpoint) => Some(endpoint.url().to_owned()),
        ConfigAccess::Local(_) => None,
    };

    let phase = apply_schema_inputs(&access, &root, &inputs).await?;

    let summary = SchemaApplySummary {
        status: "schema_applied",
        home: home_dir.display().to_string(),
        mode: access.mode(),
        graphql,
        root: root.display().to_string(),
        schema_files: phase.schema_files,
        patch_files: phase.patch_files,
    };
    print_json(&serde_json::to_value(summary)?)?;
    Ok(())
}
