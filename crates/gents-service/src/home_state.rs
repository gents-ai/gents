use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use gents_store::config_client::GraphqlEndpoint;
use gents_store::identity::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity, KeyIdentity, NodeIdentity,
};
use gents_store::local::home::INIT_PENDING_FILE_NAME;

use crate::state::{
    StoredInitConfig, StoredRuntimeState, DEFAULT_HTTP_PORT, RUNTIME_STATE_FILE_NAME,
};

pub fn resolve_home_dir(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(default_home_dir)
}

/// Exits on any failure but an unresolved user home: `resolve_home_dir`'s many
/// callers hold no `Result`, and falling back would open the wrong data.
fn default_home_dir() -> PathBuf {
    use gents_protocol::error_chain::ErrorChain;
    use gents_store::local::home::HomeError;
    #[cfg(any(test, feature = "test-home-guard"))]
    if let Some(homes) = unit_test_homes() {
        return homes.path().join("runtime");
    }
    match gents_store::local::home::default_home_dir() {
        Ok(home) => home,
        Err(HomeError::HomeDirUnresolved) => PathBuf::from(".gents"),
        Err(error) => {
            eprintln!("Error: {}", ErrorChain(&error));
            std::process::exit(1);
        }
    }
}

#[cfg(any(test, feature = "test-home-guard"))]
static UNIT_TEST_PROCESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(cfg!(test));

/// Unit tests share the operator's process environment, so they cannot select
/// a runtime home through it; a command they run without `--home` resolves
/// a throwaway home. A dependent crate's unit-test binary selects this before
/// its tests run; its spawned binaries and integration tests do not.
#[cfg(any(test, feature = "test-home-guard"))]
pub fn use_unit_test_homes() {
    UNIT_TEST_PROCESS.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(any(test, feature = "test-home-guard"))]
fn unit_test_homes() -> Option<&'static tempfile::TempDir> {
    static ROOT: std::sync::LazyLock<tempfile::TempDir> = std::sync::LazyLock::new(|| {
        tempfile::tempdir().expect("creating the unit tests' throwaway homes")
    });
    UNIT_TEST_PROCESS
        .load(std::sync::atomic::Ordering::Relaxed)
        .then(|| &*ROOT)
}

/// Never exits: a service started with `--home` must not depend on the
/// remembered file.
pub fn is_default_home(home_dir: &Path) -> bool {
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(any(test, feature = "test-home-guard"))]
    if unit_test_homes().is_some() {
        return canonical(home_dir) == canonical(&default_home_dir());
    }
    let default = gents_store::local::home::default_home_dir();
    default.is_ok_and(|default| canonical(home_dir) == canonical(&default))
}

pub fn default_data_dir(home_dir: &Path) -> PathBuf {
    gents_store::local::home::default_data_dir(home_dir)
}

pub fn default_key_path(home_dir: &Path, node_name: &str) -> PathBuf {
    gents_store::local::home::default_key_path(home_dir, node_name)
}

pub fn init_config_path(home_dir: &Path) -> PathBuf {
    gents_store::local::home::init_config_path(home_dir)
}

pub fn runtime_state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RUNTIME_STATE_FILE_NAME)
}

pub fn write_init_config(home_dir: &Path, state: &StoredInitConfig) -> Result<()> {
    Ok(gents_store::local::home::write_init_config(
        home_dir, state,
    )?)
}

pub fn read_init_config(home_dir: &Path) -> Result<Option<StoredInitConfig>> {
    Ok(gents_store::local::home::read_init_config(home_dir)?)
}

pub fn mark_init_pending(home_dir: &Path) -> Result<()> {
    let path = home_dir.join(INIT_PENDING_FILE_NAME);
    fs::write(&path, b"").with_context(|| format!("writing {}", path.display()))
}

