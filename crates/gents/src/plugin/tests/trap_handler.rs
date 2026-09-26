//! Wasmtime's process-wide trap handler thread must survive the signals the
//! runtime installs handlers for, and still turn a guest fault into an error.

use super::{build_plugin_pack, ECHO_WAT};
use crate::plugin::{PluginBudget, PluginRunner, PluginVerdict};

const CHILD_ENV: &str = "GENTS_PLUGIN_TRAP_HANDLER_SIGNAL_CHILD";
const TEST_NAME: &str =
    "plugin::tests::trap_handler::a_signal_on_every_thread_after_a_plugin_call_does_not_abort";

/// Loads past the end of its one-page memory. Wasmtime elides the bounds
/// check and lets the guard page fault, so the trap reaches the Mach
/// exception handler thread rather than an inline check.
const OUT_OF_BOUNDS_WAT: &str = r#"
      (module
        (memory (export "memory") 1)
        (func (export "_start")
          i32.const -1
          i32.load
          drop))
    "#;

const CHILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

extern "C" fn ignore_signal(_: libc::c_int) {}

extern "C" {
    static mach_task_self_: libc::mach_port_t;
    fn mach_port_deallocate(task: libc::mach_port_t, name: libc::mach_port_t) -> libc::c_int;
}

/// Delivers `SIGUSR2`, with a handler installed, to every other thread of
/// this process, the way the kernel may deliver a process-directed signal
/// such as tokio's `SIGCHLD` to any thread that does not block it.
fn signal_every_other_thread() {
    // SAFETY: plain libc/mach calls on this process's own threads; the port
    // rights and thread array `task_threads` hands out are released below.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = ignore_signal as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGUSR2, &action, std::ptr::null_mut()),
            0
        );

        let task = mach_task_self_;
        let me = libc::pthread_self();
        let mut threads: libc::thread_act_array_t = std::ptr::null_mut();
        let mut count: libc::mach_msg_type_number_t = 0;
        assert_eq!(libc::task_threads(task, &mut threads, &mut count), 0);
        for index in 0..count as usize {
            let port = *threads.add(index);
            let thread = libc::pthread_from_mach_thread_np(port);
            if thread != 0 as libc::pthread_t && libc::pthread_equal(thread, me) == 0 {
                libc::pthread_kill(thread, libc::SIGUSR2);
            }
            mach_port_deallocate(task, port);
        }
        libc::vm_deallocate(
            task,
            threads as libc::vm_address_t,
            count as libc::vm_size_t * std::mem::size_of::<libc::thread_act_t>(),
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
}

fn echo_once(runner: &PluginRunner, n: u64) {
    let arguments = serde_json::json!({ "n": n });
    let outcome = runner
        .call(&arguments, &PluginBudget::default())
        .expect("call succeeds");
    assert_eq!(outcome.verdict, PluginVerdict::Success);
    assert_eq!(outcome.output, arguments);
}

/// Runs in a child process so the signals never reach this binary's other
/// tests: the first plugin call there is what starts Wasmtime's handler thread.
#[test]
fn a_signal_on_every_thread_after_a_plugin_call_does_not_abort() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let (plugin, afb) = build_plugin_pack("signal_pack", ECHO_WAT, None);
        let echo = PluginRunner::compile(&afb, &plugin).expect("compiles");
        echo_once(&echo, 1);

        signal_every_other_thread();

        let (plugin, afb) = build_plugin_pack("fault_pack", OUT_OF_BOUNDS_WAT, None);
        let fault = PluginRunner::compile(&afb, &plugin).expect("compiles");
        let error = fault
            .call(&serde_json::json!({}), &PluginBudget::default())
            .expect_err("an out-of-bounds load is a trap, not a result");
        assert!(
            format!("{error:#}").contains("trapped"),
            "the fault must come back as a typed trap: {error:#}"
        );

        echo_once(&echo, 2);
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawns the child test");
    let deadline = std::time::Instant::now() + CHILD_TIMEOUT;
    while child.try_wait().expect("polls the child").is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().expect("reaps the child");
            panic!(
                "the child test did not finish within {CHILD_TIMEOUT:?}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("reaps the child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the child test died: {:?}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(&format!("test {TEST_NAME} ... ok"))
            && stdout.contains("test result: ok. 1 passed"),
        "the child did not run the test to the end: {stdout}"
    );
}
