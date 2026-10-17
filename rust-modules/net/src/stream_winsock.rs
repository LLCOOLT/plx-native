//! The BSD-socket calls [`super`] makes, over Winsock, for the Windows desktop build.
//!
//! `stream.rs` is written against `libc`'s socket API, and on unix it names `libc` itself. On
//! Windows it names this module instead, through one `use … as sock` alias, so the transport keeps
//! a single body: every name here has the shape and meaning of the `libc` item it stands in for,
//! and the differences are absorbed in the wrappers rather than at the call sites.
//!
//! * A socket is a `SOCKET` (`UINT_PTR`), carried as the `c_int` the stream stores. Windows
//!   documents kernel handles as 32-bit significant, so the narrowing loses nothing, and
//!   `INVALID_SOCKET` narrows to `-1` — the value every `fd < 0` check already tests.
//! * Errors are Winsock codes, and the `E*` constants below ARE those codes, so `errno()`
//!   (whose Windows arm reads [`last_error`]) compares like for like. A non-blocking `connect` in
//!   progress reports `WSAEWOULDBLOCK`, so that is also `EINPROGRESS` here.
//! * `MSG_DONTWAIT` does not exist. A recv/send carrying this module's private flag bit switches
//!   the socket to non-blocking (`FIONBIO`) for that one call and back, preserving the call's
//!   error across the restore. Winsock cannot report a socket's blocking mode, so "back" means
//!   blocking — the only mode the stream leaves a socket in between calls.
//! * `fcntl(F_GETFL)` therefore reports 0 (blocking) and `fcntl(F_SETFL)` maps `O_NONBLOCK` onto
//!   `FIONBIO`, which is all `connect_timeout_cause` asks of it.
//! * `SO_RCVTIMEO`/`SO_SNDTIMEO` are a `DWORD` of milliseconds in Winsock, not a `timeval`;
//!   `getsockopt`/`setsockopt` translate for those two options only.
//! * `poll` is `WSAPoll`. Before Windows 10 2004 `WSAPoll` did not report a refused non-blocking
//!   connect, so on those builds a dead address costs the full connect budget rather than failing
//!   at once.
#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int, c_long, c_ulong, c_ushort, c_void};
use std::sync::Once;

pub use std::os::raw::c_short;

type SOCKET = usize;
const INVALID_SOCKET: SOCKET = !0;
const SOCKET_ERROR: c_int = -1;

pub const EINTR: c_int = 10004; // WSAEINTR
pub const EWOULDBLOCK: c_int = 10035; // WSAEWOULDBLOCK
pub const EAGAIN: c_int = EWOULDBLOCK;
pub const EINPROGRESS: c_int = EWOULDBLOCK;

/// Not a Winsock flag: [`recv`]/[`send`] strip it and make that one call non-blocking.
pub const MSG_DONTWAIT: c_int = 0x4000_0000;

pub const POLLIN: c_short = 0x0300; // POLLRDNORM | POLLRDBAND
pub const POLLOUT: c_short = 0x0010; // POLLWRNORM

pub const SOL_SOCKET: c_int = 0xffff;
pub const SO_SNDTIMEO: c_int = 0x1005;
pub const SO_RCVTIMEO: c_int = 0x1006;
pub const SO_ERROR: c_int = 0x1007;
pub const IPPROTO_TCP: c_int = 6;
pub const TCP_NODELAY: c_int = 0x0001;

pub const AF_UNSPEC: c_int = 0;
pub const SOCK_STREAM: c_int = 1;
pub const AI_NUMERICHOST: c_int = 0x0004;
pub const AI_NUMERICSERV: c_int = 0x0008;
pub const AI_ADDRCONFIG: c_int = 0x0400;

pub const SHUT_RDWR: c_int = 2; // SD_BOTH

pub const F_GETFL: c_int = 3;
pub const F_SETFL: c_int = 4;
pub const O_NONBLOCK: c_int = 0x0004;

pub const FIONREAD: c_long = 0x4004_667f;
const FIONBIO: c_long = 0x8004_667e_u32 as c_long;

/// Lengths are `size_t` here because `ADDRINFOA::ai_addrlen` is; the wrappers narrow to Winsock's
/// `int` lengths.
pub type socklen_t = usize;
pub type time_t = c_long;
pub type suseconds_t = c_long;

/// The `timeval` the socket-timeout options are expressed in on unix; translated to and from
/// Winsock's millisecond `DWORD` by [`getsockopt`]/[`setsockopt`].
#[repr(C)]
pub struct timeval {
    pub tv_sec: time_t,
    pub tv_usec: suseconds_t,
}

#[repr(C)]
pub struct pollfd {
    pub fd: c_int,
    pub events: c_short,
    pub revents: c_short,
}

