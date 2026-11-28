use std::collections::BTreeSet;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::LinuxSandboxConfig;

/// Linux Landlock syscall numbers (`include/uapi/asm-generic/unistd.h`), shared
/// across x86_64 and aarch64 since Linux 5.13.
const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const FS_EXECUTE: u64 = 1 << 0;
const FS_WRITE_FILE: u64 = 1 << 1;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
const FS_REMOVE_DIR: u64 = 1 << 4;
const FS_REMOVE_FILE: u64 = 1 << 5;
const FS_MAKE_CHAR: u64 = 1 << 6;
const FS_MAKE_DIR: u64 = 1 << 7;
const FS_MAKE_REG: u64 = 1 << 8;
const FS_MAKE_SOCK: u64 = 1 << 9;
const FS_MAKE_FIFO: u64 = 1 << 10;
const FS_MAKE_BLOCK: u64 = 1 << 11;
const FS_MAKE_SYM: u64 = 1 << 12;
const FS_REFER: u64 = 1 << 13;
const FS_TRUNCATE: u64 = 1 << 14;

const NET_BIND_TCP: u64 = 1 << 0;
const NET_CONNECT_TCP: u64 = 1 << 1;

const SCOPE_SIGNAL: u64 = 1 << 1;

const FS_ABI_V1: u64 = FS_EXECUTE
    | FS_WRITE_FILE
    | FS_READ_FILE
    | FS_READ_DIR
    | FS_REMOVE_DIR
    | FS_REMOVE_FILE
    | FS_MAKE_CHAR
    | FS_MAKE_DIR
    | FS_MAKE_REG
    | FS_MAKE_SOCK
    | FS_MAKE_FIFO
    | FS_MAKE_BLOCK
    | FS_MAKE_SYM;

const FILE_READ_RIGHTS: u64 = FS_EXECUTE | FS_READ_FILE;
const FILE_WRITE_RIGHTS: u64 = FS_WRITE_FILE | FS_TRUNCATE;
const DIR_ONLY_READ_RIGHTS: u64 = FS_READ_DIR;
const DIR_ONLY_WRITE_RIGHTS: u64 = FS_REMOVE_DIR
    | FS_REMOVE_FILE
    | FS_MAKE_CHAR
    | FS_MAKE_DIR
    | FS_MAKE_REG
    | FS_MAKE_SOCK
    | FS_MAKE_FIFO
    | FS_MAKE_BLOCK
    | FS_MAKE_SYM;

/// Kernel `struct landlock_ruleset_attr` (`include/uapi/linux/landlock.h`).
/// `attr_size` passes 8 (ABI 1-3), 16 (ABI 4-5), or 24 (ABI 6+) bytes.
#[repr(C)]
#[derive(Clone, Copy)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// Kernel `struct landlock_path_beneath_attr` is `__attribute__((packed))` (12 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

struct PathRule {
    fd: OwnedFd,
    allowed_access: u64,
}

pub(crate) struct PreparedLinuxSandbox {
    ruleset_attr: LandlockRulesetAttr,
    ruleset_attr_size: usize,
    rules: Vec<PathRule>,
    seccomp_filter: Vec<SockFilter>,
}

/// Probes the running kernel's Landlock ABI version.
pub fn landlock_abi_version() -> Option<u32> {
    // SAFETY: Querying LANDLOCK_CREATE_RULESET_VERSION with a null attr
    // pointer and size 0 is a read-only kernel capability probe.
    let version = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if version >= 1 {
        Some(version as u32)
    } else {
        None
    }
}

fn handled_fs_for_abi(abi: u32) -> u64 {
    let mut rights = FS_ABI_V1;
    if abi >= 2 {
        rights |= FS_REFER;
    }
    if abi >= 3 {
        rights |= FS_TRUNCATE;
    }
    rights
}

fn rights_for(is_dir: bool, writable: bool, handled_fs: u64, deny_hard_links: bool) -> u64 {
    let mut rights = FILE_READ_RIGHTS;
    if is_dir {
        rights |= DIR_ONLY_READ_RIGHTS;
    }
    if writable {
        rights |= FILE_WRITE_RIGHTS;
        if is_dir {
            rights |= DIR_ONLY_WRITE_RIGHTS;
            if !deny_hard_links {
                rights |= FS_REFER;
            }
        }
    }
    rights & handled_fs
}

