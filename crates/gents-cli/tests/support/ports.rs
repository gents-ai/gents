use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Result};

const FIRST_TEST_PORT: u16 = 20_000;
const LAST_TEST_PORT: u16 = 45_000;

static NEXT_TEST_PORT: OnceLock<Mutex<u16>> = OnceLock::new();

pub fn allocate_port() -> Result<u16> {
    // Binding port 0 chooses from the OS ephemeral range. Once that probe is
    // dropped, any outbound test connection may claim the same port before the
    // server child binds it. Allocate monotonically from a non-ephemeral range
    // instead; the bind still rejects ports owned by another local process.
    let next_port = NEXT_TEST_PORT.get_or_init(|| Mutex::new(FIRST_TEST_PORT));
    for _ in FIRST_TEST_PORT..=LAST_TEST_PORT {
        let port = {
            let mut next_port = next_port.lock().expect("test port cursor poisoned");
            let port = *next_port;
            *next_port = if port == LAST_TEST_PORT {
                FIRST_TEST_PORT
            } else {
                port + 1
            };
            port
        };
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            drop(listener);
            return Ok(port);
        }
    }

    bail!("no free test port in {FIRST_TEST_PORT}..={LAST_TEST_PORT}")
}

pub fn graphql_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v0/graphql")
}
