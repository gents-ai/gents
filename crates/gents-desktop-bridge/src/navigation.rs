use tauri::{Manager, Runtime, Url, Webview, WebviewWindowBuilder};

use crate::state::DesktopAppState;

pub(crate) const EXPLORER_WINDOW: &str = "db-explorer";

/// Custom-protocol origins are opaque in `url::Origin`. Compare their parsed
/// authority explicitly; userinfo is never part of an application origin.
pub(crate) fn same_origin(url: &Url, expected: &Url) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some()
        && url.scheme() == expected.scheme()
        && url.host_str() == expected.host_str()
        && url.port_or_known_default() == expected.port_or_known_default()
}

pub(crate) fn allow_navigation<R>(webview: &Webview<R>, url: &Url) -> bool
where
    R: Runtime,
{
    if webview.label() == EXPLORER_WINDOW {
        return webview
            .state::<DesktopAppState>()
            .explorer_origin
            .lock()
            .expect("explorer origin lock poisoned")
            .as_ref()
            .is_some_and(|origin| same_origin(url, origin));
    }
    // WebKit reports sandboxed srcdoc frame loads through this URL-only hook.
    // This exact internal document has no URL payload; the frame keeps its CSP
    // and opaque sandbox origin. No other about:, data: or blob: URL is allowed.
    if url.as_str() == "about:srcdoc" {
        return true;
    }
    let config = webview.app_handle().config();
    if tauri::is_dev() {
        if let Some(origin) = &config.build.dev_url {
            return same_origin(url, origin);
        }
    } else if let Some(tauri::utils::config::FrontendDist::Url(origin)) =
        &config.build.frontend_dist
    {
        return same_origin(url, origin);
    }
    let origin = if cfg!(any(windows, target_os = "android")) {
        if config
            .app
            .windows
            .iter()
            .find(|window| window.label == webview.label())
            .or_else(|| config.app.windows.first())
            .is_some_and(|window| window.use_https_scheme)
        {
            "https://tauri.localhost"
        } else {
            "http://tauri.localhost"
        }
    } else {
        "tauri://localhost"
    };
    same_origin(url, &origin.parse().expect("static application origin"))
}

