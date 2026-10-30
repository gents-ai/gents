use super::*;

#[tokio::test]
async fn register_must_precede_registered_and_validate() {
    let root = unique_test_root("register");
    let socket = root.join("leader.sock");
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
        .expect("the leader should spawn");

    {
        let (mut reader, mut writer) = connect(&socket).await;
        write_client_frame(&mut writer, &ClientEnvelope::Ping).await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Error { code, .. }) => {
                assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
            }
            other => panic!("expected an error envelope, got {other:?}"),
        }
        assert!(
            next_server_frame(&mut reader).await.is_none(),
            "the leader must close after a protocol violation"
        );
    }
    {
        let (mut reader, mut writer) = connect(&socket).await;
        write_client_frame(
            &mut writer,
            &ClientEnvelope::Acp {
                payload: json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}).to_string(),
            },
        )
        .await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Error { code, .. }) => {
                assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
            }
            other => panic!("expected an error envelope, got {other:?}"),
        }
        assert!(next_server_frame(&mut reader).await.is_none());
    }
    {
        let (mut reader, mut writer) = connect(&socket).await;
        write_client_frame(
            &mut writer,
            &ClientEnvelope::Register {
                client_type: "grok-pager".to_string(),
                mode: RegisterMode::Headless,
                capabilities: test_capabilities(),
            },
        )
        .await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Registered { .. }) => {}
            other => panic!("expected registered, got {other:?}"),
        }
    }
    {
        let (mut reader, mut writer) = connect(&socket).await;
        write_client_frame(
            &mut writer,
            &ClientEnvelope::Register {
                client_type: "   ".to_string(),
                mode: RegisterMode::Stdio,
                capabilities: test_capabilities(),
            },
        )
        .await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Error { code, .. }) => {
                assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
            }
            other => panic!("expected an error envelope, got {other:?}"),
        }
        assert!(next_server_frame(&mut reader).await.is_none());
    }
    {
        let (reader, writer, _client_id) = register(&socket).await;
        drop((reader, writer));
    }

    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_second_register_is_rejected() {
    let root = unique_test_root("reregister");
    let socket = root.join("leader.sock");
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
        .expect("the leader should spawn");
    let (mut reader, mut writer, _client_id) = register(&socket).await;
    write_client_frame(&mut writer, &register_envelope()).await;
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Error { code, .. }) => {
            assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
        }
        other => panic!("expected an error envelope, got {other:?}"),
    }
    assert!(
        next_server_frame(&mut reader).await.is_none(),
        "the leader must close after a second register"
    );
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn registration_captures_client_capabilities() {
    let root = unique_test_root("caps");
    let socket = root.join("leader.sock");
    let factory = recording_factory();
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the leader should spawn");
    let (mut reader, mut writer, _client_id) = register(&socket).await;
    write_client_frame(&mut writer, &ClientEnvelope::Ping).await;
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Pong) => {}
        other => panic!("expected pong, got {other:?}"),
    }
    let constructed = factory.constructed();
    let delegate = constructed
        .first()
        .expect("registration must have constructed a delegate");
    assert_eq!(
        delegate
            .capabilities
            .lock()
            .expect("capability log")
            .clone(),
        Some((true, false, false, Some("grok-pager-test".to_string()))),
        "yolo_mode/auto_mode/terminal and the client version must reach the \
         per-connection delegate at construction time"
    );
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn control_commands_answer_method_not_found() {
    let root = unique_test_root("control");
    let socket = root.join("leader.sock");
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
        .expect("the leader should spawn");
    let (mut reader, mut writer, _client_id) = register(&socket).await;
    write_client_frame(
        &mut writer,
        &ClientEnvelope::Control {
            request_id: "req-1".to_string(),
            command: json!({"type": "relaunch"}),
        },
    )
    .await;
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Error { code, message }) => {
            assert_eq!(code, ENVELOPE_ERROR_METHOD_NOT_FOUND);
            assert!(
                message.contains("req-1"),
                "the error should name the control request"
            );
        }
        other => panic!("expected an error envelope, got {other:?}"),
    }
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn deferred_acp_pushes_arrive_after_dispatch_returns() {
    let root = unique_test_root("deferred");
    let socket = root.join("leader.sock");
    let factory = Arc::new(RecordingFactory {
        late_push: true,
        ..Default::default()
    });
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the leader should spawn");
    let (mut reader, mut writer, _client_id) = register(&socket).await;
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": "s"},
    })
    .to_string();
    write_client_frame(
        &mut writer,
        &ClientEnvelope::Acp {
            payload: payload.clone(),
        },
    )
    .await;
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Acp { payload: echoed }) => assert_eq!(echoed, payload),
        other => panic!("expected an acp echo, got {other:?}"),
    }
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Acp { payload }) => {
            assert!(payload.contains("deferred"), "got {payload}");
        }
        other => panic!("expected the deferred push, got {other:?}"),
    }
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn two_registrations_receive_distinct_delegate_instances() {
    let root = unique_test_root("per-connection");
    let socket = root.join("leader.sock");
    let factory = recording_factory();
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the leader should spawn");

    let (reader_a, writer_a, client_id_a) = register(&socket).await;
    let (reader_b, writer_b, client_id_b) = register(&socket).await;
    assert_ne!(
        client_id_a, client_id_b,
        "two registrations must receive distinct client ids"
    );
    let connections = (reader_a, writer_a, reader_b, writer_b);

    let delegates = factory.constructed();
    assert_eq!(
        delegates.len(),
        2,
        "each registered connection must construct exactly one delegate"
    );
    let instance_ids: Vec<u64> = delegates.iter().map(|d| d.instance_id).collect();
    assert_ne!(
        instance_ids[0], instance_ids[1],
        "two registrations must receive distinct delegate instances"
    );
    assert_eq!(
        factory.observed_client_ids(),
        vec![client_id_a, client_id_b],
        "the factory must see each connection's generated client id"
    );
    assert!(
        delegates[0].recorded_client_ids().contains(&client_id_a),
        "delegate A must carry client A's id"
    );
    assert!(
        delegates[1].recorded_client_ids().contains(&client_id_b),
        "delegate B must carry client B's id"
    );

    drop(connections);
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn failed_registration_constructs_zero_delegates() {
    let root = unique_test_root("failed-registration");
    let socket = root.join("leader.sock");
    let factory = recording_factory();
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the leader should spawn");

    {
        let (mut reader, mut writer) = connect(&socket).await;
        write_client_frame(
            &mut writer,
            &ClientEnvelope::Register {
                client_type: "   ".to_string(),
                mode: RegisterMode::Stdio,
                capabilities: test_capabilities(),
            },
        )
        .await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Error { code, .. }) => {
                assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
            }
            other => panic!("expected an error envelope, got {other:?}"),
        }
        assert!(next_server_frame(&mut reader).await.is_none());
    }
    {
        let (mut reader, mut writer) = connect(&socket).await;
        write_client_frame(&mut writer, &ClientEnvelope::Ping).await;
        assert!(next_server_frame(&mut reader).await.is_some());
        assert!(next_server_frame(&mut reader).await.is_none());
    }

    assert_eq!(
        factory.constructed_count(),
        0,
        "failed registrations must construct zero delegates"
    );
    assert!(
        factory.observed_client_ids().is_empty(),
        "the factory must never be invoked for a failed registration"
    );

    let (_reader, _writer, _client_id) = register(&socket).await;
    assert_eq!(factory.constructed_count(), 1);

    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_failing_factory_closes_the_connection_and_constructs_no_delegate() {
    let root = unique_test_root("failing-factory");
    let socket = root.join("leader.sock");
    let factory = Arc::new(FailingFactory::default());
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the leader should spawn");

    {
        let (mut reader, mut writer) = connect(&socket).await;
        write_client_frame(&mut writer, &register_envelope()).await;
        assert!(
            next_server_frame(&mut reader).await.is_none(),
            "the leader must close the connection when the factory fails"
        );
    }
    assert_eq!(
        factory.attempts.load(Ordering::SeqCst),
        1,
        "the factory must have been invoked exactly once"
    );

    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn disconnecting_b_notifies_only_b_and_a_still_serves() {
    let root = unique_test_root("isolation");
    let socket = root.join("leader.sock");
    let factory = recording_factory();
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the leader should spawn");

    let (mut reader_a, mut writer_a, _client_id_a) = register(&socket).await;
    let (mut reader_b, mut writer_b, _client_id_b) = register(&socket).await;

    let delegates = factory.constructed();
    assert_eq!(delegates.len(), 2);
    let delegate_a = delegates[0].clone();
    let delegate_b = delegates[1].clone();

    write_client_frame(&mut writer_b, &ClientEnvelope::Disconnect).await;
    assert!(
        next_server_frame(&mut reader_b).await.is_none(),
        "the leader should close connection B after its disconnect"
    );
    assert_eq!(
        delegate_b.disconnect_count(),
        1,
        "connection B's on_disconnect must run exactly once"
    );
    assert_eq!(
        delegate_a.disconnect_count(),
        0,
        "connection A's on_disconnect must not run for B's disconnect"
    );

    write_client_frame(&mut writer_a, &ClientEnvelope::Ping).await;
    match next_server_frame(&mut reader_a).await {
        Some(ServerEnvelope::Pong) => {}
        other => panic!("expected pong for A after B disconnected, got {other:?}"),
    }
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "initialize",
        "params": {},
    })
    .to_string();
    write_client_frame(
        &mut writer_a,
        &ClientEnvelope::Acp {
            payload: payload.clone(),
        },
    )
    .await;
    match next_server_frame(&mut reader_a).await {
        Some(ServerEnvelope::Acp { payload: echoed }) => assert_eq!(echoed, payload),
        other => panic!("expected an acp echo for A, got {other:?}"),
    }
    let payloads_b = delegate_b.payloads.lock().expect("payload log").clone();
    assert!(
        payloads_b.is_empty(),
        "B's delegate must never see A's ACP dispatch"
    );
    assert_eq!(
        delegate_a.disconnect_count(),
        0,
        "A's delegate must still be connected after serving A"
    );

    drop((reader_a, writer_a));
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn default_shutdown_waits_for_connection_on_disconnect_cleanup() {
    let root = unique_test_root("shutdown-disconnect-cleanup");
    let socket = root.join("leader.sock");
    let delegate = GatedDisconnectDelegate::new();
    let factory: Arc<dyn AcpDelegateFactory> = Arc::new({
        let delegate = delegate.clone();
        move |_client_id: u64, _registration: &Registration| {
            Ok(delegate.clone() as Arc<dyn AcpDelegate>)
        }
    });
    let handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory)
        .expect("the leader should spawn");
    let (_reader, _writer, _client_id) = register(&socket).await;

    let mut shutdown = tokio::spawn(async move {
        let mut handle = handle;
        let result = handle.shutdown().await;
        (handle, result)
    });
    timeout(TEST_TIMEOUT, delegate.started.notified())
        .await
        .expect("the connection should enter on_disconnect");
    assert!(
        timeout(Duration::from_millis(50), &mut shutdown)
            .await
            .is_err(),
        "LeaderHandle::shutdown must wait while on_disconnect is still running"
    );

    delegate.release.notify_one();
    let (handle, result) = timeout(TEST_TIMEOUT, shutdown)
        .await
        .expect("leader shutdown should finish after disconnect cleanup")
        .expect("shutdown task should join");
    result.expect("leader shutdown should succeed");
    assert!(
        delegate.completed.load(Ordering::SeqCst),
        "shutdown may return only after on_disconnect completed"
    );
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn shutdown_announces_the_stop_to_live_connections() {
    let root = unique_test_root("shutdown-frames");
    let socket = root.join("leader.sock");
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
        .expect("the leader should spawn");
    let (mut reader, _writer, _client_id) = register(&socket).await;
    handle.shutdown().await.expect("clean shutdown");
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::ShuttingDown { reason, delay_ms }) => {
            assert_eq!(reason, ShutdownReason::Manual);
            assert_eq!(delay_ms, 0);
        }
        other => panic!("expected shutting_down, got {other:?}"),
    }
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Shutdown) => {}
        other => panic!("expected shutdown, got {other:?}"),
    }
    assert!(
        next_server_frame(&mut reader).await.is_none(),
        "the leader should close the connection after shutdown"
    );
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}
