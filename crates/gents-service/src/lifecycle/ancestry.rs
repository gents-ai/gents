//! Whether a process is an ancestor of this one, as the operating system
//! reports each parent. The premises and the interleavings the walk survives
//! are Lean `ServiceLifecycle.CallerObservation` and `CallerAncestry.tla`.

/// A walk longer than this has followed parents through reused process IDs.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_DEPTH: usize = 4096;

/// `Some(true)` when `ancestor` is a strict ancestor of this process,
/// `Some(false)` when the parent chain reaches its root without it, and
/// `None` when a link of the chain cannot be read or the walk outruns
/// `MAX_DEPTH`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn is_ancestor(ancestor: u32) -> Option<bool> {
    // SAFETY: getppid has no preconditions and cannot fail.
    let mut pid = u32::try_from(unsafe { libc::getppid() }).ok()?;
    for _ in 0..MAX_DEPTH {
        if pid == ancestor {
            return Some(true);
        }
        if pid <= 1 {
            return Some(false);
        }
        pid = parent_of(pid)?;
    }
    None
}

/// Other platforms have no parent reader, so the chain is never observed.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn is_ancestor(_ancestor: u32) -> Option<bool> {
    None
}

/// The short BSD record, because the kernel serves it for any user's process:
/// the full record (`PROC_PIDTBSDINFO`) refuses a process another user owns,
/// such as the root `login` or `sshd` above every terminal session, which
/// would leave the chain unobservable for every interactive caller.
#[cfg(target_os = "macos")]
fn parent_of(pid: u32) -> Option<u32> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: proc_bsdshortinfo is plain data for which all zero bytes are valid.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdshortinfo>()).ok()?;
    // SAFETY: the buffer holds one proc_bsdshortinfo of `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    (written == size).then_some(info.pbsi_ppid)
}

#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesised and may itself contain `)`.
    stat.get(stat.rfind(')')? + 2..)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn the_parent_and_its_parent_are_ancestors_and_this_process_is_not() {
        // SAFETY: getppid has no preconditions and cannot fail.
        let parent = u32::try_from(unsafe { libc::getppid() }).unwrap();
        assert_eq!(is_ancestor(parent), Some(true));
        if let Some(grandparent) = parent_of(parent).filter(|pid| *pid > 1) {
            assert_eq!(is_ancestor(grandparent), Some(true));
        }
        assert_eq!(is_ancestor(std::process::id()), Some(false));
    }
}
