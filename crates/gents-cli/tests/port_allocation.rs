use gents::support::ports;

#[test]
fn unbound_port_is_reserved_across_test_processes() -> anyhow::Result<()> {
    let port = ports::allocate_port()?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["allocate_in_child", "--exact", "--nocapture"])
        .env("GENTS_TEST_RESERVED_PORT", port.to_string())
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "child reused reserved port: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Two CI runners on one machine give their jobs different TMPDIRs but share
/// the host's ports: a process with another TMPDIR must still see the
/// reservation (`TestPortReservation.tla`, `PerRunner`).
#[test]
fn reservation_holds_for_a_process_with_another_temp_directory() -> anyhow::Result<()> {
    let port = ports::allocate_port()?;
    let other_temp = tempfile::tempdir()?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["allocate_in_child", "--exact", "--nocapture"])
        .env("GENTS_TEST_RESERVED_PORT", port.to_string())
        .env("TMPDIR", other_temp.path())
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "a child with another TMPDIR reused the reserved port: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn allocate_in_child() -> anyhow::Result<()> {
    let Ok(reserved) = std::env::var("GENTS_TEST_RESERVED_PORT") else {
        return Ok(());
    };
    let reserved: u16 = reserved.parse()?;
    let allocated = ports::allocate_port()?;
    anyhow::ensure!(allocated != reserved, "allocated reserved port {reserved}");
    Ok(())
}

/// CI runners run as systemd services with `PrivateTmp=yes`, so no path
/// under `/tmp` is shared between two jobs on one machine; the reservation
/// must live in the network namespace that holds the ports
/// (`TestPortReservation.tla`, `PerRunner`). `/proc/net/unix` lists that
/// namespace's abstract sockets.
#[cfg(target_os = "linux")]
#[test]
fn reservation_lives_in_the_network_namespace() -> anyhow::Result<()> {
    let port = ports::allocate_port()?;
    let sockets = std::fs::read_to_string("/proc/net/unix")?;
    let name = format!("@gents-cli-test-port-{port}");
    anyhow::ensure!(
        sockets.lines().any(|line| line.ends_with(&name)),
        "no abstract socket {name} holds the reservation of port {port}"
    );
    Ok(())
}
