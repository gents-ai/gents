use std::fs::{File, OpenOptions, TryLockError};
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};

const FIRST_TEST_PORT: u16 = 20_000;
const LAST_TEST_PORT: u16 = 45_000;

static NEXT_TEST_PORT: OnceLock<Mutex<u16>> = OnceLock::new();
static PORT_RESERVATIONS: OnceLock<Mutex<Vec<(u16, File)>>> = OnceLock::new();

pub fn allocate_port() -> Result<u16> {
    // Binding port 0 chooses from the OS ephemeral range. Once that probe is
    // dropped, any outbound test connection may claim the same port before the
    // server child binds it. Allocate monotonically from a non-ephemeral range
    // instead. Keep an advisory reservation until this test process exits so
    // other test binaries/worktrees cannot allocate the same currently unbound
    // port during child startup. The bind still rejects unrelated listeners.
    // Do not unlink reservation files: replacing an inode defeats its lock.
    let reservations = std::env::temp_dir().join("gents-cli-test-port-reservations");
    std::fs::create_dir_all(&reservations).context("creating test port reservation directory")?;
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
        let reservation = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(reservations.join(port.to_string()))
            .context("opening test port reservation")?;
        match reservation.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(error)) => {
                return Err(error).context("locking test port reservation");
            }
        }
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            drop(listener);
            PORT_RESERVATIONS
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .expect("test port reservations poisoned")
                .push((port, reservation));
            return Ok(port);
        }
    }

    bail!("no free test port in {FIRST_TEST_PORT}..={LAST_TEST_PORT}")
}

/// True while this process still holds `allocate_port`'s advisory
/// reservation for `port`. The recovery paths in `process` consult it as a
/// secondary gate, so they only retry a port this registry actually handed
/// out.
///
/// This check is deliberately not what protects the fixtures that provoke
/// bind conflicts on purpose -- `server_fails_closed_when_http_port_is_occupied`
/// and `server_rejects_ephemeral_http_port_before_publishing_readiness` in
/// cli_server.rs. Those are safe structurally: they drive `spawn_server` and
/// that suite's local `wait_for_server_exit`, so they never call a recovering
/// helper and cannot reach the recovery path at all. The registry check on
/// its own would not be airtight, because `FIRST_TEST_PORT..=LAST_TEST_PORT`
/// (20000-45000) overlaps Linux's default ephemeral range (32768-60999), so a
/// port a fixture binds with port 0 can collide with one this registry
/// handed out.
pub fn is_reserved(port: u16) -> bool {
    PORT_RESERVATIONS
        .get()
        .map(|reservations| {
            reservations
                .lock()
                .expect("test port reservations poisoned")
                .iter()
                .any(|(reserved, _file)| *reserved == port)
        })
        .unwrap_or(false)
}

/// Give up this process's advisory reservation for `port` after positive
/// evidence (a captured bind-conflict diagnostic) that an unrelated process
/// now owns it, so neither a later `is_reserved` check nor another
/// `allocate_port` caller in this process treats it as still ours.
pub fn release(port: u16) {
    if let Some(reservations) = PORT_RESERVATIONS.get() {
        reservations
            .lock()
            .expect("test port reservations poisoned")
            .retain(|(reserved, _file)| *reserved != port);
    }
}

pub fn graphql_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v0/graphql")
}
