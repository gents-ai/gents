//! Pure unit tests for the per-target proxy decision: no sockets, no environment mutation.

use super::*;

fn proxy_env(https_proxy: Option<&str>, no_proxy: Option<&str>) -> ProxyEnv {
    ProxyEnv {
        https_proxy: https_proxy.map(str::to_owned),
        no_proxy: no_proxy.map(str::to_owned),
    }
}

fn target(host: &str, port: u16, tls: bool) -> Target {
    Target {
        host: host.to_owned(),
        port,
        tls,
    }
}

/// The reported blocker: a gateway named in `NO_PROXY` must bypass the proxy the API uses.
#[test]
fn gateway_in_no_proxy_connects_direct_while_the_api_stays_proxied() {
    let env = proxy_env(
        Some("http://proxy.corp:3128"),
        Some("gateway.internal,localhost"),
    );
    assert_eq!(
        route_for(&env, &target("gateway.internal", 8443, true)).unwrap(),
        Route::Direct
    );
    assert_eq!(
        route_for(&env, &target("api.anthropic.com", 443, true)).unwrap(),
        Route::Proxied
    );
}

/// The inverse list: the API bypasses, the gateway does not.
#[test]
fn api_in_no_proxy_connects_direct_while_the_gateway_is_proxied() {
    let env = proxy_env(Some("http://proxy.corp:3128"), Some("api.anthropic.com"));
    assert_eq!(
        route_for(&env, &target("api.anthropic.com", 443, true)).unwrap(),
        Route::Direct
    );
    assert_eq!(
        route_for(&env, &target("gateway.internal", 8443, true)).unwrap(),
        Route::Proxied
    );
}

/// A `NO_PROXY` entry with a port matches only that port; the same host on another port proxies.
#[test]
fn no_proxy_port_entry_matches_only_its_own_port() {
    let env = proxy_env(
        Some("http://proxy.corp:3128"),
        Some("gateway.internal:8443"),
    );
    assert_eq!(
        route_for(&env, &target("gateway.internal", 8443, true)).unwrap(),
        Route::Direct
    );
    assert_eq!(
        route_for(&env, &target("gateway.internal", 443, true)).unwrap(),
        Route::Proxied
    );
}

/// The decision uses the target's own port, not the first-party 443.
#[test]
fn a_non_default_target_port_is_decided_on_its_own_port() {
    let env = proxy_env(Some("http://proxy.corp:3128"), Some("gateway.internal:443"));
    assert_eq!(
        route_for(&env, &target("gateway.internal", 443, true)).unwrap(),
        Route::Direct
    );
    assert_eq!(
        route_for(&env, &target("gateway.internal", 8443, true)).unwrap(),
        Route::Proxied
    );
}

/// Plain text keeps its existing semantics: never proxied, whatever the environment says.
#[test]
fn plain_text_targets_stay_direct() {
    let env = proxy_env(Some("http://proxy.corp:3128"), None);
    assert_eq!(
        route_for(&env, &target("gateway.internal", 8080, false)).unwrap(),
        Route::Direct
    );
    assert_eq!(
        route_for(&env, &target("api.anthropic.com", 80, false)).unwrap(),
        Route::Direct
    );
}

/// No proxy configured means every target is direct, as before.
#[test]
fn no_proxy_configured_means_every_target_is_direct() {
    let env = proxy_env(None, None);
    assert_eq!(
        route_for(&env, &target("api.anthropic.com", 443, true)).unwrap(),
        Route::Direct
    );
    assert_eq!(
        route_for(&env, &target("gateway.internal", 8443, true)).unwrap(),
        Route::Direct
    );
}

/// A blank proxy value is no proxy, not an error.
#[test]
fn a_blank_proxy_value_is_direct() {
    let env = proxy_env(Some("   "), None);
    assert_eq!(
        route_for(&env, &target("api.anthropic.com", 443, true)).unwrap(),
        Route::Direct
    );
}

