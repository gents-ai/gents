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

/// What `gents` opens with neither `--home` nor `GENTS_HOME`.
pub fn default_home_ignoring_env() -> Result<PathBuf> {
    #[cfg(test)]
    let default = Ok::<_, gents_store::local::home::HomeError>(default_home_dir());
    #[cfg(not(test))]
    let default = gents_store::local::home::default_home_ignoring_env();
    Ok(default?)
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
    let path = runtime_state_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).context("encoding local runtime state JSON")?;
    // Replaced, never truncated in place: a server killed mid-write would
    // otherwise leave an empty file every later command fails to decode.
    crate::native_service::atomic_write(&path, &contents)
        .with_context(|| format!("writing runtime state {}", path.display()))
}

/// A record that does not decode was written by another version's runtime,
/// which this build cannot reach, so it names no runtime. Writes replace the
/// file whole, so it is never a partial write.
pub fn read_runtime_state(home_dir: &Path) -> Result<Option<StoredRuntimeState>> {
    let path = runtime_state_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).with_context(|| format!("reading runtime state {}", path.display()))?;
    match serde_json::from_slice(&bytes) {
        Ok(state) => Ok(Some(state)),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring another version's runtime state");
            Ok(None)
        }
    }
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

pub async fn resolve_graphql_endpoint(
    explicit: Option<&str>,
    home: Option<&Path>,
) -> Result<GraphqlEndpoint> {
    let home_dir = resolve_home_dir(home);
    let identity = client_identity_for(&home_dir).await?;
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
        return client_signed_endpoint(graphql, &identity);
    }

    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return home_endpoint_as(&home_dir, runtime_state.graphql, &identity);
    }

    anyhow::ensure!(
        home.is_none(),
        "home {} has no running server recorded; start `gents runtime server --home {}` or pass --graphql explicitly",
        home_dir.display(),
        home_dir.display()
    );

    home_endpoint_as(
        &home_dir,
        format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"),
        &identity,
    )
}

/// Client document access retains the user's identity. Credentials remain bound to
/// this home's endpoint or loopback; loading a runtime key never grants client access.
pub async fn home_graphql_endpoint(
    home_dir: &Path,
    url: impl Into<String>,
) -> Result<GraphqlEndpoint> {
    home_graphql_endpoint_with_paths(home_dir, url, &cli_identity_paths()?).await
}

async fn home_graphql_endpoint_with_paths(
    home_dir: &Path,
    url: impl Into<String>,
    paths: &gents_desktop_core::client::DesktopPaths,
) -> Result<GraphqlEndpoint> {
    let identity = client_identity_in(home_dir, paths).await?;
    home_endpoint_as(home_dir, url, &identity)
}

fn home_endpoint_as(
    home_dir: &Path,
    url: impl Into<String>,
    identity: &gents_desktop_core::client::PrincipalIdentity,
) -> Result<GraphqlEndpoint> {
    let url = url.into();
    anyhow::ensure!(
        endpoint_serves_home(home_dir, &url),
        "endpoint is not this home's runtime"
    );
    client_signed_endpoint(url, identity)
}

fn client_signed_endpoint(
    url: impl Into<String>,
    identity: &gents_desktop_core::client::PrincipalIdentity,
) -> Result<GraphqlEndpoint> {
    let capability = gents_store::config_client::ClientSigningCapability::from_ed25519_key(
        identity.private_key_bytes(),
    )?;
    Ok(gents::runtime::lifecycle_hooks::compose_endpoint(
        GraphqlEndpoint::as_principal(url, identity.did())
            .with_client_signing_capability(capability)?
            .with_client_signed_agent_creates(),
    )?)
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
    home: &Path,
    actor_did: &str,
) -> Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>> {
    let identity = existing_client_identity(home, &cli_identity_paths()?)?;
    anyhow::ensure!(
        identity.did() == actor_did,
        "the GraphQL endpoint is not authenticated as this CLI's client identity"
    );
    Ok(std::sync::Arc::new(identity))
}

