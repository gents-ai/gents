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

/// What `gents` opens with no `--home`.
pub fn default_home() -> Result<PathBuf> {
    #[cfg(test)]
    let default = Ok::<_, gents_store::local::home::HomeError>(default_home_dir());
    #[cfg(not(test))]
    let default = gents_store::local::home::default_home_dir();
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
        let status = gents::client_runtime::fetch_runtime_connection_payload(graphql).await?;
        let node_did = status["node_did"]
            .as_str()
            .filter(|did| !did.is_empty())
            .context("the runtime did not identify its Node")?;
        return client_signed_endpoint(graphql, &identity, node_did);
    }

    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        return home_endpoint_as(&home_dir, runtime_state.graphql, &identity).await;
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
    .await
}

/// Client document access retains the user's identity. Credentials remain bound to
/// this home's endpoint or loopback; loading a runtime key never grants client access.
pub async fn home_graphql_endpoint(
    home_dir: &Path,
    url: impl Into<String>,
) -> Result<GraphqlEndpoint> {
    home_graphql_endpoint_with_paths(home_dir, url, &cli_identity_paths(home_dir)?).await
}

async fn home_graphql_endpoint_with_paths(
    home_dir: &Path,
    url: impl Into<String>,
    paths: &gents_store::client_journal::paths::DesktopPaths,
) -> Result<GraphqlEndpoint> {
    let identity = client_identity_in(home_dir, paths).await?;
    home_endpoint_as(home_dir, url, &identity).await
}

async fn home_endpoint_as(
    home_dir: &Path,
    url: impl Into<String>,
    identity: &gents_store::client_journal::principal_identity::PrincipalIdentity,
) -> Result<GraphqlEndpoint> {
    let url = url.into();
    anyhow::ensure!(
        endpoint_serves_home(home_dir, &url),
        "endpoint is not this home's runtime"
    );
    let node_did = resolve_node_did(Some(home_dir), None)?;
    client_signed_endpoint(url, identity, &node_did)
}

/// The endpoint reads its runtime's public status before its first request
/// and mints for the audience it reports; a request refused for its
/// credential reads the status again (`Identity.HeldAudience.resend`), so an
/// endpoint held across a restart under another audience recovers on its next
/// request. The CLI keeps what the status reports only with its endpoint.
fn client_signed_endpoint(
    url: impl Into<String>,
    identity: &gents_store::client_journal::principal_identity::PrincipalIdentity,
    node_did: &str,
) -> Result<GraphqlEndpoint> {
    let url = url.into();
    let capability = gents_store::config_client::ClientSigningCapability::from_ed25519_key(
        identity.private_key_bytes(),
    )?;
    Ok(gents::runtime::lifecycle_hooks::compose_endpoint(
        gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(capability)
            .endpoint(node_did, &url)?
            .with_client_signed_agent_creates(),
    )?)
}

/// The CLI client identity that administers `home`: the folder its settings
/// record (`init --desktop-home`), else the default desktop home's `cli`
/// folder. The CLI is a separate client principal, even when desktop and CLI
/// share a user home. The identity follows the home, never the environment, so
/// a command given `--home` signs with the key that home was set up with.
pub fn cli_identity_paths(home: &Path) -> Result<gents_store::client_journal::paths::DesktopPaths> {
    match gents_store::local::home::recorded_client_identity(home) {
        Some(recorded) => Ok(gents_store::client_journal::paths::DesktopPaths::from_root(
            recorded,
        )),
        None => default_cli_identity_paths(),
    }
}

/// The CLI client identity under the default desktop home. Unit tests share
/// one throwaway CLI client home; see [`use_unit_test_homes`].
pub fn default_cli_identity_paths() -> Result<gents_store::client_journal::paths::DesktopPaths> {
    #[cfg(any(test, feature = "test-home-guard"))]
    if let Some(homes) = unit_test_homes() {
        return Ok(gents_store::client_journal::paths::DesktopPaths::from_root(
            homes.path().join("desktop"),
        )
        .cli_client());
    }
    Ok(gents_store::client_journal::paths::DesktopPaths::platform_default()?.cli_client())
}

/// The CLI's held User key signs every client-authored command; a registry
/// lookup by DID cannot present the key to owners that need its capability.
pub fn held_client_identity(
    home: &Path,
    actor_did: &str,
) -> Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>> {
    let identity = existing_client_identity(home, &cli_identity_paths(home)?)?;
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
    paths: &gents_store::client_journal::paths::DesktopPaths,
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
    paths: &gents_store::client_journal::paths::DesktopPaths,
) -> std::result::Result<
    gents_store::client_journal::principal_identity::PrincipalIdentity,
    ClientIdentityError,
> {
    if !paths
        .identity_key_path()
        .try_exists()
        .map_err(ClientIdentityError::load)?
    {
        return Err(missing_client_identity(home, paths));
    }
    gents_store::client_journal::principal_identity::PrincipalIdentity::load_existing(paths)
        .map_err(ClientIdentityError::load)
}

