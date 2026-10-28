use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::args::{P2pDiscoveryArg, P2pRelayModeArg, ServeArgs, ServiceNetworkArgs};
use crate::native_service::{CommandRunner, NativeServiceManager};

/// Listen settings `gents server` applies for each flag it is not given. The
/// native service definition stays `gents server --home <HOME>`, so the
/// desktop's own installs and updates of that definition keep these.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) http_addr: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) http_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) p2p_bind_addr: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) p2p_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) p2p_relay_mode: Option<P2pRelayModeArg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) p2p_discovery: Option<P2pDiscoveryArg>,
}

pub(crate) fn network_settings_path(home: &Path) -> PathBuf {
    home.join(gents::home::NETWORK_CONFIG_FILE_NAME)
}

impl NetworkSettings {
    pub(crate) fn read(home: &Path) -> Result<Self> {
        let path = network_settings_path(home);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading network settings {}", path.display()))
            }
        };
        let settings: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding network settings {}", path.display()))?;
        settings
            .validate()
            .with_context(|| format!("network settings {}", path.display()))?;
        Ok(settings)
    }

    /// Writes the settings, or removes the file when none remain. Returns
    /// whether the persisted settings changed.
    pub(crate) fn persist(&self, home: &Path) -> Result<bool> {
        self.validate()?;
        if *self == Self::read(home)? {
            return Ok(false);
        }
        let path = network_settings_path(home);
        if *self == Self::default() {
            std::fs::remove_file(&path)
                .with_context(|| format!("removing network settings {}", path.display()))?;
        } else {
            let mut contents =
                serde_json::to_vec_pretty(self).context("encoding network settings")?;
            contents.push(b'\n');
            crate::native_service::atomic_write(&path, &contents)
                .with_context(|| format!("writing network settings {}", path.display()))?;
        }
        Ok(true)
    }

    /// The public identity/enrollment surface also binds one interface rather
    /// than every interface; operator views require the owning DID bearer.
    pub(crate) fn validate(&self) -> Result<()> {
        if let Some(addr) = self.http_addr {
            validate_http_bind(addr)?;
        }
        if self.http_port == Some(0) {
            bail!("HTTP port 0 is not supported; choose an explicit non-zero port");
        }
        Ok(())
    }

    pub(crate) fn apply(&self, args: &ServiceNetworkArgs) -> Self {
        let base = if args.reset_network {
            Self::default()
        } else {
            self.clone()
        };
        Self {
            http_addr: args.http_addr.or(base.http_addr),
            http_port: args.http_port.or(base.http_port),
            p2p_bind_addr: args.p2p_bind_addr.or(base.p2p_bind_addr),
            p2p_port: args.p2p_port.or(base.p2p_port),
            p2p_relay_mode: args.p2p_relay_mode.or(base.p2p_relay_mode),
            p2p_discovery: args.p2p_discovery.or(base.p2p_discovery),
        }
    }

    /// Explicit `gents server` flags win over persisted settings.
    pub(crate) fn fill_unset(&self, args: &mut ServeArgs) {
        args.http_addr = args.http_addr.or(self.http_addr);
        args.http_port = args.http_port.or(self.http_port);
        args.p2p_bind_addr = args.p2p_bind_addr.or(self.p2p_bind_addr);
        args.p2p_port = args.p2p_port.or(self.p2p_port);
        args.p2p_relay_mode = args.p2p_relay_mode.or(self.p2p_relay_mode);
        args.p2p_discovery = args.p2p_discovery.or(self.p2p_discovery);
    }

    pub(crate) fn summary(&self) -> String {
        let http = SocketAddr::new(
            self.http_addr.unwrap_or(DEFAULT_HTTP_ADDR),
            self.http_port.unwrap_or(crate::DEFAULT_HTTP_PORT),
        );
        let p2p_addr = self.p2p_bind_addr.unwrap_or(DEFAULT_HTTP_ADDR);
        let p2p = match self.p2p_port.filter(|port| *port != 0) {
            Some(port) => SocketAddr::new(p2p_addr, port).to_string(),
            None => match p2p_addr {
                IpAddr::V4(addr) => format!("{addr}:random"),
                IpAddr::V6(addr) => format!("[{addr}]:random"),
            },
        };
        format!(
            "network: http={http} p2p={p2p} p2p_relay_mode={} p2p_discovery={} (configured; applies at the next start)",
            relay_mode_name(self.p2p_relay_mode.unwrap_or(DEFAULT_P2P_RELAY_MODE)),
            discovery_name(self.p2p_discovery.unwrap_or(DEFAULT_P2P_DISCOVERY)),
        )
    }
}

