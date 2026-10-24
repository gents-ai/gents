use std::fs::{File, OpenOptions, TryLockError};
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

const FIRST_TEST_PORT: u16 = 20_000;
const LAST_TEST_PORT: u16 = 45_000;

static NEXT_TEST_PORT: OnceLock<Mutex<u16>> = OnceLock::new();
static PORT_RESERVATIONS: OnceLock<Mutex<Vec<(u16, File)>>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum PortError {
    #[error("creating test port reservation directory")]
    CreateReservationDir(#[source] std::io::Error),
    #[error("opening test port reservation")]
    OpenReservation(#[source] std::io::Error),
    #[error("locking test port reservation")]
    LockReservation(#[source] std::io::Error),
    #[error("no free test port in {FIRST_TEST_PORT}..={LAST_TEST_PORT}")]
    NoFreePort,
}

type Result<T, E = PortError> = std::result::Result<T, E>;

pub fn allocate_port() -> Result<u16> {
    // Allocate monotonically from a fixed range instead of probing port 0:
    // dropping the probe lets an outbound test connection claim the port
    // before the server child binds it. The range still overlaps Linux's
    // ephemeral range, so this only narrows the collision window. Reservation
    // files must not be unlinked: replacing an inode defeats their lock.
    let reservations = std::env::temp_dir().join("gents-cli-test-port-reservations");
    std::fs::create_dir_all(&reservations).map_err(PortError::CreateReservationDir)?;
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
            .map_err(PortError::OpenReservation)?;
        match reservation.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(error)) => {
                return Err(PortError::LockReservation(error));
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

    Err(PortError::NoFreePort)
}

/// True while this process still holds `allocate_port`'s advisory reservation
/// for `port`; it stops other Gents test processes from handing out the same
/// number, not unrelated processes from binding it.
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

/// Drop this process's advisory reservation for `port` after evidence an
/// unrelated process now owns it, so later `is_reserved` checks and
/// `allocate_port` callers here do not treat it as still ours.
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