/// The one owner that decides whether this machine may create its CLI client
/// identity: only while `home` is not yet set up. A set-up home's Node and
/// sessions belong to the identity that set it up, so a key minted for it would
/// own nothing there and hide everything; that is refused with
/// [`ClientIdentityError::MissingForSetUpHome`].
pub async fn client_identity_for(
    home: &Path,
) -> std::result::Result<
    gents_store::client_journal::principal_identity::PrincipalIdentity,
    ClientIdentityError,
> {
    client_identity_in(
        home,
        &cli_identity_paths(home).map_err(ClientIdentityError::load)?,
    )
    .await
}

/// [`client_identity_for`] with the identity folder `paths`, which `init`
/// passes before it records the folder in the home's settings.
pub async fn client_identity_in(
    home: &Path,
    paths: &gents_store::client_journal::paths::DesktopPaths,
) -> std::result::Result<
    gents_store::client_journal::principal_identity::PrincipalIdentity,
    ClientIdentityError,
> {
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

/// `init` creating a new Node's store with the identity folder `paths`: the
/// Node it makes is owned by this identity, so creating one is part of setting
/// the home up.
pub async fn client_identity_for_new_node(
    paths: &gents_store::client_journal::paths::DesktopPaths,
) -> std::result::Result<
    gents_store::client_journal::principal_identity::PrincipalIdentity,
    ClientIdentityError,
> {
    load_or_create_client_identity(paths).await
}

async fn load_or_create_client_identity(
    paths: &gents_store::client_journal::paths::DesktopPaths,
) -> std::result::Result<
    gents_store::client_journal::principal_identity::PrincipalIdentity,
    ClientIdentityError,
> {
    paths
        .ensure_root_dirs()
        .await
        .map_err(ClientIdentityError::load)?;
    gents_store::client_journal::principal_identity::PrincipalIdentity::load_or_create(paths)
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
        let desktop = gents_store::client_journal::paths::DesktopPaths::from_root(home.path());
        let cli = desktop.cli_client();
        desktop.ensure_root_dirs().await.unwrap();
        cli.ensure_root_dirs().await.unwrap();
        let desktop_identity =
            gents_store::client_journal::principal_identity::PrincipalIdentity::load_or_create(
                &desktop,
            )
            .await
            .unwrap();
        let cli_identity =
            gents_store::client_journal::principal_identity::PrincipalIdentity::load_or_create(
                &cli,
            )
            .await
            .unwrap();
        assert_ne!(desktop_identity.did(), cli_identity.did());
        assert_eq!(
            cli_identity.did(),
            gents_store::client_journal::principal_identity::PrincipalIdentity::load_existing(&cli)
                .unwrap()
                .did()
        );
    }

    #[tokio::test]
    async fn home_endpoint_requires_and_uses_the_cli_principal() {
        let home = tempfile::tempdir().unwrap();
        let paths =
            gents_store::client_journal::paths::DesktopPaths::from_root(home.path().join("cli"));
        let url = "http://127.0.0.1:9191/api/v0/graphql";
        std::fs::write(
            super::init_config_path(home.path()),
            serde_json::json!({
                "home": home.path(), "node_name": "runtime", "node_did": "did:key:runtime",
                "key_path": null, "tool_ceiling": crate::args::ToolCeilingArg::Readwrite, "tool_root": null,
            })
            .to_string(),
        )
        .unwrap();
        assert!(
            super::home_graphql_endpoint_with_paths(home.path(), url, &paths)
                .await
                .is_err()
        );
        paths.ensure_root_dirs().await.unwrap();
        let identity =
            gents_store::client_journal::principal_identity::PrincipalIdentity::load_or_create(
                &paths,
            )
            .await
            .unwrap();
        let endpoint = super::home_graphql_endpoint_with_paths(home.path(), url, &paths)
            .await
            .unwrap();
        let access = gents_store::config_client::ConfigAccess::Graphql(endpoint);
        assert_eq!(access.actor_did().as_deref(), Some(identity.did()));
        let signer = access.client_signer().unwrap();
        let payload = b"client-held command signing";
        let signature = gents_store::identity::NodeIdentity::sign(&signer, payload)
            .await
            .unwrap();
        assert!(gents_store::identity::NodeIdentity::verify(
            &identity,
            identity.did(),
            payload,
            &signature,
        )
        .await
        .unwrap());
        assert!(super::home_graphql_endpoint_with_paths(
            home.path(),
            "https://unrelated.example/api/v0/graphql",
            &paths
        )
        .await
        .is_err());
    }

    /// A runtime whose status and audience the test sets: it refuses a User's
    /// identity token with the edge's code while it reports an audience, and
    /// a delegation for any other deployment.
    struct Runtime {
        url: String,
        node: std::sync::Arc<gents_store::identity::KeyIdentity>,
        status: std::sync::Arc<std::sync::Mutex<serde_json::Value>>,
        presented: std::sync::Arc<std::sync::Mutex<Vec<Option<u64>>>>,
    }

    impl Runtime {
        fn reports(&self, status: serde_json::Value) {
            *self.status.lock().unwrap() = status;
        }

        fn presented(&self) -> Vec<Option<u64>> {
            std::mem::take(&mut *self.presented.lock().unwrap())
        }
    }

    fn presented_for(headers: &axum::http::HeaderMap) -> Option<u64> {
        use base64::Engine as _;
        let bearer = headers.get("authorization")?.to_str().ok()?;
        let claims = bearer.strip_prefix("Bearer ")?.split('.').nth(1)?;
        let claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(claims)
                .ok()?,
        )
        .ok()?;
        claims["aud"].as_str()?.strip_prefix("vera:")?.parse().ok()
    }

    fn ready(deployment: Option<u64>) -> serde_json::Value {
        let mut status = serde_json::json!({ "node_did": "did:key:test-runtime", "client_authentication": {"kind":"local"}, "lifecycle": "ready" });
        if let Some(deployment) = deployment {
            status["client_authentication"] = serde_json::json!({"kind":"vera","worker":"did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK","deployment":deployment});
            status["delegation_audience"] = serde_json::json!({
                "worker": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
                "deployment": deployment,
            });
        }
        status
    }

    async fn runtime() -> Runtime {
        use axum::http::StatusCode;
        let status = std::sync::Arc::new(std::sync::Mutex::new(
            serde_json::json!({ "lifecycle": "starting" }),
        ));
        let presented = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let node = std::sync::Arc::new(
            tokio::task::spawn_blocking(|| gents_store::test_support::test_identity("runtime"))
                .await
                .unwrap(),
        );
        let (reported, taking, seen, signer) = (
            status.clone(),
            status.clone(),
            presented.clone(),
            node.clone(),
        );
        let router = axum::Router::new()
            .route(
                "/status",
                axum::routing::get(
                    move |query: axum::extract::RawQuery| {
                        let (reported, signer) = (reported.clone(), signer.clone());
                        async move {
                            let mut status = reported.lock().unwrap().clone();
                            if status.get("node_did").is_some() {
                                status["node_did"] =
                                    gents_store::identity::NodeIdentity::did(signer.as_ref()).into();
                                if let Some(nonce) = query.0.as_deref().and_then(|query| {
                                    query.split('&').find_map(|pair| {
                                        let (name, nonce) = pair.split_once('=')?;
                                        (name
                                            == gents_protocol::home_attestation::HOME_NONCE_QUERY)
                                            .then(|| {
                                                gents_protocol::home_attestation::HomeNonce::parse(
                                                    nonce,
                                                )
                                                .ok()
                                            })
                                            .flatten()
                                    })
                                }) {
                                    gents_store::identity::home_status::attest(
                                        Some(signer.as_ref()),
                                        &nonce,
                                        &mut status,
                                    )
                                    .await;
                                }
                            }
                            axum::Json(status)
                        }
                    },
                ),
            )
            .fallback(move |headers: axum::http::HeaderMap| {
                let (taking, seen) = (taking.clone(), seen.clone());
                async move {
                    let audience = presented_for(&headers);
                    seen.lock().unwrap().push(audience);
                    let takes = taking.lock().unwrap()["delegation_audience"]["deployment"].as_u64();
                    match (takes, audience) {
                        (None, None) => (
                            StatusCode::OK,
                            axum::Json(serde_json::json!({"data": {"Node": []}})),
                        ),
                        (Some(takes), Some(presented)) if takes == presented => (
                            StatusCode::OK,
                            axum::Json(serde_json::json!({"data": {"Node": []}})),
                        ),
                        (Some(_), None) => (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(serde_json::json!({
                                "code": gents_protocol::delegation_audience::DELEGATION_REQUIRED_CODE,
                            })),
                        ),
                        _ => (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({}))),
                    }
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        Runtime {
            url: format!("http://{address}/api/v0/graphql"),
            node,
            status,
            presented,
        }
    }

    async fn cli_endpoint(runtime: &Runtime) -> gents_store::config_client::GraphqlEndpoint {
        let home = tempfile::tempdir().unwrap();
        let paths = gents_store::client_journal::paths::DesktopPaths::from_root(home.path());
        paths.ensure_root_dirs().await.unwrap();
        let identity =
            gents_store::client_journal::principal_identity::PrincipalIdentity::load_or_create(
                &paths,
            )
            .await
            .unwrap();
        super::client_signed_endpoint(
            runtime.url.as_str(),
            &identity,
            gents_store::identity::NodeIdentity::did(runtime.node.as_ref()),
        )
        .unwrap()
    }

    async fn reads(endpoint: &gents_store::config_client::GraphqlEndpoint) -> bool {
        gents_store::config_client::post_graphql(endpoint, "query { Node { _docID } }")
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn cli_endpoint_recovers_audience_rotations_and_local_identity_presentation() {
        let runtime = runtime().await;
        let early = cli_endpoint(&runtime).await;
        runtime.reports(ready(Some(1)));
        assert!(reads(&early).await);
        assert_eq!(runtime.presented(), vec![Some(1)]);

        runtime.reports(ready(Some(2)));
        assert!(reads(&early).await);
        assert_eq!(runtime.presented(), vec![Some(1), Some(2)]);

        runtime.reports(ready(None));
        let local = cli_endpoint(&runtime).await;
        assert!(reads(&local).await);
        assert_eq!(runtime.presented(), vec![None]);
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