pub(crate) const DEFAULT_HTTP_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
pub(crate) const DEFAULT_P2P_RELAY_MODE: P2pRelayModeArg = P2pRelayModeArg::Disabled;
pub(crate) const DEFAULT_P2P_DISCOVERY: P2pDiscoveryArg = P2pDiscoveryArg::Disabled;

fn relay_mode_name(mode: P2pRelayModeArg) -> &'static str {
    match mode {
        P2pRelayModeArg::Default => "default",
        P2pRelayModeArg::Disabled => "disabled",
    }
}

fn discovery_name(discovery: P2pDiscoveryArg) -> &'static str {
    match discovery {
        P2pDiscoveryArg::N0 => "n0",
        P2pDiscoveryArg::Disabled => "disabled",
    }
}

/// Installs the definition before persisting settings so a refused install
/// leaves the home untouched.
pub(crate) fn install_service<R>(
    manager: &NativeServiceManager<R>,
    home: &Path,
    network: &ServiceNetworkArgs,
    environment: Option<&std::collections::BTreeMap<String, String>>,
) -> Result<NetworkSettings>
where
    R: CommandRunner,
{
    let settings = NetworkSettings::read(home)?.apply(network);
    settings.validate()?;
    match environment {
        Some(environment) => manager.install_with_environment(environment)?,
        None => manager.install()?,
    }
    if settings.persist(home)? && manager.status()?.running {
        tracing::warn!(
            home = %home.display(),
            "network settings changed while the service is running; run `gents service restart --home {}` to apply them",
            home.display()
        );
    }
    Ok(settings)
}

