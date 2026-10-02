//! `gents plugin bind` and `unbind`: choose, and later change, the inference
//! profile an installed plugin calls a model through. A plugin with its model
//! slot unbound runs without model calls.

use anyhow::{Context, Result};
use gents::plugin::install::set_model_binding;
use gents::plugin::model_calls::{AccessModels, ModelBinding, ModelResolver};
use serde_json::json;

use super::store;
use crate::cli::args::{PluginBindArgs, PluginUnbindArgs};

pub(super) async fn bind(args: PluginBindArgs) -> Result<()> {
    anyhow::ensure!(
        args.scope.graphql.is_none(),
        "plugin model bindings run on the local host; bind there with --home instead of --graphql"
    );
    let (namespace, name) = crate::commands::pack::split_namespace(&args.name);
    let coordinate = format!("{namespace}/{name}");
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let record = store::read_record(&home, namespace, name)
        .with_context(|| format!("{coordinate} is not installed under {}", home.display()))?;
    anyhow::ensure!(
        record.declaration.model_slot.is_some(),
        "{coordinate} does not call a model, so it has no slot to bind"
    );
    let (access, owner) = crate::commands::pack::resolve_scope_owner(&args.scope).await?;
    let binding = ModelBinding {
        agent_did: owner,
        profile_id: args.profile,
    };
    AccessModels(access)
        .resolve(&binding)
        .await
        .with_context(|| format!("profile {:?} cannot serve {coordinate}", binding.profile_id))?;
    let record = set_model_binding(&home, &coordinate, Some(binding))?;
    crate::print_json(&json!({ "bound": record.model_binding, "plugin": coordinate }))
}

pub(super) fn unbind(args: PluginUnbindArgs) -> Result<()> {
    let (namespace, name) = crate::commands::pack::split_namespace(&args.name);
    let coordinate = format!("{namespace}/{name}");
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    set_model_binding(&home, &coordinate, None)?;
    crate::print_json(&json!({ "bound": null, "plugin": coordinate }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn install_record(home: &std::path::Path, model_slot: Option<&str>, bound: bool) {
        let record: store::InstalledPlugin = serde_json::from_value(json!({
            "namespace": "team", "name": "ocr", "version": "1.0.0",
            "digest": format!("sha256:{}", "0".repeat(64)), "language": "rust",
            "declaration": {
                "name": "ocr", "description": "reads pages", "artifact": "plugins/ocr.afb",
                "language": "rust", "input_schema": {"type": "object"},
                "model_slot": model_slot,
            },
            "model_binding": bound.then(|| json!({"agent_did": "did:key:o", "profile_id": "chandra"})),
        }))
        .unwrap();
        store::write_record(home, &record).unwrap();
    }

    fn unbind_args(home: &std::path::Path) -> PluginUnbindArgs {
        PluginUnbindArgs {
            name: "team/ocr".to_owned(),
            home: Some(home.to_owned()),
        }
    }

    #[test]
    fn unbinding_leaves_the_slot_unbound_and_keeps_the_plugin_installed() {
        let home = tempfile::tempdir().unwrap();
        install_record(home.path(), Some("remote_ocr"), true);
        unbind(unbind_args(home.path())).unwrap();
        let record = store::read_record(home.path(), "team", "ocr").unwrap();
        assert!(record.model_binding.is_none());
        assert_eq!(record.declaration.model_slot.as_deref(), Some("remote_ocr"));
    }

    #[tokio::test]
    async fn remote_binding_is_rejected_before_reading_local_records() {
        let home = tempfile::tempdir().unwrap();
        let error = bind(PluginBindArgs {
            name: "team/ocr".to_owned(),
            profile: "chandra".to_owned(),
            scope: crate::cli::args::GraphScopeArgs {
                home: Some(home.path().to_owned()),
                graphql: Some("http://127.0.0.1:1/graphql".to_owned()),
                agent_did: None,
            },
        })
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("local host"), "{error:#}");
    }

    #[tokio::test]
    async fn a_plugin_without_a_model_slot_has_nothing_to_bind_or_unbind() {
        let home = tempfile::tempdir().unwrap();
        install_record(home.path(), None, false);
        let error = unbind(unbind_args(home.path())).unwrap_err();
        assert!(
            format!("{error:#}").contains("no slot to bind"),
            "{error:#}"
        );
        let error = bind(PluginBindArgs {
            name: "team/ocr".to_owned(),
            profile: "chandra".to_owned(),
            scope: crate::cli::args::GraphScopeArgs {
                home: Some(home.path().to_owned()),
                graphql: None,
                agent_did: None,
            },
        })
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("no slot to bind"),
            "{error:#}"
        );
        assert!(unbind(PluginUnbindArgs {
            name: "team/missing".to_owned(),
            home: Some(home.path().to_owned()),
        })
        .is_err());
    }
}
