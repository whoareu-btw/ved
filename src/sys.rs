//! Thin libc bindings so the editor needs no external crates.
//! Linux is the primary target; macOS constants/layouts are included but less tested.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::ManuallyDrop;
use std::os::raw::{c_char, c_int, c_ulong, c_void};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::OnceLock;

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct Termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_ispeed: u32,
    c_ospeed: u32,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct Termios {
    c_iflag: u64,
    c_oflag: u64,
    c_cflag: u64,
    c_lflag: u64,
    c_cc: [u8; 20],
    c_ispeed: u64,
    c_ospeed: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

#[repr(C)]
pub struct PollFd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}

pub const POLLIN: i16 = 0x1;
pub const POLLOUT: i16 = 0x4;
pub const POLLERR: i16 = 0x8;
pub const POLLHUP: i16 = 0x10;
pub const POLLNVAL: i16 = 0x20;

#[cfg(target_os = "linux")]
type NfdsT = c_ulong;
#[cfg(target_os = "macos")]
type NfdsT = std::os::raw::c_uint;

#[cfg(target_os = "linux")]
mod k {
    pub const TIOCGWINSZ: u64 = 0x5413;
    pub const TIOCSWINSZ: u64 = 0x5414;
    pub const TIOCSCTTY: u64 = 0x540E;
    pub const O_NONBLOCK: i32 = 0o4000;
    pub const O_NOCTTY: i32 = 0o400;
}
#[cfg(target_os = "macos")]
mod k {
    pub const TIOCGWINSZ: u64 = 0x40087468;
    pub const TIOCSWINSZ: u64 = 0x80087467;
    pub const TIOCSCTTY: u64 = 0x20007461;
    pub const O_NONBLOCK: i32 = 0x4;
    pub const O_NOCTTY: i32 = 0x20000;
}
/// Flags for opening a path we know nothing about: a named pipe with no writer must not block the
/// open (it is then refused as "not a regular file"), and a terminal device must not become ours.
pub const OPEN_UNKNOWN_FLAGS: i32 = k::O_NONBLOCK | k::O_NOCTTY;
const O_RDWR: c_int = 2;
const F_GETFL: c_int = 3;
const F_SETFL: c_int = 4;
const F_SETFD: c_int = 2;
const FD_CLOEXEC: c_int = 1;
const TCSAFLUSH: c_int = 2;
const SIGWINCH: c_int = 28;
const SIGHUP: c_int = 1;
const SIGINT: c_int = 2;
const SIGQUIT: c_int = 3;
const SIGTERM: c_int = 15;

extern "C" {
    fn tcgetattr(fd: c_int, t: *mut Termios) -> c_int;
    fn tcsetattr(fd: c_int, act: c_int, t: *const Termios) -> c_int;
    fn cfmakeraw(t: *mut Termios);
    fn ioctl(fd: c_int, req: c_ulong, ...) -> c_int;
    fn poll(fds: *mut PollFd, n: NfdsT, timeout: c_int) -> c_int;
    fn isatty(fd: c_int) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn posix_openpt(flags: c_int) -> c_int;
    fn grantpt(fd: c_int) -> c_int;
    fn unlockpt(fd: c_int) -> c_int;
    #[cfg(target_os = "linux")]
    fn ptsname_r(fd: c_int, buf: *mut c_char, len: usize) -> c_int;
    #[cfg(not(target_os = "linux"))]
    fn ptsname(fd: c_int) -> *mut c_char;
    fn setsid() -> c_int;
    fn tcgetpgrp(fd: c_int) -> c_int;
    fn signal(sig: c_int, handler: usize) -> usize;
    fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    fn pipe(fds: *mut c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn geteuid() -> u32;
}

/// SIGKILL a whole process group (a language server and the helpers it started). `pgid` must be the
/// group that was made for the child with `process_group(0)`; anything that is not a plausible pid is
/// ignored, because `kill(0, ..)` / `kill(-1, ..)` would hit far more than we mean to.
pub fn kill_group(pgid: u32) {
    if pgid > 1 && pgid <= i32::MAX as u32 {
        unsafe {
            kill(-(pgid as c_int), 9);
        }
    }
}

/// The effective user id (who owns what we create).
pub fn euid() -> u32 {
    unsafe { geteuid() }
}

pub fn is_tty(fd: RawFd) -> bool {
    unsafe { isatty(fd) == 1 }
}

/// (rows, cols) of the controlling terminal.
pub fn term_size() -> (usize, usize) {
    for fd in [1, 0, 2] {
        let mut ws = Winsize::default();
        let r = unsafe { ioctl(fd, k::TIOCGWINSZ as c_ulong, &mut ws as *mut Winsize) };
        if r == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            return (ws.ws_row as usize, ws.ws_col as usize);
        }
    }
    (24, 80)
}

