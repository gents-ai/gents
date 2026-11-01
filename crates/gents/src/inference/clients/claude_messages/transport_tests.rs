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
#[test]
fn an_unusable_proxy_value_fails_the_pool_build() {
    assert!(matches!(
        Shared::from_env(proxy_env(Some("socks5://proxy.corp:1080"), None)),
        Err(ClaudeTransportError::Proxy(ProxyError::UnsupportedScheme))
    ));
    assert!(matches!(
        Shared::from_env(proxy_env(Some("not a url"), None)),
        Err(ClaudeTransportError::Proxy(ProxyError::Invalid))
    ));
}
