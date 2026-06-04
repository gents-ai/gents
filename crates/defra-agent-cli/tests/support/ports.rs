use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};

static ALLOCATED_PORTS: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();

pub fn allocate_port() -> Result<u16> {
    let allocated_ports = ALLOCATED_PORTS.get_or_init(|| Mutex::new(HashSet::new()));
    for _ in 0..128 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding ephemeral port")?;
        let port = listener
            .local_addr()
            .context("reading ephemeral port")?
            .port();
        let mut allocated_ports = allocated_ports
            .lock()
            .expect("allocated port registry poisoned");
        if allocated_ports.insert(port) {
            drop(listener);
            return Ok(port);
        }
    }

    bail!("failed to allocate a fresh test port after 128 attempts")
}

pub fn graphql_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v0/graphql")
}
