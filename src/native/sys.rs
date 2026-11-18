//! Linux syscall shims for the Spid3r OCI container runtime (`DP-7` / `R11.4`).
//!
//! Sanctioned `unsafe` home per `DP-7`, gated out of core/wasm builds via
//! `#[cfg(all(target_os = "linux", feature = "container"))]`. Zero external crates.
//! Every call site validates arguments and maps `errno` to clean errors.

#![cfg(all(target_os = "linux", feature = "container"))]

use std::ffi::{c_char, c_int, c_long, c_ulong, c_void, CString};

// Linux namespace clone flags
pub const CLONE_NEWNS: c_int = 0x0002_0000;
pub const CLONE_NEWUTS: c_int = 0x0400_0000;
pub const CLONE_NEWIPC: c_int = 0x0800_0000;
pub const CLONE_NEWUSER: c_int = 0x1000_0000;
pub const CLONE_NEWPID: c_int = 0x2000_0000;
pub const CLONE_NEWNET: c_int = 0x4000_0000;

// mount / umount2 flags
pub const MS_RDONLY: c_ulong = 1;
pub const MS_NOSUID: c_ulong = 2;
pub const MS_NODEV: c_ulong = 4;
pub const MS_NOEXEC: c_ulong = 8;
pub const MS_BIND: c_ulong = 4096;
pub const MS_REC: c_ulong = 16384;
pub const MS_PRIVATE: c_ulong = 1 << 18;
pub const MNT_DETACH: c_int = 2;

// prctl constants
pub const PR_SET_NO_NEW_PRIVS: c_int = 38;
pub const PR_SET_SECCOMP: c_int = 22;
pub const SECCOMP_MODE_FILTER: c_ulong = 2;

// waitpid options
pub const WNOHANG: c_int = 1;

// x86_64 / aarch64 syscall numbers for pivot_root & seccomp/capset
#[cfg(target_arch = "x86_64")]
const SYS_PIVOT_ROOT: c_long = 155;
#[cfg(target_arch = "aarch64")]
const SYS_PIVOT_ROOT: c_long = 41;

extern "C" {
    fn unshare(flags: c_int) -> c_int;
    fn setns(fd: c_int, nstype: c_int) -> c_int;
    fn mount(
        source: *const c_char,
        target: *const c_char,
        filesystemtype: *const c_char,
        mountflags: c_ulong,
        data: *const c_void,
    ) -> c_int;
    fn umount2(target: *const c_char, flags: c_int) -> c_int;
    fn chroot(path: *const c_char) -> c_int;
    fn sethostname(name: *const c_char, len: usize) -> c_int;
    fn prctl(option: c_int, arg2: c_ulong, arg3: c_ulong, arg4: c_ulong, arg5: c_ulong) -> c_int;
    fn syscall(number: c_long, ...) -> c_long;
    fn fork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn setuid(uid: u32) -> c_int;
    fn setgid(gid: u32) -> c_int;
}

#[inline]
fn last_err(op: &str) -> String {
    format!("sys.{op}: {}", std::io::Error::last_os_error())
}

fn cstr(s: &str, label: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("sys: {label} contains interior NUL byte"))
}

/// Unshare the requested Linux namespaces (argument-whitelisted).
pub fn sys_unshare(flags: c_int) -> Result<(), String> {
    let allowed =
        CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET;
    if (flags & !allowed) != 0 {
        return Err("sys.unshare: disallowed namespace flag".to_string());
    }
    // SAFETY: `flags` is masked to documented Linux namespace bits.
    let rc = unsafe { unshare(flags) };
    if rc != 0 {
        return Err(last_err("unshare"));
    }
    Ok(())
}

/// Join an existing namespace file descriptor.
pub fn sys_setns(fd: c_int, nstype: c_int) -> Result<(), String> {
    if fd < 0 {
        return Err("sys.setns: invalid file descriptor".to_string());
    }
    // SAFETY: `fd` was validated non-negative; `setns` returns -1 on bad fd.
    let rc = unsafe { setns(fd, nstype) };
    if rc != 0 {
        return Err(last_err("setns"));
    }
    Ok(())
}

/// Mount a filesystem (e.g. `overlay`, `proc`, `tmpfs`, or bind mount).
pub fn sys_mount(
    source: &str,
    target: &str,
    fstype: Option<&str>,
    flags: c_ulong,
    data: Option<&str>,
) -> Result<(), String> {
    let cs = cstr(source, "source")?;
    let ct = cstr(target, "target")?;
    let cf = fstype.map(|f| cstr(f, "fstype")).transpose()?;
    let cd = data.map(|d| cstr(d, "data")).transpose()?;
    // SAFETY: All non-null pointers come from live `CString`s for the call duration.
    let rc = unsafe {
        mount(
            cs.as_ptr(),
            ct.as_ptr(),
            cf.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            flags,
            cd.as_ref()
                .map_or(std::ptr::null(), |c| c.as_ptr() as *const c_void),
        )
    };
    if rc != 0 {
        return Err(last_err("mount"));
    }
    Ok(())
}