/// A configured value that is not an `http://` proxy URL stays an error, never a silent direct.
#[test]
fn an_unusable_proxy_value_is_still_an_error() {
    let env = proxy_env(Some("socks5://proxy.corp:1080"), None);
    assert!(matches!(
        route_for(&env, &target("api.anthropic.com", 443, true)),
        Err(ProxyError::UnsupportedScheme)
    ));
    let env = proxy_env(Some("not a url"), None);
    assert!(matches!(
        route_for(&env, &target("api.anthropic.com", 443, true)),
        Err(ProxyError::Invalid)
    ));
}

/// The compression flag follows the same per-target decision: a bypassed first-party target
/// gzips, a proxied one does not.
#[test]
fn the_gzip_flag_follows_the_target_route() {
    let body_len = request::GZIP_MIN_UTF16_UNITS;
    let bypassed = proxy_env(Some("http://proxy.corp:3128"), Some("api.anthropic.com"));
    let proxied = proxy_env(Some("http://proxy.corp:3128"), None);
    let api = target("api.anthropic.com", 443, true);
    let direct = route_for(&bypassed, &api).unwrap() == Route::Proxied;
    let through = route_for(&proxied, &api).unwrap() == Route::Proxied;
    assert!(!direct, "a bypassed target is not proxied");
    assert!(through, "a target outside NO_PROXY is proxied");
    assert!(request::gzip_body(&api.host, body_len, direct, false));
    assert!(!request::gzip_body(&api.host, body_len, through, false));
}

/// A gateway target never gzips, proxied or not: the gzip host is the first-party one.
#[test]
fn a_gateway_target_never_gzips() {
    let env = proxy_env(Some("http://proxy.corp:3128"), Some("gateway.internal"));
    let gateway = target("gateway.internal", 8443, true);
    let proxied = route_for(&env, &gateway).unwrap() == Route::Proxied;
    assert!(!proxied);
    assert!(!request::gzip_body(
        &gateway.host,
        request::GZIP_MIN_UTF16_UNITS,
        proxied,
        false
    ));
}

/// The reported blocker, end to end through the real pools: with `NO_PROXY=api.anthropic.com`
/// the API bypasses the proxy, and the gateway must still be sent through the proxied pool.
///
/// This is the coupling the earlier `route_for`-only tests missed: the pool used to be built
/// with the same `NO_PROXY` filter, so it was `None` here and `client_for` fell back to direct.
#[test]
fn a_bypassed_api_still_leaves_the_gateway_on_the_proxied_pool() {
    let shared = Shared::from_env(proxy_env(
        Some("http://proxy.corp:3128"),
        Some("api.anthropic.com"),
    ))
    .unwrap();
    let proxied = shared.proxied.as_ref().expect(
        "HTTPS_PROXY is set, so the proxied pool must exist even though NO_PROXY names the API",
    );

    let (gateway_client, gateway_proxied) = shared
        .client_for(&target("gateway.internal", 8443, true))
        .unwrap();
    assert!(gateway_proxied, "the gateway is outside NO_PROXY");
    assert!(
        std::ptr::eq(gateway_client, proxied),
        "the gateway must use the proxied pool, not the direct one"
    );

    let (api_client, api_proxied) = shared
        .client_for(&target("api.anthropic.com", 443, true))
        .unwrap();
    assert!(!api_proxied, "the API is named in NO_PROXY");
    assert!(
        std::ptr::eq(api_client, &shared.direct),
        "the bypassed API must use the direct pool"
    );
}

/// The compression flag follows the pool actually selected, not a route computed beside it.
#[test]
fn the_gzip_flag_follows_the_selected_pool() {
    let body_len = request::GZIP_MIN_UTF16_UNITS;
    let api = target("api.anthropic.com", 443, true);

    let bypassed = Shared::from_env(proxy_env(
        Some("http://proxy.corp:3128"),
        Some("api.anthropic.com"),
    ))
    .unwrap();
    let (_, proxied) = bypassed.client_for(&api).unwrap();
    assert!(!proxied);
    assert!(
        request::gzip_body(&api.host, body_len, proxied, false),
        "a bypassed first-party target gzips"
    );

    let through = Shared::from_env(proxy_env(Some("http://proxy.corp:3128"), None)).unwrap();
    let (_, proxied) = through.client_for(&api).unwrap();
    assert!(proxied);
    assert!(
        !request::gzip_body(&api.host, body_len, proxied, false),
        "a proxied first-party target does not gzip"
    );
}

