use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc};

use crate::plugin::test_fixtures::{build_plugin_pack, ECHO_WAT};
use crate::plugin::{PluginBudget, PluginRunner, PluginVerdict};

const CHILD_ENV: &str = "GENTS_PLUGIN_TRAP_HANDLER_SIGNAL_CHILD";
const TEST_NAME: &str =
    "plugin::tests::trap_handler::a_signal_on_every_thread_after_a_plugin_call_does_not_abort";

const OUT_OF_BOUNDS_WAT: &str = r#"
      (module
        (memory (export "memory") 1)
        (func (export "_start")
          i32.const -1
          i32.load
          drop))
    "#;

const CHILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
static SIGNAL_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn ignore_signal(_: libc::c_int) {
    let fd = SIGNAL_PIPE_WRITE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = 1_u8;
        // SAFETY: `write` on a valid pipe descriptor is async-signal-safe.
        unsafe {
            libc::write(fd, std::ptr::from_ref(&byte).cast(), 1);
        }
    }
}

extern "C" {
    static mach_task_self_: libc::mach_port_t;
    fn mach_port_deallocate(task: libc::mach_port_t, name: libc::mach_port_t) -> libc::c_int;
}

fn signal_every_other_thread() {
    let mut pipe_fds = [-1_i32; 2];
    let mut sent = 0_usize;
    // SAFETY: plain libc/mach calls on this process's own threads; the pipe
    // descriptors, port rights and thread array `task_threads` hands out are
    // released before returning.
    unsafe {
        assert_eq!(libc::pipe(pipe_fds.as_mut_ptr()), 0);
        assert_eq!(libc::fcntl(pipe_fds[1], libc::F_SETFL, libc::O_NONBLOCK), 0);
        SIGNAL_PIPE_WRITE.store(pipe_fds[1], Ordering::Release);

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
            if thread != 0 as libc::pthread_t
                && libc::pthread_equal(thread, me) == 0
                && libc::pthread_kill(thread, libc::SIGUSR2) == 0
            {
                sent += 1;
            }
            mach_port_deallocate(task, port);
        }
        libc::vm_deallocate(
            task,
            threads as libc::vm_address_t,
            count as libc::vm_size_t * std::mem::size_of::<libc::thread_act_t>(),
        );

        let mut received = 0_usize;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while received < sent {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                break;
            };
            let timeout_ms = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
            let mut pfd = libc::pollfd {
                fd: pipe_fds[0],
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = libc::poll(&mut pfd, 1, timeout_ms.max(1));
            if ready <= 0 {
                if ready < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                    continue;
                }
                break;
            }
            let mut buf = [0_u8; 64];
            let n = libc::read(pipe_fds[0], buf.as_mut_ptr().cast(), buf.len());
            if n > 0 {
                received += n as usize;
            } else {
                break;
            }
        }
        SIGNAL_PIPE_WRITE.store(-1, Ordering::Release);
        libc::close(pipe_fds[0]);
        libc::close(pipe_fds[1]);
    }
}

fn echo_once(runner: &PluginRunner, n: u64) {
    let arguments = serde_json::json!({ "n": n });
    let outcome = runner
        .call(&arguments, &PluginBudget::default())
        .expect("call succeeds");
    assert_eq!(outcome.verdict, PluginVerdict::Success);
    assert_eq!(outcome.output, arguments);
}

#[test]
fn a_signal_on_every_thread_after_a_plugin_call_does_not_abort() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("host runtime");
        let node = runtime
            .block_on(defra_node::Node::builder().build())
            .expect("Lens engine starts before the plugin engine");
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
        runtime.block_on(node.shutdown()).expect("node shutdown");
        return;
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawns the child test");
    let pid = child.id() as libc::pid_t;
    let timed_out = Arc::new(AtomicBool::new(false));
    let watchdog_timed_out = Arc::clone(&timed_out);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if done_rx.recv_timeout(CHILD_TIMEOUT).is_err() {
            watchdog_timed_out.store(true, Ordering::SeqCst);
            // SAFETY: `pid` is our spawned child that has not been reaped yet.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    });
    let output = child.wait_with_output().expect("reaps the child");
    let _ = done_tx.send(());
    let _ = watchdog.join();
    if timed_out.load(Ordering::SeqCst) {
        panic!(
            "the child test did not finish within {CHILD_TIMEOUT:?}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
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
