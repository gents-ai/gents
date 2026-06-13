use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::agent::p2p_reconcile::expand_p2p_collection_profile_ids;
use serde_json::{json, Value};

use crate::cli::args::P2pJoinArgs;
use crate::request_helpers::parse_duration_suffix;
use crate::{print_json, resolve_config_access, resolve_graphql_endpoint};

use super::invite::{current_invite_token, decode, encode, profile_ids_or_default};
use super::pairings::{peer_pairing_exists, wait_for_pairing_connected, write_pairing_desired};

pub(super) async fn p2p_join(args: P2pJoinArgs) -> Result<()> {
    let remote = decode(&args.token)?;
    let profiles = accepted_profiles(&remote.profiles, &args)?;
    let collections = expand_p2p_collection_profile_ids(
        std::iter::empty::<&str>(),
        profiles.iter().map(String::as_str),
    )
    .context("expanding accepted pairing profiles")?
    .into_iter()
    .collect::<Vec<_>>();
    let addresses = vec![remote.ticket.clone()];
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let existed = peer_pairing_exists(&access, &remote.peer_id).await?;
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let doc_id = write_pairing_desired(
        &access,
        &remote.peer_id,
        Some(&remote.did),
        &collections,
        &addresses,
        &profiles,
        &now,
    )
    .await?;

    let p2p = if args.wait {
        let timeout = parse_duration_suffix(&args.timeout)?;
        Some(
            wait_for_pairing_connected(args.home.as_deref(), &graphql, &remote.peer_id, timeout)
                .await?,
        )
    } else {
        None
    };

    let reciprocal = if existed {
        None
    } else {
        let token = current_invite_token(args.home.as_deref(), &graphql, profiles.clone()).await?;
        Some(encode(&token)?)
    };

    let mut output = json!({
        "status": if existed { "pairing_exists" } else { "pairing_joined" },
        "home": home_dir,
        "graphql": graphql,
        "access_mode": access.mode(),
        "peer_id": remote.peer_id,
        "agent_did": remote.did,
        "profiles": profiles,
        "collections": collections,
        "replicator_addresses": addresses,
        "doc_id": doc_id,
        "waited": args.wait,
    });
    if let Some(reciprocal) = reciprocal {
        output["reciprocal_token"] = Value::String(reciprocal.clone());
        output["reciprocal_join_command"] =
            Value::String(format!("defra-agent p2p pairings join {reciprocal}"));
    }
    if let Some(p2p) = p2p {
        output["p2p"] = p2p;
    }

    print_json(&output)?;
    Ok(())
}

fn accepted_profiles(offered: &[String], args: &P2pJoinArgs) -> Result<Vec<String>> {
    let offered_set = offered
        .iter()
        .map(|profile| profile.trim())
        .filter(|profile| !profile.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    if offered_set.is_empty() {
        anyhow::bail!("pairing invite did not offer any profiles");
    }

    if args.profiles.is_empty() {
        return Ok(offered_set.into_iter().collect());
    }

    let requested = profile_ids_or_default(&args.profiles)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let accepted = offered_set
        .intersection(&requested)
        .cloned()
        .collect::<Vec<_>>();
    if accepted.is_empty() {
        anyhow::bail!(
            "pairing invite does not offer any requested profiles; offered: {}; requested: {}",
            offered.join(","),
            requested.into_iter().collect::<Vec<_>>().join(",")
        );
    }
    Ok(accepted)
}
