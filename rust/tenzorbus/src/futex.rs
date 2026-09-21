//! Linux futex wait/wake over words living in the shared mapping.
//!
//! The words are in MAP_SHARED memory mapped at different addresses in each
//! process, so these are *shared* futexes: FUTEX_PRIVATE_FLAG must not be set.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::shm::pid_alive_with_token;

const FUTEX_WAIT: libc::c_int = 0;
const FUTEX_WAKE: libc::c_int = 1;

fn futex(
    uaddr: *const AtomicU32,
    op: libc::c_int,
    val: u32,
    timeout: *const libc::timespec,
) -> i64 {
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            uaddr as *const u32,
            op,
            val,
            timeout,
            std::ptr::null::<u32>(),
            0u32,
        )
    }
}

/// Sleep until the word changes away from `expected`, or the timeout elapses.
/// Spurious wakeups are normal; callers must re-check their condition.
pub fn wait(word: &AtomicU32, expected: u32, timeout: Duration) {
    let ts = libc::timespec {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_nsec: timeout.subsec_nanos() as libc::c_long,
    };
    futex(word as *const AtomicU32, FUTEX_WAIT, expected, &ts);
}

pub fn wake(word: &AtomicU32, count: i32) {
    futex(
        word as *const AtomicU32,
        FUTEX_WAKE,
        count as u32,
        std::ptr::null(),
    );
}

pub fn wake_all(word: &AtomicU32) {
    wake(word, i32::MAX);
}

/// Bump a counter word and wake everyone waiting on it.
pub fn signal(word: &AtomicU32) {
    word.fetch_add(1, Ordering::Release);
    wake_all(word);
}

// ---------------------------------------------------------------------------
// Cross-process mutex with owner-death recovery.
// ---------------------------------------------------------------------------

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
const CONTENDED: u32 = 2;

/// Acquire the lock. If the recorded owner process is dead, the lock is stolen
/// so that a producer killed inside its critical section cannot wedge the ring.
pub fn lock(word: &AtomicU32, owner: &AtomicU32, owner_token: &AtomicU32, me: u32, me_token: u32) {
    if word
        .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        owner.store(me, Ordering::Relaxed);
        owner_token.store(me_token, Ordering::Relaxed);
        return;
    }
    loop {
        let previous = word.swap(CONTENDED, Ordering::Acquire);
        if previous == UNLOCKED {
            owner.store(me, Ordering::Relaxed);
            owner_token.store(me_token, Ordering::Relaxed);
            return;
        }
        wait(word, CONTENDED, Duration::from_millis(20));
        // Recovery path: the holder died without unlocking.
        if word.load(Ordering::Relaxed) != UNLOCKED {
            let holder = owner.load(Ordering::Relaxed);
            let holder_token = owner_token.load(Ordering::Relaxed);
            if holder != 0 && holder != me && !pid_alive_with_token(holder, holder_token) {
                owner.store(me, Ordering::Relaxed);
                owner_token.store(me_token, Ordering::Relaxed);
                word.store(CONTENDED, Ordering::Release);
                return;
            }
        }
    }
}

pub fn unlock(word: &AtomicU32, owner: &AtomicU32, owner_token: &AtomicU32) {
    owner.store(0, Ordering::Relaxed);
    owner_token.store(0, Ordering::Relaxed);
    if word.swap(UNLOCKED, Ordering::Release) == CONTENDED {
        wake(word, 1);
    }
}
