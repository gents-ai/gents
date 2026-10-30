use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn accept_loop(
    listener: UnixListener,
    lock: LeaderLock,
    socket_path: PathBuf,
    factory: Arc<dyn AcpDelegateFactory>,
    lifecycle: broadcast::Sender<ServerEnvelope>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let next_client_id = AtomicU64::new(1);
    let mut connections = JoinSet::new();

    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                // `Err` means the handle is gone, which is an implicit stop.
                let _ = changed;
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _address)) => {
                        let client_id = next_client_id.fetch_add(1, Ordering::Relaxed);
                        let lifecycle_rx = lifecycle.subscribe();
                        connections.spawn(handle_connection(
                            stream,
                            client_id,
                            factory.clone(),
                            lifecycle_rx,
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            %error,
                            "grok shim leader failed to accept a pager connection"
                        );
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    let _ = lifecycle.send(ServerEnvelope::ShuttingDown {
        reason: ShutdownReason::Manual,
        delay_ms: WIRE_SHUTDOWN_DELAY_MS,
    });
    let _ = lifecycle.send(ServerEnvelope::Shutdown);
    drop(lifecycle);

    let drain = async { while connections.join_next().await.is_some() {} };
    let cleanup_result = if tokio::time::timeout(CONNECTION_CLEANUP_GRACE, drain)
        .await
        .is_err()
    {
        tracing::warn!(
            target: LOG_TARGET,
            grace_ms = CONNECTION_CLEANUP_GRACE.as_millis(),
            "grok shim leader connection cleanup grace elapsed"
        );
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Err(anyhow!(
            "grok shim leader connection cleanup exceeded {}ms",
            CONNECTION_CLEANUP_GRACE.as_millis()
        ))
    } else {
        Ok(())
    };

    drop(listener);

    remove_published_socket(&socket_path);
    lock.release();
    tracing::info!(
        target: LOG_TARGET,
        socket = %socket_path.display(),
        "grok shim leader stopped cleanly"
    );
    cleanup_result
}

fn remove_published_socket(socket_path: &Path) {
    match std::fs::remove_file(socket_path) {
        Ok(()) => {
            tracing::debug!(
                target: LOG_TARGET,
                socket = %socket_path.display(),
                "removed the published grok shim socket on clean stop"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                socket = %socket_path.display(),
                "failed to remove the published grok shim socket on clean stop"
            );
        }
    }
}

async fn handle_connection(
    stream: UnixStream,
    client_id: u64,
    factory: Arc<dyn AcpDelegateFactory>,
    mut lifecycle: broadcast::Receiver<ServerEnvelope>,
) {
    let (mut reader, writer) = stream.into_split();
    let (frames_tx, mut frames_rx) = mpsc::channel::<ServerEnvelope>(OUTBOUND_FRAME_CAPACITY);
    let frames_tx = FrameSender::Bounded(frames_tx);
    let (client_frames_tx, mut client_frames_rx) =
        mpsc::channel::<std::result::Result<ClientEnvelope, ProtocolError>>(INBOUND_FRAME_CAPACITY);
    let reader_task = tokio::spawn(async move {
        loop {
            let frame = read_client_envelope(&mut reader).await;
            let terminal = frame.is_err();
            if client_frames_tx.send(frame).await.is_err() || terminal {
                break;
            }
        }
    });
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(envelope) = frames_rx.recv().await {
            if let Err(error) = write_server_envelope(&mut writer, &envelope).await {
                tracing::debug!(
                    target: LOG_TARGET,
                    %error,
                    "grok shim leader stopped writing to a pager connection"
                );
                break;
            }
        }
    });

    let registration =
        match register_client(&mut client_frames_rx, &frames_tx, &mut lifecycle).await {
            Ok(registration) => registration,
            Err(()) => {
                drop(frames_tx);
                reader_task.abort();
                let _ = reader_task.await;
                let _ = tokio::time::timeout(MAX_SHUTDOWN_GRACE, writer_task).await;
                return;
            }
        };

    let delegate: Arc<dyn AcpDelegate> =
        match factory.create_delegate(client_id, &registration).await {
            Ok(delegate) => delegate,
            Err(error) => {
                tracing::error!(
                    target: LOG_TARGET,
                    client_id,
                    %error,
                    "grok shim leader could not construct an ACP delegate for a connection"
                );
                drop(frames_tx);
                reader_task.abort();
                let _ = reader_task.await;
                let _ = tokio::time::timeout(MAX_SHUTDOWN_GRACE, writer_task).await;
                return;
            }
        };

    let _ = frames_tx.try_send(ServerEnvelope::Registered {
        client_id,
        ready: true,
        leader_protocol_version: LEADER_PROTOCOL_VERSION,
        leader_binary_version: leader_binary_version(),
        leader_capabilities: leader_capabilities(),
    });
    tracing::info!(
        target: LOG_TARGET,
        client_id,
        client_type = %registration.client_type,
        mode = registration.mode.wire_name(),
        yolo_mode = registration.capabilities.yolo_mode,
        auto_mode = registration.capabilities.auto_mode,
        terminal = registration.capabilities.terminal,
        "grok shim leader registered a pager client"
    );

    let outbound = AcpOutbound {
        frames: frames_tx.clone(),
    };
    let mut acp_tasks: JoinSet<()> = JoinSet::new();
    loop {
        reap_acp_dispatches(&mut acp_tasks);
        tokio::select! {
            biased;
            lifecycle_frame = lifecycle.recv() => {
                match lifecycle_frame {
                    Ok(envelope) => {
                        if frames_tx.try_send(envelope).is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            client_id,
                            missed,
                            "grok shim leader connection missed lifecycle frames; closing it"
                        );
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            frame = client_frames_rx.recv() => {
                match frame {
                    Some(Ok(ClientEnvelope::Ping)) => {
                        if frames_tx.try_send(ServerEnvelope::Pong).is_err() {
                            break;
                        }
                    }
                    Some(Ok(ClientEnvelope::Acp { payload })) => {
                        if !acp_dispatch_has_capacity(&acp_tasks) {
                            tracing::warn!(
                                target: LOG_TARGET,
                                client_id,
                                active = acp_tasks.len(),
                                "grok shim leader rejected ACP dispatch at the per-connection limit"
                            );
                            if let Some(response) = internal_error_response(&payload) {
                                if outbound.try_send(response).is_err() {
                                    break;
                                }
                            } else {
                                // Notifications have no response channel; closing the
                                // connection invokes `on_disconnect`, which cancels
                                // every pending turn.
                                break;
                            }
                        } else {
                            spawn_acp_dispatch(
                                &mut acp_tasks,
                                delegate.clone(),
                                outbound.clone(),
                                payload,
                            );
                        }
                    }
                    Some(Ok(ClientEnvelope::Control { request_id, command })) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            client_id,
                            request_id = %request_id,
                            command = %command,
                            "grok shim leader received an unsupported control command"
                        );
                        if frames_tx.try_send(ServerEnvelope::Error {
                            code: ENVELOPE_ERROR_METHOD_NOT_FOUND,
                            message: format!(
                                "the Gents leader shim does not implement control commands \
                                 (request {request_id:?}); leader_capabilities.control_v1 is false"
                            ),
                        }).is_err() {
                            break;
                        }
                    }
                    Some(Ok(ClientEnvelope::Register { .. })) => {
                        protocol_violation(
                            &frames_tx,
                            "register is only valid as the first frame on a connection",
                        );
                        break;
                    }
                    Some(Ok(ClientEnvelope::Disconnect)) => {
                        tracing::debug!(
                            target: LOG_TARGET,
                            client_id,
                            "grok shim leader pager client disconnected"
                        );
                        break;
                    }
                    Some(Err(error)) if error.is_connection_closed() => break,
                    Some(Err(error)) => {
                        tracing::debug!(
                            target: LOG_TARGET,
                            client_id,
                            %error,
                            "grok shim leader received an undecodable frame"
                        );
                        protocol_violation(
                            &frames_tx,
                            &format!("undecodable leader frame: {error}"),
                        );
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    // Connection cleanup: drain the delegate's state first so in-flight
    // handlers observe the drained turn table, then flush queued frames.
    delegate.on_disconnect().await;
    let drain = async { while acp_tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(MAX_SHUTDOWN_GRACE, drain)
        .await
        .is_err()
    {
        tracing::warn!(
            target: LOG_TARGET,
            client_id,
            "grok shim leader closed a connection with ACP dispatch still running"
        );
        acp_tasks.abort_all();
        while acp_tasks.join_next().await.is_some() {}
    }
    drop(outbound);
    drop(frames_tx);
    reader_task.abort();
    let _ = reader_task.await;
    let _ = tokio::time::timeout(MAX_SHUTDOWN_GRACE, writer_task).await;
}

/// Read frames until a valid `register` arrives. `Err(())` means the
/// connection must close; the error envelope (when the protocol requires one)
/// has already been queued.
async fn register_client(
    client_frames: &mut mpsc::Receiver<std::result::Result<ClientEnvelope, ProtocolError>>,
    frames: &FrameSender,
    lifecycle: &mut broadcast::Receiver<ServerEnvelope>,
) -> std::result::Result<Registration, ()> {
    let frame = tokio::select! {
        lifecycle_frame = lifecycle.recv() => {
            let _ = lifecycle_frame;
            // The leader is stopping before this client registered.
            return Err(());
        }
        frame = client_frames.recv() => match frame {
            Some(frame) => frame,
            None => return Err(()),
        },
    };
    match frame {
        Ok(ClientEnvelope::Register {
            client_type,
            mode,
            capabilities,
        }) => {
            if let Err(reason) = validate_register(&client_type, &mode) {
                tracing::warn!(
                    target: LOG_TARGET,
                    client_type = %client_type,
                    mode = %mode.wire_name(),
                    reason = %format!("{reason:#}"),
                    "grok shim leader rejected an invalid register frame"
                );
                let _ = frames.try_send(ServerEnvelope::Error {
                    code: ENVELOPE_ERROR_INVALID_REQUEST,
                    message: format!("{reason:#}"),
                });
                return Err(());
            }
            Ok(Registration {
                client_type,
                mode,
                capabilities,
            })
        }
        Ok(_) => {
            protocol_violation(
                frames,
                "the first frame on a leader connection must be register",
            );
            Err(())
        }
        Err(error) if error.is_connection_closed() => {
            tracing::debug!(
                target: LOG_TARGET,
                "grok shim leader connection closed before register"
            );
            Err(())
        }
        Err(error) => {
            protocol_violation(
                frames,
                &format!("undecodable leader frame before register: {error}"),
            );
            Err(())
        }
    }
}

fn validate_register(client_type: &str, _mode: &RegisterMode) -> Result<()> {
    if client_type.trim().is_empty() {
        bail!("register requires a non-empty client_type");
    }
    Ok(())
}

fn protocol_violation(frames: &FrameSender, message: &str) {
    tracing::warn!(target: LOG_TARGET, message, "grok shim leader protocol violation");
    let _ = frames.try_send(ServerEnvelope::Error {
        code: ENVELOPE_ERROR_INVALID_REQUEST,
        message: message.to_string(),
    });
}

fn reap_acp_dispatches(tasks: &mut JoinSet<()>) {
    while let Some(result) = tasks.try_join_next() {
        if let Err(error) = result {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                "grok shim leader ACP dispatch task failed to join"
            );
        }
    }
}

fn acp_dispatch_has_capacity(tasks: &JoinSet<()>) -> bool {
    tasks.len() < MAX_ACP_DISPATCH_TASKS
}

fn spawn_acp_dispatch(
    tasks: &mut JoinSet<()>,
    delegate: Arc<dyn AcpDelegate>,
    outbound: AcpOutbound,
    payload: String,
) {
    tasks.spawn(async move {
        if let Err(error) = delegate.handle_acp(&payload, outbound.clone()).await {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                "grok shim leader ACP dispatch failed"
            );
            // A request must never hang: answer it with a JSON-RPC internal
            // error through the ACP channel so the pager can recover.
            if let Some(response) = internal_error_response(&payload) {
                if let Err(error) = outbound.send(response).await {
                    tracing::debug!(
                        target: LOG_TARGET,
                        %error,
                        "grok shim leader could not deliver an ACP failure response"
                    );
                }
            }
        }
    });
}

fn internal_error_response(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let id = value.get("id")?;
    // Only requests are answered; notifications expect no response.
    value.get("method")?;
    Some(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": JSONRPC_INTERNAL_ERROR,
                "message": "Gents leader shim ACP dispatch failed",
            },
        })
        .to_string(),
    )
}

fn leader_binary_version() -> String {
    format!("gents-{}", env!("CARGO_PKG_VERSION"))
}

fn leader_capabilities() -> LeaderCapabilities {
    LeaderCapabilities {
        control_v1: false,
        runtime_cpu_profile: false,
        profile_formats: Vec::new(),
        workspace_exposure: false,
        relaunch_v1: false,
    }
}
