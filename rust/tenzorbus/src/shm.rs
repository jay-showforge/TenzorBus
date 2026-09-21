//! Named POSIX shared memory mapping.
//!
//! Names match the executed Python reference exactly: logical ring `frames`
//! maps the POSIX object `/tzbus_frames`, which is what
//! `multiprocessing.shared_memory.SharedMemory(name="tzbus_frames")` creates.
//! A Rust producer and a Python consumer therefore attach to the same object.

use std::ffi::CString;
use std::io;
use std::ptr;

pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

// The mapping is MAP_SHARED memory coordinated by atomics inside the region.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
        }
    }
}

pub fn object_name(logical: &str) -> String {
    format!("/tzbus_{logical}")
}

fn cname(logical: &str) -> io::Result<CString> {
    CString::new(object_name(logical)).map_err(|_| io::Error::other("ring name contains NUL"))
}

fn map_fd(fd: libc::c_int, len: usize) -> io::Result<Mapping> {
    let addr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if addr == libc::MAP_FAILED {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }
    unsafe { libc::close(fd) };
    Ok(Mapping {
        ptr: addr as *mut u8,
        len,
    })
}

/// Create the shared object. Fails if it already exists unless `force`.
pub fn create(logical: &str, len: usize, force: bool) -> io::Result<Mapping> {
    let name = cname(logical)?;
    if force {
        unsafe { libc::shm_unlink(name.as_ptr()) };
    }
    let fd = unsafe {
        libc::shm_open(
            name.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::ftruncate(fd, len as libc::off_t) } != 0 {
        let err = io::Error::last_os_error();
        unsafe {
            libc::close(fd);
            libc::shm_unlink(name.as_ptr());
        }
        return Err(err);
    }
    map_fd(fd, len)
}

/// Attach to an existing shared object, discovering its size from the fd.
pub fn attach(logical: &str) -> io::Result<Mapping> {
    let name = cname(logical)?;
    let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR, 0o600) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }
    let len = st.st_size as usize;
    if len == 0 {
        unsafe { libc::close(fd) };
        return Err(io::Error::other("shared object has zero length"));
    }
    map_fd(fd, len)
}

pub fn unlink(logical: &str) -> io::Result<()> {
    let name = cname(logical)?;
    if unsafe { libc::shm_unlink(name.as_ptr()) } != 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}

/// Fields 3 (state) and 22 (starttime) of `/proc/<pid>/stat`.
///
/// Parsed from the last `)` forward, because field 2 is the executable name in
/// parentheses and may itself contain spaces and parentheses.
fn proc_stat(pid: u32) -> Option<(char, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &text[text.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    // `rest` starts at field 3, so state is first and starttime is 20 further on.
    let state = fields.next()?.chars().next()?;
    let starttime = fields.nth(19)?.parse().ok()?;
    Some((state, starttime))
}

/// The process's start time in clock ticks since boot, used to tell a live
/// process apart from a new one that happens to have inherited its pid.
pub fn pid_start_token(pid: u32) -> u32 {
    proc_stat(pid).map_or(0, |(_, start)| start as u32)
}

/// True when the process is alive and still able to run code.
///
/// `kill(pid, 0)` is not sufficient on its own. A consumer that was the
/// producer's own child stays in the process table as a zombie until someone
/// waits on it, and `kill` reports a zombie as alive — so a supervisor that
/// spawns its workers (an entirely normal shape) would see a killed consumer
/// pin its slot forever. A zombie has been reaped by the kernel and can never
/// execute again, so it can never release a lease: it is dead for our purposes.
///
/// When `start_token` is non-zero it must match the pid's recorded start time,
/// which distinguishes the original process from an unrelated one that later
/// inherited the same pid.
pub fn pid_alive_with_token(pid: u32, start_token: u32) -> bool {
    if pid == 0 {
        return false;
    }
    match proc_stat(pid) {
        Some((state, start)) => {
            if state == 'Z' || state == 'X' || state == 'x' {
                return false;
            }
            start_token == 0 || start as u32 == start_token
        }
        None => {
            // No /proc entry. Fall back to kill(2) so a system without procfs
            // still behaves like the earlier implementation rather than
            // declaring every consumer dead.
            let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
            rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }
}

/// True when the process exists and can still run. See [`pid_alive_with_token`].
pub fn pid_alive(pid: u32) -> bool {
    pid_alive_with_token(pid, 0)
}

pub fn now_ns() -> u64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