pub fn clear_init_pending(home_dir: &Path) -> Result<()> {
    let path = home_dir.join(INIT_PENDING_FILE_NAME);
    match fs::remove_file(&path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// `None` also for a home `init` has not finished, which `init` can resume.
pub fn read_finished_init_config(home_dir: &Path) -> Result<Option<StoredInitConfig>> {
    if home_dir.join(INIT_PENDING_FILE_NAME).exists() {
        return Ok(None);
    }
    read_init_config(home_dir)
}

/// Load and register the signer recorded by an initialized home; every
/// embedded-node entry point uses this same loader so opening the data
/// directory outside `gents runtime server` never silently produces unsigned commits.
pub fn load_initialized_home_identity(
    home_dir: &Path,
    config: &StoredInitConfig,
) -> Result<Arc<dyn NodeIdentity>> {
    let expected_did = config.node_did.trim();
    if expected_did.is_empty() {
        anyhow::bail!("initialized home {} has no node DID", home_dir.display());
    }

    let identity: Arc<dyn NodeIdentity> = if let Some(key_path) = config
        .key_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let key_path = PathBuf::from(key_path);
        if !key_path.exists() {
            anyhow::bail!(
                "initialized home node DID {expected_did} requires identity key {} to already exist",
                key_path.display()
            );
        }
        Arc::new(
            KeyIdentity::load_existing(&key_path, None)
                .with_context(|| format!("loading identity key {}", key_path.display()))?,
        )
    } else {
        match config
            .identity_backend
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some("macos-keychain") => {
                let label = config
                    .keychain_label
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "initialized home {} uses macos-keychain but has no keychain_label",
                            home_dir.display()
                        )
                    })?;
                Arc::new(
                    load_macos_keychain_identity(label, None)
                        .with_context(|| format!("loading macOS keychain identity {label}"))?,
                )
            }
            Some("macos-secure-enclave") => {
                let label = config
                    .secure_enclave_label
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "initialized home {} uses macos-secure-enclave but has no secure_enclave_label",
                            home_dir.display()
                        )
                    })?;
                Arc::new(
                    load_macos_secure_enclave_identity(label, None).with_context(|| {
                        format!("loading macOS Secure Enclave identity {label}")
                    })?,
                )
            }
            backend => anyhow::bail!(
                "initialized home {} has no key_path and unsupported identity_backend {backend:?}",
                home_dir.display()
            ),
        }
    };

    if identity.did() != expected_did {
        anyhow::bail!(
            "initialized home node DID {expected_did} does not match loaded identity DID {}",
            identity.did()
        );
    }
    Ok(identity)
}

pub fn write_runtime_state(home_dir: &Path, state: &StoredRuntimeState) -> Result<()> {
    fs::create_dir_all(home_dir)
        .with_context(|| format!("creating home directory {}", home_dir.display()))?;
    let path = runtime_state_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).context("encoding local runtime state JSON")?;
    // Replaced, never truncated in place: a server killed mid-write would
    // otherwise leave an empty file every later command fails to decode.
    crate::native_service::atomic_write(&path, &contents)
        .with_context(|| format!("writing runtime state {}", path.display()))
}

pub fn read_runtime_state(home_dir: &Path) -> Result<Option<StoredRuntimeState>> {
    let path = runtime_state_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).with_context(|| format!("reading runtime state {}", path.display()))?;
    let state = serde_json::from_slice(&bytes)
        .with_context(|| format!("decoding runtime state {}", path.display()))?;
    Ok(Some(state))
}

pub fn clear_runtime_state(home_dir: &Path) -> Result<bool> {
    let path = runtime_state_path(home_dir);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("removing stale runtime state {}", path.display()))?;
        return Ok(true);
    }
    Ok(false)
}

