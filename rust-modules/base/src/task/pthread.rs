//! The pthread mutex and condition-variable calls, for the Windows desktop build.
//!
//! Two C-shaped structures lock with raw lock/unlock pairs rather than guards: the media AU queue
//! (`plx_media::aq`, a port of `aq.c` whose objects live inside a `#[repr(C)]` struct) and
//! libcurl's legacy OpenSSL lock table (`plx_net::net`). On unix they call `libc`'s pthreads
//! directly. Windows has no pthreads, so this module supplies the same names and signatures over
//! the native slim primitives, and those call sites name `libc` or this module through one `use`
//! alias with no other change:
//!
//! * a mutex is an `SRWLOCK` taken exclusively — like a default pthread mutex, not recursive;
//! * a condition variable is a `CONDITION_VARIABLE`, waited on with `SleepConditionVariableSRW`.
//!
//! Both are one pointer, valid when zeroed (`SRWLOCK_INIT` / `CONDITION_VARIABLE_INIT`), never
//! allocate and need no destruction — so `*_init` zeroes and `*_destroy` does nothing. Like their
//! pthread counterparts they must not move once in use.
//!
//! Timed waits take an ABSOLUTE deadline, as `pthread_cond_timedwait` does, on [`monotonic_now`]'s
//! clock: Windows cannot bind a condvar to a clock, so the deadline is turned back into a relative
//! wait here, against the same monotonic origin that produced it.
#![allow(non_camel_case_types)]

use std::os::raw::{c_int, c_long, c_void};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// `EBUSY`, as `pthread_mutex_trylock` reports a held mutex (the CRT's value).
pub const EBUSY: c_int = 16;
/// `ETIMEDOUT`, as `pthread_cond_timedwait` reports an expired deadline (the CRT's value).
pub const ETIMEDOUT: c_int = 138;

/// An `SRWLOCK`.
#[repr(C)]
pub struct pthread_mutex_t(*mut c_void);
/// A `CONDITION_VARIABLE`.
#[repr(C)]
pub struct pthread_cond_t(*mut c_void);
// The pointer inside is the lock word, never dereferenced by Rust; like libc's pthread types these
// are shared between threads by design.
unsafe impl Send for pthread_mutex_t {}
unsafe impl Sync for pthread_mutex_t {}
unsafe impl Send for pthread_cond_t {}
unsafe impl Sync for pthread_cond_t {}

/// Attribute objects are never used on Windows; only a null pointer is ever passed.
pub type pthread_mutexattr_t = c_void;
pub type pthread_condattr_t = c_void;

/// An absolute deadline on [`monotonic_now`]'s clock.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct timespec {
    pub tv_sec: i64,
    pub tv_nsec: c_long,
}

#[link(name = "kernel32")]
extern "system" {
    fn AcquireSRWLockExclusive(lock: *mut pthread_mutex_t);
    fn ReleaseSRWLockExclusive(lock: *mut pthread_mutex_t);
    fn TryAcquireSRWLockExclusive(lock: *mut pthread_mutex_t) -> u8;
    fn SleepConditionVariableSRW(
        cond: *mut pthread_cond_t,
        lock: *mut pthread_mutex_t,
        millis: u32,
        flags: u32,
    ) -> c_int;
    fn WakeConditionVariable(cond: *mut pthread_cond_t);
    fn WakeAllConditionVariable(cond: *mut pthread_cond_t);
}

const INFINITE: u32 = u32::MAX;

pub unsafe fn pthread_mutex_init(m: *mut pthread_mutex_t, _attr: *const pthread_mutexattr_t) -> c_int {
    m.write(pthread_mutex_t(std::ptr::null_mut()));
    0
}

pub unsafe fn pthread_mutex_destroy(_m: *mut pthread_mutex_t) -> c_int {
    0
}

pub unsafe fn pthread_mutex_lock(m: *mut pthread_mutex_t) -> c_int {
    AcquireSRWLockExclusive(m);
    0
}

pub unsafe fn pthread_mutex_unlock(m: *mut pthread_mutex_t) -> c_int {
    ReleaseSRWLockExclusive(m);
    0
}

pub unsafe fn pthread_mutex_trylock(m: *mut pthread_mutex_t) -> c_int {
    if TryAcquireSRWLockExclusive(m) != 0 {
        0
    } else {
        EBUSY
    }
}

pub unsafe fn pthread_cond_init(c: *mut pthread_cond_t, _attr: *const pthread_condattr_t) -> c_int {
    c.write(pthread_cond_t(std::ptr::null_mut()));
    0
}

pub unsafe fn pthread_cond_destroy(_c: *mut pthread_cond_t) -> c_int {
    0
}

pub unsafe fn pthread_cond_signal(c: *mut pthread_cond_t) -> c_int {
    WakeConditionVariable(c);
    0
}

pub unsafe fn pthread_cond_broadcast(c: *mut pthread_cond_t) -> c_int {
    WakeAllConditionVariable(c);
    0
}

pub unsafe fn pthread_cond_wait(c: *mut pthread_cond_t, m: *mut pthread_mutex_t) -> c_int {
    SleepConditionVariableSRW(c, m, INFINITE, 0);
    0
}

/// Wait until signalled or until the absolute `deadline` on [`monotonic_now`]'s clock. A spurious
/// or early return is allowed, exactly as for the pthread call: every caller re-checks its
/// predicate in a loop.
pub unsafe fn pthread_cond_timedwait(
    c: *mut pthread_cond_t,
    m: *mut pthread_mutex_t,
    deadline: *const timespec,
) -> c_int {
    let now = since_origin();
    let at = Duration::new((*deadline).tv_sec.max(0) as u64, (*deadline).tv_nsec.clamp(0, 999_999_999) as u32);
    let Some(left) = at.checked_sub(now) else {
        return ETIMEDOUT;
    };
    // Rounded UP to whole milliseconds so a sub-millisecond remainder still waits rather than
    // spinning back with an immediate timeout; clamped below INFINITE.
    let ms = left.as_nanos().div_ceil(1_000_000).min(u128::from(INFINITE - 1)) as u32;
    if SleepConditionVariableSRW(c, m, ms, 0) != 0 {
        0
    } else {
        ETIMEDOUT
    }
}

/// The monotonic clock [`pthread_cond_timedwait`] measures deadlines on: time since this module
/// was first asked, from `Instant` (`QueryPerformanceCounter`), so a wall-clock step cannot
/// stretch a timed wait.
pub fn monotonic_now() -> timespec {
    let d = since_origin();
    timespec { tv_sec: d.as_secs() as i64, tv_nsec: d.subsec_nanos() as c_long }
}

fn since_origin() -> Duration {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed()
}