/// Why this machine's CLI client identity cannot be used for a home.
#[derive(Debug, thiserror::Error)]
pub enum ClientIdentityError {
    /// The home finished setup, so its Node and sessions were made by a client
    /// identity whose key is not on this machine. ACP lets only that Node's
    /// owner or an administrator it granted read the sessions or grant another
    /// identity, so no command can enrol a new identity into this home from here.
    #[error("{}", missing_for_set_up_home(.home, .identity_dir))]
    MissingForSetUpHome {
        home: PathBuf,
        identity_dir: PathBuf,
    },
    #[error(
        "this machine has no Gents client identity yet; run `gents` to set up a home, which creates one"
    )]
    NotCreated,
    #[error("loading this machine's Gents client identity")]
    Load(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl ClientIdentityError {
    fn load(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Load(source.into())
    }
}

/// `gents runtime desktop reset` sets the home aside as a backup and is
/// macOS-only; elsewhere a new home in another folder is the only fresh start.
fn missing_for_set_up_home(home: &Path, identity_dir: &Path) -> String {
    let home = home.display();
    let identity_dir = identity_dir.display();
    let start_over = if cfg!(target_os = "macos") {
        format!(
            "To start over with a new identity, run `gents runtime desktop reset --home {home}`: it previews, then keeps {home} as a backup, and the next `gents` sets up a new home"
        )
    } else {
        "To start over with a new identity, run `gents --home NEW_FOLDER` to set up a new home"
            .to_owned()
    };
    format!(
        "This machine has no Gents client identity for {home}; its sessions were made by an identity whose key belongs in {identity_dir}, and this machine does not have it. No command can give a new identity access to them: restore {identity_dir} from a backup and run this again. {start_over}; the new identity will not see the earlier sessions."
    )
}

/// Whether this machine loads, creates or refuses its CLI client identity for
/// a home: the Lean `clientIdentityAction`.
pub fn client_identity_action(
    observed: &gents_protocol::operations::install_repair::ClientIdentityObservation,
) -> gents_protocol::operations::install_repair::ClientIdentityAction {
    use gents_protocol::operations::install_repair::ClientIdentityAction;
    if observed.key_present {
        ClientIdentityAction::Load
    } else if observed.settings_present && !observed.pending {
        ClientIdentityAction::Refuse
    } else {
        ClientIdentityAction::Create
    }
}

/// A home whose setup finished has a Node owned by the client identity that set
/// it up; an interrupted setup has none yet and may still take this machine's.
fn home_is_set_up(home: &Path) -> std::io::Result<bool> {
    use gents_protocol::operations::install_repair::{
        ClientIdentityAction, ClientIdentityObservation,
    };
    Ok(client_identity_action(&ClientIdentityObservation {
        key_present: false,
        settings_present: init_config_path(home).try_exists()?,
        pending: home.join(INIT_PENDING_FILE_NAME).try_exists()?,
    }) == ClientIdentityAction::Refuse)
}

fn missing_client_identity(
    home: &Path,
    paths: &gents_desktop_core::client::DesktopPaths,
) -> ClientIdentityError {
    match home_is_set_up(home) {
        Ok(true) => ClientIdentityError::MissingForSetUpHome {
            home: home.to_path_buf(),
            identity_dir: paths.root().to_path_buf(),
        },
        Ok(false) => ClientIdentityError::NotCreated,
        Err(error) => ClientIdentityError::load(error),
    }
}

fn existing_client_identity(
    home: &Path,
    paths: &gents_desktop_core::client::DesktopPaths,
) -> std::result::Result<gents_desktop_core::client::PrincipalIdentity, ClientIdentityError> {
    if !paths
        .identity_key_path()
        .try_exists()
        .map_err(ClientIdentityError::load)?
    {
        return Err(missing_client_identity(home, paths));
    }
    gents_desktop_core::client::PrincipalIdentity::load_existing(paths)
        .map_err(ClientIdentityError::load)
}

/// The one owner that decides whether this machine may create its CLI client
/// identity: only while `home` is not yet set up. A set-up home's Node and
/// sessions belong to the identity that set it up, so a key minted for it would
/// own nothing there and hide everything; that is refused with
/// [`ClientIdentityError::MissingForSetUpHome`].
pub async fn client_identity_for(
    home: &Path,
) -> std::result::Result<gents_desktop_core::client::PrincipalIdentity, ClientIdentityError> {
    client_identity_in(
        home,
        &cli_identity_paths().map_err(ClientIdentityError::load)?,
    )
    .await
}

async fn client_identity_in(
    home: &Path,
    paths: &gents_desktop_core::client::DesktopPaths,
) -> std::result::Result<gents_desktop_core::client::PrincipalIdentity, ClientIdentityError> {
    let key_present = tokio::fs::try_exists(paths.identity_key_path())
        .await
        .map_err(ClientIdentityError::load)?;
    if !key_present {
        let selected = home.to_path_buf();
        let set_up = tokio::task::spawn_blocking(move || home_is_set_up(&selected))
            .await
            .map_err(ClientIdentityError::load)?
            .map_err(ClientIdentityError::load)?;
        if set_up {
            return Err(ClientIdentityError::MissingForSetUpHome {
                home: home.to_path_buf(),
                identity_dir: paths.root().to_path_buf(),
            });
        }
    }
    load_or_create_client_identity(paths).await
}

/// `init` creating a new Node's store: the Node it makes is owned by this
/// identity, so creating one is part of setting the home up.
pub async fn client_identity_for_new_node(
) -> std::result::Result<gents_desktop_core::client::PrincipalIdentity, ClientIdentityError> {
    let paths = cli_identity_paths().map_err(ClientIdentityError::load)?;
    load_or_create_client_identity(&paths).await
}

async fn load_or_create_client_identity(
    paths: &gents_desktop_core::client::DesktopPaths,
) -> std::result::Result<gents_desktop_core::client::PrincipalIdentity, ClientIdentityError> {
    paths
        .ensure_root_dirs()
        .await
        .map_err(ClientIdentityError::load)?;
    gents_desktop_core::client::PrincipalIdentity::load_or_create(paths)
        .await
        .map_err(ClientIdentityError::load)
}

type HeldUserIdentity = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>>,
            > + Send,
    >,
>;

/// [`client_identity_for`] `home` in the shape a pack wire takes to load the
/// held User identity only when it writes through a local transaction.
pub fn load_held_user_identity(home: &Path) -> HeldUserIdentity {
    let home = home.to_path_buf();
    Box::pin(async move {
        let identity: std::sync::Arc<dyn gents_store::identity::NodeIdentity> =
            std::sync::Arc::new(client_identity_for(&home).await?);
        Ok(identity)
    })
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
        std::fs::write(super::init_config_path(home.path()), "{}").unwrap();
        assert!(
            super::home_graphql_endpoint_with_paths(home.path(), url, &paths)
                .await
                .is_err()
        );
        paths.ensure_root_dirs().await.unwrap();
        let identity = gents_desktop_core::client::PrincipalIdentity::load_or_create(&paths)
            .await
            .unwrap();
        let endpoint = super::home_graphql_endpoint_with_paths(home.path(), url, &paths)
            .await
            .unwrap();
        let access = gents_store::config_client::ConfigAccess::Graphql(endpoint);
        assert_eq!(access.actor_did().as_deref(), Some(identity.did()));
        assert!(super::home_graphql_endpoint_with_paths(
            home.path(),
            "https://unrelated.example/api/v0/graphql",
            &paths
        )
        .await
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
    #[test]
    fn another_versions_runtime_record_names_no_runtime_this_build_can_reach() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            super::runtime_state_path(home.path()),
            r#"{"home":"/Users/someone/.gents","graphql":"http://127.0.0.1:9191/api/v0/graphql","node_name":"Forge","node_did":"did:key:z6Mk","default_agent_id":"bae-1","p2p_transport":"iroh","p2p_peer_id":"peer","p2p_listen_addresses":[],"p2p_admission":{}}"#,
        )
        .unwrap();
        assert!(super::read_runtime_state(home.path()).unwrap().is_none());
    }
}