pub fn resolve_graphql_endpoint(
    explicit: Option<&str>,
    home: Option<&Path>,
) -> Result<GraphqlEndpoint> {
    let home_dir = resolve_home_dir(home);
    if let Some(graphql) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        let url = reqwest::Url::parse(graphql).context("invalid GraphQL endpoint")?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "GraphQL endpoint must be an HTTP URL without credentials, query or fragment"
        );
        let identity =
            gents_desktop_core::client::PrincipalIdentity::load_existing(&cli_identity_paths()?)
                .context("CLI client identity is unavailable; initialize the CLI client first")?;
        let capability = gents_store::config_client::ClientSigningCapability::from_ed25519_key(
            identity.private_key_bytes(),
        )?;
        return Ok(GraphqlEndpoint::as_principal(graphql, identity.did())
            .with_client_signing_capability(capability)?
            .with_client_signed_agent_creates());
    }

    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return home_graphql_endpoint(&home_dir, runtime_state.graphql);
    }

    anyhow::ensure!(
        home.is_none(),
        "home {} has no running server recorded; start `gents runtime server --home {}` or pass --graphql explicitly",
        home_dir.display(),
        home_dir.display()
    );

    home_graphql_endpoint(
        &home_dir,
        format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"),
    )
}

/// Client document access retains the user's identity. Credentials remain bound to
/// this home's endpoint or loopback; loading a runtime key never grants client access.
pub fn home_graphql_endpoint(
    home_dir: &Path,
    url: impl Into<String>,
) -> Result<GraphqlEndpoint> {
    home_graphql_endpoint_with_paths(home_dir, url, &cli_identity_paths()?)
}

fn home_graphql_endpoint_with_paths(
    home_dir: &Path,
    url: impl Into<String>,
    paths: &gents_desktop_core::client::DesktopPaths,
) -> Result<GraphqlEndpoint> {
    let url = url.into();
    anyhow::ensure!(
        endpoint_serves_home(home_dir, &url),
        "endpoint is not this home's runtime"
    );
    let identity = gents_desktop_core::client::PrincipalIdentity::load_existing(paths)
        .context("CLI client identity is unavailable; initialize the CLI client first")?;
    let capability = gents_store::config_client::ClientSigningCapability::from_ed25519_key(
        identity.private_key_bytes(),
    )?;
    Ok(GraphqlEndpoint::as_principal(url, identity.did())
        .with_client_signing_capability(capability)?
        .with_client_signed_agent_creates())
}

/// The CLI is a separate client principal, even when desktop and CLI share a user home.
pub fn cli_identity_paths() -> Result<gents_desktop_core::client::DesktopPaths> {
    Ok(cli_identity_paths_under(&desktop_paths()?))
}

/// Unit tests share one throwaway CLI client home; see [`use_unit_test_homes`].
fn desktop_paths() -> Result<gents_desktop_core::client::DesktopPaths> {
    #[cfg(any(test, feature = "test-home-guard"))]
    if let Some(homes) = unit_test_homes() {
        return Ok(gents_desktop_core::client::DesktopPaths::from_root(
            homes.path().join("desktop"),
        ));
    }
    gents_desktop_core::client::DesktopPaths::discover()
}

fn cli_identity_paths_under(
    desktop: &gents_desktop_core::client::DesktopPaths,
) -> gents_desktop_core::client::DesktopPaths {
    desktop.cli_client()
}

/// The CLI's held User key signs every client-authored command; a registry
/// lookup by DID cannot present the key to owners that need its capability.
pub fn held_client_identity(
    actor_did: &str,
) -> Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>> {
    let identity =
        gents_desktop_core::client::PrincipalIdentity::load_existing(&cli_identity_paths()?)
            .context("CLI client identity is unavailable; initialize the CLI client first")?;
    anyhow::ensure!(
        identity.did() == actor_did,
        "the GraphQL endpoint is not authenticated as this CLI's client identity"
    );
    Ok(std::sync::Arc::new(identity))
}

pub async fn load_client_identity() -> Result<gents_desktop_core::client::PrincipalIdentity>
{
    let paths = cli_identity_paths()?;
    paths.ensure_root_dirs().await?;
    gents_desktop_core::client::PrincipalIdentity::load_or_create(&paths).await
}

fn endpoint_serves_home(home_dir: &Path, url: &str) -> bool {
    let url = url.trim();
    if read_runtime_state(home_dir)
        .ok()
        .flatten()
        .is_some_and(|state| state.graphql.trim() == url)
    {
        return true;
    }
    reqwest::Url::parse(url).is_ok_and(|parsed| match parsed.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    })
}

