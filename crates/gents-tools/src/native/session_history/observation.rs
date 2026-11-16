use std::collections::BTreeMap;

use anyhow::{Context, Result};
use gents_session::views::bindings::SessionRequestBindings;

use super::{clean, context_calls};

pub(super) enum RequestBindings<'a> {
    Physical(&'a [String]),
    Page(Option<&'a str>),
}

pub(super) async fn load_session_request_bindings(
    access: &dyn gents_store::config_client::ConfigRead,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    selection: RequestBindings<'_>,
) -> Result<BTreeMap<String, String>> {
    anyhow::ensure!(
        !node_did.trim().is_empty() && !session_id.trim().is_empty(),
        "session request selection requires a node and session"
    );
    let mut view = SessionRequestBindings::new(
        node_did.trim(),
        requester_did.map(str::to_owned),
        session_id.trim(),
    );
    view = match selection {
        RequestBindings::Physical(ids) => {
            if ids.is_empty() {
                return Ok(BTreeMap::new());
            }
            view.physical(ids.to_vec())
        }
        RequestBindings::Page(after) => {
            view = view.limit(context_calls::PAGE_SIZE as u64);
            if let Some(after) = after {
                view = view.after(after);
            }
            view
        }
    };
    gents_store::views::read_view(access, &view)
        .await
        .context("session accounting ownership")?
        .into_iter()
        .map(|request| {
            Ok((
                clean(Some(&request.doc_id))
                    .context("accounting request lacks physical identity")?,
                clean(Some(&request.request_id))
                    .context("accounting request lacks logical identity")?,
            ))
        })
        .collect()
}