static ORIG_TERMIOS: OnceLock<Termios> = OnceLock::new();

/// Escape sequences that undo everything `setup_screen` did.
pub const LEAVE_SEQ: &str = "\x1b[0m\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2004l\x1b[?25h\x1b[0 q\x1b[?1049l";
/// Mouse reports: button presses and releases and the wheel (1000), in the SGR encoding (1006), which
/// has no coordinate limit. `LEAVE_SEQ` turns them off again, also on the way out of a crash.
pub const MOUSE_ON_SEQ: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
pub const MOUSE_OFF_SEQ: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";
pub const ENTER_SEQ: &str = "\x1b[?1049h\x1b[?2004h\x1b[2J";

/// Set while the terminal's default background colour is changed (see `default_bg_seq`).
static BG_SET: AtomicBool = AtomicBool::new(false);

/// OSC 111: back to the colour the terminal was configured with.
const RESET_BG_SEQ: &str = "\x1b]111\x1b\\";

/// Can we expect the terminal to understand OSC 11 / 111? The Linux console and `dumb` terminals would
/// print the sequence as text.
pub fn terminal_takes_bg() -> bool {
    term_takes_bg(std::env::var("TERM").ok().as_deref())
}

pub fn term_takes_bg(term: Option<&str>) -> bool {
    match term {
        Some(t) => !(t.is_empty() || t == "dumb" || t.starts_with("linux")),
        None => false,
    }
}

/// The escape sequence that makes the terminal's *own* default background `rgb` (OSC 11), or puts it back
/// (OSC 111) for `None`. The margins around the character grid (window padding, and the part of a cell
/// the window is too small for) are painted with that colour, which ved cannot reach with cells.
pub fn default_bg_seq(rgb: Option<(u8, u8, u8)>) -> String {
    match rgb {
        Some((r, g, b)) => {
            BG_SET.store(true, Ordering::SeqCst);
            format!("\x1b]11;rgb:{:02x}/{:02x}/{:02x}\x1b\\", r, g, b)
        }
        None => {
            BG_SET.store(false, Ordering::SeqCst);
            RESET_BG_SEQ.to_string()
        }
    }
}

/// Put the terminal back the way we found it. Safe to call more than once and
/// from a panic hook.
pub fn restore_tty() {
    unsafe {
        let b = LEAVE_SEQ.as_bytes();
        write(1, b.as_ptr() as *const c_void, b.len());
        if BG_SET.swap(false, Ordering::SeqCst) {
            let b = RESET_BG_SEQ.as_bytes();
            write(1, b.as_ptr() as *const c_void, b.len());
        }
        if let Some(t) = ORIG_TERMIOS.get() {
            tcsetattr(0, TCSAFLUSH, t);
        }
    }
}

pub struct RawMode;

