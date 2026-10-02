//! `gents plugin run`: run one installed plugin to completion and print
//! what it returned. The surface that proves a plugin actually runs, not
//! just that it parses.
//!
//! Running is [`gents::plugin::PluginRunner`]'s job, not this file's: it is
//! the one place in this workspace that hands a plugin's `.afb` to
//! `afterburner::afb_run::run_afb_bytes` and refuses admission outright
//! when the real artifact would dispatch through a path that cannot honor
//! every declared bound (see that module's own doc, rule 4). This file's
//! job is narrower: find the installed bytes and retained
//! [`gents::pack::PackPlugin`] declaration by name, and turn
//! its typed [`gents::plugin::PluginOutcome`] into either the plugin's own
//! JSON value or a clear refusal naming which bound was hit.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::plugin::executor::PluginExecutor;
use gents::plugin::model_calls::AccessModels;
use gents::plugin::{BoundDir, PluginOutcome, PluginVerdict};

use crate::cli::args::PluginRunArgs;

use super::store::{self, InstalledPlugin};

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
    // The control plane is opened only for a plugin whose model slot is
    // bound: any other call touches no store.
    if record.declaration.model_slot.is_some() && record.model_binding.is_some() {
        let (access, _) = crate::resolve_config_access(Some(&home), None).await?;
        executor = executor.with_models(Arc::new(AccessModels(access)));
    }
    let output = run_plugin(&executor, &record, input, args.bind_dir.as_deref()).await?;
    crate::print_json(&output)
}

/// Admits and calls one installed plugin, returning its own JSON result or
/// an error naming which bound was hit.
///
/// Admission uses the declaration retained at installation. Artifact capability
/// metadata is not a substitute for a pack author's narrower declaration.
/// `bind_dir` is the operator's own directory (`--bind-dir`, `within =
/// None`: the operator already named it directly, so nothing here narrows
/// it further); `None` calls ordinarily. The executor runs the guest on a
/// blocking thread (the WASI runtime under it blocks on its own runtime for
/// host I/O, which panics on an async task's thread) and answers the
/// plugin's model requests when its slot is bound.
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
    outcome_output(&coordinate, call.outcome)
}

fn outcome_output(coordinate: &str, outcome: PluginOutcome) -> Result<serde_json::Value> {
    match outcome.verdict {
        PluginVerdict::Success => Ok(outcome.output),
        PluginVerdict::Refused => {
            anyhow::bail!(
                "{coordinate} was refused before it ran: {}",
                outcome.diagnostics
            )
        }
        PluginVerdict::OutOfFuel => {
            anyhow::bail!(
                "{coordinate} ran out of its fuel budget: {}",
                outcome.diagnostics
            )
        }
        PluginVerdict::OutOfMemory => {
            anyhow::bail!(
                "{coordinate} exceeded its memory budget: {}",
                outcome.diagnostics
            )
        }
        PluginVerdict::Timeout => {
            anyhow::bail!(
                "{coordinate} exceeded its wall-clock budget: {}",
                outcome.diagnostics
            )
        }
        PluginVerdict::BadOutput => anyhow::bail!(
            "{coordinate} did not return a single JSON value: {}",
            outcome.diagnostics
        ),
        PluginVerdict::Failed => anyhow::bail!("{coordinate} failed: {}", outcome.diagnostics),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::plugin::PluginRunner;

    /// Stores `record` and its bytes in a fresh home and runs it there.
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

    /// The identity plugin, from the shared fixture: reads all of stdin
    /// and writes it back unchanged, which is what makes it the vehicle
    /// for every "does the ABI carry arguments through" test below.
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
        // No declared authority: never replace this with artifact capabilities.
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

        // An invalid authored admission declaration must fail, not get silently
        // replaced by the valid manifold embedded in the artifact.
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
        let message = format!("{error:#}");
        assert!(message.contains("gents/does_not_exist"), "{message}");
        assert!(message.contains("not installed"), "{message}");
    }

    /// The `bind_plugin_fixture` plugin lists whatever directory it is
    /// bound to, and nothing without a binding: with a binding it returns
    /// exactly the files this test put there, and without one the same
    /// `root` (a real, existing directory) is still unreachable, because
    /// its declared manifold has no standing `fs` grant of its own -
    /// binding is the only way in.
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
            |path| archive.asset(path),
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

        // Same real directory, but named directly instead of bound: the
        // plugin still has no way to reach it, proving the binding (not the
        // directory's existence) is what grants access. With no preopen for
        // an unbound path, the guest's own `std::fs::read_dir` call fails
        // and its `.expect` panics, which surfaces here as a trap - not the
        // `Success`/`Failed` verdict a routine bound would report.
        let real_root = serde_json::json!({"root": listing.path().to_str().unwrap()});
        let error = run_installed(&bytes, &record, real_root, None)
            .await
            .expect_err("without a binding the plugin has no filesystem access at all");
        assert!(
            format!("{error:#}").contains("trapped"),
            "expected a hard trap from the guest's own failed filesystem access: {error:#}"
        );
    }

    /// `gents plugin run <name> --bind-dir DIR` with no `--input` at all: the
    /// binding supplies the plugin's only required field, so an omitted
    /// operator input (which `run` turns into `Value::Null`) must not be
    /// refused for not being an object.
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
            |path| archive.asset(path),
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

    /// A plugin that asks for access is refused without consent, and the
    /// grant is recorded with it; the same request later needs none.
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
            false,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("--grant-authority"),
            "{error:#}"
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
            false,
        )
        .expect("the recorded grant covers the same request");
    }
}
