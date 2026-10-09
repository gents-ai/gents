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
    DEFAULT_NODE_NAME,
};

pub(crate) async fn provision(args: ProvisionArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let node_name = resolve_provision_node_name(&args);
    let identity = ensure_home_identity(
        &home_dir,
        &node_name,
        args.bootstrap_file_identity,
        args.bootstrap_macos_keychain,
        args.bootstrap_macos_secure_enclave,
        args.keychain_label.as_deref(),
        args.secure_enclave_label.as_deref(),
        crate::cli::args::store_key_custody(args.store_key_custody),
    )
    .await?;

    let (access, _) = resolve_config_access(Some(&home_dir), None).await?;
    let bound = binding::load_bound_manifest(binding::ManifestBindingOptions {
        root: &args.root,
        home: Some(&home_dir),
        graphql: None,
        bind_node_did: Some(ManifestNodeDidBindingArg::Home),
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
        node_did: bound.context.target_node_did.clone(),
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
    node_did: String,
    identity: ProvisionIdentityReport,
    apply: ConfigApplyReport,
    diff: desired_state::DesiredStateDiffReport,
    next_steps: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ProvisionIdentityReport {
    status: &'static str,
    node_name: String,
    node_did: String,
    key_path: Option<String>,
    identity_backend: Option<String>,
    keychain_label: Option<String>,
    secure_enclave_label: Option<String>,
}

fn resolve_provision_node_name(args: &ProvisionArgs) -> String {
    if let Some(node_name) = args
        .node_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return node_name.to_string();
    }
    args.root
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_NODE_NAME)
        .to_string()
}

async fn ensure_home_identity(
    home_dir: &Path,
    node_name: &str,
    bootstrap_file_identity: bool,
    bootstrap_macos_keychain: bool,
    bootstrap_macos_secure_enclave: bool,
    keychain_label: Option<&str>,
    secure_enclave_label: Option<&str>,
    store_key_custody: gents::store_key::StoreKeyCustodyChoice,
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

    if let Some(report) = read_init_config(home_dir)?.and_then(report_from_stored_identity) {
        return Ok(report);
    }

    if !bootstrap_file_identity && !bootstrap_macos_keychain && !bootstrap_macos_secure_enclave {
        anyhow::bail!(
            "initialized home identity is required before provisioning {}; run `gents init --identity-only --home {}` for file-key development, or bootstrap the host identity backend first",
            home_dir.display(),
            home_dir.display()
        );
    }

    let data_dir = crate::default_data_dir(home_dir);
    let _store_lock = crate::commands::init::lock_init_store(home_dir, &data_dir, false)?;
    if let Some(report) = read_init_config(home_dir)?.and_then(report_from_stored_identity) {
        return Ok(report);
    }

    let key_path = if bootstrap_macos_keychain || bootstrap_macos_secure_enclave {
        None
    } else {
        Some(default_key_path(home_dir, node_name))
    };
    let initialized = write_identity_only_home_metadata(IdentityOnlyHomeOptions {
        home: home_dir,
        node_name,
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
        store_key_custody,
    })
    .await
    .with_context(|| format!("initializing identity metadata in {}", home_dir.display()))?;
    Ok(report_from_initialized_identity(initialized))
}

fn report_from_stored_identity(config: StoredInitConfig) -> Option<ProvisionIdentityReport> {
    let node_did = config.node_did.trim().to_string();
    (!node_did.is_empty()).then_some(ProvisionIdentityReport {
        status: "existing",
        node_name: config.node_name,
        node_did,
        key_path: config.key_path,
        identity_backend: config.identity_backend,
        keychain_label: config.keychain_label,
        secure_enclave_label: config.secure_enclave_label,
    })
}

fn report_from_initialized_identity(summary: IdentityOnlyHomeSummary) -> ProvisionIdentityReport {
    ProvisionIdentityReport {
        status: "initialized",
        node_name: summary.node_name,
        node_did: summary.node_did,
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
            "gents config diff --root {} --home {} --bind-node-did home",
            root.display(),
            home_dir.display()
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn existing_provision_identity_remains_readable_while_runtime_holds_the_store() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let original = ensure_home_identity(
            &home,
            "owner",
            true,
            false,
            false,
            None,
            None,
            gents::store_key::StoreKeyCustodyChoice::File,
        )
        .await
        .unwrap();
        let before = std::fs::read(gents::home::init_config_path(&home)).unwrap();
        let runtime_lock = gents::home::lock_home_store(&home).unwrap();
        let observed = ensure_home_identity(
            &home,
            "different-requested-name",
            false,
            false,
            false,
            None,
            None,
            gents::store_key::StoreKeyCustodyChoice::File,
        )
        .await
        .unwrap();
        assert_eq!(observed.status, "existing");
        assert_eq!(observed.node_did, original.node_did);
        assert_eq!(observed.node_name, "owner");
        assert_eq!(
            std::fs::read(gents::home::init_config_path(&home)).unwrap(),
            before
        );
        drop(runtime_lock);

        let new_home = temp.path().join("new-home");
        let data = crate::default_data_dir(&new_home);
        std::fs::create_dir_all(&data).unwrap();
        let _held = gents::home::lock_store(&new_home, &data).unwrap();
        let error = match ensure_home_identity(
            &new_home,
            "owner",
            true,
            false,
            false,
            None,
            None,
            gents::store_key::StoreKeyCustodyChoice::File,
        )
        .await
        {
            Ok(_) => panic!("bootstrap must hold the exclusive store lock"),
            Err(error) => error,
        };
        assert!(error.downcast_ref::<gents::home::StoreLockHeld>().is_some());
        assert!(!gents::home::init_config_path(&new_home).exists());
    }
}
