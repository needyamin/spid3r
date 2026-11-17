//! Kernel-backed event loop (`event_loop_run`, `event_loop_stop`,
//! `on_readable`, `on_writable`, `on_timer`, `event_loop_unwatch`).
//!
//! Architecture (R2.3 / DP-2):
//! - A dedicated background OS thread owns the kernel readiness set
//!   (`epoll` on Linux, `kqueue` on macOS, `WSAPoll` on Windows; 2 ms probe
//!   fallback on wasm32) and receives registrations over an `mpsc` channel.
//! - Ready socket/timer callbacks are dispatched as `Job`s on `sched::Scheduler`
//!   with disarm-while-dispatching so level-triggered readiness never busy-loops.
//! - `event_loop_run()` parks the calling Spid3r task inside
//!   [`crate::sched::enter_blocked`] (never `enter_untimed_park`), so GC
//!   quiescence and the deadlock watchdog are never obstructed.

use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::sched;
use crate::value::{NativeCtx, Value};

use super::{net, timer, want_int, NativeDef};

/// Engine-agnostic callback invoker captured from the active engine (Tree, VM,
/// or AOT) when a watcher is registered.
pub type CallbackInvoker = Arc<dyn Fn(Value, Vec<Value>) -> Result<Value, String> + Send + Sync>;

thread_local! {
    static CURRENT_INVOKER: RefCell<Option<CallbackInvoker>> = const { RefCell::new(None) };
}

/// Restores the previous thread-local [`CallbackInvoker`] when dropped.
pub struct InvokerGuard(Option<CallbackInvoker>);

impl Drop for InvokerGuard {
    fn drop(&mut self) {
        CURRENT_INVOKER.with(|c| *c.borrow_mut() = self.0.take());
    }
}

static AOT_INVOKER: OnceLock<CallbackInvoker> = OnceLock::new();

/// Register the process-wide AOT callback invoker (called once from `rt_init`).
pub fn set_aot_invoker(inv: CallbackInvoker) {
    let _ = AOT_INVOKER.set(inv);
}

/// Install an engine callback invoker for the duration of the current scope.
pub fn push_invoker(inv: CallbackInvoker) -> InvokerGuard {
    let prev = CURRENT_INVOKER.with(|c| c.borrow_mut().replace(inv));
    InvokerGuard(prev)
}

