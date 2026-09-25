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
    // server child binds it. Allocate monotonically from a fixed range
    // instead. That range is not free of ephemeral collisions -- it overlaps
    // Linux's default 32768-60999 -- so this only narrows the window, it does
    // not close it. Keep an advisory reservation until this test process exits
    // so other test binaries/worktrees cannot allocate the same currently
    // unbound port during child startup.
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
/// reservation for `port`.
///
/// The reservation is a lock on a file named after the port, not on the
/// socket: it stops other Gents test processes from handing out the same
/// number, and does nothing to stop an unrelated process from binding it.
/// A true answer therefore means "this process asked for that port", not
/// "that port is ours".
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
