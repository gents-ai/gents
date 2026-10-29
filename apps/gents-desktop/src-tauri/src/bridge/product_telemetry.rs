use serde::Serialize;
use std::{
    future::Future,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::sync::{watch, Mutex, Semaphore};

struct Config {
    endpoint: reqwest::Url,
    website: String,
}

impl Config {
    fn from_env() -> Option<Self> {
        if cfg!(debug_assertions) || cfg!(test) {
            return None;
        }
        Self::parse(
            &std::env::var("GENTS_UMAMI_ENDPOINT").ok()?,
            &std::env::var("GENTS_UMAMI_WEBSITE_ID").ok()?,
        )
    }

    fn parse(endpoint: &str, website: &str) -> Option<Self> {
        let endpoint = reqwest::Url::parse(endpoint).ok()?;
        if endpoint.scheme() != "https"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.path().ends_with("/api/send")
            || !valid_website_id(website)
        {
            return None;
        }
        Some(Self {
            endpoint,
            website: website.to_owned(),
        })
    }
}

fn valid_website_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[derive(Serialize)]
pub struct Status {
    configured: bool,
    enabled: bool,
}

pub struct ProductTelemetry {
    path: PathBuf,
    config: Option<Config>,
    consent: Mutex<Option<bool>>,
    changes: watch::Sender<bool>,
    capacity: Semaphore,
    opened: AtomicBool,
}

impl ProductTelemetry {
    pub fn new(path: PathBuf) -> Self {
        Self::with_config(path, Config::from_env())
    }

    fn with_config(path: PathBuf, config: Option<Config>) -> Self {
        let (changes, _) = watch::channel(false);
        Self {
            path,
            config,
            consent: Mutex::new(None),
            changes,
            capacity: Semaphore::new(1),
            opened: AtomicBool::new(false),
        }
    }

    async fn status(&self) -> Status {
        let mut consent = self.consent.lock().await;
        if consent.is_none() {
            let enabled = tokio::fs::read(&self.path)
                .await
                .is_ok_and(|bytes| bytes == b"true");
            *consent = Some(enabled);
            self.changes.send_replace(enabled);
        }
        Status {
            configured: self.config.is_some(),
            enabled: consent.unwrap_or(false),
        }
    }

    async fn set_enabled(&self, enabled: bool) -> Result<Status, String> {
        let mut consent = self.consent.lock().await;
        if enabled && self.config.is_none() {
            return Err("Usage telemetry is not configured".into());
        }
        if !enabled {
            *consent = Some(false);
            self.changes.send_replace(false);
            match tokio::fs::remove_file(&self.path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Could not save telemetry preference".into()),
            }
            return Ok(Status {
                configured: self.config.is_some(),
                enabled: false,
            });
        }
        let parent = self
            .path
            .parent()
            .ok_or("Invalid telemetry settings path")?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| "Could not save telemetry preference")?;
        tokio::fs::write(&self.path, b"true")
            .await
            .map_err(|_| "Could not save telemetry preference")?;
        *consent = Some(enabled);
        self.changes.send_if_modified(|current| {
            if *current == enabled {
                false
            } else {
                *current = enabled;
                true
            }
        });
        Ok(Status {
            configured: self.config.is_some(),
            enabled,
        })
    }

    async fn deliver<F>(&self, request: F)
    where
        F: Future<Output = ()>,
    {
        if !self.status().await.enabled || self.config.is_none() {
            return;
        }
        let Ok(_permit) = self.capacity.try_acquire() else {
            return;
        };
        let mut changes = self.changes.subscribe();
        if !*changes.borrow_and_update() {
            return;
        }
        tokio::select! {
            biased;
            _ = changes.changed() => {},
            _ = tokio::time::timeout(Duration::from_secs(3), request) => {},
        }
    }

    async fn send_app_opened(&self) {
        if self.opened.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(config) = &self.config else {
            return;
        };
        let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("GentsDesktop/{}", env!("CARGO_PKG_VERSION")))
            .build()
        else {
            return;
        };
        self.deliver(async {
            let _ = client
                .post(config.endpoint.clone())
                .json(&payload(&config.website))
                .send()
                .await;
        })
        .await;
    }
}

#[tauri::command]
pub async fn product_telemetry_status(
    state: tauri::State<'_, ProductTelemetry>,
) -> Result<Status, String> {
    Ok(state.status().await)
}

#[tauri::command]
pub async fn product_telemetry_set_enabled(
    state: tauri::State<'_, ProductTelemetry>,
    enabled: bool,
) -> Result<Status, String> {
    state.set_enabled(enabled).await
}

