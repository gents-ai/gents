use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::plugin::executor::PluginExecutor;
use gents::plugin::model_calls::AccessModels;
use gents::plugin::{BoundDir, PluginOutcome, PluginVerdict};

use crate::cli::args::PluginRunArgs;

use super::store::{self, InstalledPlugin};

#[derive(Debug, thiserror::Error)]
enum PluginRunError {
    #[error("{coordinate} was refused before it ran: {diagnostics}")]
    Refused {
        coordinate: String,
        diagnostics: String,
    },
    #[error("{coordinate} ran out of its fuel budget: {diagnostics}")]
    OutOfFuel {
        coordinate: String,
        diagnostics: String,
    },
    #[error("{coordinate} exceeded its memory budget: {diagnostics}")]
    OutOfMemory {
        coordinate: String,
        diagnostics: String,
    },
    #[error("{coordinate} exceeded its wall-clock budget: {diagnostics}")]
    Timeout {
        coordinate: String,
        diagnostics: String,
    },
    #[error("{coordinate} did not return a single JSON value: {diagnostics}")]
    BadOutput {
        coordinate: String,
        diagnostics: String,
    },
    #[error("{coordinate} failed: {diagnostics}")]
    Failed {
        coordinate: String,
        diagnostics: String,
    },
}

pub(crate) async fn run(args: PluginRunArgs) -> Result<()> {
    let (namespace, name) = crate::commands::pack::split_namespace(&args.name);
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let record = store::read_record(&home, namespace, name).with_context(|| {
        format!(
            "{namespace}/{name} is not installed under {}; run `gents plugin install {name}` \
             first",
            home.display()
        )
    })?;
    let input = match &args.input {
        Some(raw) => serde_json::from_str::<serde_json::Value>(raw)
            .with_context(|| format!("--input is not valid JSON: {raw:?}"))?,
        None => serde_json::Value::Null,
    };
    let mut executor = PluginExecutor::new(Some(home.clone()));
    if record.declaration.model_slot.is_some() && record.model_binding.is_some() {
        let (access, _) = crate::resolve_config_access(Some(&home), None).await?;
        executor = executor.with_models(Arc::new(AccessModels(access)));
    }
    let output = run_plugin(&executor, &record, input, args.bind_dir.as_deref()).await?;
    Ok(crate::print_json(&output)?)
}

