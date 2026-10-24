type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[allow(dead_code)]
#[path = "support/ports.rs"]
mod ports;

#[test]
fn unbound_port_is_reserved_across_test_processes() -> TestResult<()> {
    let port = ports::allocate_port()?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["allocate_in_child", "--exact", "--nocapture"])
        .env("GENTS_TEST_RESERVED_PORT", port.to_string())
        .output()?;
    assert!(
        output.status.success(),
        "child reused reserved port: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn allocate_in_child() -> TestResult<()> {
    let Ok(reserved) = std::env::var("GENTS_TEST_RESERVED_PORT") else {
        return Ok(());
    };
    let reserved: u16 = reserved.parse()?;
    let allocated = ports::allocate_port()?;
    assert!(allocated != reserved, "allocated reserved port {reserved}");
    Ok(())
}