#[repr(C)]
pub struct sockaddr {
    pub sa_family: c_ushort,
    pub sa_data: [c_char; 14],
}

/// `ADDRINFOA` — note the Windows field order (`ai_canonname` before `ai_addr`).
#[repr(C)]
pub struct addrinfo {
    pub ai_flags: c_int,
    pub ai_family: c_int,
    pub ai_socktype: c_int,
    pub ai_protocol: c_int,
    pub ai_addrlen: usize,
    pub ai_canonname: *mut c_char,
    pub ai_addr: *mut sockaddr,
    pub ai_next: *mut addrinfo,
}

#[repr(C)]
struct WSAPOLLFD {
    fd: SOCKET,
    events: c_short,
    revents: c_short,
}

#[repr(C)]
struct WSADATA {
    // Larger than either layout of the real struct (whose field order differs between 32 and
    // 64 bits); only written by WSAStartup and never read.
    _opaque: [u8; 512],
}

#[link(name = "ws2_32")]
extern "system" {
    fn WSAStartup(version: u16, data: *mut WSADATA) -> c_int;
    fn WSAGetLastError() -> c_int;
    fn WSASetLastError(err: c_int);
    #[link_name = "socket"]
    fn ws_socket(af: c_int, ty: c_int, protocol: c_int) -> SOCKET;
    #[link_name = "connect"]
    fn ws_connect(s: SOCKET, name: *const sockaddr, namelen: c_int) -> c_int;
    #[link_name = "recv"]
    fn ws_recv(s: SOCKET, buf: *mut c_char, len: c_int, flags: c_int) -> c_int;
    #[link_name = "send"]
    fn ws_send(s: SOCKET, buf: *const c_char, len: c_int, flags: c_int) -> c_int;
    #[link_name = "getsockopt"]
    fn ws_getsockopt(s: SOCKET, level: c_int, name: c_int, val: *mut c_char, len: *mut c_int) -> c_int;
    #[link_name = "setsockopt"]
    fn ws_setsockopt(s: SOCKET, level: c_int, name: c_int, val: *const c_char, len: c_int) -> c_int;
    fn ioctlsocket(s: SOCKET, cmd: c_long, argp: *mut c_ulong) -> c_int;
    fn closesocket(s: SOCKET) -> c_int;
    #[link_name = "shutdown"]
    fn ws_shutdown(s: SOCKET, how: c_int) -> c_int;
    fn WSAPoll(fds: *mut WSAPOLLFD, nfds: c_ulong, timeout: c_int) -> c_int;
    #[link_name = "getaddrinfo"]
    fn ws_getaddrinfo(
        node: *const c_char,
        service: *const c_char,
        hints: *const addrinfo,
        res: *mut *mut addrinfo,
    ) -> c_int;
    #[link_name = "freeaddrinfo"]
    fn ws_freeaddrinfo(ai: *mut addrinfo);
}

/// Winsock must be started before the first call; the count is never dropped (no `WSACleanup`),
/// which is what a process that uses sockets until it exits wants. std's own sockets start it
/// independently, and the two counts do not interfere.
fn startup() {
    static START: Once = Once::new();
    START.call_once(|| unsafe {
        let mut data = WSADATA { _opaque: [0; 512] };
        WSAStartup(0x0202, &mut data);
    });
}

/// The calling thread's last Winsock error — the `errno` the `E*` constants above compare against.
pub fn last_error() -> c_int {
    unsafe { WSAGetLastError() }
}

fn sock(fd: c_int) -> SOCKET {
    fd as isize as SOCKET
}

/// Narrow a Winsock length; the stream never asks for more than one 64 KiB buffer at a time.
fn len_int(n: usize) -> c_int {
    n.min(c_int::MAX as usize) as c_int
}

/// Set or clear `FIONBIO`. Keeps the caller's pending Winsock error intact.
unsafe fn set_nonblocking(s: SOCKET, on: bool) -> c_int {
    let mut arg: c_ulong = on.into();
    ioctlsocket(s, FIONBIO, &mut arg)
}

/// Run `op` with the socket non-blocking when `flags` carries [`MSG_DONTWAIT`], restoring
/// blocking mode afterwards without disturbing the error `op` left.
unsafe fn dontwait(s: SOCKET, flags: c_int, op: impl FnOnce(c_int) -> c_int) -> isize {
    if flags & MSG_DONTWAIT == 0 {
        return op(flags) as isize;
    }
    if set_nonblocking(s, true) != 0 {
        return -1;
    }
    let r = op(flags & !MSG_DONTWAIT);
    let err = WSAGetLastError();
    set_nonblocking(s, false);
    WSASetLastError(err);
    r as isize
}