/// Unmount a filesystem with `MNT_DETACH` or `0`.
pub fn sys_umount2(target: &str, flags: c_int) -> Result<(), String> {
    let ct = cstr(target, "target")?;
    // SAFETY: `ct` is a valid NUL-terminated C string.
    let rc = unsafe { umount2(ct.as_ptr(), flags) };
    if rc != 0 {
        return Err(last_err("umount2"));
    }
    Ok(())
}

/// Change the root mount (`pivot_root(new_root, put_old)`).
pub fn sys_pivot_root(new_root: &str, put_old: &str) -> Result<(), String> {
    let cn = cstr(new_root, "new_root")?;
    let co = cstr(put_old, "put_old")?;
    // SAFETY: `SYS_PIVOT_ROOT` takes two valid NUL-terminated paths.
    let rc = unsafe { syscall(SYS_PIVOT_ROOT, cn.as_ptr(), co.as_ptr()) };
    if rc != 0 {
        return Err(last_err("pivot_root"));
    }
    Ok(())
}

/// Chroot into `path`.
pub fn sys_chroot(path: &str) -> Result<(), String> {
    let cp = cstr(path, "chroot_path")?;
    // SAFETY: `cp` is a valid NUL-terminated path.
    let rc = unsafe { chroot(cp.as_ptr()) };
    if rc != 0 {
        return Err(last_err("chroot"));
    }
    Ok(())
}

/// Set UTS hostname inside a container namespace.
pub fn sys_sethostname(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("sys.sethostname: hostname must be 1..=64 bytes".to_string());
    }
    // SAFETY: `name.as_ptr()` is valid for `name.len()` bytes.
    let rc = unsafe { sethostname(name.as_ptr() as *const c_char, name.len()) };
    if rc != 0 {
        return Err(last_err("sethostname"));
    }
    Ok(())
}

/// Enable `PR_SET_NO_NEW_PRIVS` so unprivileged seccomp filters can load and setuid bits are ignored.
pub fn sys_set_no_new_privs() -> Result<(), String> {
    // SAFETY: `PR_SET_NO_NEW_PRIVS` takes `(38, 1, 0, 0, 0)` per Linux `prctl(2)`.
    let rc = unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        return Err(last_err("prctl(PR_SET_NO_NEW_PRIVS)"));
    }
    Ok(())
}

/// Drop UID/GID cleanly inside a user or rootful namespace.
pub fn sys_set_uid_gid(uid: u32, gid: u32) -> Result<(), String> {
    // SAFETY: Standard POSIX `setgid`/`setuid` calls.
    unsafe {
        if setgid(gid) != 0 {
            return Err(last_err("setgid"));
        }
        if setuid(uid) != 0 {
            return Err(last_err("setuid"));
        }
    }
    Ok(())
}

/// Fork a child process (`0` in child, `pid > 0` in parent).
pub fn sys_fork() -> Result<i32, String> {
    // SAFETY: Single-threaded pre-exec container spawn helper.
    let pid = unsafe { fork() };
    if pid < 0 {
        return Err(last_err("fork"));
    }
    Ok(pid)
}

/// Wait for a child PID (`options = 0` or `WNOHANG`).
pub fn sys_waitpid(pid: i32, options: c_int) -> Result<Option<i32>, String> {
    let mut status: c_int = 0;
    // SAFETY: `status` is a valid stack `c_int`.
    let rc = unsafe { waitpid(pid, &mut status as *mut c_int, options) };
    if rc < 0 {
        return Err(last_err("waitpid"));
    }
    if rc == 0 {
        return Ok(None);
    }
    // Decode POSIX wait status: normal exit `(status & 0x7f) == 0` -> `(status >> 8) & 0xff`.
    let code = if (status & 0x7f) == 0 {
        (status >> 8) & 0xff
    } else {
        128 + (status & 0x7f)
    };
    Ok(Some(code))
}

/// Send a signal to a container process (`pid > 0`).
pub fn sys_kill(pid: i32, sig: i32) -> Result<(), String> {
    if pid <= 0 {
        return Err("sys.kill: pid must be > 0".to_string());
    }
    // SAFETY: Validated `pid > 0`.
    let rc = unsafe { kill(pid, sig) };
    if rc != 0 {
        return Err(last_err("kill"));
    }
    Ok(())
}
