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

use anyhow::{Context, Result};
use gents::plugin::{PluginBudget, PluginRunner, PluginVerdict};

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
    let digest_hex = record.digest.strip_prefix("sha256:").with_context(|| {
        format!("the installed record for {namespace}/{name} has a malformed digest")
    })?;
    let bytes = store::read_bytes(&home, digest_hex)
        .with_context(|| format!("reading the installed bytes for {namespace}/{name}"))?;

    let input = match &args.input {
        Some(raw) => serde_json::from_str::<serde_json::Value>(raw)
            .with_context(|| format!("--input is not valid JSON: {raw:?}"))?,
        None => serde_json::Value::Null,
    };

    // On a blocking thread, never on the async executor's own. Running a
    // guest is synchronous, and the WASI runtime under it blocks on its own
    // runtime to do host I/O: called directly from an async task, that
    // panics with "cannot start a runtime from within a runtime" before the
    // guest produces a byte. `spawn_blocking` uses tokio's existing
    // blocking pool, so this costs no thread of its own.
    let coordinate = format!("{namespace}/{name}");
    let output = tokio::task::spawn_blocking(move || run_plugin(&bytes, &record, &input))
        .await
        .with_context(|| format!("running plugin {coordinate}"))?
        .with_context(|| format!("running plugin {coordinate}"))?;
    crate::print_json(&output)
}

/// Admits and calls one installed plugin, returning its own JSON result or
/// an error naming which bound was hit.
///
/// Admission uses the declaration retained at installation. Artifact capability
/// metadata is not a substitute for a pack author's narrower declaration.
fn run_plugin(
    bytes: &[u8],
    record: &InstalledPlugin,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    let afb =
        afterburner_cloud::Afb::from_bytes(bytes).context("this is not a readable plugin .afb")?;
    let coordinate = format!("{}/{}", record.namespace, record.name);
    let runner = PluginRunner::compile(bytes, &record.declaration)
        .with_context(|| format!("admitting plugin {coordinate}"))?;
    // The default budget for *this* artifact, not the generic one: a
    // plugin that has to boot an interpreter needs a memory ceiling its
    // runtime can instantiate under and a wall clock that covers the boot,
    // or it is admitted and then fails every call.
    let outcome = runner
        .call(input, &PluginBudget::for_artifact(&afb))
        .with_context(|| format!("calling plugin {coordinate}"))?;

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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            declaration: store::declaration_from_artifact(
                &afterburner_cloud::Afb::from_bytes(bytes).unwrap(),
            )
            .unwrap(),
        }
    }

    #[test]
    fn plugin_run_returns_the_plugins_own_output() {
        let bytes = build_echo_plugin();
        let input = serde_json::json!({"hello": "world", "n": 42});
        let output =
            run_plugin(&bytes, &sample_record(&bytes), &input).expect("the echo plugin must run");
        assert_eq!(output, input);
    }

    #[test]
    fn plugin_run_defaults_to_null_input() {
        let bytes = build_echo_plugin();
        let output =
            run_plugin(&bytes, &sample_record(&bytes), &serde_json::Value::Null).expect("must run");
        assert_eq!(output, serde_json::Value::Null);
    }

    #[test]
    fn installed_pack_retains_authored_admission_metadata() {
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
        super::super::install_from_pack(home.path(), "team", "1.0.0", &declaration, &bytes)
            .unwrap();
        let record = store::read_record(home.path(), "team", "echo").unwrap();
        assert_eq!(record.declaration, declaration);
        let runner = PluginRunner::compile(&bytes, &record.declaration).unwrap();
        assert_eq!(runner.definition(), &declaration);
        assert_eq!(
            run_plugin(&bytes, &record, &serde_json::json!({"message": "ok"})).unwrap(),
            serde_json::json!({"message": "ok"})
        );

        // An invalid authored admission declaration must fail, not get silently
        // replaced by the valid manifold embedded in the artifact.
        let mut invalid = record;
        invalid.declaration.manifold = Some(serde_json::json!({"fs": "invalid"}));
        assert!(run_plugin(&bytes, &invalid, &serde_json::Value::Null).is_err());
        let empty_home = tempfile::tempdir().unwrap();
        assert!(super::super::install_from_pack(
            empty_home.path(),
            "team",
            "1.0.0",
            &invalid.declaration,
            &bytes
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
        })
        .await
        .expect_err("an uninstalled plugin must be refused");
        let message = format!("{error:#}");
        assert!(message.contains("gents/does_not_exist"), "{message}");
        assert!(message.contains("not installed"), "{message}");
    }
}