/// A proxied route with no pool is an error, never a silent direct connection.
#[test]
fn a_proxied_route_without_a_pool_is_an_error() {
    let shared = Shared {
        direct: Client::new(ClientOptions::default()).unwrap(),
        proxied: None,
        env: proxy_env(Some("http://proxy.corp:3128"), None),
    };
    assert!(matches!(
        shared.client_for(&target("gateway.internal", 8443, true)),
        Err(ClaudeTransportError::ProxyPoolMissing)
    ));
}

/// No `HTTPS_PROXY` means no proxied pool and every target direct, as before.
#[test]
fn no_proxy_configured_builds_no_proxied_pool() {
    let shared = Shared::from_env(proxy_env(None, None)).unwrap();
    assert!(shared.proxied.is_none());
    let (client, proxied) = shared
        .client_for(&target("api.anthropic.com", 443, true))
        .unwrap();
    assert!(!proxied);
    assert!(std::ptr::eq(client, &shared.direct));
}

/// An unusable `HTTPS_PROXY` fails the build, so no request is ever sent silently direct.
///
/// The Grok review counterexample: `NO_PROXY=*` must not let an unusable value through. The
/// pool is built by `configured_proxy`, which passes `None` for the bypass list, so the build
/// fails closed whatever `NO_PROXY` says — including the empty list and a `:0` port entry that
/// matches no target.
#[test]
fn an_unusable_proxy_value_fails_the_pool_build() {
    for no_proxy in [None, Some("*"), Some(""), Some(":0")] {
        assert!(
            matches!(
                Shared::from_env(proxy_env(Some("socks5://proxy.corp:1080"), no_proxy)),
                Err(ClaudeTransportError::Proxy(ProxyError::UnsupportedScheme))
            ),
            "NO_PROXY={no_proxy:?} must not hide an unsupported proxy scheme"
        );
        assert!(
            matches!(
                Shared::from_env(proxy_env(Some("not a url"), no_proxy)),
                Err(ClaudeTransportError::Proxy(ProxyError::Invalid))
            ),
            "NO_PROXY={no_proxy:?} must not hide an invalid proxy URL"
        );
    }
}

/// `route_for` alone may bypass an excluded target without parsing the proxy value; the build
/// does not. Only a list that actually excludes the target answers `Direct` here.
#[test]
fn an_excluded_target_bypasses_without_parsing_the_proxy() {
    let api = target("api.anthropic.com", 443, true);
    let wildcard = proxy_env(Some("not a url"), Some("*"));
    assert_eq!(route_for(&wildcard, &api).unwrap(), Route::Direct);

    let empty = proxy_env(Some("not a url"), Some(""));
    assert!(matches!(route_for(&empty, &api), Err(ProxyError::Invalid)));

    let other_port = proxy_env(Some("not a url"), Some(":0"));
    assert!(matches!(
        route_for(&other_port, &api),
        Err(ProxyError::Invalid)
    ));
}

/// `NO_PROXY=*` bypasses every target but still builds the pool from `HTTPS_PROXY` alone.
#[test]
fn a_wildcard_no_proxy_bypasses_every_target_but_still_builds_the_pool() {
    let shared = Shared::from_env(proxy_env(Some("http://proxy.corp:3128"), Some("*"))).unwrap();
    assert!(
        shared.proxied.is_some(),
        "the pool comes from HTTPS_PROXY alone, whatever NO_PROXY says"
    );
    let (client, proxied) = shared
        .client_for(&target("api.anthropic.com", 443, true))
        .unwrap();
    assert!(!proxied, "NO_PROXY=* excludes every target");
    assert!(std::ptr::eq(client, &shared.direct));
}