impl RawMode {
    pub fn enable() -> io::Result<RawMode> {
        unsafe {
            let mut t: Termios = std::mem::zeroed();
            if tcgetattr(0, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            let _ = ORIG_TERMIOS.set(t);
            cfmakeraw(&mut t);
            if tcsetattr(0, TCSAFLUSH, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        restore_tty();
    }
}

pub fn set_nonblocking(fd: RawFd, on: bool) {
    unsafe {
        let fl = fcntl(fd, F_GETFL);
        if fl >= 0 {
            let nf = if on { fl | k::O_NONBLOCK } else { fl & !k::O_NONBLOCK };
            fcntl(fd, F_SETFL, nf);
        }
    }
}

/// poll(2) wrapper. EINTR is reported as "0 ready" so callers just loop.
pub fn poll_fds(fds: &mut [PollFd], timeout_ms: i32) -> io::Result<usize> {
    let r = unsafe { poll(fds.as_mut_ptr(), fds.len() as NfdsT, timeout_ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(0);
        }
        return Err(e);
    }
    Ok(r as usize)
}

static WINCH_FD: AtomicI32 = AtomicI32::new(-1);
/// The signal (SIGHUP, SIGTERM...) that asked us to stop, or 0.
static TERMINATED: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_terminate(sig: c_int) {
    TERMINATED.store(sig, Ordering::SeqCst);
    let fd = WINCH_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe {
            write(fd, b"t".as_ptr() as *const c_void, 1);
        }
    }
}

/// The signal that asked us to stop, if one did. The event loop saves the unsaved work and leaves.
pub fn terminated() -> Option<i32> {
    match TERMINATED.load(Ordering::SeqCst) {
        0 => None,
        n => Some(n),
    }
}

static WINCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_sig: c_int) {
    WINCH.store(true, Ordering::Relaxed);
    let fd = WINCH_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe {
            write(fd, b"w".as_ptr() as *const c_void, 1);
        }
    }
}

/// Create the self-pipe used to wake the event loop (from SIGWINCH and from background
/// threads) and install the SIGWINCH handler. Returns the read end to poll.
pub fn create_wake() -> io::Result<RawFd> {
    let mut fds = [0 as c_int; 2];
    if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        set_nonblocking(fd, true);
        unsafe {
            fcntl(fd, F_SETFD, FD_CLOEXEC);
        }
    }
    WINCH_FD.store(fds[1], Ordering::Relaxed);
    unsafe {
        signal(SIGWINCH, on_winch as *const () as usize);
        // closing the terminal window sends SIGHUP: by default that kills us on the spot, with every
        // unsaved change gone; these let the event loop save first
        for sig in [SIGHUP, SIGTERM, SIGINT, SIGQUIT] {
            signal(sig, on_terminate as *const () as usize);
        }
    }
    Ok(fds[0])
}

/// Wake the event loop. Safe to call from any thread.
pub fn wake() {
    let fd = WINCH_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe {
            write(fd, b"w".as_ptr() as *const c_void, 1);
        }
    }
}

/// Empty the wake pipe.
pub fn drain_wake(fd: RawFd) {
    let mut f = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    let mut buf = [0u8; 64];
    while matches!(f.read(&mut buf), Ok(n) if n > 0) {}
}

pub fn take_winch() -> bool {
    WINCH.swap(false, Ordering::Relaxed)
}

/// Raw fd 0 without std's internal buffering (we poll() before reading).
pub struct RawStdin(ManuallyDrop<File>);
impl Default for RawStdin {
    fn default() -> Self {
        Self::new()
    }
}

impl RawStdin {
    pub fn new() -> RawStdin {
        RawStdin(ManuallyDrop::new(unsafe { File::from_raw_fd(0) }))
    }
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.0.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                r => return r,
            }
        }
    }
}

/// Raw fd 1, written in one go.
///
/// Returns false when the terminal is gone (EIO / EPIPE...), so the caller can save and leave rather
/// than carry on drawing into nothing. A terminal that is merely slow (stdout non-blocking and full)
/// is waited for, for a few seconds.
pub fn write_stdout(data: &[u8]) -> bool {
    let mut f = ManuallyDrop::new(unsafe { File::from_raw_fd(1) });
    let mut rest = data;
    let mut stalls = 0;
    while !rest.is_empty() {
        match f.write(rest) {
            Ok(0) => return false,
            Ok(n) => {
                rest = &rest[n..];
                stalls = 0;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                stalls += 1;
                if stalls > 100 {
                    return false;
                }
                let mut p = [PollFd { fd: 1, events: POLLOUT, revents: 0 }];
                let _ = poll_fds(&mut p, 50);
            }
            Err(_) => return false,
        }
    }
    true
}

