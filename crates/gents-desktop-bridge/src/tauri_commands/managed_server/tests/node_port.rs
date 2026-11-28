use super::*;
use gents_service::network_settings::NetworkSettings;
use gents_service::node_port::PortSource;
use gents_service::server_host::ServerConfig;
use tokio::io::AsyncWriteExt as _;

/// A listener that answers HTTP, but not as Gents.
async fn other_app() -> (tokio::task::JoinHandle<()>, u16) {
    // The move searches upward, so the last port would leave it nowhere to go.
    let (listener, port) = loop {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        if port < u16::MAX {
            break (listener, port);
        }
    };
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        }
    });
    (task, port)
}

fn listening_on(home: &Path, port: u16) -> ServerConfig {
    ServerConfig {
        http_port: port,
        ..ServerConfig::standard(home.to_path_buf())
    }
}

#[tokio::test]
async fn a_new_home_moves_off_a_port_another_app_holds() {
    let home = tempfile::tempdir().unwrap();
    let (app, taken) = other_app().await;

    settle_port_at(
        &listening_on(home.path(), taken),
        PortSource::Unset,
        false,
        None,
    )
    .await
    .unwrap();

    let recorded = NetworkSettings::read(home.path()).unwrap().http_port;
    assert!(recorded.is_some_and(|port| port != taken), "{recorded:?}");
    app.abort();
}

#[tokio::test]
async fn a_home_that_has_served_keeps_its_port_and_names_the_holder() {
    let home = tempfile::tempdir().unwrap();
    let (app, taken) = other_app().await;

    let error = settle_port_at(
        &listening_on(home.path(), taken),
        PortSource::Unset,
        true,
        None,
    )
    .await
    .expect_err("a home that has served is not moved");

    assert!(
        error.message.contains("is in use by another app"),
        "{}",
        error.message
    );
    assert_eq!(
        NetworkSettings::read(home.path()).unwrap(),
        NetworkSettings::default()
    );
    app.abort();
}

#[tokio::test]
async fn a_chosen_port_is_kept_and_names_the_holder() {
    let home = tempfile::tempdir().unwrap();
    let (app, taken) = other_app().await;
    let chosen = NetworkSettings {
        http_port: Some(taken),
        ..NetworkSettings::default()
    };
    chosen.persist(home.path()).unwrap();

    let error = settle_port_at(
        &listening_on(home.path(), taken),
        PortSource::Chosen,
        false,
        None,
    )
    .await
    .expect_err("a port someone chose is not moved");

    assert!(
        error.message.contains(&taken.to_string()),
        "{}",
        error.message
    );
    assert_eq!(NetworkSettings::read(home.path()).unwrap(), chosen);
    app.abort();
}

#[tokio::test]
async fn a_picked_port_taken_before_the_first_serve_is_picked_again() {
    let home = tempfile::tempdir().unwrap();
    let (app, taken) = other_app().await;
    NetworkSettings {
        http_port: Some(taken),
        http_port_picked: true,
        ..NetworkSettings::default()
    }
    .persist(home.path())
    .unwrap();

    settle_port_at(
        &listening_on(home.path(), taken),
        PortSource::Picked,
        false,
        None,
    )
    .await
    .unwrap();

    let settings = NetworkSettings::read(home.path()).unwrap();
    assert!(
        settings.http_port.is_some_and(|port| port != taken) && settings.http_port_picked,
        "{settings:?}"
    );
    app.abort();
}