fn current_invoker() -> Result<CallbackInvoker, String> {
    CURRENT_INVOKER
        .with(|c| c.borrow().clone())
        .or_else(|| AOT_INVOKER.get().cloned())
        .ok_or_else(|| "event loop callback invoker is not active".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct Handler {
    cb: Value,
    invoker: CallbackInvoker,
    in_flight: bool,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct Watchers {
    read: HashMap<i64, Handler>,
    write: HashMap<i64, Handler>,
    timers: HashMap<i64, (Value, CallbackInvoker)>,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum LoopCmd {
    WatchReadable {
        id: i64,
        cb: Value,
        invoker: CallbackInvoker,
    },
    WatchWritable {
        id: i64,
        cb: Value,
        invoker: CallbackInvoker,
    },
    WatchTimer {
        id: i64,
        cb: Value,
        invoker: CallbackInvoker,
    },
    RearmReadable(i64),
    RearmWritable(i64),
    Unwatch(i64),
    Wake,
}

struct LoopControl {
    tx: Mutex<Sender<LoopCmd>>,
    running: Mutex<bool>,
    cv: Condvar,
    stopped: AtomicBool,
    sched: Mutex<Option<Arc<sched::Scheduler>>>,
}

static CONTROL: OnceLock<LoopControl> = OnceLock::new();

fn control() -> &'static LoopControl {
    CONTROL.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        #[cfg(not(target_arch = "wasm32"))]
        spawn_loop_thread(rx);
        #[cfg(target_arch = "wasm32")]
        let _ = rx;
        LoopControl {
            tx: Mutex::new(tx),
            running: Mutex::new(false),
            cv: Condvar::new(),
            stopped: AtomicBool::new(false),
            sched: Mutex::new(None),
        }
    })
}

fn send_cmd(cmd: LoopCmd) {
    let ctl = control();
    let _ = ctl.tx.lock().unwrap().send(cmd);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    wake_pipe();
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_loop_thread(rx: Receiver<LoopCmd>) {
    std::thread::Builder::new()
        .name("spid3r-eventloop".to_string())
        .spawn(move || loop_thread_main(rx))
        .expect("failed to spawn event loop thread");
}

#[cfg(not(target_arch = "wasm32"))]
fn dispatch_socket_cb(id: i64, cb: Value, invoker: CallbackInvoker, is_write: bool) {
    let sched = control()
        .sched
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(sched::Scheduler::shared);
    sched.spawn_job(Box::new(move || {
        let res = invoker(cb, vec![Value::Int(id)]);
        send_cmd(if is_write {
            LoopCmd::RearmWritable(id)
        } else {
            LoopCmd::RearmReadable(id)
        });
        match res {
            Ok(v) => sched::TaskOutcome {
                result: Ok(v),
                thrown: None,
            },
            Err(e) => sched::TaskOutcome {
                result: Err(e),
                thrown: None,
            },
        }
    }));
}

#[cfg(not(target_arch = "wasm32"))]
fn dispatch_timer_cb(id: i64, cb: Value, invoker: CallbackInvoker) {
    let sched = control()
        .sched
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(sched::Scheduler::shared);
    sched.spawn_job(Box::new(move || match invoker(cb, vec![Value::Int(id)]) {
        Ok(v) => sched::TaskOutcome {
            result: Ok(v),
            thrown: None,
        },
        Err(e) => sched::TaskOutcome {
            result: Err(e),
            thrown: None,
        },
    }));
}

#[cfg(not(target_arch = "wasm32"))]
fn drain_cmds(rx: &Receiver<LoopCmd>, w: &mut Watchers) {
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            LoopCmd::WatchReadable { id, cb, invoker } => {
                w.read.insert(
                    id,
                    Handler {
                        cb,
                        invoker,
                        in_flight: false,
                    },
                );
            }
            LoopCmd::WatchWritable { id, cb, invoker } => {
                w.write.insert(
                    id,
                    Handler {
                        cb,
                        invoker,
                        in_flight: false,
                    },
                );
            }
            LoopCmd::WatchTimer { id, cb, invoker } => {
                w.timers.insert(id, (cb, invoker));
            }
            LoopCmd::RearmReadable(id) => {
                if net::is_open(id) {
                    if let Some(h) = w.read.get_mut(&id) {
                        h.in_flight = false;
                    }
                } else {
                    w.read.remove(&id);
                }
            }
            LoopCmd::RearmWritable(id) => {
                if net::is_open(id) {
                    if let Some(h) = w.write.get_mut(&id) {
                        h.in_flight = false;
                    }
                } else {
                    w.write.remove(&id);
                }
            }
            LoopCmd::Unwatch(id) => {
                w.read.remove(&id);
                w.write.remove(&id);
            }
            LoopCmd::Wake => {}
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fire_due_timers(w: &mut Watchers) {
    let fired = timer::pop_expired(sched::now_ms());
    for tid in fired {
        if let Some((cb, invoker)) = w.timers.get(&tid).cloned() {
            if !timer::is_pending(tid) {
                w.timers.remove(&tid);
            }
            dispatch_timer_cb(tid, cb, invoker);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn next_poll_timeout_ms(max_ms: i32) -> i32 {
    match timer::next_deadline_ms() {
        Some(dl) => {
            let rem = dl.saturating_sub(sched::now_ms());
            if rem <= 0 {
                0
            } else {
                rem.min(max_ms as i64) as i32
            }
        }
        None => max_ms,
    }
}

// --------------------------------------------------------- platform backends

#[cfg(any(target_os = "linux", target_os = "macos"))]
static WAKE_WRITE_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wake_pipe() {
    let fd = WAKE_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte: u8 = 1;
        // SAFETY: `fd` is the valid write-end of the self-pipe created on loop init.
        unsafe {
            unix_sys::write(fd, &byte as *const u8 as *const std::ffi::c_void, 1);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix_sys {
    use std::ffi::c_void;
    extern "C" {
        pub fn pipe(fds: *mut i32) -> i32;
        pub fn read(fd: i32, buf: *mut c_void, count: usize) -> isize;
        pub fn write(fd: i32, buf: *const c_void, count: usize) -> isize;
    }
}

#[cfg(target_os = "linux")]
mod epoll_sys {
    // Hand-declared Linux epoll ABI (no libc crate dependency).
    #[repr(C, packed)]
    #[derive(Clone, Copy)]
    pub struct EpollEvent {
        pub events: u32,
        pub data: u64,
    }
    pub const EPOLLIN: u32 = 0x001;
    pub const EPOLLOUT: u32 = 0x004;
    pub const EPOLLERR: u32 = 0x008;
    pub const EPOLLHUP: u32 = 0x010;
    pub const EPOLL_CTL_ADD: i32 = 1;
    pub const EPOLL_CTL_DEL: i32 = 2;
    pub const EPOLL_CLOEXEC: i32 = 0x80000;
    extern "C" {
        pub fn epoll_create1(flags: i32) -> i32;
        pub fn epoll_ctl(epfd: i32, op: i32, fd: i32, event: *mut EpollEvent) -> i32;
        pub fn epoll_wait(epfd: i32, events: *mut EpollEvent, maxevents: i32, timeout: i32) -> i32;
    }
}

#[cfg(target_os = "linux")]
fn loop_thread_main(rx: Receiver<LoopCmd>) {
    let mut pipe_fds = [-1i32; 2];
    // SAFETY: `pipe_fds` is a 2-element stack buffer for `pipe(2)`.
    if unsafe { unix_sys::pipe(pipe_fds.as_mut_ptr()) } == 0 {
        WAKE_WRITE_FD.store(pipe_fds[1], Ordering::Relaxed);
    }
    // SAFETY: `epoll_create1(EPOLL_CLOEXEC)` creates a new epoll instance.
    let epfd = unsafe { epoll_sys::epoll_create1(epoll_sys::EPOLL_CLOEXEC) };
    if epfd >= 0 && pipe_fds[0] >= 0 {
        let mut ev = epoll_sys::EpollEvent {
            events: epoll_sys::EPOLLIN,
            data: u64::MAX,
        };
        // SAFETY: registering the pipe read-end into `epfd`.
        unsafe {
            epoll_sys::epoll_ctl(epfd, epoll_sys::EPOLL_CTL_ADD, pipe_fds[0], &mut ev);
        }
    }

    let mut watchers = Watchers::default();
    let mut events = vec![epoll_sys::EpollEvent { events: 0, data: 0 }; 64];

    loop {
        drain_cmds(&rx, &mut watchers);
        fire_due_timers(&mut watchers);

        if epfd < 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        // Sync active fds into epoll (handles are small; rebuild registration set).
        let mut active: Vec<(i64, i32, u32)> = Vec::new();
        for (&id, h) in &watchers.read {
            if !h.in_flight {
                if let Some(fd) = net::raw_fd_of(id) {
                    active.push((id, fd, epoll_sys::EPOLLIN));
                }
            }
        }
        for (&id, h) in &watchers.write {
            if !h.in_flight {
                if let Some(fd) = net::raw_fd_of(id) {
                    if let Some(existing) = active.iter_mut().find(|x| x.0 == id) {
                        existing.2 |= epoll_sys::EPOLLOUT;
                    } else {
                        active.push((id, fd, epoll_sys::EPOLLOUT));
                    }
                }
            }
        }
        for &(id, fd, mask) in &active {
            let mut ev = epoll_sys::EpollEvent {
                events: mask,
                data: id as u64,
            };
            // SAFETY: adding armed socket fd to `epfd`.
            unsafe {
                epoll_sys::epoll_ctl(epfd, epoll_sys::EPOLL_CTL_ADD, fd, &mut ev);
            }
        }

        let timeout = next_poll_timeout_ms(50);
        // SAFETY: `events` has capacity 64 and `epfd` is open.
        let n = unsafe { epoll_sys::epoll_wait(epfd, events.as_mut_ptr(), 64, timeout) };

        for &(_, fd, _) in &active {
            // SAFETY: removing armed socket fd after wait so closed fds never linger.
            unsafe {
                epoll_sys::epoll_ctl(epfd, epoll_sys::EPOLL_CTL_DEL, fd, std::ptr::null_mut());
            }
        }

        if n > 0 {
            for ev in &events[..n as usize] {
                let data = ev.data;
                let mask = ev.events;
                if data == u64::MAX {
                    let mut buf = [0u8; 64];
                    // SAFETY: drain self-pipe wakeup bytes.
                    unsafe {
                        unix_sys::read(
                            pipe_fds[0],
                            buf.as_mut_ptr() as *mut std::ffi::c_void,
                            buf.len(),
                        );
                    }
                    continue;
                }
                let id = data as i64;
                if mask & (epoll_sys::EPOLLIN | epoll_sys::EPOLLERR | epoll_sys::EPOLLHUP) != 0 {
                    if let Some(h) = watchers.read.get_mut(&id) {
                        if !h.in_flight {
                            h.in_flight = true;
                            dispatch_socket_cb(id, h.cb.clone(), h.invoker.clone(), false);
                        }
                    }
                }
                if mask & epoll_sys::EPOLLOUT != 0 {
                    if let Some(h) = watchers.write.get_mut(&id) {
                        if !h.in_flight {
                            h.in_flight = true;
                            dispatch_socket_cb(id, h.cb.clone(), h.invoker.clone(), true);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod kqueue_sys {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Kevent {
        pub ident: usize,
        pub filter: i16,
        pub flags: u16,
        pub fflags: u32,
        pub data: isize,
        pub udata: *mut c_void,
    }
    #[repr(C)]
    pub struct Timespec {
        pub tv_sec: isize,
        pub tv_nsec: isize,
    }
    pub const EVFILT_READ: i16 = -1;
    pub const EVFILT_WRITE: i16 = -2;
    pub const EV_ADD: u16 = 0x0001;
    pub const EV_ONESHOT: u16 = 0x0010;
    pub const EV_EOF: u16 = 0x8000;
    extern "C" {
        pub fn kqueue() -> i32;
        pub fn kevent(
            kq: i32,
            changelist: *const Kevent,
            nchanges: i32,
            eventlist: *mut Kevent,
            nevents: i32,
            timeout: *const Timespec,
        ) -> i32;
    }
}

#[cfg(target_os = "macos")]
fn loop_thread_main(rx: Receiver<LoopCmd>) {
    let mut pipe_fds = [-1i32; 2];
    // SAFETY: `pipe_fds` is a 2-element stack buffer for `pipe(2)`.
    if unsafe { unix_sys::pipe(pipe_fds.as_mut_ptr()) } == 0 {
        WAKE_WRITE_FD.store(pipe_fds[1], Ordering::Relaxed);
    }
    // SAFETY: `kqueue()` allocates a kernel event queue fd.
    let kq = unsafe { kqueue_sys::kqueue() };
    let mut watchers = Watchers::default();
    let mut out = vec![
        kqueue_sys::Kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        64
    ];

    loop {
        drain_cmds(&rx, &mut watchers);
        fire_due_timers(&mut watchers);

        if kq < 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        let mut changes: Vec<kqueue_sys::Kevent> = Vec::new();
        if pipe_fds[0] >= 0 {
            changes.push(kqueue_sys::Kevent {
                ident: pipe_fds[0] as usize,
                filter: kqueue_sys::EVFILT_READ,
                flags: kqueue_sys::EV_ADD | kqueue_sys::EV_ONESHOT,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            });
        }
        for (&id, h) in &watchers.read {
            if !h.in_flight {
                if let Some(fd) = net::raw_fd_of(id) {
                    changes.push(kqueue_sys::Kevent {
                        ident: fd as usize,
                        filter: kqueue_sys::EVFILT_READ,
                        flags: kqueue_sys::EV_ADD | kqueue_sys::EV_ONESHOT,
                        fflags: 0,
                        data: 0,
                        udata: id as usize as *mut std::ffi::c_void,
                    });
                }
            }
        }
        for (&id, h) in &watchers.write {
            if !h.in_flight {
                if let Some(fd) = net::raw_fd_of(id) {
                    changes.push(kqueue_sys::Kevent {
                        ident: fd as usize,
                        filter: kqueue_sys::EVFILT_WRITE,
                        flags: kqueue_sys::EV_ADD | kqueue_sys::EV_ONESHOT,
                        fflags: 0,
                        data: 0,
                        udata: id as usize as *mut std::ffi::c_void,
                    });
                }
            }
        }

        let ms = next_poll_timeout_ms(50);
        let ts = kqueue_sys::Timespec {
            tv_sec: (ms / 1000) as isize,
            tv_nsec: ((ms % 1000) as isize) * 1_000_000,
        };
        // SAFETY: `changes` and `out` are valid slices for `kevent`.
        let n = unsafe {
            kqueue_sys::kevent(
                kq,
                changes.as_ptr(),
                changes.len() as i32,
                out.as_mut_ptr(),
                out.len() as i32,
                &ts,
            )
        };

        if n > 0 {
            for ev in &out[..n as usize] {
                if pipe_fds[0] >= 0 && ev.ident == pipe_fds[0] as usize {
                    let mut buf = [0u8; 64];
                    // SAFETY: drain self-pipe wakeup bytes.
                    unsafe {
                        unix_sys::read(
                            pipe_fds[0],
                            buf.as_mut_ptr() as *mut std::ffi::c_void,
                            buf.len(),
                        );
                    }
                    continue;
                }
                let id = ev.udata as usize as i64;
                if ev.filter == kqueue_sys::EVFILT_READ || (ev.flags & kqueue_sys::EV_EOF != 0) {
                    if let Some(h) = watchers.read.get_mut(&id) {
                        if !h.in_flight {
                            h.in_flight = true;
                            dispatch_socket_cb(id, h.cb.clone(), h.invoker.clone(), false);
                        }
                    }
                }
                if ev.filter == kqueue_sys::EVFILT_WRITE {
                    if let Some(h) = watchers.write.get_mut(&id) {
                        if !h.in_flight {
                            h.in_flight = true;
                            dispatch_socket_cb(id, h.cb.clone(), h.invoker.clone(), true);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
mod win_sys {
    #[repr(C)]
    pub struct PollFd {
        pub fd: usize,
        pub events: i16,
        pub revents: i16,
    }
    pub const POLLIN: i16 = 0x0100;
    pub const POLLOUT: i16 = 0x0010;
    pub const POLLERR_HUP: i16 = 0x0003;
    extern "system" {
        pub fn WSAPoll(fd_array: *mut PollFd, fds: u32, timeout: i32) -> i32;
    }
}

#[cfg(windows)]
fn loop_thread_main(rx: Receiver<LoopCmd>) {
    let mut watchers = Watchers::default();
    loop {
        drain_cmds(&rx, &mut watchers);
        fire_due_timers(&mut watchers);

        let mut poll_ids: Vec<(i64, usize, i16)> = Vec::new();
        for (&id, h) in &watchers.read {
            if !h.in_flight {
                if let Some(fd) = net::raw_fd_of(id) {
                    poll_ids.push((id, fd, win_sys::POLLIN));
                }
            }
        }
        for (&id, h) in &watchers.write {
            if !h.in_flight {
                if let Some(fd) = net::raw_fd_of(id) {
                    if let Some(e) = poll_ids.iter_mut().find(|x| x.0 == id) {
                        e.2 |= win_sys::POLLOUT;
                    } else {
                        poll_ids.push((id, fd, win_sys::POLLOUT));
                    }
                }
            }
        }

        let timeout = next_poll_timeout_ms(10);
        if poll_ids.is_empty() {
            std::thread::sleep(Duration::from_millis(timeout.max(1) as u64));
            continue;
        }

        let mut fds: Vec<win_sys::PollFd> = poll_ids
            .iter()
            .map(|&(_, fd, events)| win_sys::PollFd {
                fd,
                events,
                revents: 0,
            })
            .collect();
        // SAFETY: `fds` is a valid contiguous buffer of `PollFd`.
        let rc = unsafe { win_sys::WSAPoll(fds.as_mut_ptr(), fds.len() as u32, timeout) };
        if rc > 0 {
            for (i, &(id, _, _)) in poll_ids.iter().enumerate() {
                let rev = fds[i].revents;
                if rev & (win_sys::POLLIN | win_sys::POLLERR_HUP) != 0 {
                    if let Some(h) = watchers.read.get_mut(&id) {
                        if !h.in_flight {
                            h.in_flight = true;
                            dispatch_socket_cb(id, h.cb.clone(), h.invoker.clone(), false);
                        }
                    }
                }
                if rev & win_sys::POLLOUT != 0 {
                    if let Some(h) = watchers.write.get_mut(&id) {
                        if !h.in_flight {
                            h.in_flight = true;
                            dispatch_socket_cb(id, h.cb.clone(), h.invoker.clone(), true);
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------ Spid3r natives

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("event_loop_run", n_event_loop_run),
        ("event_loop_stop", n_event_loop_stop),
        ("on_readable", n_on_readable),
        ("on_writable", n_on_writable),
        ("on_timer", n_on_timer),
        ("event_loop_unwatch", n_event_loop_unwatch),
    ]
}

fn n_on_readable(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    super::collections::want("on_readable", &args, 2)?;
    let id = want_int(&args[0], "on_readable")?;
    if !net::is_open(id) {
        return Err(format!("on_readable: handle {} is not open", id));
    }
    let invoker = current_invoker()?;
    send_cmd(LoopCmd::WatchReadable {
        id,
        cb: args[1].clone(),
        invoker,
    });
    Ok(Value::Null)
}

fn n_on_writable(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    super::collections::want("on_writable", &args, 2)?;
    let id = want_int(&args[0], "on_writable")?;
    if !net::is_open(id) {
        return Err(format!("on_writable: handle {} is not open", id));
    }
    let invoker = current_invoker()?;
    send_cmd(LoopCmd::WatchWritable {
        id,
        cb: args[1].clone(),
        invoker,
    });
    Ok(Value::Null)
}

fn n_on_timer(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    super::collections::want("on_timer", &args, 2)?;
    let id = want_int(&args[0], "on_timer")?;
    if !timer::is_pending(id) {
        return Err(format!("on_timer: timer {} is not pending", id));
    }
    let invoker = current_invoker()?;
    send_cmd(LoopCmd::WatchTimer {
        id,
        cb: args[1].clone(),
        invoker,
    });
    Ok(Value::Null)
}

fn n_event_loop_unwatch(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    super::collections::want("event_loop_unwatch", &args, 1)?;
    let id = want_int(&args[0], "event_loop_unwatch")?;
    send_cmd(LoopCmd::Unwatch(id));
    Ok(Value::Null)
}

fn n_event_loop_stop(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    super::collections::want("event_loop_stop", &args, 0)?;
    let ctl = control();
    {
        let mut r = ctl.running.lock().unwrap();
        if *r {
            ctl.stopped.store(true, Ordering::SeqCst);
            *r = false;
            ctl.cv.notify_all();
        }
    }
    send_cmd(LoopCmd::Wake);
    Ok(Value::Null)
}

fn n_event_loop_run(ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    super::collections::want("event_loop_run", &args, 0)?;
    let ctl = control();
    *ctl.sched.lock().unwrap() = Some(ctx.sched.clone());

    // Park inside `enter_blocked()` (never `enter_untimed_park`) so GC
    // quiescence and the deadlock watchdog are never blocked while the loop
    // waits for stop or process shutdown.
    let _blocked = sched::enter_blocked();
    let mut r = ctl.running.lock().unwrap();
    *r = true;
    ctl.stopped.store(false, Ordering::SeqCst);
    drop(r);
    send_cmd(LoopCmd::Wake);

    let mut r = ctl.running.lock().unwrap();
    while *r && !ctl.stopped.load(Ordering::SeqCst) && !super::shutdown::is_requested() {
        if sched::current_task_cancelled() {
            break;
        }
        let (guard, _) = ctl.cv.wait_timeout(r, Duration::from_millis(25)).unwrap();
        r = guard;
    }
    *r = false;
    ctl.stopped.store(false, Ordering::SeqCst);
    Ok(Value::Null)
}
