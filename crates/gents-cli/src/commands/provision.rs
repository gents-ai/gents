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
    let identity = ensure_home_identity(
        &home_dir,
        &agent_name,
        args.bootstrap_file_identity,
        args.bootstrap_macos_keychain,
        args.bootstrap_macos_secure_enclave,
        args.keychain_label.as_deref(),
        args.secure_enclave_label.as_deref(),
    )
    .await?;

    let (access, _) = resolve_config_access(Some(&home_dir), None, true).await?;
    let bound = binding::load_bound_manifest(binding::ManifestBindingOptions {
        root: &args.root,
        home: Some(&home_dir),
        graphql: None,
        bind_agent_did: Some(ManifestAgentDidBindingArg::Home),
        // Provisioning deliberately binds a portable manifest to the identity
        // initialized for this home. Interactive config binding keeps the
        // explicit force requirement for replacing a different concrete DID.
        force_rebind_concrete_did: true,
        access: Some(&access),
    })
    .await?
    .require_valid()?;

    let apply_report =
        apply::apply_bound_desired_manifest(&args.root, &access, &bound, false).await?;
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
    identity_backend: Option<String>,
    keychain_label: Option<String>,
    secure_enclave_label: Option<String>,
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
    bootstrap_file_identity: bool,
    bootstrap_macos_keychain: bool,
    bootstrap_macos_secure_enclave: bool,
    keychain_label: Option<&str>,
    secure_enclave_label: Option<&str>,
) -> Result<ProvisionIdentityReport> {
    let bootstrap_count = [
        bootstrap_file_identity,
        bootstrap_macos_keychain,
        bootstrap_macos_secure_enclave,
    ]
    .into_iter()
    .filter(|value| *value)
    .count();
    if bootstrap_count > 1 {
        anyhow::bail!("bootstrap identity flags are mutually exclusive");
    }

    if let Some(init_config) = read_init_config(home_dir)? {
        let agent_did = init_config.agent_did.trim().to_string();
        if !agent_did.is_empty() {
            return Ok(ProvisionIdentityReport {
                status: "existing",
                agent_name: init_config.agent_name,
                agent_did,
                key_path: init_config.key_path,
                identity_backend: init_config.identity_backend,
                keychain_label: init_config.keychain_label,
                secure_enclave_label: init_config.secure_enclave_label,
            });
        }
    }

    if !bootstrap_file_identity && !bootstrap_macos_keychain && !bootstrap_macos_secure_enclave {
        anyhow::bail!(
            "initialized home identity is required before provisioning {}; run `gents init --identity-only --home {}` for file-key development, or bootstrap the host identity backend first",
            home_dir.display(),
            home_dir.display()
        );
    }

    let key_path = if bootstrap_macos_keychain || bootstrap_macos_secure_enclave {
        None
    } else {
        Some(default_key_path(home_dir, agent_name))
    };
    let initialized = write_identity_only_home_metadata(IdentityOnlyHomeOptions {
        home: home_dir,
        agent_name,
        key_path: key_path.as_deref(),
        identity_backend: if bootstrap_macos_secure_enclave {
            IdentityBackendArg::MacosSecureEnclave
        } else if bootstrap_macos_keychain {
            IdentityBackendArg::MacosKeychain
        } else {
            IdentityBackendArg::File
        },
        keychain_label,
        secure_enclave_label,
        tool_package: ToolPackageArg::Readonly,
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
        identity_backend: summary.identity_backend,
        keychain_label: summary.keychain_label,
        secure_enclave_label: summary.secure_enclave_label,
    }
}

fn next_steps(home_dir: &Path, root: &Path) -> Vec<String> {
    vec![
        format!("gents server --home {}", home_dir.display()),
        format!(
            "gents config diff --root {} --home {} --bind-agent-did home",
            root.display(),
            home_dir.display()
        ),
    ]
}
