use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::codec::LengthDelimitedCodec;
use tokio_util::sync::CancellationToken;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::types::ClientUpdateEvent;

use super::protocol::{
    ClientCommand, ClientMessage, HostMessage, MAX_FRAME_BYTES, MAX_PENDING_CALLS, PROTOCOL_VERSION,
};
use super::ClientHost;

pub(super) async fn serve<S>(
    host: Arc<ClientHost>,
    stream: S,
    stop: CancellationToken,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut frames = LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME_BYTES)
        .new_framed(stream);
    let hello = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = stop.cancelled() => bail!("client attachment stopped before initialization"),
            frame = frames.next() => {
                let bytes = frame.context("client attachment closed before initialization")??;
                serde_json::from_slice::<ClientMessage>(&bytes).context("invalid client initialization")
            }
        }
    }).await.context("client initialization timed out")??;
    if !matches!(
        hello,
        ClientMessage::Hello {
            version: PROTOCOL_VERSION
        }
    ) {
        bail!("unsupported client protocol; expected version {PROTOCOL_VERSION}");
    }

    let _observation = host.core.observe_all_nodes()?;

    // Subscribe before acknowledging initialization: a first snapshot can then
    // overlap updates without leaving a snapshot-to-live observation gap.
    let mut store = host.core.store_change_updates();
    let mut health = host.core.sync_state_updates();
    frames
        .send(pack(HostMessage::Hello {
            version: PROTOCOL_VERSION,
            commands: ClientCommand::NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            max_frame_bytes: MAX_FRAME_BYTES,
        })?)
        .await?;

    let (mut sink, mut source) = frames.split();
    let local_stop = stop.child_token();
    let writer_stop = local_stop.clone();
    let (out, mut replies) = mpsc::channel::<HostMessage>(MAX_PENDING_CALLS);
    let writer = tokio::spawn(async move {
        let result: Result<()> = async {
            while let Some(message) = replies.recv().await {
                tokio::time::timeout(Duration::from_secs(30), sink.send(pack(message)?))
                    .await
                    .context("client stopped reading")??;
            }
            Ok(())
        }
        .await;
        writer_stop.cancel();
        result
    });
    let mut calls = JoinSet::new();
    let mut pending = RapidHashMap::<u32, CancellationToken>::new();
    let mut sequence = 0_u64;
    let approvals = Arc::new(super::approvals::AttachmentApprovals::default());
    let mut approval_changes = approvals.changes();
    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = local_stop.cancelled() => break,
                frame = source.next() => {
                    let Some(frame) = frame else { break };
                    let message = serde_json::from_slice::<ClientMessage>(&frame?)
                        .context("invalid client message")?;
                    match message {
                        ClientMessage::Hello { .. } => bail!("client is already initialized"),
                        ClientMessage::Cancel { id } => {
                            if let Some(cancel) = pending.get(&id) { cancel.cancel(); }
                        }
                        ClientMessage::Call { id, command } => {
                            if pending.contains_key(&id) { bail!("duplicate pending request id {id}"); }
                            if pending.len() >= MAX_PENDING_CALLS {
                                publish(&out, &local_stop, HostMessage::response(id, Err(
                                    BridgeError::new(BridgeErrorCode::Backend, "too many pending client operations"),
                                ))).await?;
                                continue;
                            }
                            let command = match serde_json::from_value::<ClientCommand>(command) {
                                Ok(command) => command,
                                Err(error) => {
                                    publish(&out, &local_stop, HostMessage::response(id, Err(
                                        BridgeError::new(BridgeErrorCode::InvalidArgument, error.to_string()),
                                    ))).await?;
                                    continue;
                                }
                            };
                            let cancel = CancellationToken::new();
                            pending.insert(id, cancel.clone());
                            let host = host.clone();
                            let approvals = approvals.clone();
                            calls.spawn(async move {
                                let cancellable = command.is_cancellable();
                                let result = run_operation(cancellable, cancel.clone(), host.execute(command, &approvals, cancel)).await;
                                (id, result)
                            });
                        }
                    }
                }
                completed = calls.join_next(), if !calls.is_empty() => {
                    let (id, result) = completed.context("client operation disappeared")?
                        .context("client operation panicked")?;
                    pending.remove(&id);
                    publish(&out, &local_stop, HostMessage::response(id, result)).await?;
                }
                changed = store.changed() => {
                    if changed.is_err() { break; }
                    sequence = sequence.checked_add(1).context("client notification sequence exhausted")?;
                    let payload = ClientUpdateEvent::store(store.borrow_and_update().clone());
                    publish(&out, &local_stop, HostMessage::Updated { sequence, payload }).await?;
                }
                changed = health.changed() => {
                    if changed.is_err() { break; }
                    health.borrow_and_update();
                    sequence = sequence.checked_add(1).context("client notification sequence exhausted")?;
                    publish(&out, &local_stop, HostMessage::Updated {
                        sequence, payload: ClientUpdateEvent::coarse("health"),
                    }).await?;
                }
                changed = approval_changes.changed() => {
                    if changed.is_err() { break; }
                    approval_changes.borrow_and_update();
                    sequence = sequence.checked_add(1).context("client notification sequence exhausted")?;
                    publish(&out, &local_stop, HostMessage::Updated {
                        sequence, payload: ClientUpdateEvent::coarse("approvals"),
                    }).await?;
                }
            }
        }
        Ok(())
    }.await;

    approvals.close();
    for cancel in pending.values() {
        cancel.cancel();
    }
    let mut drain_error = None;
    while let Some(completed) = calls.join_next().await {
        match completed {
            Ok((id, Err(error))) => tracing::debug!(
                id,
                code = error.code.as_str(),
                "detached client operation failed"
            ),
            Err(error) => {
                drain_error.get_or_insert(error);
            }
            _ => {}
        }
    }
    drop(out);
    writer.abort();
    match writer.await {
        Ok(result) => result?,
        Err(error) if error.is_cancelled() => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(error) = drain_error {
        return Err(error.into());
    }
    result
}

