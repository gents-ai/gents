use std::net::TcpListener;

use anyhow::{Context, Result};

pub fn allocate_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding ephemeral port")?;
    let port = listener
        .local_addr()
        .context("reading ephemeral port")?
        .port();
    drop(listener);
    Ok(port)
}

pub fn graphql_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v0/graphql")
}