/// Runs the installed `namespace/name` with `input` and returns its own
/// JSON result, for an embedding caller (the desktop's `ctx.plugin`, a ux
/// module producer) that holds no argv. The model slot is honored exactly
/// as the CLI honors it. The input's declared `bind_dir` field is bound
/// through the same owner a model tool's call uses
/// ([`PluginExecutor::call_data_bound`]): the folder it names must be one
/// the operator allowed, or be approved by them when the caller runs under
/// [`gents::plugin::approval::scope_interactive`]; an input without the
/// field runs sealed. There is no `--bind-dir` here: the operator never
/// names a folder directly on this path.
pub async fn call_installed(
    home: &Path,
    namespace: &str,
    name: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value> {
    let record = store::read_record(home, namespace, name).with_context(|| {
        format!(
            "{namespace}/{name} is not installed under {}",
            home.display()
        )
    })?;
    let mut executor = PluginExecutor::new(Some(home.to_owned()));
    if record.declaration.model_slot.is_some() && record.model_binding.is_some() {
        let (access, _) = crate::resolve_config_access(Some(home), None).await?;
        executor = executor.with_models(Arc::new(AccessModels(access)));
    }
    let coordinate = format!("{namespace}/{name}");
    // No tool-effect grant: a call from the desktop or a module producer
    // runs outside any model turn, so the guest has no runtime to call back.
    let call = executor
        .call_data_bound(&record, input, None, false)
        .await
        .with_context(|| format!("calling plugin {coordinate}"))?;
    if let Some(note) = &call.binding_note {
        tracing::warn!(plugin = %coordinate, "{note}");
    }
    Ok(outcome_output(&coordinate, call.outcome)?)
}

async fn run_plugin(
    executor: &PluginExecutor,
    record: &InstalledPlugin,
    input: serde_json::Value,
    bind_dir: Option<&Path>,
) -> Result<serde_json::Value> {
    let coordinate = format!("{}/{}", record.namespace, record.name);
    let call = match bind_dir {
        Some(dir) => {
            let bound = BoundDir::new(dir, None)
                .with_context(|| format!("binding {} for {coordinate}", dir.display()))?;
            executor.call_bound(record, input, bound).await
        }
        None => executor.call(record, input).await,
    }
    .with_context(|| format!("calling plugin {coordinate}"))?;
    if let Some(note) = &call.binding_note {
        eprintln!("{note}");
    }
    Ok(outcome_output(&coordinate, call.outcome)?)
}

fn outcome_output(
    coordinate: &str,
    outcome: PluginOutcome,
) -> Result<serde_json::Value, PluginRunError> {
    let coordinate = coordinate.to_owned();
    let diagnostics = outcome.diagnostics;
    match outcome.verdict {
        PluginVerdict::Success => Ok(outcome.output),
        PluginVerdict::Refused => Err(PluginRunError::Refused {
            coordinate,
            diagnostics,
        }),
        PluginVerdict::OutOfFuel => Err(PluginRunError::OutOfFuel {
            coordinate,
            diagnostics,
        }),
        PluginVerdict::OutOfMemory => Err(PluginRunError::OutOfMemory {
            coordinate,
            diagnostics,
        }),
        PluginVerdict::Timeout => Err(PluginRunError::Timeout {
            coordinate,
            diagnostics,
        }),
        PluginVerdict::BadOutput => Err(PluginRunError::BadOutput {
            coordinate,
            diagnostics,
        }),
        PluginVerdict::Failed => Err(PluginRunError::Failed {
            coordinate,
            diagnostics,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::plugin::PluginRunner;
    use gents_protocol::error_chain::ErrorChain;

    async fn run_installed(
        bytes: &[u8],
        record: &InstalledPlugin,
        input: serde_json::Value,
        bind_dir: Option<&Path>,
    ) -> Result<serde_json::Value> {
        let home = tempfile::tempdir().unwrap();
        let digest_hex = record.digest.strip_prefix("sha256:").unwrap();
        store::store_bytes(home.path(), digest_hex, bytes).unwrap();
        store::write_record(home.path(), record).unwrap();
        let executor = PluginExecutor::new(Some(home.path().to_owned()));
        run_plugin(&executor, record, input, bind_dir).await
    }

    fn build_echo_plugin() -> Vec<u8> {
        crate::commands::plugin::testing::build_echo_plugin()
    }

    fn sample_record(bytes: &[u8]) -> InstalledPlugin {
        InstalledPlugin {
            namespace: "gents".to_owned(),
            name: "echo".to_owned(),
            version: "0.1.0".to_owned(),
            digest: format!("sha256:{:x}", <sha2::Sha256 as sha2::Digest>::digest(bytes)),
            language: "rust".to_owned(),
            granted: None,
            instructions: None,
            declaration: crate::commands::plugin::declaration_from_artifact(
                &afterburner_cloud::Afb::from_bytes(bytes).unwrap(),
            )
            .unwrap(),
            owner_pack_coordinate: None,
            owner_pack_digest: None,
            model_binding: None,
            data_unique_keys: Default::default(),
            data_collection_fields: Default::default(),
        }
    }

    #[tokio::test]
    async fn plugin_run_returns_the_plugins_own_output() {
        let bytes = build_echo_plugin();
        let input = serde_json::json!({"hello": "world", "n": 42});
        let output = run_installed(&bytes, &sample_record(&bytes), input.clone(), None)
            .await
            .expect("the echo plugin must run");
        assert_eq!(output, input);
    }

    #[tokio::test]
    async fn plugin_run_defaults_to_null_input() {
        let bytes = build_echo_plugin();
        let output = run_installed(
            &bytes,
            &sample_record(&bytes),
            serde_json::Value::Null,
            None,
        )
        .await
        .expect("must run");
        assert_eq!(output, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn installed_pack_retains_authored_admission_metadata() {
        let bytes = build_echo_plugin();
        let home = tempfile::tempdir().unwrap();
        let mut declaration = sample_record(&bytes).declaration;
        declaration.description = "Authored pack description".into();
        declaration.input_schema = serde_json::json!({
            "type": "object", "required": ["message"],
            "properties": {"message": {"type": "string"}},
        });
        declaration.manifold = None;
        gents::plugin::install::install_from_pack(
            home.path(),
            "team",
            "team/echo",
            "1.0.0",
            "sha256:test-pack-digest",
            &declaration,
            &bytes,
            None,
            Default::default(),
            Default::default(),
            false,
        )
        .unwrap();
        let record = store::read_record(home.path(), "team", "echo").unwrap();
        assert_eq!(record.declaration, declaration);
        let runner = PluginRunner::compile(&bytes, &record.declaration).unwrap();
        assert_eq!(runner.definition(), &declaration);
        assert_eq!(
            run_installed(&bytes, &record, serde_json::json!({"message": "ok"}), None)
                .await
                .unwrap(),
            serde_json::json!({"message": "ok"})
        );

        let mut invalid = record;
        invalid.declaration.manifold = Some(serde_json::json!({"fs": "invalid"}));
        assert!(
            run_installed(&bytes, &invalid, serde_json::Value::Null, None)
                .await
                .is_err()
        );
        let empty_home = tempfile::tempdir().unwrap();
        assert!(gents::plugin::install::install_from_pack(
            empty_home.path(),
            "team",
            "team/echo",
            "1.0.0",
            "sha256:test-pack-digest",
            &invalid.declaration,
            &bytes,
            None,
            Default::default(),
            Default::default(),
            false
        )
        .is_err());
        assert!(store::list_records(empty_home.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_reads_the_installed_plugin_by_name_and_prints_its_output() {
        let bytes = build_echo_plugin();
        let home = tempfile::tempdir().unwrap();
        let record = sample_record(&bytes);
        let digest_hex = record.digest.strip_prefix("sha256:").unwrap();
        store::store_bytes(home.path(), digest_hex, &bytes).unwrap();
        store::write_record(home.path(), &record).unwrap();

        run(PluginRunArgs {
            name: "gents/echo".to_owned(),
            input: Some(r#"{"ok":true}"#.to_owned()),
            home: Some(home.path().to_owned()),
            bind_dir: None,
        })
        .await
        .expect("running an installed plugin by name must succeed");
    }

    #[tokio::test]
    async fn running_a_plugin_that_is_not_installed_names_it() {
        let home = tempfile::tempdir().unwrap();
        let error = run(PluginRunArgs {
            name: "gents/does_not_exist".to_owned(),
            input: None,
            home: Some(home.path().to_owned()),
            bind_dir: None,
        })
        .await
        .expect_err("an uninstalled plugin must be refused");
        let message = ErrorChain(error.as_ref()).to_string();
        assert!(message.contains("gents/does_not_exist"), "{message}");
        assert!(message.contains("not installed"), "{message}");
    }

    #[tokio::test]
    async fn run_with_bind_dir_reads_only_the_bound_directory() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let manifest: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();
        let (archive_bytes, _) = gents::pack_archive::pack_dir(fixture.path()).unwrap();
        let archive = gents::pack_archive::PackArchive::from_bytes(&archive_bytes).unwrap();
        let home = tempfile::tempdir().unwrap();
        gents::plugin::install::install_pack_plugins(
            home.path(),
            &manifest,
            archive.digest(),
            |path| Ok(archive.asset(path)?),
            false,
        )
        .unwrap();
        let record = store::read_record(home.path(), "fixture", "list_files").unwrap();
        let digest_hex = record.digest.strip_prefix("sha256:").unwrap();
        let bytes = store::read_bytes(home.path(), digest_hex).unwrap();

        let listing = tempfile::tempdir().unwrap();
        std::fs::write(listing.path().join("one.txt"), b"1").unwrap();
        std::fs::write(listing.path().join("two.txt"), b"2").unwrap();

        let output = run_installed(
            &bytes,
            &record,
            serde_json::json!({"root": "unused"}),
            Some(listing.path()),
        )
        .await
        .expect("running bound to the listing directory must succeed");
        assert_eq!(output, serde_json::json!({"files": ["one.txt", "two.txt"]}));

        let real_root = serde_json::json!({"root": listing.path().to_str().unwrap()});
        let error = run_installed(&bytes, &record, real_root, None)
            .await
            .expect_err("without a binding the plugin has no filesystem access at all");
        assert!(
            ErrorChain(error.as_ref()).to_string().contains("trapped"),
            "expected a hard trap from the guest's own failed filesystem access: {}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn run_with_bind_dir_and_no_input_succeeds() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let manifest: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();
        let (archive_bytes, _) = gents::pack_archive::pack_dir(fixture.path()).unwrap();
        let archive = gents::pack_archive::PackArchive::from_bytes(&archive_bytes).unwrap();
        let home = tempfile::tempdir().unwrap();
        gents::plugin::install::install_pack_plugins(
            home.path(),
            &manifest,
            archive.digest(),
            |path| Ok(archive.asset(path)?),
            false,
        )
        .unwrap();

        let listing = tempfile::tempdir().unwrap();
        std::fs::write(listing.path().join("one.txt"), b"1").unwrap();

        run(PluginRunArgs {
            name: "fixture/list_files".to_owned(),
            input: None,
            home: Some(home.path().to_owned()),
            bind_dir: Some(listing.path().to_owned()),
        })
        .await
        .expect("a binding with no --input must supply the only field the plugin needs");
    }

    #[test]
    fn requested_authority_needs_consent_and_is_recorded() {
        let bytes = build_echo_plugin();
        let mut declaration = sample_record(&bytes).declaration;
        declaration.manifold = Some(serde_json::json!({
            "fs": "None", "net": {"OutboundHttp": ["api.example.com"]}, "env": "None",
            "crypto": false, "child_process": false
        }));
        let home = tempfile::tempdir().unwrap();
        let error = gents::plugin::install::install_from_pack(
            home.path(),
            "team",
            "team/echo",
            "1.0.0",
            "sha256:test-pack-digest",
            &declaration,
            &bytes,
            None,
            Default::default(),
            Default::default(),
            false,
        )
        .unwrap_err();
        assert!(
            ErrorChain(&error).to_string().contains("--grant-authority"),
            "{}",
            ErrorChain(&error)
        );
        assert!(store::list_records(home.path()).unwrap().is_empty());

        let record = gents::plugin::install::install_from_pack(
            home.path(),
            "team",
            "team/echo",
            "1.0.0",
            "sha256:test-pack-digest",
            &declaration,
            &bytes,
            None,
            Default::default(),
            Default::default(),
            true,
        )
        .unwrap();
        assert!(record.granted.is_some());
        gents::plugin::install::install_from_pack(
            home.path(),
            "team",
            "team/echo",
            "1.0.1",
            "sha256:test-pack-digest-2",
            &declaration,
            &bytes,
            None,
            Default::default(),
            Default::default(),
            false,
        )
        .expect("the recorded grant covers the same request");
    }

    /// A `ctx.plugin` call names no folder on the command line: the one its
    /// input carries under the declared `bind_dir` field is bound only when
    /// the operator allowed it, and refused naming the field until then. An
    /// input without the field runs sealed, where the fixture's own
    /// `read_dir` traps.
    #[tokio::test]
    async fn call_installed_binds_the_named_root_only_when_allowed() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let manifest: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();
        let (archive_bytes, _) = gents::pack_archive::pack_dir(fixture.path()).unwrap();
        let archive = gents::pack_archive::PackArchive::from_bytes(&archive_bytes).unwrap();
        let home = tempfile::tempdir().unwrap();
        gents::plugin::install::install_pack_plugins(
            home.path(),
            &manifest,
            archive.digest(),
            |path| Ok(archive.asset(path)?),
            false,
        )
        .unwrap();

        let listing = tempfile::tempdir().unwrap();
        std::fs::write(listing.path().join("one.txt"), b"1").unwrap();
        std::fs::write(listing.path().join("two.txt"), b"2").unwrap();
        let input = serde_json::json!({ "root": listing.path().to_str().unwrap() });

        let error = call_installed(home.path(), "fixture", "list_files", input.clone())
            .await
            .expect_err("a folder the operator has not allowed must be refused");
        let message = ErrorChain(error.as_ref()).to_string();
        assert!(message.contains("cannot read \"root\""), "{message}");

        gents::plugin::allowed::add(home.path(), listing.path(), gents::pack::BindAccess::Read)
            .unwrap();
        let output = call_installed(home.path(), "fixture", "list_files", input)
            .await
            .expect("an allowed folder binds and the plugin lists it");
        assert_eq!(output, serde_json::json!({"files": ["one.txt", "two.txt"]}));

        let error = call_installed(home.path(), "fixture", "list_files", serde_json::json!({}))
            .await
            .expect_err("without the field the plugin runs sealed and cannot read anything");
        let message = ErrorChain(error.as_ref()).to_string();
        assert!(!message.contains("cannot read"), "{message}");
        assert!(message.contains("trapped"), "{message}");
    }
}
