use std::fs::{File, OpenOptions, TryLockError};
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};

const FIRST_TEST_PORT: u16 = 20_000;
const LAST_TEST_PORT: u16 = 45_000;

static NEXT_TEST_PORT: OnceLock<Mutex<u16>> = OnceLock::new();
static PORT_RESERVATIONS: OnceLock<Mutex<Vec<File>>> = OnceLock::new();

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
                .push(reservation);
            return Ok(port);
        }
    }

    bail!("no free test port in {FIRST_TEST_PORT}..={LAST_TEST_PORT}")
}

pub fn graphql_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v0/graphql")
}
