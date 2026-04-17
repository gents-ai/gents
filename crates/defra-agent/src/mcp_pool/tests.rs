use super::*;

#[test]
fn resolve_mcp_url_same_host_uses_localhost() {
    let url = resolve_mcp_url(
        "studio-1",
        "100.69.4.79",
        "192.168.1.104",
        9200,
        "/mcp",
        "studio-1",
        Some("192.168.1.0/24"),
    );
    assert_eq!(url, "http://127.0.0.1:9200/mcp");
}

#[test]
fn resolve_mcp_url_same_subnet_uses_lan_ip() {
    let url = resolve_mcp_url(
        "studio-2",
        "100.76.203.120",
        "192.168.1.152",
        9200,
        "/mcp",
        "studio-1",
        Some("192.168.1.0/24"),
    );
    assert_eq!(url, "http://192.168.1.152:9200/mcp");
}

#[test]
fn resolve_mcp_url_cross_site_uses_tailscale_when_subnet_differs() {
    let url = resolve_mcp_url(
        "mini-1",
        "100.86.62.91",
        "192.168.1.101",
        9200,
        "/mcp",
        "studio-1",
        Some("10.0.0.0/24"),
    );
    assert_eq!(url, "http://100.86.62.91:9200/mcp");
}

#[test]
fn resolve_mcp_url_no_lan_ip_uses_tailscale() {
    let url = resolve_mcp_url(
        "vps-1",
        "5.78.68.132",
        "",
        9200,
        "/mcp",
        "studio-1",
        Some("192.168.1.0/24"),
    );
    assert_eq!(url, "http://5.78.68.132:9200/mcp");
}

#[test]
fn resolve_mcp_url_no_subnet_uses_tailscale() {
    let url = resolve_mcp_url(
        "studio-2",
        "100.76.203.120",
        "192.168.1.152",
        9200,
        "/mcp",
        "studio-1",
        None,
    );
    assert_eq!(url, "http://100.76.203.120:9200/mcp");
}