/// Only native application commands create windows. A document or a terminal
/// hyperlink cannot create another WebView, even at the application's origin.
pub fn restrict_window<R>(
    builder: WebviewWindowBuilder<'_, R, tauri::AppHandle<R>>,
) -> WebviewWindowBuilder<'_, R, tauri::AppHandle<R>>
where
    R: Runtime,
{
    builder.on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BridgeConfig, HomePolicy};
    use crate::state::resolve_policy;

    fn application_url() -> &'static str {
        if cfg!(any(windows, target_os = "android")) {
            "http://tauri.localhost/index.html#chat"
        } else {
            "tauri://localhost/index.html#chat"
        }
    }

    #[test]
    fn origins_reject_credentials_ports_hosts_and_opaque_schemes() {
        for base in [
            "tauri://localhost",
            "http://tauri.localhost",
            "http://localhost:1420",
        ] {
            let origin = base.parse().unwrap();
            assert!(same_origin(
                &format!("{base}/index.html#chat").parse().unwrap(),
                &origin
            ));
            for hostile in [
                "https://example.com",
                "http://localhost:1421",
                "tauri://other",
                "data:text/html,hello",
                "about:blank",
                "file:///etc/passwd",
                "tauri://user@localhost",
                "http://user@localhost:1420",
                "http://localhost.evil:1420",
            ] {
                assert!(
                    !same_origin(&hostile.parse().unwrap(), &origin),
                    "{base}: {hostile}"
                );
            }
        }
    }

    #[test]
    fn navigation_stays_with_the_native_window_owner() {
        let temporary = tempfile::tempdir().unwrap();
        let config = BridgeConfig {
            home: HomePolicy::FixedRoot(temporary.path().join("desktop")),
            ..Default::default()
        };
        let app = tauri::test::mock_builder()
            .manage(DesktopAppState::new(resolve_policy(&config, None).unwrap()))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let main = restrict_window(tauri::WebviewWindowBuilder::new(
            app.handle(),
            "main",
            Default::default(),
        ))
        .build()
        .unwrap();
        let explorer = restrict_window(tauri::WebviewWindowBuilder::new(
            app.handle(),
            EXPLORER_WINDOW,
            Default::default(),
        ))
        .build()
        .unwrap();
        let selected = "http://127.0.0.1:9181/explorer/".parse().unwrap();
        assert!(!allow_navigation(explorer.as_ref(), &selected));
        *app.state::<DesktopAppState>()
            .explorer_origin
            .lock()
            .unwrap() = Some(selected.clone());
        assert!(allow_navigation(explorer.as_ref(), &selected));
        assert!(!allow_navigation(main.as_ref(), &selected));
        let internal = application_url().parse().unwrap();
        assert!(allow_navigation(main.as_ref(), &internal));
        assert!(!allow_navigation(explorer.as_ref(), &internal));
        for url in [
            "https://example.test",
            "http://127.0.0.1:9182/explorer/",
            "about:blank",
        ] {
            let url = url.parse().unwrap();
            assert!(!allow_navigation(main.as_ref(), &url));
            assert!(!allow_navigation(explorer.as_ref(), &url));
        }
        let proofs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/proofs");
        let output = gents_lean_contract::run_prepared_generator(
            &proofs,
            "Proofs.Conformance.DesktopShellSecurity:olean",
            "Proofs/Conformance/DesktopShellSecurity.lean",
        )
        .unwrap();
        let cases: serde_json::Value = serde_json::from_str(&output).unwrap();
        let cases = cases["navigation"].as_array().unwrap();
        assert_eq!(cases.len(), 10);
        for case in cases {
            let application = case["application"].as_bool().unwrap();
            let runtime = case["explorer"].as_bool().unwrap();
            let url: Url = if case["sandboxFrame"].as_bool().unwrap() {
                "about:srcdoc"
            } else if application {
                application_url()
            } else if runtime {
                "http://127.0.0.1:9181/explorer/"
            } else {
                "https://unrelated.test/"
            }
            .parse()
            .unwrap();
            let runtime_origin = if runtime {
                url.clone()
            } else {
                "http://127.0.0.1:9182/explorer/".parse().unwrap()
            };
            *app.state::<DesktopAppState>()
                .explorer_origin
                .lock()
                .unwrap() = Some(runtime_origin);
            let window = if case["applicationWindow"].as_bool().unwrap() {
                &main
            } else {
                &explorer
            };
            assert_eq!(
                allow_navigation(window.as_ref(), &url),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
        app.state::<DesktopAppState>()
            .window_event(EXPLORER_WINDOW, &tauri::WindowEvent::Destroyed);
        assert!(!allow_navigation(explorer.as_ref(), &selected));
    }

    #[test]
    fn configured_frontend_origin_is_exact_and_does_not_open_other_dev_ports() {
        let temporary = tempfile::tempdir().unwrap();
        let config = BridgeConfig {
            home: HomePolicy::FixedRoot(temporary.path().join("desktop")),
            ..Default::default()
        };
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        let origin: Url = "http://localhost:1420".parse().unwrap();
        context.config_mut().build.dev_url = Some(origin.clone());
        context.config_mut().build.frontend_dist =
            Some(tauri::utils::config::FrontendDist::Url(origin));
        let app = tauri::test::mock_builder()
            .manage(DesktopAppState::new(resolve_policy(&config, None).unwrap()))
            .build(context)
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(app.handle(), "main", Default::default())
            .build()
            .unwrap();
        assert!(allow_navigation(
            window.as_ref(),
            &"http://localhost:1420/chat".parse().unwrap()
        ));
        for url in [
            "http://localhost:1421",
            "http://user@localhost:1420",
            "https://localhost:1420",
            "tauri://localhost",
            "about:srcdoc?payload=html",
            "data:text/html,hi",
            "blob:http://localhost:1420/1",
        ] {
            assert!(
                !allow_navigation(window.as_ref(), &url.parse().unwrap()),
                "{url}"
            );
        }
        assert!(allow_navigation(
            window.as_ref(),
            &"about:srcdoc".parse().unwrap()
        ));
    }
}