fn payload(website: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "event", "payload": {
            "website": website, "hostname": "gents", "url": "/", "name": "app-opened",
            "data": { "version": env!("CARGO_PKG_VERSION"), "platform": std::env::consts::OS }
        }
    })
}

#[tauri::command]
pub async fn product_telemetry_app_opened(
    state: tauri::State<'_, ProductTelemetry>,
) -> Result<(), String> {
    state.send_app_opened().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn configured(path: PathBuf) -> ProductTelemetry {
        ProductTelemetry::with_config(
            path,
            Some(Config {
                endpoint: reqwest::Url::parse("https://example.invalid/api/send").unwrap(),
                website: "test-site".into(),
            }),
        )
    }

    #[tokio::test]
    async fn off_and_unconfigured_do_not_poll_delivery() {
        let directory = tempfile::tempdir().unwrap();
        let calls = AtomicUsize::new(0);
        let off = configured(directory.path().join("off"));
        off.deliver(async {
            calls.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        let missing = ProductTelemetry::with_config(directory.path().join("missing"), None);
        assert!(missing.set_enabled(true).await.is_err());
        missing
            .deliver(async {
                calls.fetch_add(1, Ordering::SeqCst);
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn consent_persists_and_disabling_cancels_and_releases_capacity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("consent");
        let owner = configured(path.clone());
        owner.set_enabled(true).await.unwrap();
        assert!(configured(path).status().await.enabled);
        let (started, received) = tokio::sync::oneshot::channel();
        let delivery = owner.deliver(async {
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        let test = async {
            received.await.unwrap();
            let calls = AtomicUsize::new(0);
            owner
                .deliver(async {
                    calls.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            owner.set_enabled(false).await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(delivery, test);
        })
        .await
        .unwrap();
        assert_eq!(owner.capacity.available_permits(), 1);
        assert!(!owner.status().await.enabled);
    }

    #[tokio::test]
    async fn repeated_enable_preserves_in_flight_delivery() {
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let case = snapshot["client_telemetry_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| {
                case["initial"]["configured"] == true
                    && case["initial"]["consent"] == true
                    && case["actions"].as_array().unwrap().len() == 2
                    && case["actions"][0]["kind"] == "open"
                    && case["actions"][1]["kind"] == "enable"
            })
            .unwrap();
        assert_eq!(case["expected"][1]["state"]["in_flight"], true);
        let directory = tempfile::tempdir().unwrap();
        let owner = configured(directory.path().join("consent"));
        owner.set_enabled(true).await.unwrap();
        let (started, received) = tokio::sync::oneshot::channel();
        let (release, proceed) = tokio::sync::oneshot::channel();
        let completed = AtomicBool::new(false);
        let delivery = owner.deliver(async {
            started.send(()).unwrap();
            proceed.await.unwrap();
            completed.store(true, Ordering::SeqCst);
        });
        let test = async {
            received.await.unwrap();
            owner.set_enabled(true).await.unwrap();
            assert_eq!(owner.capacity.available_permits(), 0);
            release.send(()).unwrap();
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(delivery, test);
        })
        .await
        .unwrap();
        assert_eq!(owner.capacity.available_permits(), 1);
        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn payload_contains_only_fixed_usage_fields() {
        let event = payload("test-site");
        let fields = event["payload"].as_object().unwrap();
        assert_eq!(fields.len(), 5);
        assert_eq!(fields["name"], "app-opened");
        let data = fields["data"].as_object().unwrap();
        assert_eq!(data.len(), 2);
        assert!(data.contains_key("platform") && data.contains_key("version"));
    }

    #[test]
    fn configuration_requires_the_umami_collection_endpoint() {
        let website = "12345678-1234-1234-1234-123456789abc";
        assert!(Config::parse("https://metrics.example/api/send", website).is_some());
        assert!(Config::parse("https://metrics.example/proxy/api/send", website).is_some());
        for endpoint in [
            "http://metrics.example/api/send",
            "https://metrics.example/",
            "https://user:password@metrics.example/api/send",
            "https://metrics.example/api/send?secret=value",
        ] {
            assert!(Config::parse(endpoint, website).is_none(), "{endpoint}");
        }
        assert!(Config::parse("https://metrics.example/api/send", " ").is_none());
        assert!(Config::parse("https://metrics.example/api/send", "not-a-uuid").is_none());
    }

    #[tokio::test]
    async fn consent_can_be_revoked_while_unconfigured() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("consent");
        configured(path.clone()).set_enabled(true).await.unwrap();
        let offline = ProductTelemetry::with_config(path.clone(), None);
        let status = offline.status().await;
        assert!(!status.configured && status.enabled);
        offline.set_enabled(false).await.unwrap();
        assert!(tokio::fs::metadata(&path).await.is_err());
        assert!(!configured(path).status().await.enabled);
    }

    #[tokio::test]
    async fn native_transport_sends_only_after_consent_and_stops_after_revocation() {
        use axum::{
            extract::State,
            http::{HeaderMap, StatusCode},
            routing::post,
            Json, Router,
        };
        use std::sync::Arc;

        type Events = Arc<Mutex<Vec<(HeaderMap, serde_json::Value)>>>;
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/api/send",
                post(
                    |State(events): State<Events>,
                     headers: HeaderMap,
                     Json(body): Json<serde_json::Value>| async move {
                        events.lock().await.push((headers, body));
                        StatusCode::NO_CONTENT
                    },
                ),
            )
            .with_state(events.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("consent");
        let configured_owner = || {
            ProductTelemetry::with_config(
                path.clone(),
                Some(Config {
                    endpoint: reqwest::Url::parse(&format!("http://{address}/api/send")).unwrap(),
                    website: "12345678-1234-1234-1234-123456789abc".into(),
                }),
            )
        };
        let owner = configured_owner();
        owner.send_app_opened().await;
        assert!(events.lock().await.is_empty());
        owner.set_enabled(true).await.unwrap();
        owner.send_app_opened().await;
        assert!(events.lock().await.is_empty());
        let owner = configured_owner();
        owner.send_app_opened().await;
        owner.send_app_opened().await;
        {
            let events = events.lock().await;
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].1, payload("12345678-1234-1234-1234-123456789abc"));
            assert!(events[0].0["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("GentsDesktop/"));
        }
        owner.set_enabled(false).await.unwrap();
        configured_owner().send_app_opened().await;
        assert_eq!(events.lock().await.len(), 1);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn generated_telemetry_traces_match_native_consent_and_http() {
        use axum::{extract::State, http::StatusCode, routing::post, Router};
        use std::sync::Arc;
        let count = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/api/send",
                post(|State(count): State<Arc<AtomicUsize>>| async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }),
            )
            .with_state(count.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["client_telemetry_cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("consent");
            let initial = &case["initial"];
            if initial["persisted"].as_bool().unwrap() {
                tokio::fs::write(&path, b"true").await.unwrap();
            }
            let configured = initial["configured"].as_bool().unwrap();
            let make_owner = || {
                ProductTelemetry::with_config(
                    path.clone(),
                    configured.then(|| Config {
                        endpoint: reqwest::Url::parse(&format!("http://{address}/api/send"))
                            .unwrap(),
                        website: "12345678-1234-1234-1234-123456789abc".into(),
                    }),
                )
            };
            let mut owner = make_owner();
            for (action, expected) in case["actions"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["expected"].as_array().unwrap())
            {
                let before = count.load(Ordering::SeqCst);
                let kind = action["kind"].as_str().unwrap();
                let write_ok = action["write_ok"].as_bool().unwrap_or(true);
                if !write_ok && (kind == "disable" || (kind == "enable" && configured)) {
                    // The native owner must read its current consent before the
                    // external storage fault replaces the marker with a directory.
                    owner.status().await;
                    match tokio::fs::remove_file(&path).await {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => panic!("preparing settings fault: {error}"),
                    }
                    tokio::fs::create_dir(&path).await.unwrap();
                }
                match kind {
                    "open" => owner.send_app_opened().await,
                    "enable" => assert_eq!(
                        owner.set_enabled(true).await.is_ok(),
                        configured && write_ok
                    ),
                    "disable" => assert_eq!(owner.set_enabled(false).await.is_ok(), write_ok),
                    "restart" => owner = make_owner(),
                    _ => panic!("unknown generated telemetry action"),
                }
                assert_eq!(
                    count.load(Ordering::SeqCst) - before,
                    usize::from(expected["send"].as_bool().unwrap()),
                    "{case}"
                );
                let state = &expected["state"];
                assert_eq!(
                    owner.status().await.enabled,
                    state["consent"].as_bool().unwrap(),
                    "{case}"
                );
                assert_eq!(
                    tokio::fs::read(&path)
                        .await
                        .is_ok_and(|bytes| bytes == b"true"),
                    state["persisted"].as_bool().unwrap(),
                    "{case}"
                );
                assert_eq!(
                    owner.opened.load(Ordering::Acquire),
                    state["opened"].as_bool().unwrap(),
                    "{case}"
                );
            }
        }
        server.abort();
        let _ = server.await;
    }
}