pub fn resolve_node_did(home: Option<&Path>, explicit: Option<&str>) -> Result<String> {
    if let Some(node_did) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(node_did.to_string());
    }

    let home_dir = resolve_home_dir(home);
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return Ok(runtime_state.node_did);
    }
    if let Some(init_config) = read_init_config(&home_dir)? {
        return Ok(init_config.node_did);
    }

    anyhow::bail!(
        "node DID is required; run `gents` to set up this home and start its runtime, then retry `gents status`, or pass --node-did explicitly"
    )
}

pub fn display_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(addr) if addr == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        _ => host.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::endpoint_serves_home;

    #[tokio::test]
    async fn cli_and_desktop_keep_distinct_stable_identities() {
        let home = tempfile::tempdir().unwrap();
        let desktop = gents_desktop_core::client::DesktopPaths::from_root(home.path());
        let cli = super::cli_identity_paths_under(&desktop);
        desktop.ensure_root_dirs().await.unwrap();
        cli.ensure_root_dirs().await.unwrap();
        let desktop_identity =
            gents_desktop_core::client::PrincipalIdentity::load_or_create(&desktop)
                .await
                .unwrap();
        let cli_identity = gents_desktop_core::client::PrincipalIdentity::load_or_create(&cli)
            .await
            .unwrap();
        assert_ne!(desktop_identity.did(), cli_identity.did());
        assert_eq!(
            cli_identity.did(),
            gents_desktop_core::client::PrincipalIdentity::load_existing(&cli)
                .unwrap()
                .did()
        );
    }

    #[tokio::test]
    async fn home_endpoint_requires_and_uses_the_cli_principal() {
        let home = tempfile::tempdir().unwrap();
        let paths = gents_desktop_core::client::DesktopPaths::from_root(home.path().join("cli"));
        let url = "http://127.0.0.1:9191/api/v0/graphql";
        assert!(super::home_graphql_endpoint_with_paths(home.path(), url, &paths).is_err());
        paths.ensure_root_dirs().await.unwrap();
        let identity = gents_desktop_core::client::PrincipalIdentity::load_or_create(&paths)
            .await
            .unwrap();
        let endpoint = super::home_graphql_endpoint_with_paths(home.path(), url, &paths).unwrap();
        let access = gents_store::config_client::ConfigAccess::Graphql(endpoint);
        assert_eq!(access.actor_did().as_deref(), Some(identity.did()));
        assert!(super::home_graphql_endpoint_with_paths(
            home.path(),
            "https://unrelated.example/api/v0/graphql",
            &paths
        )
        .is_err());
    }

    #[test]
    fn bearers_go_only_to_the_homes_runtime_or_loopback() {
        let home = tempfile::tempdir().unwrap();
        assert!(endpoint_serves_home(
            home.path(),
            "http://127.0.0.1:9191/api/v0/graphql"
        ));
        assert!(endpoint_serves_home(
            home.path(),
            "http://localhost:9191/api/v0/graphql"
        ));
        assert!(endpoint_serves_home(
            home.path(),
            "http://[::1]:9191/api/v0/graphql"
        ));
        assert!(!endpoint_serves_home(
            home.path(),
            "http://100.69.4.79:9191/api/v0/graphql"
        ));
        assert!(!endpoint_serves_home(
            home.path(),
            "https://runtime.example.com/api/v0/graphql"
        ));

        std::fs::write(
            home.path().join(crate::state::RUNTIME_STATE_FILE_NAME),
            serde_json::to_vec(&serde_json::json!({
                "home": home.path(),
                "graphql": "http://100.69.4.79:9191/api/v0/graphql",
                "node_name": "a",
                "node_did": "did:key:z6Mk",
                "default_agent_did": "b",
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(endpoint_serves_home(
            home.path(),
            "http://100.69.4.79:9191/api/v0/graphql"
        ));
        assert!(!endpoint_serves_home(
            home.path(),
            "http://100.69.4.80:9191/api/v0/graphql"
        ));
    }
}