/// Persisted settings and explicit server flags have the same listener boundary.
pub(crate) fn validate_http_bind(addr: IpAddr) -> Result<()> {
    if addr.is_unspecified() {
        bail!("refusing to bind the service HTTP listener on unspecified address {addr}; bind loopback or a specific interface, such as this host's tailnet address, instead");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::sync::{Arc, Mutex};

    use clap::Parser as _;

    use super::*;
    use crate::cli::{Cli, Command, ServiceAction};
    use crate::native_service::{
        CommandOutput, NativeServiceConfig, NativeServicePlatform, SERVICE_LABEL, SYSTEMD_UNIT,
    };

    /// Answers every supervisor query as a stopped, never-loaded job.
    #[derive(Clone, Default)]
    struct StoppedSupervisor {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl CommandRunner for StoppedSupervisor {
        fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
            let args: Vec<String> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let call = format!("{} {}", program.to_string_lossy(), args.join(" "));
            self.calls.lock().unwrap().push(call);
            let (success, stdout) = match (program.to_str().unwrap(), args.as_slice()) {
                ("/usr/bin/id", _) => (true, "501"),
                ("launchctl", [verb, ..]) if verb == "print" => (false, "Could not find service"),
                ("launchctl", [verb, ..]) if verb == "print-disabled" => (true, "{}"),
                ("systemctl", [_, verb, ..]) if verb == "is-active" => (false, "inactive"),
                ("systemctl", [_, verb, ..]) if verb == "is-enabled" => (false, "disabled"),
                _ => (true, ""),
            };
            Ok(CommandOutput {
                success,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            })
        }
    }

    fn service_config(root: &Path) -> NativeServiceConfig {
        let home = root.join("node-home");
        fs::create_dir_all(&home).unwrap();
        let executable = root.join("gents");
        fs::write(&executable, b"fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        NativeServiceConfig {
            home: fs::canonicalize(&home).unwrap(),
            executable: fs::canonicalize(&executable).unwrap(),
            user_home: root.to_path_buf(),
            service_config_dir: root.join("service-definitions"),
            search_path: None,
            associated_bundle_id: None,
            stderr_path: None,
        }
    }

    fn install_network(argv: &[&str]) -> ServiceNetworkArgs {
        let mut full = vec!["gents", "service", "install"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full)
            .expect("service install parses")
            .command
        {
            Command::Service { command } => match command.into_parts().2 {
                ServiceAction::Install { network, .. } => network,
                _ => panic!("expected install"),
            },
            _ => panic!("expected service"),
        }
    }

    fn serve_args(argv: &[&str]) -> ServeArgs {
        let mut full = vec!["gents", "server"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full).expect("server parses").command {
            Command::Server(args) => args,
            _ => panic!("expected server"),
        }
    }

    const TAILNET: &[&str] = &[
        "--http-addr",
        "100.64.0.7",
        "--http-port",
        "9393",
        "--p2p-bind-addr",
        "0.0.0.0",
        "--p2p-port",
        "4455",
        "--p2p-relay-mode",
        "default",
        "--p2p-discovery",
        "n0",
    ];

    fn tailnet_settings() -> NetworkSettings {
        NetworkSettings {
            http_addr: Some("100.64.0.7".parse().unwrap()),
            http_port: Some(9393),
            p2p_bind_addr: Some("0.0.0.0".parse().unwrap()),
            p2p_port: Some(4455),
            p2p_relay_mode: Some(P2pRelayModeArg::Default),
            p2p_discovery: Some(P2pDiscoveryArg::N0),
        }
    }

    #[test]
    fn service_install_parses_every_network_flag() {
        let network = install_network(TAILNET);
        assert_eq!(
            NetworkSettings::default().apply(&network),
            tailnet_settings()
        );
        assert!(!network.reset_network);
        assert!(install_network(&["--reset-network"]).reset_network);
    }

    #[test]
    fn server_network_flags_default_to_unset_so_persisted_settings_can_fill_them() {
        let args = serve_args(&[]);
        assert_eq!(args.http_addr, None);
        assert_eq!(args.http_port, None);
        assert_eq!(args.p2p_bind_addr, None);
        assert_eq!(args.p2p_port, None);
        assert_eq!(args.p2p_relay_mode, None);
        assert_eq!(args.p2p_discovery, None);
    }

    #[test]
    fn explicit_server_flags_win_over_persisted_settings() {
        let mut args = serve_args(&["--http-addr", "127.0.0.1", "--p2p-port", "5000"]);
        tailnet_settings().fill_unset(&mut args);
        assert_eq!(args.http_addr, Some(DEFAULT_HTTP_ADDR));
        assert_eq!(args.http_port, Some(9393));
        assert_eq!(args.p2p_bind_addr, Some("0.0.0.0".parse().unwrap()));
        assert_eq!(args.p2p_port, Some(5000));
        assert_eq!(args.p2p_relay_mode, Some(P2pRelayModeArg::Default));
        assert_eq!(args.p2p_discovery, Some(P2pDiscoveryArg::N0));
    }

    #[test]
    fn later_installs_merge_and_reset_clears_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        assert!(NetworkSettings::default()
            .apply(&install_network(TAILNET))
            .persist(home)
            .unwrap());
        let merged = NetworkSettings::read(home)
            .unwrap()
            .apply(&install_network(&["--http-port", "9494"]));
        assert_eq!(merged.http_port, Some(9494));
        assert_eq!(merged.http_addr, tailnet_settings().http_addr);
        assert!(!tailnet_settings().persist(home).unwrap(), "unchanged");

        let reset = NetworkSettings::read(home)
            .unwrap()
            .apply(&install_network(&["--reset-network"]));
        assert_eq!(reset, NetworkSettings::default());
        assert!(reset.persist(home).unwrap());
        assert!(!network_settings_path(home).exists());
    }

    #[test]
    fn persisted_file_is_stable_json() {
        let temp = tempfile::tempdir().unwrap();
        tailnet_settings().persist(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(network_settings_path(temp.path())).unwrap(),
            "{\n  \"http_addr\": \"100.64.0.7\",\n  \"http_port\": 9393,\n  \"p2p_bind_addr\": \"0.0.0.0\",\n  \"p2p_port\": 4455,\n  \"p2p_relay_mode\": \"default\",\n  \"p2p_discovery\": \"n0\"\n}\n"
        );
    }

    #[test]
    fn unspecified_http_bind_and_port_zero_are_refused() {
        for addr in ["0.0.0.0", "::"] {
            let error = NetworkSettings::default()
                .apply(&install_network(&["--http-addr", addr]))
                .validate()
                .unwrap_err();
            assert!(error.to_string().contains("unspecified address"), "{error}");
        }
        assert!(NetworkSettings::default()
            .apply(&install_network(&["--http-port", "0"]))
            .validate()
            .is_err());

        let temp = tempfile::tempdir().unwrap();
        fs::write(
            network_settings_path(temp.path()),
            br#"{"http_addr":"0.0.0.0"}"#,
        )
        .unwrap();
        assert!(NetworkSettings::read(temp.path()).is_err());
        fs::write(network_settings_path(temp.path()), br#"{"http_bind":"x"}"#).unwrap();
        assert!(NetworkSettings::read(temp.path()).is_err());
    }

    #[test]
    fn summary_reports_effective_values() {
        assert_eq!(
            NetworkSettings::default().summary(),
            "network: http=127.0.0.1:9191 p2p=127.0.0.1:random p2p_relay_mode=disabled p2p_discovery=disabled (configured; applies at the next start)"
        );
        assert_eq!(
            tailnet_settings().summary(),
            "network: http=100.64.0.7:9393 p2p=0.0.0.0:4455 p2p_relay_mode=default p2p_discovery=n0 (configured; applies at the next start)"
        );
    }

    fn install_on(
        platform: NativeServicePlatform,
        argv: &[&str],
    ) -> (tempfile::TempDir, NativeServiceConfig, String) {
        let temp = tempfile::tempdir().unwrap();
        let config = service_config(temp.path());
        let manager = NativeServiceManager::with_runner(
            config.clone(),
            platform,
            StoppedSupervisor::default(),
        );
        install_service(&manager, &config.home, &install_network(argv), None).unwrap();
        let definition = fs::read_to_string(config.definition_path(platform)).unwrap();
        (temp, config, definition)
    }

    #[test]
    fn launchd_definition_runs_the_home_whatever_its_network_settings() {
        let (_plain_root, config, plain) = install_on(NativeServicePlatform::Macos, &[]);
        let (_tailnet_root, tailnet_config, tailnet) =
            install_on(NativeServicePlatform::Macos, TAILNET);
        let arguments = |config: &NativeServiceConfig| {
            format!(
                "<key>ProgramArguments</key><array>\n    <string>{}</string><string>server</string><string>--home</string><string>{}</string>\n  </array>",
                config.executable.display(),
                config.home.display()
            )
        };
        assert!(plain.contains(&arguments(&config)), "{plain}");
        assert!(tailnet.contains(&arguments(&tailnet_config)), "{tailnet}");
        assert!(tailnet.contains(&format!("<key>Label</key><string>{SERVICE_LABEL}</string>")));
        assert!(!tailnet.contains("100.64.0.7") && !tailnet.contains("--http"));
        assert_eq!(
            NetworkSettings::read(&tailnet_config.home).unwrap(),
            tailnet_settings()
        );
        assert!(!network_settings_path(&config.home).exists());
    }

    #[test]
    fn systemd_definition_runs_the_home_whatever_its_network_settings() {
        let (_plain_root, config, plain) = install_on(NativeServicePlatform::Linux, &[]);
        let (_tailnet_root, tailnet_config, tailnet) =
            install_on(NativeServicePlatform::Linux, TAILNET);
        let exec_start = |config: &NativeServiceConfig| {
            format!(
                "ExecStart=\"/usr/bin/env\" -- \"GENTS_SYSTEM_LOG=1\" \"{}\" \"server\" \"--home\" \"{}\"\n",
                config.executable.display(),
                config.home.display()
            )
        };
        assert!(plain.contains(&exec_start(&config)), "{plain}");
        assert!(tailnet.contains(&exec_start(&tailnet_config)), "{tailnet}");
        assert!(tailnet_config
            .definition_path(NativeServicePlatform::Linux)
            .ends_with(SYSTEMD_UNIT));
        assert!(!tailnet.contains("100.64.0.7") && !tailnet.contains("--http"));
        assert_eq!(
            NetworkSettings::read(&tailnet_config.home).unwrap(),
            tailnet_settings()
        );
    }

    #[test]
    fn a_refused_setting_leaves_no_definition_or_settings() {
        let temp = tempfile::tempdir().unwrap();
        let config = service_config(temp.path());
        let runner = StoppedSupervisor::default();
        let manager = NativeServiceManager::with_runner(
            config.clone(),
            NativeServicePlatform::Linux,
            runner.clone(),
        );
        let error = install_service(
            &manager,
            &config.home,
            &install_network(&["--http-addr", "0.0.0.0"]),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unspecified address"));
        assert!(!config
            .definition_path(NativeServicePlatform::Linux)
            .exists());
        assert!(!network_settings_path(&config.home).exists());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn install_accepts_an_explicit_environment_file() {
        let cli = Cli::try_parse_from([
            "gents",
            "service",
            "install",
            "--environment-file",
            "operator.json",
        ])
        .unwrap();
        let Command::Service { command } = cli.command else {
            panic!("service command");
        };
        let ServiceAction::Install {
            environment_file, ..
        } = command.into_parts().2
        else {
            panic!("service install");
        };
        assert_eq!(
            environment_file.as_deref(),
            Some(Path::new("operator.json"))
        );
    }
}