async fn publish(
    out: &mpsc::Sender<HostMessage>,
    stop: &CancellationToken,
    message: HostMessage,
) -> Result<()> {
    tokio::select! {
        _ = stop.cancelled() => bail!("client attachment stopped"),
        sent = out.send(message) => sent.context("client output closed"),
    }
}

fn pack(message: HostMessage) -> Result<Bytes> {
    let bytes = serde_json::to_vec(&message)?;
    if bytes.len() <= MAX_FRAME_BYTES {
        return Ok(bytes.into());
    }
    match message {
        HostMessage::Response { id, .. } => Ok(serde_json::to_vec(&HostMessage::response(
            id,
            Err(BridgeError::new(
                BridgeErrorCode::Backend,
                "client result exceeds the transport limit; request a smaller history page",
            )),
        ))?
        .into()),
        _ => bail!("client notification exceeds the transport limit"),
    }
}

fn cancelled() -> BridgeError {
    BridgeError::new(
        BridgeErrorCode::Unknown,
        "client wait cancelled; a submitted write may still complete",
    )
}

async fn run_operation<F>(
    cancellable: bool,
    cancel: CancellationToken,
    work: F,
) -> Result<serde_json::Value, BridgeError>
where
    F: std::future::Future<Output = Result<serde_json::Value, BridgeError>>,
{
    if cancellable {
        tokio::select! {
            _ = cancel.cancelled() => Err(cancelled()),
            result = work => result,
        }
    } else {
        // Transport cancellation ends the wait, not ownership of a submitted write.
        let result = work.await;
        if cancel.is_cancelled() {
            Err(cancelled())
        } else {
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancelling_a_mutating_operation_retains_its_future_until_it_settles() {
        let cancel = CancellationToken::new();
        let release = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let work_release = release.clone();
        let work = async move {
            let _guard = guard;
            work_release.notified().await;
            Ok(serde_json::Value::Null)
        };
        let operation = run_operation(false, cancel.clone(), work);
        tokio::pin!(operation);
        assert!(futures::poll!(&mut operation).is_pending());
        cancel.cancel();
        assert!(futures::poll!(&mut operation).is_pending());
        assert!(!dropped.load(Ordering::SeqCst));
        release.notify_one();
        assert!(operation.await.is_err());
        assert!(dropped.load(Ordering::SeqCst));
    }
}