fn open_path_fd(path: &Path, follow_symlinks: bool) -> io::Result<OwnedFd> {
    let cpath = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "sandbox path contains NUL"))?;
    let mut flags = libc::O_PATH | libc::O_CLOEXEC;
    if !follow_symlinks {
        flags |= libc::O_NOFOLLOW;
    }
    // SAFETY: `cpath` is a valid NUL-terminated path string; `O_PATH` opens a
    // path-only descriptor without triggering device side effects.
    let fd = unsafe { libc::open(cpath.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a newly opened descriptor owned exclusively by this call.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

struct RuleBuilder {
    handled_fs: u64,
    deny_hard_links: bool,
    denied: BTreeSet<PathBuf>,
    ancestors_of_denied: BTreeSet<PathBuf>,
    rules: Vec<PathRule>,
}

impl RuleBuilder {
    fn new(handled_fs: u64, deny_hard_links: bool, denied_paths: &[PathBuf]) -> Self {
        let mut denied = BTreeSet::new();
        let mut ancestors_of_denied = BTreeSet::new();
        for path in denied_paths {
            let normalized = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            for ancestor in normalized.ancestors().skip(1) {
                ancestors_of_denied.insert(ancestor.to_path_buf());
            }
            denied.insert(normalized);
        }
        Self {
            handled_fs,
            deny_hard_links,
            denied,
            ancestors_of_denied,
            rules: Vec::new(),
        }
    }

    fn push_rule(
        &mut self,
        path: &Path,
        is_dir: bool,
        writable: bool,
        follow_symlinks: bool,
        required: bool,
    ) -> io::Result<()> {
        let allowed_access = rights_for(is_dir, writable, self.handled_fs, self.deny_hard_links);
        if allowed_access == 0 {
            return Ok(());
        }
        match open_path_fd(path, follow_symlinks) {
            Ok(fd) => {
                self.rules.push(PathRule { fd, allowed_access });
                Ok(())
            }
            Err(error)
                if !required
                    && matches!(
                        error.raw_os_error(),
                        Some(libc::ENOENT | libc::EACCES | libc::ELOOP | libc::ENOTDIR)
                    ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn add_subtree(&mut self, dir: &Path, writable: bool, required: bool) -> io::Result<()> {
        if self.denied.contains(dir) {
            if required {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "sandbox writable root is denied",
                ));
            }
            return Ok(());
        }
        if !self.ancestors_of_denied.contains(dir) {
            return self.push_rule(dir, true, writable, required, required);
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error)
                if !required
                    && matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let child = entry.path();
            if self.denied.contains(&child) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                self.add_subtree(&child, writable, false)?;
            } else {
                self.push_rule(&child, false, writable, false, false)?;
            }
        }
        Ok(())
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn build_seccomp_filter(
    allow_network: bool,
    landlock_handles_net: bool,
    deny_hard_links: bool,
) -> Vec<SockFilter> {
    if allow_network && !deny_hard_links {
        return Vec::new();
    }

    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_ALU_AND_K: u16 = 0x54;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;

    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO_EACCES: u32 = 0x0005_0000 | (libc::EACCES as u32);

    const OFFSET_NR: u32 = 0;
    const OFFSET_ARCH: u32 = 4;
    const OFFSET_ARG0_LO: u32 = 16;
    const OFFSET_ARG1_LO: u32 = 24;

    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xc000_00b7;

    let sys_socket = libc::SYS_socket as u32;
    let sys_linkat = libc::SYS_linkat as u32;
    #[cfg(target_arch = "x86_64")]
    let sys_link: Option<u32> = Some(libc::SYS_link as u32);
    #[cfg(target_arch = "aarch64")]
    let sys_link: Option<u32> = None;

    const SOCK_TYPE_MASK: u32 = 0x0f;

    let mut insns: Vec<SockFilter> = Vec::with_capacity(20);
    // Index 0..1: verify architecture; mismatch jumps to DENY (patched below).
    insns.push(SockFilter {
        code: BPF_LD_W_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_ARCH,
    });
    let arch_jmp = insns.len();
    insns.push(SockFilter {
        code: BPF_JMP_JEQ_K,
        jt: 0,
        jf: 0,
        k: AUDIT_ARCH,
    });
    insns.push(SockFilter {
        code: BPF_LD_W_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_NR,
    });

    let mut deny_jumps_on_true: Vec<usize> = Vec::new();
    let mut allow_jumps_on_false: Vec<usize> = Vec::new();
    let mut allow_jumps_on_true: Vec<usize> = Vec::new();

    if deny_hard_links {
        if let Some(nr) = sys_link {
            deny_jumps_on_true.push(insns.len());
            insns.push(SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 0,
                k: nr,
            });
        }
        deny_jumps_on_true.push(insns.len());
        insns.push(SockFilter {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: 0,
            k: sys_linkat,
        });
    }

    if !allow_network {
        allow_jumps_on_false.push(insns.len());
        insns.push(SockFilter {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: 0,
            k: sys_socket,
        });
        insns.push(SockFilter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: OFFSET_ARG0_LO,
        });
        if landlock_handles_net {
            // Landlock ABI >= 4 denies TCP connect/bind with EACCES while allowing
            // socket(AF_INET*, SOCK_STREAM) to construct; Seccomp blocks UDP/raw.
            insns.push(SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 1,
                jf: 0,
                k: libc::AF_INET as u32,
            });
            allow_jumps_on_false.push(insns.len());
            insns.push(SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 0,
                k: libc::AF_INET6 as u32,
            });
            insns.push(SockFilter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: OFFSET_ARG1_LO,
            });
            insns.push(SockFilter {
                code: BPF_ALU_AND_K,
                jt: 0,
                jf: 0,
                k: SOCK_TYPE_MASK,
            });
            allow_jumps_on_true.push(insns.len());
            insns.push(SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 0,
                k: libc::SOCK_STREAM as u32,
            });
            insns.push(SockFilter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ERRNO_EACCES,
            });
        } else {
            deny_jumps_on_true.push(insns.len());
            insns.push(SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 0,
                k: libc::AF_INET as u32,
            });
            deny_jumps_on_true.push(insns.len());
            insns.push(SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 0,
                k: libc::AF_INET6 as u32,
            });
        }
    }

    let allow_idx = insns.len();
    insns.push(SockFilter {
        code: BPF_RET_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    let deny_idx = insns.len();
    insns.push(SockFilter {
        code: BPF_RET_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ERRNO_EACCES,
    });

    insns[arch_jmp].jf = (deny_idx - arch_jmp - 1) as u8;
    for idx in deny_jumps_on_true {
        insns[idx].jt = (deny_idx - idx - 1) as u8;
    }
    for idx in allow_jumps_on_false {
        insns[idx].jf = (allow_idx - idx - 1) as u8;
    }
    for idx in allow_jumps_on_true {
        insns[idx].jt = (allow_idx - idx - 1) as u8;
    }
    insns
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn build_seccomp_filter(
    _allow_network: bool,
    _landlock_handles_net: bool,
    _deny_hard_links: bool,
) -> Vec<SockFilter> {
    Vec::new()
}

impl PreparedLinuxSandbox {
    pub(crate) fn prepare(config: &LinuxSandboxConfig) -> io::Result<Self> {
        let abi = landlock_abi_version().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "Linux Landlock kernel sandbox is unavailable on this host",
            )
        })?;
        let handled_fs = handled_fs_for_abi(abi);
        let landlock_handles_net = abi >= 4 && !config.allow_network;
        let handled_net = if landlock_handles_net {
            NET_BIND_TCP | NET_CONNECT_TCP
        } else {
            0
        };
        let scoped = if abi >= 6 && !config.allow_writes_outside_denied {
            SCOPE_SIGNAL
        } else {
            0
        };
        let ruleset_attr_size = if abi >= 6 {
            24
        } else if abi >= 4 {
            16
        } else {
            8
        };
        let ruleset_attr = LandlockRulesetAttr {
            handled_access_fs: handled_fs,
            handled_access_net: handled_net,
            scoped,
        };

        let mut builder =
            RuleBuilder::new(handled_fs, config.deny_hard_links, &config.denied_paths);
        builder.add_subtree(Path::new("/"), config.allow_writes_outside_denied, false)?;
        if let Some(root) = &config.writable_root {
            let canonical = std::fs::canonicalize(root)?;
            builder.add_subtree(&canonical, true, true)?;
        }
        builder.push_rule(Path::new("/dev/null"), false, true, true, false)?;

        let seccomp_filter = build_seccomp_filter(
            config.allow_network,
            landlock_handles_net,
            config.deny_hard_links,
        );
        Ok(Self {
            ruleset_attr,
            ruleset_attr_size,
            rules: builder.rules,
            seccomp_filter,
        })
    }

    /// Applies Landlock and Seccomp-BPF to the current thread/process.
    /// Async-signal-safe: allocates nothing and invokes only raw syscalls.
    pub(crate) fn apply_in_child(&self) -> io::Result<()> {
        // SAFETY: PR_SET_NO_NEW_PRIVS is required by both Landlock and
        // unprivileged Seccomp-BPF and is async-signal-safe.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `self.ruleset_attr` lives for the duration of the syscall and
        // `self.ruleset_attr_size` matches the probed Landlock ABI struct prefix.
        let ruleset_fd = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &self.ruleset_attr as *const LandlockRulesetAttr,
                self.ruleset_attr_size,
                0u32,
            )
        };
        if ruleset_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let ruleset_fd = ruleset_fd as libc::c_int;

        for rule in &self.rules {
            let attr = LandlockPathBeneathAttr {
                allowed_access: rule.allowed_access,
                parent_fd: rule.fd.as_raw_fd(),
            };
            // SAFETY: `ruleset_fd` and `attr.parent_fd` are open descriptors and
            // `attr` is valid for the duration of the syscall.
            let status = unsafe {
                libc::syscall(
                    SYS_LANDLOCK_ADD_RULE,
                    ruleset_fd,
                    LANDLOCK_RULE_PATH_BENEATH,
                    &attr as *const LandlockPathBeneathAttr,
                    0u32,
                )
            };
            if status != 0 {
                let error = io::Error::last_os_error();
                // SAFETY: `ruleset_fd` is owned by this function.
                unsafe { libc::close(ruleset_fd) };
                return Err(error);
            }
        }

        // SAFETY: `ruleset_fd` is a valid populated Landlock ruleset descriptor.
        let restrict_status =
            unsafe { libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) };
        let restrict_error = if restrict_status != 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        // SAFETY: `ruleset_fd` is no longer needed after restrict_self.
        unsafe { libc::close(ruleset_fd) };
        if let Some(error) = restrict_error {
            return Err(error);
        }

        if !self.seccomp_filter.is_empty() {
            let fprog = SockFprog {
                len: self.seccomp_filter.len() as u16,
                filter: self.seccomp_filter.as_ptr(),
            };
            // SAFETY: `fprog` points to the pre-built BPF array owned by `self`
            // and PR_SET_NO_NEW_PRIVS is already set above.
            if unsafe {
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &fprog as *const SockFprog,
                    0,
                    0,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::sync::Arc;

    fn run_sandboxed(
        config: &LinuxSandboxConfig,
        program: &str,
        args: &[&str],
    ) -> std::process::Output {
        let prepared = Arc::new(PreparedLinuxSandbox::prepare(config).expect("prepare sandbox"));
        let mut command = Command::new(program);
        command.args(args);
        // SAFETY: apply_in_child performs only raw prctl/landlock/close syscalls.
        unsafe {
            command.pre_exec(move || prepared.apply_in_child());
        }
        command.output().expect("spawn sandboxed child")
    }

    #[test]
    fn landlock_confines_writes_blocks_denied_paths_and_offline_network() {
        if landlock_abi_version().is_none() {
            return;
        }
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret_dir = tempfile::tempdir().unwrap();
        let secret_file = secret_dir.path().join("token");
        std::fs::write(&secret_file, "top-secret").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let config = LinuxSandboxConfig {
            writable_root: Some(work.path().to_path_buf()),
            allow_network: false,
            deny_hard_links: true,
            denied_paths: vec![secret_dir.path().to_path_buf()],
            allow_writes_outside_denied: false,
        };

        let ok_file = work.path().join("ok.txt");
        let outside_file = outside.path().join("escaped.txt");
        let link_file = work.path().join("linked.txt");

        let script = r#"
set -e
printf 'inside' > "$1"
printf 'discard' > /dev/null
if printf 'outside' > "$2" 2>/dev/null; then exit 11; fi
if cat "$3" >/dev/null 2>&1; then exit 12; fi
if ln "$1" "$4" 2>/dev/null; then exit 13; fi
"#;
        let output = run_sandboxed(
            &config,
            "/bin/sh",
            &[
                "-c",
                script,
                "probe",
                ok_file.to_str().unwrap(),
                outside_file.to_str().unwrap(),
                secret_file.to_str().unwrap(),
                link_file.to_str().unwrap(),
            ],
        );
        assert!(
            output.status.success(),
            "status={:?} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_to_string(&ok_file).unwrap(), "inside");
        assert!(!outside_file.exists());
        assert!(!link_file.exists());

        listener.set_nonblocking(true).unwrap();
        let net_output = run_sandboxed(
            &config,
            "/bin/bash",
            &[
                "-c",
                "exec 3<>/dev/tcp/127.0.0.1/$1",
                "probe",
                &port.to_string(),
            ],
        );
        assert!(
            !net_output.status.success(),
            "offline TCP connect unexpectedly succeeded"
        );
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == io::ErrorKind::WouldBlock),
            "offline sandbox reached local TCP listener"
        );
    }
}
