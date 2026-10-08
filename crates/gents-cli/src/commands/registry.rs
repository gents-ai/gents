//! `gents registry`: the registries a home knows. The master registry is
//! an ordinary pack registry that also serves `/api/v1/registries`, its
//! index of the vertical registries it vouches for; a vertical registry is
//! another instance of the same server at its own URL. The index is cached
//! in `<home>/registry/index.json` so `--registry <id>` resolves offline,
//! and a registry added by hand sits beside the listed ones as `user` tier.
//! Every operation here is also what the desktop's Packs panel calls
//! (`gents_server::packs::registry_*`).

use std::path::Path;

use anyhow::Result;
use gents::pack_registry::{index, RegistryClient};
use serde_json::{json, Value};

use crate::cli::args::{
    RegistryAddArgs, RegistryCommand, RegistryListArgs, RegistryRefreshArgs, RegistryRemoveArgs,
};

pub(crate) async fn dispatch(command: RegistryCommand) -> Result<()> {
    let report = match command {
        RegistryCommand::List(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            list(&home)?
        }
        RegistryCommand::Refresh(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            refresh(&home, args.master.as_deref()).await?
        }
        RegistryCommand::Add(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            add(&home, &args.id, &args.url, args.label.as_deref())?
        }
        RegistryCommand::Remove(args) => {
            let home = crate::home_state::resolve_home_dir(args.home.as_deref());
            remove(&home, &args.id)?
        }
    };
    crate::print_json(&report)
}

pub(crate) fn list(home: &Path) -> Result<Value> {
    let index = index::read(home)?;
    Ok(serde_json::to_value(index)?)
}

/// Fetches the master's index and merges it over the cache, keeping every
/// registry the operator added by hand. The master is named explicitly or
/// resolved like any other registry (`GENTS_REGISTRY`, the default).
pub(crate) async fn refresh(home: &Path, master: Option<&str>) -> Result<Value> {
    let master_url = gents::pack_registry::resolve_registry_url_in(home, master)?;
    let fetched = RegistryClient::new(master_url.clone()).registries().await?;
    let current = index::read(home)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let next = index::merge_refresh(&current, &master_url, fetched, &now);
    index::write(home, &next)?;
    Ok(
        json!({ "master": master_url, "registries": next.registries, "refreshed_at": next.refreshed_at }),
    )
}

pub(crate) fn add(home: &Path, id: &str, url: &str, label: Option<&str>) -> Result<Value> {
    let mut current = index::read(home)?;
    index::add_user(&mut current, id, url, label)?;
    index::write(home, &current)?;
    Ok(json!({
        "added": id,
        "url": url.trim_end_matches('/'),
        "listed": false,
        "note": "this registry is not listed by the master; packs from it are allowed and shown as unlisted",
    }))
}

pub(crate) fn remove(home: &Path, id: &str) -> Result<Value> {
    let mut current = index::read(home)?;
    index::remove_user(&mut current, id)?;
    index::write(home, &current)?;
    Ok(json!({ "removed": id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_list_remove_round_trip_through_a_home() {
        let home = tempfile::tempdir().unwrap();
        add(home.path(), "legal", "https://legal.example", Some("Legal")).unwrap();
        let listed = list(home.path()).unwrap();
        let ids: Vec<&str> = listed["registries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![index::MASTER_ID, "legal"]);
        assert_eq!(listed["registries"][1]["tier"], "user");
        remove(home.path(), "legal").unwrap();
        assert_eq!(
            list(home.path()).unwrap()["registries"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(remove(home.path(), index::MASTER_ID).is_err());
    }

    #[tokio::test]
    async fn refresh_against_an_unreachable_master_leaves_the_cache_alone() {
        let home = tempfile::tempdir().unwrap();
        add(home.path(), "mine", "https://mine.example", None).unwrap();
        let error = refresh(home.path(), Some("http://127.0.0.1:1"))
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("could not reach"),
            "{error:#}"
        );
        assert!(list(home.path()).unwrap()["registries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "mine"));
    }
}