fn outgoing(uri: &'static str) -> Outgoing {
    Outgoing::new("POST", &Uri::from_static(uri), &HeaderMap::new()).unwrap()
}

/// The reviewed finding: the pool must be chosen for the target actually connected to.
///
/// `send_once` used to pick the pool from the URL's target and only then rewrite the target
/// for the test route, so a loopback target that `NO_PROXY` excludes was still sent through
/// the proxy the API host's route selected. This asserts the coupling through `select`, the
/// one function `send_once` uses, so restoring the old ordering turns it red.
#[test]
fn a_rewritten_target_selects_the_pool_for_where_the_bytes_go() {
    let shared = Shared::from_env(proxy_env(
        Some("http://proxy.corp:3128"),
        Some("127.0.0.1,localhost"),
    ))
    .unwrap();
    let proxied = shared
        .proxied
        .as_ref()
        .expect("HTTPS_PROXY builds the pool");

    let outgoing = outgoing("https://api.anthropic.com/v1/messages");
    let client = ClaudeHttpClient::new(None).with_test_route(
        Arc::new(AtomicBool::new(false)),
        "127.0.0.1",
        8443,
    );

    let (selected, is_proxied, target) = client.select(&shared, &outgoing).unwrap();
    assert_eq!(target.host, "127.0.0.1");
    assert_eq!(target.port, 8443);
    assert!(
        target.tls,
        "the rewrite changes where, not whether TLS is used"
    );
    assert!(!is_proxied, "the loopback target is named in NO_PROXY");
    assert!(
        std::ptr::eq(selected, &shared.direct),
        "the rewritten target must use the direct pool"
    );

    // The URL's own target would have gone through the proxy: that is the mismatch.
    let (url_client, url_proxied) = shared.client_for(&outgoing.target).unwrap();
    assert!(url_proxied, "the API host is outside NO_PROXY");
    assert!(std::ptr::eq(url_client, proxied));

    // The `Host` header and the gzip decision still follow the URL's host.
    assert_eq!(outgoing.host_header, "api.anthropic.com");
    assert!(
        request::gzip_body(
            &outgoing.target.host,
            request::GZIP_MIN_UTF16_UNITS,
            is_proxied,
            false
        ),
        "a bypassed first-party target still gzips, whatever the rewrite"
    );
}

/// The reverse direction: a URL that bypasses the proxy, rewritten to a target that does not.
#[test]
fn a_rewritten_target_that_is_proxied_uses_the_proxied_pool() {
    let shared =
        Shared::from_env(proxy_env(Some("http://proxy.corp:3128"), Some("127.0.0.1"))).unwrap();
    let proxied = shared
        .proxied
        .as_ref()
        .expect("HTTPS_PROXY builds the pool");

    let outgoing = outgoing("https://127.0.0.1:8443/v1/messages");
    let client = ClaudeHttpClient::new(None).with_test_route(
        Arc::new(AtomicBool::new(false)),
        "gateway.internal",
        8443,
    );

    let (selected, is_proxied, target) = client.select(&shared, &outgoing).unwrap();
    assert_eq!(target.host, "gateway.internal");
    assert!(is_proxied, "the gateway is outside NO_PROXY");
    assert!(
        std::ptr::eq(selected, proxied),
        "the rewritten target must use the proxied pool"
    );

    let (_, url_proxied) = shared.client_for(&outgoing.target).unwrap();
    assert!(!url_proxied, "the URL's own target is named in NO_PROXY");
}

/// With no test route the effective target is the URL's target, unchanged.
#[test]
fn without_a_test_route_the_effective_target_is_the_url_target() {
    let outgoing = outgoing("https://api.anthropic.com/v1/messages");
    let client = ClaudeHttpClient::new(None);
    let (_, _, target) = client
        .select(
            &Shared::from_env(proxy_env(Some("http://proxy.corp:3128"), None)).unwrap(),
            &outgoing,
        )
        .unwrap();
    assert_eq!(target.host, outgoing.target.host);
    assert_eq!(target.port, outgoing.target.port);
    assert_eq!(target.tls, outgoing.target.tls);
}
