use std::collections::BTreeMap;

use anyhow::{Context, Result};
use gents_session::views::bindings::SessionRequestBindings;

use super::{clean, context_calls};

pub(super) enum RequestBindings<'a> {
    Physical(&'a [String]),
    Page(Option<&'a str>),
}

/// The scoped requests' physical to logical identities. A page also returns
/// the cursor token to resume from while more requests remain.
pub(super) async fn load_session_request_bindings(
    access: &dyn gents_store::config_client::ConfigRead,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    selection: RequestBindings<'_>,
) -> Result<(BTreeMap<String, String>, Option<String>)> {
    anyhow::ensure!(
        !node_did.trim().is_empty() && !session_id.trim().is_empty(),
        "session request selection requires a node and session"
    );
    let view = SessionRequestBindings::new(
        node_did.trim(),
        requester_did.map(str::to_owned),
        session_id.trim(),
    );
    let (rows, next) = match selection {
        RequestBindings::Physical(ids) => {
            if ids.is_empty() {
                return Ok((BTreeMap::new(), None));
            }
            let rows = gents_store::views::read_view(access, &view.physical(ids.to_vec()))
                .await
                .context("session accounting ownership")?;
            (rows, None)
        }
        RequestBindings::Page(after) => gents_store::views::read_view_page(
            access,
            &view,
            context_calls::PAGE_SIZE as u64,
            after,
        )
        .await
        .context("session accounting ownership")?,
    };
    let bindings = rows
        .into_iter()
        .map(|request| {
            Ok((
                clean(Some(&request.doc_id))
                    .context("accounting request lacks physical identity")?,
                clean(Some(&request.request_id))
                    .context("accounting request lacks logical identity")?,
            ))
        })
        .collect::<Result<_>>()?;
    Ok((bindings, next))
}
