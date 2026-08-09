use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Result};

const TEST_PORT_FIRST: u16 = 20_000;
const TEST_PORT_COUNT: u16 = 10_000;

struct PortRegistry {
    allocated: HashSet<u16>,
    next: u16,
}

static PORT_REGISTRY: OnceLock<Mutex<PortRegistry>> = OnceLock::new();

pub fn allocate_port() -> Result<u16> {
    let registry = PORT_REGISTRY.get_or_init(|| {
        let process_offset = (std::process::id() % u32::from(TEST_PORT_COUNT)) as u16;
        Mutex::new(PortRegistry {
            allocated: HashSet::new(),
            next: TEST_PORT_FIRST + process_offset,
        })
    });
    let mut registry = registry.lock().expect("allocated port registry poisoned");

    // Do not ask the OS for port 0 here. These ports remain unbound while a
    // CLI child initializes, and another concurrently starting DefraDB node
    // may otherwise receive the dropped ephemeral port for an internal
    // listener before the intended HTTP or shim server binds it.
    for _ in 0..TEST_PORT_COUNT {
        let port = registry.next;
        let offset = (port - TEST_PORT_FIRST + 1) % TEST_PORT_COUNT;
        registry.next = TEST_PORT_FIRST + offset;
        if registry.allocated.contains(&port) {
            continue;
        }
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            registry.allocated.insert(port);
            drop(listener);
            return Ok(port);
        }
    }

    bail!(
        "failed to allocate a fresh test port in {TEST_PORT_FIRST}..{}",
        TEST_PORT_FIRST + TEST_PORT_COUNT
    )
}

pub fn graphql_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v0/graphql")
}
