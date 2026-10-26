use gents_protocol::error_chain::ErrorChain;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::Mutex;

use super::store::InstalledPlugin;
use crate::managed_exec::{
    spawn_managed_process, ManagedExecKind, ManagedProcess, SpawnManagedProcessRequest,
};

#[derive(Default)]
pub(super) struct Services(Mutex<BTreeMap<String, Service>>);

struct Service {
    process: Option<ManagedProcess>,
    data: Value,
    pack_digest: String,
    log_path: PathBuf,
    drains: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Service {
    fn drop(&mut self) {
        for task in &self.drains {
            task.abort();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Store(#[from] crate::pack_store::PackStoreError),
    #[error(transparent)]
    Materialize(#[from] crate::pack::materialize::MaterializeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("managed service requires an object input")]
    ObjectInput,
    #[error("plugin service is not authorized; reinstall with --grant-authority")]
    NotAuthorized,
    #[error("managed service requires an installed owner pack; install the .pack")]
    OwnerDigest,
    #[error("managed service requires an owner pack coordinate")]
    OwnerCoordinate,
    #[error("plugin service exited; inspect {} and restart the runtime. Previous actions may have executed; do not replay writes blindly", log_path.display())]
    Exited { log_path: PathBuf },
    #[error("installed service declaration does not match its owner pack; reinstall the pack")]
    DeclarationMismatch,
    #[error("runtime managed-plugin service limit reached (8); restart the runtime to release cached services")]
    LimitReached,
    #[error("starting plugin service {key}; state retained at {}. The tool request was not sent to the service", state.display())]
    Start {
        key: String,
        state: PathBuf,
        #[source]
        source: Box<ServiceError>,
    },
    #[error("{spawn}")]
    Spawn { spawn: String },
    #[error("service stdout unavailable")]
    StdoutUnavailable,
    #[error("service stderr unavailable")]
    StderrUnavailable,
    #[error("service must emit one bounded JSON readiness line on stdout")]
    ReadinessLine,
    #[error("invalid service readiness JSON")]
    ReadinessJson(#[source] serde_json::Error),
    #[error("service readiness must be a JSON object")]
    ReadinessNotObject,
    #[error("service exited during startup")]
    ExitedDuringStartup,
    #[error("service readiness timed out")]
    ReadinessTimeout(#[source] tokio::time::error::Elapsed),
    #[error("{}; service stderr: {stderr}", ErrorChain(&**error))]
    ReadinessFailed {
        error: Box<ServiceError>,
        stderr: String,
    },
}

type Result<T, E = ServiceError> = std::result::Result<T, E>;

impl Services {
    pub(super) async fn prepare(
        &self,
        home: &Path,
        record: &InstalledPlugin,
        input: &mut Value,
    ) -> Result<()> {
        let Some(declaration) = &record.declaration.service else {
            return Ok(());
        };
        let input = input.as_object_mut().ok_or(ServiceError::ObjectInput)?;
        input.remove("service");
        if (!declaration.skip_when.is_empty()
            && declaration
                .skip_when
                .iter()
                .all(|(key, value)| input.get(key) == Some(value)))
            || record.declaration.bind_dir.as_ref().is_some_and(|binding| {
                input
                    .get(&binding.input_field)
                    .is_some_and(|v| !v.is_null())
            })
        {
            return Ok(());
        }
        if !record.ceiling().child_process {
            return Err(ServiceError::NotAuthorized);
        }
        let digest = record
            .owner_pack_digest
            .as_deref()
            .ok_or(ServiceError::OwnerDigest)?;
        let owner = record
            .owner_pack_coordinate
            .as_deref()
            .ok_or(ServiceError::OwnerCoordinate)?;
        let specification = super::store::content_digest_hex(&serde_json::to_vec(declaration)?);
        let key = format!("{owner}/{specification}");
        let mut services = self.0.lock().await;
        let prefix = format!("{owner}/");
        let obsolete: Vec<_> = services
            .iter()
            .filter(|(key, service)| key.starts_with(&prefix) && service.pack_digest != digest)
            .map(|(key, _)| key.clone())
            .collect();
        for old_key in obsolete {
            if let Some(mut previous) = services.remove(&old_key) {
                previous.terminate().await;
            }
        }
        if let Some(service) = services.get_mut(&key) {
            if service
                .process
                .as_mut()
                .map(ManagedProcess::has_exited)
                .transpose()?
                .unwrap_or(true)
            {
                service.terminate().await;
                let error = ServiceError::Exited {
                    log_path: service.log_path.clone(),
                };
                return optional_start(Err(error), declaration.optional, input).map(|_| ());
            }
            input.insert("service".into(), service.data.clone());
            return Ok(());
        }
        let archive = crate::pack_store::PackStore::new(home).open(digest)?;
        if !archive
            .manifest()
            .metadata
            .plugins
            .iter()
            .any(|plugin| plugin == &record.declaration)
        {
            return Err(ServiceError::DeclarationMismatch);
        }
        if services.len() >= 8 {
            return optional_start(Err(ServiceError::LimitReached), declaration.optional, input)
                .map(|_| ());
        }
        let root = home.join(crate::home::PLUGIN_SERVICES_DIR_NAME);
        std::fs::create_dir_all(&root)?;
        let state = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(root)?
            .keep();
        let pack = state.join("pack");
        crate::pack::materialize::materialize(&archive, &pack)?;
        let argv = declaration
            .argv
            .iter()
            .map(|arg| {
                arg.replace("{pack}", &pack.to_string_lossy())
                    .replace("{state}", &state.to_string_lossy())
            })
            .collect();
        let started = Service::start(argv, &state, &key, digest, declaration.startup_timeout_secs)
            .await
            .map_err(|source| ServiceError::Start {
                key: key.clone(),
                state: state.clone(),
                source: Box::new(source),
            });
        let Some(service) = optional_start(started, declaration.optional, input)? else {
            return Ok(());
        };
        input.insert("service".into(), service.data.clone());
        services.insert(key, service);
        Ok(())
    }
}

fn optional_start(
    started: Result<Service>,
    optional: bool,
    input: &mut serde_json::Map<String, Value>,
) -> Result<Option<Service>> {
    match started {
        Ok(service) => Ok(Some(service)),
        Err(error) if optional => {
            input.insert(
                "service".into(),
                serde_json::json!({
                    "status":"unavailable", "error":format!("{}", ErrorChain(&error)), "code_executed":false
                }),
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

impl Service {
    async fn terminate(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.terminate().await;
        }
    }

    async fn start(
        argv: Vec<String>,
        state: &Path,
        name: &str,
        digest: &str,
        timeout: u32,
    ) -> Result<Self> {
        let mut process = spawn_managed_process(SpawnManagedProcessRequest {
            argv,
            cwd: state.to_path_buf(),
            environment: None,
            tool_name: Some(name.to_owned()),
            kind: ManagedExecKind::PersistentService,
        })
        .await
        .map_err(|spawn| ServiceError::Spawn { spawn })?;
        let stdout = process
            .stdout
            .take()
            .ok_or(ServiceError::StdoutUnavailable)?;
        let mut stderr = process
            .stderr
            .take()
            .ok_or(ServiceError::StderrUnavailable)?;
        let tail = Arc::new(StdMutex::new(Vec::new()));
        let captured = tail.clone();
        let log_path = state.join("service.stderr.log");
        let mut log = std::fs::File::create(&log_path)?;
        let stderr_task = tokio::spawn(async move {
            let mut buf = [0; 4096];
            let mut saved = 0usize;
            while let Ok(count) = stderr.read(&mut buf).await {
                if count == 0 {
                    break;
                }
                let keep = count.min((1024usize * 1024).saturating_sub(saved));
                let _ = log.write_all(&buf[..keep]);
                saved += keep;
                let mut tail = captured.lock().unwrap();
                tail.extend_from_slice(&buf[..count]);
                let excess = tail.len().saturating_sub(4096);
                tail.drain(..excess);
            }
        });
        let mut service = Self {
            process: Some(process),
            data: Value::Null,
            pack_digest: digest.to_owned(),
            log_path,
            drains: vec![stderr_task],
        };
        let mut reader = BufReader::new(stdout);
        let ready = match tokio::time::timeout(Duration::from_secs(u64::from(timeout)), async {
            let mut line = Vec::new();
            (&mut reader)
                .take(65537)
                .read_until(b'\n', &mut line)
                .await?;
            if line.len() > 65536 || line.last() != Some(&b'\n') {
                return Err(ServiceError::ReadinessLine);
            }
            let value: Value =
                serde_json::from_slice(&line).map_err(ServiceError::ReadinessJson)?;
            if !value.is_object() {
                return Err(ServiceError::ReadinessNotObject);
            }
            if service.process.as_mut().unwrap().has_exited()? {
                return Err(ServiceError::ExitedDuringStartup);
            }
            Ok::<_, ServiceError>(value)
        })
        .await
        {
            Ok(result) => result,
            Err(elapsed) => Err(ServiceError::ReadinessTimeout(elapsed)),
        };
        match ready {
            Ok(value) => service.data = value,
            Err(error) => {
                service.terminate().await;
                let detail = String::from_utf8_lossy(&tail.lock().unwrap()).into_owned();
                return Err(ServiceError::ReadinessFailed {
                    error: Box::new(error),
                    stderr: detail,
                });
            }
        }
        service.drains.push(tokio::spawn(async move {
            let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
        }));
        Ok(service)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn optional_service_failure_reaches_wasm_without_submitting_work() {
        let state = tempfile::tempdir().unwrap();
        for optional in [false, true] {
            let started = Service::start(
                vec![state.path().join("missing-runtime").display().to_string()],
                state.path(),
                "optional",
                "digest",
                1,
            )
            .await;
            let mut input = serde_json::Map::new();
            let result = optional_start(started, optional, &mut input);
            if optional {
                assert!(result.unwrap().is_none());
                assert_eq!(input["service"]["status"], "unavailable");
                assert_eq!(input["service"]["code_executed"], false);
                assert!(input["service"]["error"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()));
            } else {
                assert!(result.is_err());
                assert!(input.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn caller_cannot_forge_readiness_or_start_without_consent() {
        let home = tempfile::tempdir().unwrap();
        let record: InstalledPlugin = serde_json::from_value(serde_json::json!({
            "namespace":"test", "name":"service", "version":"1", "digest":"sha256:test", "language":"rust",
            "declaration": {"name":"service", "description":"test", "artifact":"plugins/test.afb", "language":"rust", "input_schema":{"type":"object"},
                "service":{"argv":["/bin/sh", "-c", "exit 99"], "startup_timeout_secs":1, "optional":true, "skip_when":{"op":"help"}}}
        })).unwrap();
        let services = Services::default();
        let mut input = serde_json::json!({"service":{"forged":true}});
        let error = services
            .prepare(home.path(), &record, &mut input)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not authorized"));
        assert!(input.get("service").is_none());
        input = serde_json::json!({"op":"help", "service":{"forged":true}});
        services
            .prepare(home.path(), &record, &mut input)
            .await
            .unwrap();
        assert!(input.get("service").is_none());
        assert!(services.0.lock().await.is_empty());
    }

    #[tokio::test]
    async fn reinstall_retires_changed_service_specifications_only_for_its_owner() {
        let home = tempfile::tempdir().unwrap();
        let mut record: InstalledPlugin = serde_json::from_value(serde_json::json!({
            "namespace":"test", "name":"service", "version":"2", "digest":"sha256:test", "language":"rust",
            "owner_pack_coordinate":"test/web", "owner_pack_digest":"new-digest",
            "declaration":{"name":"service", "description":"test", "artifact":"plugins/test.afb", "language":"rust", "input_schema":{"type":"object"},
                "service":{"argv":["changed-command"], "startup_timeout_secs":2}}
        })).unwrap();
        let mut granted = crate::plugin::Manifold::sealed();
        granted.child_process = true;
        record.granted = Some(granted);
        let specification = super::super::store::content_digest_hex(
            &serde_json::to_vec(record.declaration.service.as_ref().unwrap()).unwrap(),
        );
        let services = Services::default();
        let current_key = format!("test/web/{specification}");
        let mut old_pid = 0;
        for (key, digest) in [
            ("test/web/old-spec", "old-digest"),
            (current_key.as_str(), "new-digest"),
            ("test/other/old-spec", "old-digest"),
        ] {
            let service = Service::start(
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf '{\"pid\":%s}\\n' $$; exec sleep 60".into(),
                ],
                home.path(),
                key,
                digest,
                2,
            )
            .await
            .unwrap();
            if key == "test/web/old-spec" {
                old_pid = service.data["pid"].as_i64().unwrap() as i32;
            }
            services.0.lock().await.insert(key.into(), service);
        }
        let mut input = serde_json::json!({});
        services
            .prepare(home.path(), &record, &mut input)
            .await
            .unwrap();
        assert!(input["service"]["pid"].is_number());
        let mut entries = services.0.lock().await;
        assert_eq!(entries.len(), 2);
        assert!(!entries
            .get_mut("test/other/old-spec")
            .unwrap()
            .process
            .as_mut()
            .unwrap()
            .has_exited()
            .unwrap());
        assert_ne!(unsafe { libc::kill(old_pid, 0) }, 0);
    }

    #[tokio::test]
    async fn readiness_and_drop_own_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = Service::start(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf '{\"pid\":%s}\\n' $$; exec sleep 60".into(),
            ],
            dir.path(),
            "test",
            "digest",
            2,
        )
        .await
        .unwrap();
        let pid = service.data["pid"].as_i64().unwrap() as i32;
        assert!(!service.process.as_mut().unwrap().has_exited().unwrap());
        drop(service);
        for _ in 0..100 {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("managed service survived owner drop");
    }

    #[tokio::test]
    async fn exited_service_keeps_identity_until_cleanup_and_then_stays_retired() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = Service::start(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf '{\"pid\":%s}\\n' $$; exec sleep 60".into(),
            ],
            dir.path(),
            "identity",
            "digest",
            2,
        )
        .await
        .unwrap();
        let pid = service.data["pid"].as_i64().unwrap() as i32;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !service.process.as_mut().unwrap().has_exited().unwrap() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        assert_eq!(unsafe { info.si_pid() }, pid);
        service.terminate().await;
        assert!(service.process.is_none());
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
        service.terminate().await;
        assert!(service.process.is_none());
    }

    #[tokio::test]
    async fn invalid_readiness_kills_term_resistant_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let result = Service::start(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                r#"/bin/sh -c 'trap "" TERM; echo $$ > descendant; exec sleep 60' &
while [ ! -s descendant ]; do sleep 0.01; done
echo invalid-json
wait"#
                    .into(),
            ],
            dir.path(),
            "descendants",
            "digest",
            2,
        )
        .await;
        assert!(result.is_err());
        let pid: i32 = std::fs::read_to_string(dir.path().join("descendant"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_stopped(pid).await;
    }

    async fn assert_stopped(pid: i32) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while unsafe { libc::kill(pid, 0) } == 0 {
                #[cfg(target_os = "linux")]
                if std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                    stat.rsplit_once(") ")
                        .is_some_and(|(_, fields)| fields.starts_with("Z "))
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("TERM-resistant descendant survived cleanup");
    }

    #[tokio::test]
    async fn cancelled_cached_retirement_kills_descendants_without_restarting() {
        let home = tempfile::tempdir().unwrap();
        let mut record: InstalledPlugin = serde_json::from_value(serde_json::json!({
            "namespace":"test", "name":"service", "version":"1", "digest":"sha256:test", "language":"rust",
            "owner_pack_coordinate":"test/cancel", "owner_pack_digest":"digest",
            "declaration":{"name":"service", "description":"test", "artifact":"plugin.afb", "language":"rust", "input_schema":{"type":"object"},
                "service":{"argv":["must-not-restart"], "startup_timeout_secs":1, "optional":true}}
        })).unwrap();
        let mut granted = crate::plugin::Manifold::sealed();
        granted.child_process = true;
        record.granted = Some(granted);
        let key = format!(
            "test/cancel/{}",
            super::super::store::content_digest_hex(
                &serde_json::to_vec(record.declaration.service.as_ref().unwrap()).unwrap()
            )
        );
        let mut service = Service::start(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                r#"/bin/sh -c 'trap "" TERM; echo $$ > descendant; exec sleep 60' &
while [ ! -s descendant ]; do sleep 0.01; done
printf '{"pid":%s}\n' $$
wait"#
                    .into(),
            ],
            home.path(),
            "cancel",
            "digest",
            2,
        )
        .await
        .unwrap();
        let pid = service.data["pid"].as_i64().unwrap() as i32;
        let descendant: i32 = std::fs::read_to_string(home.path().join("descendant"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !service.process.as_mut().unwrap().has_exited().unwrap() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let services = Services::default();
        services.0.lock().await.insert(key.clone(), service);
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            services.prepare(home.path(), &record, &mut serde_json::json!({}))
        )
        .await
        .is_err());
        assert!(services.0.lock().await[&key].process.is_none());
        assert_stopped(descendant).await;
        let mut input = serde_json::json!({});
        services
            .prepare(home.path(), &record, &mut input)
            .await
            .unwrap();
        assert_eq!(input["service"]["status"], "unavailable");
        assert!(input["service"]["error"]
            .as_str()
            .unwrap()
            .contains("service exited"));
        assert!(!home
            .path()
            .join(crate::home::PLUGIN_SERVICES_DIR_NAME)
            .exists());
    }

    #[tokio::test]
    async fn capacity_failure_is_optional_but_archive_mismatch_is_not() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("plugins")).unwrap();
        let mut granted = crate::plugin::Manifold::sealed();
        granted.child_process = true;
        std::fs::write(source.path().join("README.md"), "# Capacity fixture").unwrap();
        let services = Services::default();
        for index in 0..8 {
            let service = Service::start(
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo '{}'; exec sleep 60".into(),
                ],
                home.path(),
                "capacity",
                "other",
                2,
            )
            .await
            .unwrap();
            services
                .0
                .lock()
                .await
                .insert(format!("other/{index}"), service);
        }
        std::fs::write(
            source.path().join("plugins/plugin.afb"),
            super::super::tests::build_plugin_afb(super::super::tests::ECHO_WAT),
        )
        .unwrap();
        for optional in [false, true] {
            let declaration = serde_json::json!({
                "name":"service", "description":"test", "artifact":"plugins/plugin.afb", "language":"rust",
                "input_schema":{"type":"object"}, "manifold":granted,
                "service":{"argv":["missing-runtime"],"startup_timeout_secs":1,"optional":optional}
            });
            let manifest = serde_json::json!({
                "manifest_version":1,"namespace":"test","name":"capacity","version":"1.0.0",
                "description":"test","authors":["test"],"tags":[],"kind":"plugins",
                "assets":["README.md","plugins/plugin.afb"],"plugins":[declaration]
            });
            std::fs::write(
                source.path().join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            let (bytes, _) = crate::pack_archive::pack_dir(source.path()).unwrap();
            let stored = crate::pack_store::PackStore::new(home.path())
                .import(bytes.as_slice(), None)
                .unwrap();
            let mut record: InstalledPlugin = serde_json::from_value(serde_json::json!({
                "namespace":"test", "name":"service", "version":"1", "digest":"sha256:test", "language":"rust",
                "owner_pack_coordinate":"test/capacity", "owner_pack_digest":stored.header.digest,
                "declaration":declaration, "granted":granted
            })).unwrap();
            let mut input = serde_json::json!({});
            let result = services.prepare(home.path(), &record, &mut input).await;
            if optional {
                result.unwrap();
                assert_eq!(input["service"]["status"], "unavailable");
                assert_eq!(input["service"]["code_executed"], false);
                assert!(input["service"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("limit reached"));
            } else {
                assert!(result.unwrap_err().to_string().contains("limit reached"));
            }
            record.declaration.service.as_mut().unwrap().argv = vec!["tampered".into()];
            assert!(services
                .prepare(home.path(), &record, &mut serde_json::json!({}))
                .await
                .unwrap_err()
                .to_string()
                .contains("does not match"));
        }
        assert!(!home
            .path()
            .join(crate::home::PLUGIN_SERVICES_DIR_NAME)
            .exists());
    }

    #[tokio::test]
    async fn invalid_readiness_and_timeout_are_failures() {
        let dir = tempfile::tempdir().unwrap();
        for command in ["echo not-json; exec sleep 60", "exec sleep 60"] {
            assert!(Service::start(
                vec!["/bin/sh".into(), "-c".into(), command.into()],
                dir.path(),
                "test",
                "digest",
                1
            )
            .await
            .is_err());
        }
    }
}
