use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents_codex_protocol as codex;

use crate::commands::codex_shim::ShimState;

use super::{load_codex_thread, CodexThreadRecord};

pub(in crate::commands::codex_shim) async fn set_codex_thread_loaded(
    state: &ShimState,
    thread_id: &str,
    loaded: bool,
) -> Result<()> {
    if super::storage::load_scoped_session(state, thread_id)
        .await?
        .is_none()
        && load_codex_thread(state, thread_id).await?.is_none()
    {
        return Ok(());
    }
    state.set_thread_loaded(thread_id, loaded).await;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_archived(
    state: &ShimState,
    thread_id: &str,
    archived: bool,
) -> Result<bool> {
    if load_codex_thread(state, thread_id).await?.is_none() {
        return Ok(false);
    }
    state.set_thread_archived(thread_id, archived).await;
    Ok(true)
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_name(
    state: &ShimState,
    thread_id: &str,
    name: &str,
) -> Result<bool> {
    let has_session = super::storage::load_scoped_session(state, thread_id)
        .await?
        .is_some();
    if !has_session && load_codex_thread(state, thread_id).await?.is_none() {
        return Ok(false);
    }

    let name = name.trim();
    if !has_session {
        state.set_thread_name(thread_id, name).await;
        return Ok(true);
    }

    let now = chrono::Utc::now().to_rfc3339();
    ConfigAccess::transact_local(&state.node, None, "codex.thread.set_name", |txn| {
        let now = now.clone();
        Box::pin(async move {
            let row = gents::session::load_agent_session_row_in_txn(
                txn,
                &state.agent_did,
                thread_id,
                Some(state.local_requester_did()),
            )
            .await?
            .context("thread session vanished during rename")?;
            anyhow::ensure!(
                row.session.behavior_id == state.behavior_id.as_ref(),
                "thread behavior changed during rename"
            );
            gents::session::apply_title_in_txn(
                txn,
                &state.agent_did,
                Some(state.local_requester_did()),
                thread_id,
                (!name.is_empty()).then_some(name),
                gents_protocol::session::SessionTitleSource::User,
                &now,
            )
            .await
        })
    })
    .await?;
    state.set_thread_name(thread_id, name).await;
    Ok(true)
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_memory_mode(
    state: &ShimState,
    thread_id: &str,
    mode: codex::ThreadMemoryMode,
) -> Result<()> {
    if load_codex_thread(state, thread_id).await?.is_none() {
        return Ok(());
    }
    state.set_thread_memory_mode(thread_id, mode.as_str()).await;
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_settings(
    state: &ShimState,
    thread_id: &str,
    settings: &codex::ThreadSettingsUpdateParams,
) -> Result<()> {
    if load_codex_thread(state, thread_id).await?.is_none() {
        return Ok(());
    }
    let settings_json =
        serde_json::to_string(settings).context("encoding Codex thread settings")?;
    state.set_thread_settings(thread_id, &settings_json).await;
    if let Some(cwd) = settings.cwd.as_deref() {
        let cwd = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            state.cwd.join(cwd)
        };
        state.set_thread_cwd(thread_id, cwd).await;
    }
    Ok(())
}

pub(in crate::commands::codex_shim) async fn set_codex_thread_git_info(
    state: &ShimState,
    thread_id: &str,
    _git_info: &Option<codex::ThreadMetadataGitInfoUpdateParams>,
) -> Result<Option<CodexThreadRecord>> {
    load_codex_thread(state, thread_id).await
}
