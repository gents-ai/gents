use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};

const FIRST_TEST_PORT: u16 = 20_000;
/// Below 32768, where Linux's default `ip_local_port_range` starts (macOS
/// starts at 49152): an outbound connection is never given a port in this
/// range, so it cannot take one between the probe and the server's bind.
/// `last_test_port` lowers it further on a host configured below that.
const LAST_TEST_PORT: u16 = 32_767;

static NEXT_TEST_PORT: OnceLock<Mutex<u16>> = OnceLock::new();
static PORT_RESERVATIONS: OnceLock<Mutex<Vec<(u16, Reservation)>>> = OnceLock::new();

/// A reservation must be visible to every test process that shares the
/// port space (`TestPortReservation.tla`: `PerRunner` is any namespace
/// narrower than that). A filesystem lock is not: TMPDIR differs per CI
/// runner, and each runner's systemd service runs with `PrivateTmp=yes`,
/// so even `/tmp` differs per runner. On Linux the reservation is an
/// abstract Unix socket, whose namespace is the network namespace that
/// holds the ports; the kernel releases it when the process exits.
#[cfg(target_os = "linux")]
type Reservation = std::os::unix::net::UnixDatagram;

#[cfg(target_os = "linux")]
fn reserve(port: u16) -> Result<Option<Reservation>> {
    use std::os::linux::net::SocketAddrExt;
    let name =
        std::os::unix::net::SocketAddr::from_abstract_name(format!("gents-cli-test-port-{port}"))
            .context("naming test port reservation")?;
    match std::os::unix::net::UnixDatagram::bind_addr(&name) {
        Ok(reservation) => Ok(Some(reservation)),
        Err(error) if error.kind() == ErrorKind::AddrInUse => Ok(None),
        Err(error) => Err(error).context("binding test port reservation"),
    }
}

/// Elsewhere a lock file. Reservation files must not be unlinked:
/// replacing an inode defeats their lock.
#[cfg(not(target_os = "linux"))]
type Reservation = std::fs::File;

#[cfg(not(target_os = "linux"))]
fn reserve(port: u16) -> Result<Option<Reservation>> {
    use std::fs::TryLockError;
    #[cfg(unix)]
    let directory = std::path::PathBuf::from("/tmp/gents-cli-test-port-reservations");
    #[cfg(not(unix))]
    let directory = std::env::temp_dir().join("gents-cli-test-port-reservations");
    match std::fs::create_dir(&directory) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o1777))
                    .context("opening the test port reservation directory to every user")?;
            }
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("creating test port reservation directory"),
    }
    let reservation = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(port.to_string()))
    {
        Ok(reservation) => reservation,
        // Another user's process created this reservation; it is theirs.
        Err(error) if error.kind() == ErrorKind::PermissionDenied => return Ok(None),
        Err(error) => return Err(error).context("opening test port reservation"),
    };
    match reservation.try_lock() {
        Ok(()) => Ok(Some(reservation)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error).context("locking test port reservation"),
    }
}

#[cfg(target_os = "linux")]
fn last_test_port() -> Result<u16> {
    let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .context("reading the ephemeral port range")?;
    let first_ephemeral: u16 = range
        .split_whitespace()
        .next()
        .context("empty ephemeral port range")?
        .parse()
        .context("parsing the ephemeral port range")?;
    let last = LAST_TEST_PORT.min(first_ephemeral.saturating_sub(1));
    if last < FIRST_TEST_PORT {
        bail!("the ephemeral port range starts at {first_ephemeral}, inside the test range from {FIRST_TEST_PORT}");
    }
    Ok(last)
}

#[cfg(not(target_os = "linux"))]
fn last_test_port() -> Result<u16> {
    Ok(LAST_TEST_PORT)
}

pub fn allocate_port() -> Result<u16> {
    // Allocate monotonically from a fixed range instead of probing port 0:
    // dropping the probe lets an outbound test connection claim the port
    // before the server child binds it.
    let last_port = last_test_port()?;
    let next_port = NEXT_TEST_PORT.get_or_init(|| Mutex::new(FIRST_TEST_PORT));
    for _ in FIRST_TEST_PORT..=last_port {
        let port = {
            let mut next_port = next_port.lock().expect("test port cursor poisoned");
            let port = *next_port;
            *next_port = if port >= last_port {
                FIRST_TEST_PORT
            } else {
                port + 1
            };
            port
        };
        let Some(reservation) = reserve(port)? else {
            continue;
        };
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

    bail!("no free test port in {FIRST_TEST_PORT}..={last_port}")
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
                .any(|(reserved, _reservation)| *reserved == port)
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
            .retain(|(reserved, _reservation)| *reserved != port);
    }
}

pub fn graphql_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v0/graphql")
}
