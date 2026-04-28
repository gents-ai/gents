use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::*;
use crate::commands::config::{apply, binding, diff};
use crate::commands::init::{
    write_identity_only_home_metadata, IdentityOnlyHomeOptions, IdentityOnlyHomeSummary,
};
use crate::desired_state;
use crate::shared::*;
use crate::{
    default_key_path, print_json, read_init_config, resolve_config_access, resolve_home_dir,
    DEFAULT_AGENT_NAME,
};

pub(crate) async fn provision(args: ProvisionArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let agent_name = resolve_provision_agent_name(&args);
    let identity = ensure_home_identity(&home_dir, &agent_name).await?;

    let (access, _) = resolve_config_access(Some(&home_dir), None, true).await?;
    let bound = binding::load_bound_manifest(binding::ManifestBindingOptions {
        root: &args.root,
        home: Some(&home_dir),
        graphql: None,
        bind_agent_did: Some(ManifestAgentDidBindingArg::Home),
        force_rebind_concrete_did: false,
        access: Some(&access),
    })
    .await?
    .require_valid()?;

    let apply_report =
        apply::apply_bound_desired_manifest(&args.root, &access, &bound, true).await?;
    let diff_report = diff::diff_bound_desired_manifest(&args.root, &access, &bound).await?;
    let ok = apply_report.ok && diff_report.ok;
    let report = ProvisionReport {
        status: if ok { "provisioned" } else { "failed" },
        ok,
        home: home_dir.display().to_string(),
        root: args.root.display().to_string(),
        agent_did: bound.context.target_agent_did.clone(),
        identity,
        apply: apply_report,
        diff: diff_report,
        next_steps: next_steps(&home_dir, &args.root),
    };
    print_json(&serde_json::to_value(&report)?)?;

    if report.ok {
        Ok(())
    } else {
        anyhow::bail!("provision did not converge")
    }
}

#[derive(Debug, Serialize)]
struct ProvisionReport {
    status: &'static str,
    ok: bool,
    home: String,
    root: String,
    agent_did: String,
    identity: ProvisionIdentityReport,
    apply: ConfigApplyReport,
    diff: desired_state::DesiredStateDiffReport,
    next_steps: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ProvisionIdentityReport {
    status: &'static str,
    agent_name: String,
    agent_did: String,
    key_path: Option<String>,
}

fn resolve_provision_agent_name(args: &ProvisionArgs) -> String {
    if let Some(agent_name) = args
        .agent_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return agent_name.to_string();
    }
    args.root
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_AGENT_NAME)
        .to_string()
}

async fn ensure_home_identity(
    home_dir: &Path,
    agent_name: &str,
) -> Result<ProvisionIdentityReport> {
    if let Some(init_config) = read_init_config(home_dir)? {
        let agent_did = init_config.agent_did.trim().to_string();
        if !agent_did.is_empty() && !agent_did.starts_with("did:defra-agent:") {
            return Ok(ProvisionIdentityReport {
                status: "existing",
                agent_name: init_config.agent_name,
                agent_did,
                key_path: init_config.key_path,
            });
        }
    }

    let key_path = default_key_path(home_dir, agent_name);
    let initialized = write_identity_only_home_metadata(IdentityOnlyHomeOptions {
        home: home_dir,
        agent_name,
        key_path: &key_path,
        write_tools: false,
        tool_root: None,
        reset: false,
    })
    .await
    .with_context(|| format!("initializing identity metadata in {}", home_dir.display()))?;
    Ok(report_from_initialized_identity(initialized))
}

fn report_from_initialized_identity(summary: IdentityOnlyHomeSummary) -> ProvisionIdentityReport {
    ProvisionIdentityReport {
        status: "initialized",
        agent_name: summary.agent_name,
        agent_did: summary.agent_did,
        key_path: summary.key_path,
    }
}

fn next_steps(home_dir: &Path, root: &Path) -> Vec<String> {
    vec![
        format!("defra-agent server --home {}", home_dir.display()),
        format!(
            "defra-agent config diff --root {} --home {} --bind-agent-did home",
            root.display(),
            home_dir.display()
        ),
    ]
}
