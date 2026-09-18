#[allow(dead_code)]
#[path = "support/ports.rs"]
mod ports;

#[test]
fn unbound_port_is_reserved_across_test_processes() -> anyhow::Result<()> {
    let port = ports::allocate_port()?;
    // No listener exists yet: the CLI child has not started. A second test
    // process must still avoid the port while this process owns its allocation.
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