pub struct Pty {
    pub master: File,
    pub child: Child,
}

fn set_winsize(fd: RawFd, rows: u16, cols: u16) {
    let ws = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe {
        ioctl(fd, k::TIOCSWINSZ as c_ulong, &ws as *const Winsize);
    }
}

/// The path of the slave side of a pty. `ptsname` hands out a pointer to static storage, so where
/// there is no `ptsname_r` the call is serialised and the name copied out at once.
#[cfg(target_os = "linux")]
fn pty_slave_name(master: RawFd) -> io::Result<String> {
    let mut buf = [0 as c_char; 256];
    // SAFETY: `buf` is valid for `len` bytes; ptsname_r writes a NUL-terminated name or fails
    let rc = unsafe { ptsname_r(master, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    // SAFETY: on success the buffer holds a NUL-terminated string
    let c = unsafe { CStr::from_ptr(buf.as_ptr()) };
    c.to_str().map(|s| s.to_owned()).map_err(|_| io::Error::other("bad pty name"))
}

#[cfg(not(target_os = "linux"))]
fn pty_slave_name(master: RawFd) -> io::Result<String> {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the pointer is read and copied while the lock is held
    unsafe {
        let name = ptsname(master);
        if name.is_null() {
            return Err(io::Error::last_os_error());
        }
        CStr::from_ptr(name).to_str().map(|s| s.to_owned()).map_err(|_| io::Error::other("bad pty name"))
    }
}

impl Pty {
    pub fn spawn(program: &str, args: &[String], rows: u16, cols: u16, cwd: Option<&Path>) -> io::Result<Pty> {
        unsafe {
            let m = posix_openpt(O_RDWR | k::O_NOCTTY);
            if m < 0 {
                return Err(io::Error::last_os_error());
            }
            let master = File::from_raw_fd(m);
            fcntl(m, F_SETFD, FD_CLOEXEC);
            if grantpt(m) != 0 || unlockpt(m) != 0 {
                return Err(io::Error::last_os_error());
            }
            let path = pty_slave_name(m)?;
            set_winsize(m, rows, cols);
            let slave = OpenOptions::new().read(true).write(true).open(&path)?;
            let mut cmd = Command::new(program);
            cmd.args(args);
            if let Some(d) = cwd {
                cmd.current_dir(d);
            }
            cmd.env("TERM", "xterm-256color").env("COLORTERM", "truecolor");
            cmd.env_remove("LINES").env_remove("COLUMNS");
            cmd.stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            cmd.pre_exec(|| {
                setsid();
                ioctl(0, k::TIOCSCTTY as c_ulong, 0 as c_int);
                Ok(())
            });
            let child = cmd.spawn()?;
            drop(cmd); // close our copies of the slave side
            set_nonblocking(m, true);
            Ok(Pty { master, child })
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        set_winsize(self.master.as_raw_fd(), rows, cols);
    }

    /// The process group in the foreground of the pty: whatever is reading the keyboard right now
    /// (the shell at its prompt, or a program the shell started). `None` if it cannot be told.
    pub fn foreground_pgrp(&self) -> Option<u32> {
        let p = unsafe { tcgetpgrp(self.master.as_raw_fd()) };
        if p > 0 {
            Some(p as u32)
        } else {
            None
        }
    }

    /// Write to the child, tolerating a non-blocking master.
    pub fn write_all(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            match self.master.write(data) {
                Ok(0) => return,
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let mut p = [PollFd { fd: self.master.as_raw_fd(), events: POLLOUT, revents: 0 }];
                    let _ = poll_fds(&mut p, 50);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