pub unsafe fn socket(af: c_int, ty: c_int, protocol: c_int) -> c_int {
    startup();
    let s = ws_socket(af, ty, protocol);
    if s == INVALID_SOCKET {
        -1
    } else {
        s as c_int
    }
}

pub unsafe fn connect(fd: c_int, sa: *const sockaddr, salen: socklen_t) -> c_int {
    ws_connect(sock(fd), sa, len_int(salen))
}

pub unsafe fn recv(fd: c_int, buf: *mut c_void, n: usize, flags: c_int) -> isize {
    let s = sock(fd);
    dontwait(s, flags, |f| ws_recv(s, buf as *mut c_char, len_int(n), f))
}

pub unsafe fn send(fd: c_int, buf: *const c_void, n: usize, flags: c_int) -> isize {
    let s = sock(fd);
    dontwait(s, flags, |f| ws_send(s, buf as *const c_char, len_int(n), f))
}

fn is_timeout_opt(level: c_int, opt: c_int) -> bool {
    level == SOL_SOCKET && (opt == SO_RCVTIMEO || opt == SO_SNDTIMEO)
}

pub unsafe fn getsockopt(fd: c_int, level: c_int, opt: c_int, val: *mut c_void, len: *mut socklen_t) -> c_int {
    if is_timeout_opt(level, opt) {
        if *len < std::mem::size_of::<timeval>() {
            WSASetLastError(10014); // WSAEFAULT, as for a short buffer
            return SOCKET_ERROR;
        }
        let mut ms: u32 = 0;
        let mut ms_len = std::mem::size_of::<u32>() as c_int;
        if ws_getsockopt(sock(fd), level, opt, &mut ms as *mut u32 as *mut c_char, &mut ms_len) != 0 {
            return SOCKET_ERROR;
        }
        let tv = val as *mut timeval;
        (*tv).tv_sec = (ms / 1000) as time_t;
        (*tv).tv_usec = ((ms % 1000) * 1000) as suseconds_t;
        *len = std::mem::size_of::<timeval>();
        return 0;
    }
    let mut n = len_int(*len);
    let r = ws_getsockopt(sock(fd), level, opt, val as *mut c_char, &mut n);
    if r == 0 {
        *len = n.max(0) as usize;
    }
    r
}

pub unsafe fn setsockopt(fd: c_int, level: c_int, opt: c_int, val: *const c_void, len: socklen_t) -> c_int {
    if is_timeout_opt(level, opt) && len >= std::mem::size_of::<timeval>() {
        let tv = &*(val as *const timeval);
        let ms = (tv.tv_sec.max(0) as u32)
            .saturating_mul(1000)
            .saturating_add((tv.tv_usec.max(0) / 1000) as u32);
        return ws_setsockopt(
            sock(fd),
            level,
            opt,
            &ms as *const u32 as *const c_char,
            std::mem::size_of::<u32>() as c_int,
        );
    }
    ws_setsockopt(sock(fd), level, opt, val as *const c_char, len_int(len))
}

/// `F_GETFL` / `F_SETFL` with `O_NONBLOCK` only — see the module doc.
pub unsafe fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int {
    match cmd {
        F_GETFL => 0,
        F_SETFL => set_nonblocking(sock(fd), arg & O_NONBLOCK != 0),
        _ => {
            WSASetLastError(10022); // WSAEINVAL
            -1
        }
    }
}

pub unsafe fn ioctl(fd: c_int, req: c_long, arg: *mut c_int) -> c_int {
    ioctlsocket(sock(fd), req, arg as *mut c_ulong)
}

pub unsafe fn close(fd: c_int) -> c_int {
    closesocket(sock(fd))
}

pub unsafe fn shutdown(fd: c_int, how: c_int) -> c_int {
    ws_shutdown(sock(fd), how)
}

pub unsafe fn poll(fds: *mut pollfd, nfds: u32, timeout: c_int) -> c_int {
    let fds = std::slice::from_raw_parts_mut(fds, nfds as usize);
    let mut ws: Vec<WSAPOLLFD> = fds
        .iter()
        .map(|p| WSAPOLLFD { fd: sock(p.fd), events: p.events, revents: 0 })
        .collect();
    let r = WSAPoll(ws.as_mut_ptr(), nfds as c_ulong, timeout);
    for (p, w) in fds.iter_mut().zip(&ws) {
        p.revents = w.revents;
    }
    r
}

pub unsafe fn getaddrinfo(
    node: *const c_char,
    service: *const c_char,
    hints: *const addrinfo,
    res: *mut *mut addrinfo,
) -> c_int {
    startup();
    ws_getaddrinfo(node, service, hints, res)
}

pub unsafe fn freeaddrinfo(ai: *mut addrinfo) {
    ws_freeaddrinfo(ai)
}
