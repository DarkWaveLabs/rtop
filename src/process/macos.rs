//! The native macOS process collector. `sysinfo` copies the argument area of every
//! process on every refresh (`KERN_PROCARGS2`, twice) and repeats calls that fail with
//! `EPERM` on other users' processes; this collector reads each process with the fewest
//! `libproc` calls instead. Here `ProcInfo::ident` is `p_uniqueid`: unique for each process
//! since boot, and readable for every live process, other users' ones included.
//!
//! Calls on each tick, after one `proc_listallpids`:
//!
//! - A process seen before as readable: `PROC_PIDTASKALLINFO` and `proc_pid_rusage`. A
//!   changed start time means a new process at the PID.
//! - A process seen before as denied (another user's): none on a fast tick. On a slow
//!   tick `PROC_PIDUNIQIDENTIFIERINFO` and `PROC_PIDT_SHORTBSDINFO`.
//! - A new PID: `PROC_PIDUNIQIDENTIFIERINFO`, then `PROC_PIDTASKALLINFO`. Readable: one
//!   `KERN_PROCARGS2` read for the name and command line, and `proc_pid_rusage`. Denied
//!   (`EPERM`): `PROC_PIDT_SHORTBSDINFO` and `proc_pidpath`.
//!
//! The decisions live in `State`, which reaches the kernel only through the `Kernel`
//! trait, so the tests drive it with fake call results.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::mem::{MaybeUninit, size_of};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use libc::{c_int, pid_t};
use sysinfo::System;

use super::{
    KillError, KillReport, KillRequest, KillSignal, ProcInfo, ProcState, UserNames, check_target,
    cpu_percent, io_rate, join_cmd, online_cpus, system_user_names,
};

/// `PROC_PIDUNIQIDENTIFIERINFO` from XNU `sys/proc_info.h`. Not in `libc` 0.2.190.
const PROC_PIDUNIQIDENTIFIERINFO: c_int = 17;

/// `struct proc_uniqidentifierinfo` from XNU `sys/proc_info.h`. Not in `libc` 0.2.190.
/// The kernel writes the whole struct; rtop reads only `p_uniqueid`.
#[repr(C)]
struct ProcUniqIdentifierInfo {
    /// UUID of the main executable.
    p_uuid: [u8; 16],
    /// Unique for each process since boot.
    p_uniqueid: u64,
    /// `p_uniqueid` of the parent.
    p_puniqueid: u64,
    p_idversion: i32,
    p_reserve2: u32,
    p_reserve3: u64,
    p_reserve4: u64,
}

// The C struct is 56 bytes on both arm64 and x86_64.
const _: () = assert!(size_of::<ProcUniqIdentifierInfo>() == 56);

/// The size of the `KERN_PROCARGS2` buffer when `kern.argmax` cannot be read: the
/// `ARG_MAX` of macOS.
const ARG_MAX_FALLBACK: usize = 1 << 20;
/// The largest `KERN_PROCARGS2` buffer rtop allocates.
const ARG_MAX_CAP: usize = 16 << 20;

/// Why a call failed, from `errno`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fail {
    /// `EPERM`: another user's process.
    Denied,
    /// `ESRCH`: the process is gone, or a zombie.
    Gone,
    /// Any other error, or a short read.
    Other,
}

impl Fail {
    /// Pure. Maps `errno` after a failed call.
    fn from_errno(errno: Option<i32>) -> Self {
        match errno {
            Some(libc::EPERM) => Fail::Denied,
            Some(libc::ESRCH) => Fail::Gone,
            _ => Fail::Other,
        }
    }
}

/// The fields rtop reads from `PROC_PIDTASKALLINFO`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Task {
    /// `pbi_status`: `SIDL`, `SRUN` and so on.
    status: u32,
    ppid: u32,
    /// The real UID.
    ruid: u32,
    /// `pbi_start_tvsec` and `pbi_start_tvusec`.
    start: (u64, u64),
    /// `pbi_name` and `pbi_comm`, NUL-padded.
    bsd_name: [u8; 32],
    comm: [u8; 16],
    threads: i32,
    /// Threads that are running now.
    running: i32,
    /// User plus system CPU time, in mach time units.
    cpu: u64,
    resident: u64,
}

/// The fields rtop reads from `PROC_PIDT_SHORTBSDINFO`, which works on other users'
/// processes too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Short {
    status: u32,
    ppid: u32,
    /// The real UID, shown as the user.
    ruid: u32,
    /// The effective UID, which decides if `PROC_PIDTASKALLINFO` works.
    uid: u32,
}

/// The calls the collector makes. `Live` makes them; the tests fake them.
trait Kernel {
    /// `PROC_PIDUNIQIDENTIFIERINFO`: `p_uniqueid`.
    fn unique_id(&mut self, pid: pid_t) -> Result<u64, Fail>;
    /// `PROC_PIDTASKALLINFO`.
    fn task(&mut self, pid: pid_t) -> Result<Task, Fail>;
    /// `PROC_PIDT_SHORTBSDINFO`.
    fn short(&mut self, pid: pid_t) -> Result<Short, Fail>;
    /// `proc_pid_rusage`: total bytes read from and written to disk.
    fn disk_io(&mut self, pid: pid_t) -> Option<(u64, u64)>;
    /// The file name of `proc_pidpath`.
    fn path_name(&mut self, pid: pid_t) -> Option<String>;
    /// One `KERN_PROCARGS2` read: the name and the joined command line.
    fn args(&mut self, pid: pid_t) -> Option<(String, String)>;
    /// The user list of the machine.
    fn user_names(&mut self) -> HashMap<u32, Arc<str>>;
    /// `kill`. True when it worked.
    fn kill(&mut self, pid: pid_t, signal: KillSignal) -> bool;
}

/// Owned by the sampler. Caches the identity, name, command line and the CPU and I/O
/// baselines of each process.
pub struct ProcCollector {
    live: Live,
    state: State,
    pids: Vec<pid_t>,
}

impl Default for ProcCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcCollector {
    pub fn new() -> Self {
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        let euid = unsafe { libc::geteuid() };
        Self {
            live: Live::default(),
            state: State::new(online_cpus(), euid, Timebase::read()),
            pids: Vec::new(),
        }
    }

    /// Reads all processes. `_sys` is for the `sysinfo` collector only.
    pub fn collect(&mut self, _sys: &mut System, secs: f64, slow: bool) -> Vec<ProcInfo> {
        if !list_pids(&mut self.pids) {
            // Keep the cache, so the next tick still has its baselines.
            return Vec::new();
        }
        self.state.collect(&mut self.live, &self.pids, secs, slow)
    }

    /// Reads `p_uniqueid` of the PID again and sends the signal only when it equals the
    /// `ident` of the target. It changes no baseline in the cache.
    pub fn signal(&mut self, _sys: &mut System, req: &KillRequest, self_pid: u32) -> KillReport {
        KillReport {
            request: req.clone(),
            result: signal(&mut self.live, req, self_pid),
        }
    }
}

/// The kill check: protected PIDs, then the identity, then `kill`.
fn signal(kernel: &mut impl Kernel, req: &KillRequest, self_pid: u32) -> Result<(), KillError> {
    let target = &req.target;
    check_target(target.pid, self_pid)?;
    let pid = pid_t::try_from(target.pid).map_err(|_| KillError::Gone)?;
    if kernel.unique_id(pid) != Ok(target.ident) {
        return Err(KillError::Gone);
    }
    if kernel.kill(pid, req.signal) {
        Ok(())
    } else {
        Err(KillError::Failed)
    }
}

/// Converts mach time units to nanoseconds: 125/3 on Apple silicon, 1/1 on Intel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timebase {
    numer: u32,
    denom: u32,
}

impl Timebase {
    #[allow(
        deprecated,
        reason = "libc points to the mach2 crate for mach_timebase_info; rtop adds no crate"
    )]
    fn read() -> Self {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `info` is a live, writable `mach_timebase_info` for the whole call, which
        // writes only that struct.
        let rc = unsafe { libc::mach_timebase_info(&raw mut info) };
        if rc == libc::KERN_SUCCESS && info.numer > 0 && info.denom > 0 {
            Self {
                numer: info.numer,
                denom: info.denom,
            }
        } else {
            Self { numer: 1, denom: 1 }
        }
    }

    /// Pure. Saturates at `u64::MAX`.
    fn to_ns(self, ticks: u64) -> u64 {
        let ns = u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom.max(1));
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}

/// What the collector remembers about one process between ticks.
#[derive(Clone, Debug, PartialEq)]
struct Known {
    /// `p_uniqueid`.
    ident: u64,
    /// The `State::tick` that last saw the process. The others are gone.
    tick: u64,
    name: Arc<str>,
    cmd: Arc<str>,
    access: Access,
}

#[derive(Clone, Debug, PartialEq)]
enum Access {
    /// `PROC_PIDTASKALLINFO` works.
    Readable {
        start: (u64, u64),
        /// CPU time at the last sample, in mach time units.
        cpu: u64,
        /// Disk bytes read and written at the last sample. `None` until a read works.
        io: Option<(u64, u64)>,
    },
    /// Another user's process: only `PROC_PIDT_SHORTBSDINFO` works. Read on slow ticks.
    Denied {
        parent: Option<u32>,
        ruid: u32,
        state: ProcState,
    },
}

/// What to do with a PID after its cached entry was checked.
enum Step {
    /// Show this row.
    Show(ProcInfo, u32),
    /// Read it as a new process: identity first.
    New,
    /// Leave it out this tick.
    Drop,
}

/// The collector without the system calls.
struct State {
    known: HashMap<pid_t, Known>,
    users: UserNames,
    tick: u64,
    cpus: usize,
    /// The effective UID of rtop: `PROC_PIDTASKALLINFO` works on processes with this one.
    euid: u32,
    timebase: Timebase,
}

impl State {
    fn new(cpus: usize, euid: u32, timebase: Timebase) -> Self {
        Self {
            known: HashMap::new(),
            users: UserNames::default(),
            tick: 0,
            cpus,
            euid,
            timebase,
        }
    }

    fn collect(
        &mut self,
        kernel: &mut impl Kernel,
        pids: &[pid_t],
        secs: f64,
        slow: bool,
    ) -> Vec<ProcInfo> {
        self.tick = self.tick.wrapping_add(1);
        let mut out = Vec::with_capacity(pids.len());
        // Rows without a user name, with their real UID.
        let mut missed = Vec::new();
        for &pid in pids {
            // PID 0 is `kernel_task`, which `sysinfo` never lists either.
            if pid <= 0 {
                continue;
            }
            if let Some((info, ruid)) = self.read(kernel, pid, secs, slow) {
                if info.user.is_none() {
                    missed.push((out.len(), ruid));
                }
                out.push(info);
            }
        }
        let tick = self.tick;
        self.known.retain(|_, known| known.tick == tick);
        if slow
            && !missed.is_empty()
            && self
                .users
                .reload_after_miss(Instant::now(), || kernel.user_names())
        {
            for (i, ruid) in missed {
                out[i].user = self.users.get(ruid);
            }
        }
        out
    }

    /// One PID: the row and its real UID, or `None` to leave it out.
    fn read(
        &mut self,
        kernel: &mut impl Kernel,
        pid: pid_t,
        secs: f64,
        slow: bool,
    ) -> Option<(ProcInfo, u32)> {
        let readable = self
            .known
            .get(&pid)
            .map(|known| matches!(known.access, Access::Readable { .. }));
        let step = match readable {
            None => Step::New,
            Some(true) => self.readable(kernel, pid, secs, slow),
            Some(false) => self.denied(kernel, pid, slow),
        };
        match step {
            Step::Show(info, ruid) => Some((info, ruid)),
            Step::Drop => None,
            Step::New => self.read_new(kernel, pid),
        }
    }

    /// A PID last seen as readable.
    fn readable(&mut self, kernel: &mut impl Kernel, pid: pid_t, secs: f64, slow: bool) -> Step {
        let task = match kernel.task(pid) {
            Ok(task) => task,
            Err(Fail::Gone) => return Step::Drop,
            // `EPERM`: the process changed its user, or another user's process has the
            // PID now. Either way, read its identity again.
            Err(_) => return Step::New,
        };
        let Some(known) = self.known.get_mut(&pid) else {
            return Step::New;
        };
        let Access::Readable { start, cpu, io } = &mut known.access else {
            return Step::New;
        };
        if *start != task.start {
            return Step::New;
        }
        let delta_ns = self.timebase.to_ns(task.cpu.saturating_sub(*cpu));
        *cpu = task.cpu;
        let (read_rate, write_rate) = match kernel.disk_io(pid) {
            Some(now) => {
                let first = io.is_none();
                let last = io.replace(now).unwrap_or(now);
                (
                    io_rate(now.0.saturating_sub(last.0), secs, first),
                    io_rate(now.1.saturating_sub(last.1), secs, first),
                )
            }
            None => (0, 0),
        };
        if slow
            && known.cmd.is_empty()
            && let Some((_, cmd)) = kernel.args(pid)
            && !cmd.is_empty()
        {
            known.cmd = Arc::from(cmd);
        }
        known.tick = self.tick;
        let mut info = readable_info(pid, known, &task, &self.users);
        info.cpu = cpu_percent(delta_ns, secs, self.cpus);
        info.read_rate = read_rate;
        info.write_rate = write_rate;
        Step::Show(info, task.ruid)
    }

    /// A PID last seen as denied: no call on a fast tick.
    fn denied(&mut self, kernel: &mut impl Kernel, pid: pid_t, slow: bool) -> Step {
        let euid = self.euid;
        let Some(known) = self.known.get_mut(&pid) else {
            return Step::New;
        };
        if slow {
            match kernel.unique_id(pid) {
                Ok(ident) if ident == known.ident => {}
                Ok(_) => return Step::New,
                Err(_) => return Step::Drop,
            }
            let Ok(short) = kernel.short(pid) else {
                return Step::Drop;
            };
            // It runs as rtop's user now (for example after `login` dropped root), so
            // `PROC_PIDTASKALLINFO` should work: read it as a new process.
            if short.uid == euid {
                return Step::New;
            }
            known.access = Access::Denied {
                parent: parent(short.ppid),
                ruid: short.ruid,
                state: denied_state(short.status),
            };
        }
        known.tick = self.tick;
        let Access::Denied {
            parent,
            ruid,
            state,
        } = known.access
        else {
            return Step::New;
        };
        let info = ProcInfo {
            pid: pid.unsigned_abs(),
            parent,
            ident: known.ident,
            name: Arc::clone(&known.name),
            cmd: Arc::clone(&known.cmd),
            user: self.users.get(ruid),
            state,
            ..ProcInfo::default()
        };
        Step::Show(info, ruid)
    }

    /// A PID to read as a new process. Its CPU and I/O show 0 on this first sample.
    fn read_new(&mut self, kernel: &mut impl Kernel, pid: pid_t) -> Option<(ProcInfo, u32)> {
        let ident = kernel.unique_id(pid).ok()?;
        let tick = self.tick;
        match kernel.task(pid) {
            Ok(task) => {
                let (from_args, cmd) = kernel.args(pid).unzip();
                let name = choose_name(
                    from_args,
                    || kernel.path_name(pid),
                    &task.bsd_name,
                    &task.comm,
                );
                let known = Known {
                    ident,
                    tick,
                    name: Arc::from(name),
                    cmd: Arc::from(cmd.unwrap_or_default()),
                    access: Access::Readable {
                        start: task.start,
                        cpu: task.cpu,
                        io: kernel.disk_io(pid),
                    },
                };
                let info = readable_info(pid, &known, &task, &self.users);
                self.known.insert(pid, known);
                Some((info, task.ruid))
            }
            Err(Fail::Denied) => {
                let short = kernel.short(pid).ok()?;
                let name = kernel.path_name(pid)?;
                let known = Known {
                    ident,
                    tick,
                    name: Arc::from(name),
                    cmd: Arc::from(""),
                    access: Access::Denied {
                        parent: parent(short.ppid),
                        ruid: short.ruid,
                        state: denied_state(short.status),
                    },
                };
                let info = ProcInfo {
                    pid: pid.unsigned_abs(),
                    parent: parent(short.ppid),
                    ident,
                    name: Arc::clone(&known.name),
                    cmd: Arc::clone(&known.cmd),
                    user: self.users.get(short.ruid),
                    state: denied_state(short.status),
                    ..ProcInfo::default()
                };
                self.known.insert(pid, known);
                Some((info, short.ruid))
            }
            // `ESRCH` (gone, or a zombie) or another error: leave it out.
            Err(_) => None,
        }
    }
}

/// The row of a readable process, with CPU and I/O at 0.
fn readable_info(pid: pid_t, known: &Known, task: &Task, users: &UserNames) -> ProcInfo {
    ProcInfo {
        pid: pid.unsigned_abs(),
        parent: parent(task.ppid),
        ident: known.ident,
        name: Arc::clone(&known.name),
        cmd: Arc::clone(&known.cmd),
        user: users.get(task.ruid),
        state: readable_state(task.status, task.running),
        threads: u32::try_from(task.threads).ok(),
        cpu: 0.0,
        memory: task.resident,
        read_rate: 0,
        write_rate: 0,
    }
}

/// Pure. Parent PID 0 means none, as in `sysinfo`.
fn parent(ppid: u32) -> Option<u32> {
    (ppid != 0).then_some(ppid)
}

/// Pure. The state of a readable process. macOS reports `SRUN` for most sleeping
/// processes, so the running thread count decides between Run and Sleep.
fn readable_state(status: u32, running: i32) -> ProcState {
    match status {
        libc::SIDL => ProcState::Idle,
        libc::SRUN if running > 0 => ProcState::Run,
        libc::SRUN | libc::SSLEEP => ProcState::Sleep,
        libc::SSTOP => ProcState::Stop,
        libc::SZOMB => ProcState::Zombie,
        _ => ProcState::Unknown,
    }
}

/// Pure. The state of another user's process. Without the thread counts `SRUN` says
/// nothing, so it is Unknown.
fn denied_state(status: u32) -> ProcState {
    match status {
        libc::SIDL => ProcState::Idle,
        libc::SSTOP => ProcState::Stop,
        libc::SZOMB => ProcState::Zombie,
        _ => ProcState::Unknown,
    }
}

/// Pure. The first name that is not empty: the exec path from `KERN_PROCARGS2`, then
/// `proc_pidpath` (called only when needed), then `pbi_name`, then `pbi_comm`.
fn choose_name(
    from_args: Option<String>,
    path: impl FnOnce() -> Option<String>,
    bsd_name: &[u8],
    comm: &[u8],
) -> String {
    from_args
        .filter(|name| !name.is_empty())
        .or_else(|| path().filter(|name| !name.is_empty()))
        .or_else(|| c_text(bsd_name))
        .or_else(|| c_text(comm))
        .unwrap_or_default()
}

/// Pure. The text before the first NUL, or `None` when it is empty.
fn c_text(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    (end > 0).then(|| String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// Pure. The file name of a path, as `sysinfo` takes it (`Path::file_name`, lossy UTF-8).
fn file_name(path: &[u8]) -> String {
    Path::new(OsStr::from_bytes(path))
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Pure. The NUL bytes after the NUL of the exec path in a `KERN_PROCARGS2` buffer. XNU
/// (`exec_extract_strings`) pads the saved path to a multiple of the pointer size, 8 bytes,
/// counted from the start of the path. Measured on macOS 27 (arm64) for exec paths of
/// every length modulo 8, and for every readable process.
fn path_padding(path_len: usize) -> usize {
    (8 - (path_len + 1) % 8) % 8
}

/// Pure. Parses a `KERN_PROCARGS2` buffer into the name (the file name of the exec path)
/// and the command line. The buffer holds `argc` (a native `int`), the exec path, its
/// NUL and the padding of `path_padding`, then `argc` NUL-terminated arguments and the
/// environment. The last argument may lack its NUL.
///
/// Only that padding is skipped: an empty `argv[0]` (Node's `process.title = ""` blanks
/// the whole argument area) and empty arguments after it are NULs too, and each one
/// counts towards `argc`. Apple's `ps`, `psutil` and `sysinfo` skip every NUL after the
/// path, so for each empty leading argument they read one environment string into the
/// command line. The kernel shares the bug: on macOS 27 it trims the environment from
/// another process's buffer, but it finds the end of argv the same way, so the buffer
/// then holds one environment string for each empty leading argument. Empty arguments
/// are left out of the command line, as `sysinfo` does.
///
/// If the kernel ever padded more than the rule says, the extra NULs would count as
/// empty arguments: the last arguments would be lost, but no environment text shown.
fn parse_procargs(buf: &[u8]) -> Option<(String, String)> {
    let (argc, rest) = buf.split_first_chunk::<4>()?;
    let argc = c_int::from_ne_bytes(*argc);
    let exe_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let (exe, mut rest) = rest.split_at(exe_end);
    let skip = rest
        .iter()
        .take(1 + path_padding(exe_end))
        .take_while(|&&b| b == 0)
        .count();
    rest = &rest[skip..];
    let mut args = Vec::new();
    for _ in 0..argc.max(0) {
        if rest.is_empty() {
            break;
        }
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        if end > 0 {
            args.push(OsStr::from_bytes(&rest[..end]));
        }
        rest = rest.get(end + 1..).unwrap_or_default();
    }
    Some((file_name(exe), join_cmd(args)))
}

/// Pure. The size of the `KERN_PROCARGS2` buffer from `kern.argmax`.
fn args_buffer_len(arg_max: Option<usize>) -> usize {
    arg_max
        .filter(|&n| n > 0)
        .unwrap_or(ARG_MAX_FALLBACK)
        .min(ARG_MAX_CAP)
}

/// Fills `pids` with all PIDs. False when the list cannot be read.
fn list_pids(pids: &mut Vec<pid_t>) -> bool {
    // SAFETY: a null buffer of size 0 only asks for the number of PIDs; nothing is written.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return false;
    };
    // Room for processes that start between the two calls.
    let mut room = count + count / 8 + 16;
    for _ in 0..3 {
        pids.clear();
        pids.resize(room, 0);
        let Ok(bytes) = c_int::try_from(room * size_of::<pid_t>()) else {
            return false;
        };
        // SAFETY: `pids` holds `room` initialised `pid_t`s, which is `bytes` bytes, and
        // lives until the call returns. The call writes at most `bytes` bytes and returns
        // the number of PIDs it wrote, or -1.
        let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let Ok(got) = usize::try_from(got) else {
            pids.clear();
            return false;
        };
        if got < room {
            pids.truncate(got);
            return true;
        }
        // A full buffer may have cut the list: try again with more room.
        room *= 2;
    }
    pids.clear();
    false
}

/// A C struct that `proc_pidinfo` fills for one flavour.
///
/// # Safety
///
/// All-zero bytes must be a valid value of the type, and the type must have the layout
/// that the kernel writes for `FLAVOUR`.
unsafe trait PidInfo {
    const FLAVOUR: c_int;
}

// SAFETY: libc's copy of the C struct in `sys/proc_info.h`: integers and byte arrays.
unsafe impl PidInfo for libc::proc_taskallinfo {
    const FLAVOUR: c_int = libc::PROC_PIDTASKALLINFO;
}

// SAFETY: libc's copy of the C struct in `sys/proc_info.h`: integers and byte arrays.
unsafe impl PidInfo for libc::proc_bsdshortinfo {
    const FLAVOUR: c_int = libc::PROC_PIDT_SHORTBSDINFO;
}

// SAFETY: written above from `sys/proc_info.h`, integers and bytes only, with its size
// checked at compile time.
unsafe impl PidInfo for ProcUniqIdentifierInfo {
    const FLAVOUR: c_int = PROC_PIDUNIQIDENTIFIERINFO;
}

/// `proc_pidinfo` for one flavour. A read shorter than the struct is a failure.
fn pid_info<T: PidInfo>(pid: pid_t) -> Result<T, Fail> {
    let size = c_int::try_from(size_of::<T>()).map_err(|_| Fail::Other)?;
    let mut info = MaybeUninit::<T>::zeroed();
    // SAFETY: `info` is `size` bytes, writable, and lives until the call returns.
    // `proc_pidinfo` writes at most `size` bytes and returns how many it wrote, or 0.
    let written = unsafe { libc::proc_pidinfo(pid, T::FLAVOUR, 0, info.as_mut_ptr().cast(), size) };
    if written == size {
        // SAFETY: `T: PidInfo`, so the zeroed bytes were already a valid `T`, and the
        // kernel wrote all `size` bytes of it for this flavour.
        Ok(unsafe { info.assume_init() })
    } else if written <= 0 {
        // Nothing has run since the call, so `errno` is still its own.
        Err(Fail::from_errno(
            std::io::Error::last_os_error().raw_os_error(),
        ))
    } else {
        Err(Fail::Other)
    }
}

/// The real system calls, with buffers kept between calls.
#[derive(Default)]
struct Live {
    /// `KERN_PROCARGS2` buffer of `kern.argmax` bytes, allocated on first use.
    args: Vec<u8>,
    /// `proc_pidpath` buffer.
    path: Vec<u8>,
}

impl Kernel for Live {
    fn unique_id(&mut self, pid: pid_t) -> Result<u64, Fail> {
        pid_info::<ProcUniqIdentifierInfo>(pid).map(|info| info.p_uniqueid)
    }

    fn task(&mut self, pid: pid_t) -> Result<Task, Fail> {
        let all = pid_info::<libc::proc_taskallinfo>(pid)?;
        let (bsd, task) = (&all.pbsd, &all.ptinfo);
        Ok(Task {
            status: bsd.pbi_status,
            ppid: bsd.pbi_ppid,
            ruid: bsd.pbi_ruid,
            start: (bsd.pbi_start_tvsec, bsd.pbi_start_tvusec),
            bsd_name: bsd.pbi_name.map(|c| c as u8),
            comm: bsd.pbi_comm.map(|c| c as u8),
            threads: task.pti_threadnum,
            running: task.pti_numrunning,
            cpu: task.pti_total_user.saturating_add(task.pti_total_system),
            resident: task.pti_resident_size,
        })
    }

    fn short(&mut self, pid: pid_t) -> Result<Short, Fail> {
        let info = pid_info::<libc::proc_bsdshortinfo>(pid)?;
        Ok(Short {
            status: info.pbsi_status,
            ppid: info.pbsi_ppid,
            ruid: info.pbsi_ruid,
            uid: info.pbsi_uid,
        })
    }

    fn disk_io(&mut self, pid: pid_t) -> Option<(u64, u64)> {
        let mut info = MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: for `RUSAGE_INFO_V2` the kernel writes one `rusage_info_v2` (the call
        // returns no size to check), and `info` is exactly that, writable and alive until
        // the call returns. The API types the buffer as `rusage_info_t *` (a `void **`),
        // so the pointer is cast.
        let rc =
            unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) };
        if rc != 0 {
            return None;
        }
        // SAFETY: integers and a byte array, so the zeroed bytes were already valid, and
        // the call succeeded.
        let info = unsafe { info.assume_init() };
        Some((info.ri_diskio_bytesread, info.ri_diskio_byteswritten))
    }

    fn path_name(&mut self, pid: pid_t) -> Option<String> {
        let len = usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?;
        self.path.resize(len, 0);
        let size = u32::try_from(len).ok()?;
        // SAFETY: `self.path` is `size` bytes, writable, and lives until the call
        // returns. The call writes at most `size` bytes and returns the path length.
        let n = unsafe { libc::proc_pidpath(pid, self.path.as_mut_ptr().cast(), size) };
        let n = usize::try_from(n).ok().filter(|&n| n > 0 && n <= len)?;
        Some(file_name(&self.path[..n]))
    }

    fn args(&mut self, pid: pid_t) -> Option<(String, String)> {
        if self.args.is_empty() {
            // `vec!` of zeros takes fresh pages from the system, so only the pages that
            // a read touches use memory.
            self.args = vec![0; args_buffer_len(kern_argmax())];
        }
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size: libc::size_t = self.args.len();
        // SAFETY: `mib` is 3 ints. `self.args` is `size` bytes, writable, and lives until
        // the call returns; `sysctl` writes at most `size` bytes and stores the length it
        // wrote in `size`. No new value is set (null, 0). A buffer of `kern.argmax` bytes
        // holds any argument area, so the kernel copies it whole from its start.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                self.args.as_mut_ptr().cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        parse_procargs(self.args.get(..size)?)
    }

    fn user_names(&mut self) -> HashMap<u32, Arc<str>> {
        system_user_names()
    }

    fn kill(&mut self, pid: pid_t, signal: KillSignal) -> bool {
        let signal = match signal {
            KillSignal::Term => libc::SIGTERM,
            KillSignal::Kill => libc::SIGKILL,
        };
        // SAFETY: a plain system call with no pointers.
        unsafe { libc::kill(pid, signal) == 0 }
    }
}

/// `kern.argmax`: the largest argument area of any process.
fn kern_argmax() -> Option<usize> {
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut value: c_int = 0;
    let mut size: libc::size_t = size_of::<c_int>();
    // SAFETY: `mib` is 2 ints; `value` is a writable `c_int` of `size` bytes that lives
    // until the call returns. No new value is set (null, 0).
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut value).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size != size_of::<c_int>() {
        return None;
    }
    usize::try_from(value).ok()
}

#[cfg(test)]
mod tests {
    use super::super::KillTarget;
    use super::*;

    /// A fake kernel: each map holds what a call returns for a PID; a missing PID fails
    /// as gone. `calls` logs each call.
    #[derive(Default)]
    struct Fake {
        unique: HashMap<pid_t, Result<u64, Fail>>,
        task: HashMap<pid_t, Result<Task, Fail>>,
        short: HashMap<pid_t, Result<Short, Fail>>,
        io: HashMap<pid_t, (u64, u64)>,
        path: HashMap<pid_t, String>,
        args: HashMap<pid_t, (String, String)>,
        users: HashMap<u32, Arc<str>>,
        kill_works: bool,
        calls: Vec<(&'static str, pid_t)>,
    }

    impl Fake {
        fn calls(&mut self) -> Vec<(&'static str, pid_t)> {
            std::mem::take(&mut self.calls)
        }

        /// A readable process of UID 501.
        fn readable(&mut self, pid: pid_t, ident: u64, name: &str) {
            self.unique.insert(pid, Ok(ident));
            self.task.insert(
                pid,
                Ok(Task {
                    status: libc::SRUN,
                    ppid: 1,
                    ruid: 501,
                    start: (1_000, 5),
                    threads: 4,
                    running: 1,
                    resident: 4096,
                    ..Task::default()
                }),
            );
            self.args
                .insert(pid, (name.to_string(), format!("/bin/{name} -x")));
            self.io.insert(pid, (100, 200));
        }

        /// Another user's process (UID 0).
        fn denied(&mut self, pid: pid_t, ident: u64, name: &str) {
            self.unique.insert(pid, Ok(ident));
            self.task.insert(pid, Err(Fail::Denied));
            self.short.insert(
                pid,
                Ok(Short {
                    status: libc::SRUN,
                    ppid: 1,
                    ruid: 0,
                    uid: 0,
                }),
            );
            self.path.insert(pid, name.to_string());
        }

        fn task_mut(&mut self, pid: pid_t) -> &mut Task {
            self.task.get_mut(&pid).unwrap().as_mut().unwrap()
        }
    }

    impl Kernel for Fake {
        fn unique_id(&mut self, pid: pid_t) -> Result<u64, Fail> {
            self.calls.push(("unique", pid));
            self.unique.get(&pid).copied().unwrap_or(Err(Fail::Gone))
        }
        fn task(&mut self, pid: pid_t) -> Result<Task, Fail> {
            self.calls.push(("task", pid));
            self.task.get(&pid).copied().unwrap_or(Err(Fail::Gone))
        }
        fn short(&mut self, pid: pid_t) -> Result<Short, Fail> {
            self.calls.push(("short", pid));
            self.short.get(&pid).copied().unwrap_or(Err(Fail::Gone))
        }
        fn disk_io(&mut self, pid: pid_t) -> Option<(u64, u64)> {
            self.calls.push(("rusage", pid));
            self.io.get(&pid).copied()
        }
        fn path_name(&mut self, pid: pid_t) -> Option<String> {
            self.calls.push(("path", pid));
            self.path.get(&pid).cloned()
        }
        fn args(&mut self, pid: pid_t) -> Option<(String, String)> {
            self.calls.push(("args", pid));
            self.args.get(&pid).cloned()
        }
        fn user_names(&mut self) -> HashMap<u32, Arc<str>> {
            self.calls.push(("users", 0));
            self.users.clone()
        }
        fn kill(&mut self, pid: pid_t, _signal: KillSignal) -> bool {
            self.calls.push(("kill", pid));
            self.kill_works
        }
    }

    const APPLE_SILICON: Timebase = Timebase {
        numer: 125,
        denom: 3,
    };
    const INTEL: Timebase = Timebase { numer: 1, denom: 1 };

    fn state() -> State {
        State::new(4, 501, APPLE_SILICON)
    }

    fn row(rows: &[ProcInfo], pid: u32) -> &ProcInfo {
        rows.iter().find(|p| p.pid == pid).expect("row listed")
    }

    #[test]
    fn timebase_converts_mach_units() {
        assert_eq!(APPLE_SILICON.to_ns(24_000_000), 1_000_000_000);
        assert_eq!(APPLE_SILICON.to_ns(3), 125);
        assert_eq!(INTEL.to_ns(1_000_000_000), 1_000_000_000);
        assert_eq!(APPLE_SILICON.to_ns(u64::MAX), u64::MAX, "saturates");
        let zero = Timebase { numer: 1, denom: 0 };
        assert_eq!(zero.to_ns(7), 7, "denom 0 counts as 1");
    }

    #[test]
    fn cpu_maths_with_both_timebases() {
        // Half a core over one second: 0.5 s of CPU time in mach units.
        let half = cpu_percent(APPLE_SILICON.to_ns(12_000_000), 1.0, 10);
        assert!((half - 50.0).abs() < 1e-3, "{half}");
        let half = cpu_percent(INTEL.to_ns(500_000_000), 1.0, 10);
        assert!((half - 50.0).abs() < 1e-3, "{half}");
        let capped = cpu_percent(APPLE_SILICON.to_ns(24_000_000 * 20), 1.0, 10);
        assert_eq!(capped, 1_000.0, "capped at cpus × 100");
    }

    #[test]
    fn readable_state_map() {
        let table = [
            (libc::SIDL, 0, ProcState::Idle),
            (libc::SRUN, 1, ProcState::Run),
            (libc::SRUN, 0, ProcState::Sleep),
            (libc::SSLEEP, 3, ProcState::Sleep),
            (libc::SSTOP, 0, ProcState::Stop),
            (libc::SZOMB, 0, ProcState::Zombie),
            (0, 0, ProcState::Unknown),
            (99, 1, ProcState::Unknown),
        ];
        for (status, running, state) in table {
            assert_eq!(readable_state(status, running), state, "{status} {running}");
        }
    }

    #[test]
    fn denied_state_map() {
        let table = [
            (libc::SIDL, ProcState::Idle),
            (libc::SRUN, ProcState::Unknown),
            (libc::SSLEEP, ProcState::Unknown),
            (libc::SSTOP, ProcState::Stop),
            (libc::SZOMB, ProcState::Zombie),
            (99, ProcState::Unknown),
        ];
        for (status, state) in table {
            assert_eq!(denied_state(status), state, "{status}");
        }
    }

    #[test]
    fn name_choice_order() {
        let none = || None;
        let bsd = *b"long-bsd-name\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let comm = *b"comm\0\0\0\0\0\0\0\0\0\0\0\0";
        let path = || Some("from-path".to_string());
        assert_eq!(
            choose_name(Some("zsh".into()), || panic!("not needed"), &bsd, &comm),
            "zsh"
        );
        assert_eq!(choose_name(None, path, &bsd, &comm), "from-path");
        assert_eq!(
            choose_name(Some(String::new()), path, &bsd, &comm),
            "from-path",
            "an empty name from the arguments does not count"
        );
        assert_eq!(choose_name(None, none, &bsd, &comm), "long-bsd-name");
        assert_eq!(
            choose_name(None, || Some(String::new()), &[0; 32], &comm),
            "comm"
        );
        assert_eq!(choose_name(None, none, &[0; 32], &[0; 16]), "");
        assert_eq!(c_text(b"no-nul"), Some("no-nul".into()));
    }

    /// A `KERN_PROCARGS2` buffer: argc, then the rest as given.
    fn procargs(argc: c_int, rest: &[u8]) -> Vec<u8> {
        let mut buf = argc.to_ne_bytes().to_vec();
        buf.extend_from_slice(rest);
        buf
    }

    #[test]
    fn parses_procargs() {
        let buf = procargs(
            3,
            b"/usr/bin/ssh\0\0\0\0ssh\0-v\0host\0HOME=/Users/x\0SHELL=/bin/zsh\0",
        );
        assert_eq!(
            parse_procargs(&buf),
            Some(("ssh".into(), "ssh -v host".into()))
        );
    }

    #[test]
    fn procargs_skips_empty_arguments_without_reading_the_environment() {
        let buf = procargs(4, b"/bin/echo\0\0\0\0\0\0\0echo\0\0b\0\0SECRET=1\0");
        assert_eq!(
            parse_procargs(&buf),
            Some(("echo".into(), "echo b".into())),
            "two empty arguments count towards argc"
        );
    }

    #[test]
    fn path_padding_fills_to_eight_bytes() {
        // The path and its NUL plus the padding make a multiple of 8.
        for len in 0..40 {
            assert_eq!((len + 1 + path_padding(len)) % 8, 0, "{len}");
            assert!(path_padding(len) < 8);
        }
        assert_eq!(path_padding(10), 5, "/bin/sleep");
        assert_eq!(path_padding(7), 0, "/bin/sh");
    }

    /// Buffers read from children of this Mac (macOS 27, arm64) with empty leading
    /// arguments. The kernel drops the environment of another process, but its own count
    /// of argv skips every NUL after the path too, so one environment string is left in
    /// for each empty leading argument.
    #[test]
    fn procargs_empty_argv0_layouts_seen_on_macos() {
        let pad5 = b"/bin/sleep\0\0\0\0\0\0";
        let cases: [(c_int, &[u8], &str); 5] = [
            // argv ["", "30"]
            (2, b"\x0030\0E1=1\0", "30"),
            // argv ["", "", "30"]
            (3, b"\0\x0030\0E1=1\0E2=2\0", "30"),
            // argv ["", "", "", "30"]
            (4, b"\0\0\x0030\0E1=1\0E2=2\0E3=3\0", "30"),
            // argv ["x", "", "30"]
            (3, b"x\0\x0030\0", "x 30"),
            // ten empty arguments
            (
                10,
                b"\0\0\0\0\0\0\0\0\0\0E1=1\0E2=2\0E3=3\0E4=4\0E5=5\0\0\0\0\0\0\0",
                "",
            ),
        ];
        for (argc, args, cmd) in cases {
            let buf = procargs(argc, &[&pad5[..], args].concat());
            assert_eq!(
                parse_procargs(&buf),
                Some(("sleep".into(), cmd.into())),
                "argc {argc}"
            );
        }
        // `/bin/sh -c 'read _'` with argv[0] "": no padding after a 7-byte path.
        let sh = procargs(3, b"/bin/sh\0\0-c\0read _\0RTOP_LEAK_PROBE=1\0");
        assert_eq!(parse_procargs(&sh), Some(("sh".into(), "-c read _".into())));
    }

    #[test]
    fn procargs_empty_argv0_with_every_path_length() {
        for len in 1..=24 {
            let path = format!("/{}", "s".repeat(len - 1));
            let mut rest = path.clone().into_bytes();
            rest.resize(len + 1 + path_padding(len), 0);
            // argv ["", "", "30"], then the environment.
            rest.extend_from_slice(b"\0\x0030\0RTOP_LEAK_PROBE=1\0B=2\0");
            let (name, cmd) = parse_procargs(&procargs(3, &rest)).unwrap();
            assert_eq!(cmd, "30", "path length {len}");
            assert_eq!(name, path.trim_start_matches('/'), "path length {len}");
        }
    }

    #[test]
    fn procargs_edge_cases() {
        let no_nul = procargs(2, b"/bin/sleep\0sleep\x0030");
        assert_eq!(
            parse_procargs(&no_nul),
            Some(("sleep".into(), "sleep 30".into())),
            "no NUL at the end"
        );
        let zero = procargs(0, b"/sbin/launchd\0\0ENV=1\0");
        assert_eq!(
            parse_procargs(&zero),
            Some(("launchd".into(), String::new()))
        );
        let negative = procargs(-1, b"/bin/x\0x\0");
        assert_eq!(parse_procargs(&negative), Some(("x".into(), String::new())));
        let more = procargs(9, b"/bin/x\0x\0y");
        assert_eq!(
            parse_procargs(&more),
            Some(("x".into(), "x y".into())),
            "argc larger than the buffer"
        );
        assert_eq!(
            parse_procargs(&procargs(1, b"")),
            Some((String::new(), String::new()))
        );
        assert_eq!(parse_procargs(&[1, 0, 0]), None, "shorter than argc");
        let lossy = procargs(1, b"/bin/caf\xe9\0caf\xe9\0");
        assert_eq!(
            parse_procargs(&lossy),
            Some(("caf\u{fffd}".into(), "caf\u{fffd}".into()))
        );
    }

    #[test]
    fn args_buffer_len_rules() {
        assert_eq!(args_buffer_len(Some(1 << 20)), 1 << 20);
        assert_eq!(args_buffer_len(None), ARG_MAX_FALLBACK);
        assert_eq!(args_buffer_len(Some(0)), ARG_MAX_FALLBACK);
        assert_eq!(args_buffer_len(Some(1 << 30)), ARG_MAX_CAP);
    }

    #[test]
    fn errno_map() {
        assert_eq!(Fail::from_errno(Some(libc::EPERM)), Fail::Denied);
        assert_eq!(Fail::from_errno(Some(libc::ESRCH)), Fail::Gone);
        assert_eq!(Fail::from_errno(Some(libc::EINVAL)), Fail::Other);
        assert_eq!(Fail::from_errno(None), Fail::Other);
    }

    #[test]
    fn readable_process_call_plan_and_rates() {
        let mut k = Fake::default();
        k.readable(10, 77, "zsh");
        k.users.insert(501, "chad".into());
        let mut s = state();

        let first = s.collect(&mut k, &[10], 1.0, true);
        assert_eq!(
            k.calls(),
            [
                ("unique", 10),
                ("task", 10),
                ("args", 10),
                ("rusage", 10),
                ("users", 0)
            ],
            "new PID: identity first; one argument read; users on the slow-tick miss"
        );
        let p = row(&first, 10);
        assert_eq!(p.ident, 77);
        assert_eq!(&*p.name, "zsh");
        assert_eq!(&*p.cmd, "/bin/zsh -x");
        assert_eq!(p.user.as_deref(), Some("chad"));
        assert_eq!(p.parent, Some(1));
        assert_eq!(p.threads, Some(4));
        assert_eq!(p.memory, 4096);
        assert_eq!(p.state, ProcState::Run);
        assert_eq!(
            (p.cpu, p.read_rate, p.write_rate),
            (0.0, 0, 0),
            "first sample"
        );

        // Half a second of CPU time and some I/O over half a second.
        k.task_mut(10).cpu += 12_000_000;
        k.task_mut(10).running = 0;
        k.io.insert(10, (1_100, 700));
        let next = s.collect(&mut k, &[10], 0.5, false);
        assert_eq!(k.calls(), [("task", 10), ("rusage", 10)], "fast tick");
        let p = row(&next, 10);
        assert!((p.cpu - 100.0).abs() < 1e-3, "{}", p.cpu);
        assert_eq!((p.read_rate, p.write_rate), (2_000, 1_000));
        assert_eq!(p.state, ProcState::Sleep);
        assert_eq!(p.ident, 77);

        // An idle process shows 0%, not its last value.
        let idle = s.collect(&mut k, &[10], 1.0, false);
        assert_eq!(row(&idle, 10).cpu, 0.0);
    }

    #[test]
    fn changed_start_time_is_a_new_process() {
        let mut k = Fake::default();
        k.readable(10, 77, "zsh");
        let mut s = state();
        s.collect(&mut k, &[10], 1.0, false);
        k.calls();

        k.readable(10, 78, "vim");
        k.task_mut(10).start = (2_000, 0);
        k.task_mut(10).cpu = 999_999;
        k.io.insert(10, (5_000, 5_000));
        let rows = s.collect(&mut k, &[10], 1.0, false);
        assert_eq!(
            k.calls(),
            [
                ("task", 10),
                ("unique", 10),
                ("task", 10),
                ("args", 10),
                ("rusage", 10)
            ]
        );
        let p = row(&rows, 10);
        assert_eq!((p.ident, &*p.name), (78, "vim"));
        assert_eq!(
            (p.cpu, p.read_rate),
            (0.0, 0),
            "first sample of the new process"
        );
    }

    #[test]
    fn denied_process_call_plan() {
        let mut k = Fake::default();
        k.denied(20, 55, "WindowServer");
        k.users.insert(0, "root".into());
        let mut s = state();

        let first = s.collect(&mut k, &[20], 1.0, true);
        assert_eq!(
            k.calls(),
            [
                ("unique", 20),
                ("task", 20),
                ("short", 20),
                ("path", 20),
                ("users", 0)
            ]
        );
        let p = row(&first, 20);
        assert_eq!((p.ident, &*p.name, &*p.cmd), (55, "WindowServer", ""));
        assert_eq!(p.user.as_deref(), Some("root"), "the user now shows");
        assert_eq!(p.parent, Some(1), "the parent now shows");
        assert_eq!(p.state, ProcState::Unknown, "SRUN says nothing here");
        assert_eq!((p.threads, p.cpu, p.memory, p.read_rate), (None, 0.0, 0, 0));

        let fast = s.collect(&mut k, &[20], 1.0, false);
        assert_eq!(k.calls(), [], "no call on a fast tick");
        assert_eq!(row(&fast, 20), p);

        k.short.get_mut(&20).unwrap().as_mut().unwrap().status = libc::SSTOP;
        let slow = s.collect(&mut k, &[20], 1.0, true);
        assert_eq!(k.calls(), [("unique", 20), ("short", 20)], "slow tick");
        assert_eq!(row(&slow, 20).state, ProcState::Stop);
    }

    #[test]
    fn denied_pid_with_a_new_unique_id_is_a_new_process() {
        let mut k = Fake::default();
        k.denied(20, 55, "old");
        let mut s = state();
        s.collect(&mut k, &[20], 1.0, false);
        k.denied(20, 56, "new");
        let fast = s.collect(&mut k, &[20], 1.0, false);
        assert_eq!(&*row(&fast, 20).name, "old", "a fast tick does not look");
        k.calls();
        let slow = s.collect(&mut k, &[20], 1.0, true);
        let p = row(&slow, 20);
        assert_eq!((p.ident, &*p.name), (56, "new"));
        assert_eq!(
            k.calls(),
            [
                ("unique", 20),
                ("unique", 20),
                ("task", 20),
                ("short", 20),
                ("path", 20),
                ("users", 0)
            ]
        );
    }

    #[test]
    fn denied_process_that_turns_into_ours_is_read_again() {
        let mut k = Fake::default();
        k.denied(20, 55, "login");
        let mut s = state();
        s.collect(&mut k, &[20], 1.0, false);
        // `login` dropped root to UID 501 and ran a shell.
        k.readable(20, 55, "zsh");
        k.short.insert(
            20,
            Ok(Short {
                status: libc::SRUN,
                ppid: 1,
                ruid: 501,
                uid: 501,
            }),
        );
        let rows = s.collect(&mut k, &[20], 1.0, true);
        let p = row(&rows, 20);
        assert_eq!((&*p.name, p.threads), ("zsh", Some(4)));
    }

    #[test]
    fn readable_process_that_turns_denied_is_identified_again() {
        let mut k = Fake::default();
        k.readable(10, 77, "su");
        let mut s = state();
        s.collect(&mut k, &[10], 1.0, false);
        k.calls();
        k.denied(10, 77, "su");
        let rows = s.collect(&mut k, &[10], 1.0, false);
        assert_eq!(
            k.calls(),
            [
                ("task", 10),
                ("unique", 10),
                ("task", 10),
                ("short", 10),
                ("path", 10)
            ]
        );
        let p = row(&rows, 10);
        assert_eq!((p.ident, p.threads), (77, None));
    }

    #[test]
    fn error_rules_drop_what_cannot_be_read() {
        let mut k = Fake::default();
        // A zombie: every call fails with ESRCH.
        k.unique.insert(1, Err(Fail::Gone));
        // A denied process without a path, like `kernel_task`.
        k.denied(2, 2, "x");
        k.path.remove(&2);
        // A denied process whose short info fails.
        k.denied(3, 3, "x");
        k.short.insert(3, Err(Fail::Other));
        // TASKALLINFO fails with ESRCH or another error.
        k.readable(4, 4, "x");
        k.task.insert(4, Err(Fail::Gone));
        k.readable(5, 5, "x");
        k.task.insert(5, Err(Fail::Other));
        // A readable process: kept.
        k.readable(6, 6, "kept");
        // PID 0 is skipped without a call.
        let rows = s_collect(&mut k, &[0, 1, 2, 3, 4, 5, 6]);
        let pids: Vec<u32> = rows.iter().map(|p| p.pid).collect();
        assert_eq!(pids, [6]);
        assert!(!k.calls().iter().any(|&(_, pid)| pid == 0));
    }

    fn s_collect(k: &mut Fake, pids: &[pid_t]) -> Vec<ProcInfo> {
        state().collect(k, pids, 1.0, false)
    }

    #[test]
    fn readable_process_gone_or_exited_is_dropped_from_the_cache() {
        let mut k = Fake::default();
        k.readable(10, 1, "a");
        k.readable(11, 2, "b");
        let mut s = state();
        s.collect(&mut k, &[10, 11], 1.0, false);
        k.task.insert(10, Err(Fail::Gone));
        let rows = s.collect(&mut k, &[10, 11], 1.0, false);
        assert_eq!(rows.len(), 1);
        assert!(!s.known.contains_key(&10), "ESRCH drops the entry");
        s.collect(&mut k, &[], 1.0, false);
        assert!(s.known.is_empty(), "unlisted PIDs leave the cache");
    }

    #[test]
    fn empty_command_line_is_read_again_on_a_slow_tick() {
        let mut k = Fake::default();
        k.readable(10, 1, "a");
        k.args.remove(&10);
        k.path.insert(10, "from-path".into());
        let mut s = state();
        let rows = s.collect(&mut k, &[10], 1.0, false);
        let p = row(&rows, 10);
        assert_eq!((&*p.name, &*p.cmd), ("from-path", ""));
        k.args.insert(10, ("a".into(), "a --late".into()));
        k.calls();
        s.collect(&mut k, &[10], 1.0, false);
        assert!(!k.calls().contains(&("args", 10)), "not on a fast tick");
        let rows = s.collect(&mut k, &[10], 1.0, true);
        assert!(k.calls().contains(&("args", 10)));
        let p = row(&rows, 10);
        assert_eq!(
            (&*p.name, &*p.cmd),
            ("from-path", "a --late"),
            "the name stays"
        );
        s.collect(&mut k, &[10], 1.0, true);
        assert!(!k.calls().contains(&("args", 10)), "read once it is set");
    }

    #[test]
    fn user_names_load_on_a_slow_tick_miss_only() {
        let mut k = Fake::default();
        k.readable(10, 1, "a");
        let mut s = state();
        let rows = s.collect(&mut k, &[10], 1.0, false);
        assert_eq!(row(&rows, 10).user, None);
        assert!(!k.calls().contains(&("users", 0)), "not on a fast tick");
        k.users.insert(501, "chad".into());
        let rows = s.collect(&mut k, &[10], 1.0, true);
        assert_eq!(row(&rows, 10).user.as_deref(), Some("chad"));
        assert!(k.calls().contains(&("users", 0)));
        s.collect(&mut k, &[10], 1.0, true);
        assert!(!k.calls().contains(&("users", 0)), "no miss, no reload");
    }

    fn kill_request(pid: u32, ident: u64) -> KillRequest {
        KillRequest {
            target: KillTarget {
                pid,
                ident,
                name: "x".into(),
            },
            signal: KillSignal::Term,
        }
    }

    #[test]
    fn signal_rules() {
        let mut k = Fake::default();
        k.unique.insert(10, Ok(77));
        assert_eq!(
            signal(&mut k, &kill_request(1, 0), 99),
            Err(KillError::Protected)
        );
        assert_eq!(
            signal(&mut k, &kill_request(99, 0), 99),
            Err(KillError::Protected)
        );
        assert_eq!(k.calls(), [], "no call for a protected PID");

        assert_eq!(
            signal(&mut k, &kill_request(10, 78), 99),
            Err(KillError::Gone)
        );
        assert_eq!(k.calls(), [("unique", 10)], "no kill on a wrong identity");
        assert_eq!(
            signal(&mut k, &kill_request(11, 78), 99),
            Err(KillError::Gone)
        );
        assert_eq!(
            signal(&mut k, &kill_request(u32::MAX, 1), 99),
            Err(KillError::Gone),
            "not a pid_t"
        );
        k.calls();

        assert_eq!(
            signal(&mut k, &kill_request(10, 77), 99),
            Err(KillError::Failed)
        );
        k.kill_works = true;
        assert_eq!(signal(&mut k, &kill_request(10, 77), 99), Ok(()));
        assert_eq!(
            k.calls(),
            [("unique", 10), ("kill", 10), ("unique", 10), ("kill", 10)]
        );
    }

    // Live tests: they read this Mac.

    #[test]
    fn unique_id_struct_matches_the_kernel() {
        let me = pid_t::try_from(std::process::id()).unwrap();
        let mut live = Live::default();
        let ident = live.unique_id(me).expect("own unique ID");
        assert!(ident > 0);
        assert_eq!(live.unique_id(me), Ok(ident), "stable");
        assert!(live.task(me).is_ok());
        assert!(live.short(me).is_ok());
        assert_eq!(live.task(-5), Err(Fail::Gone), "no such PID");
    }

    #[test]
    fn collector_reads_this_process() {
        let mut sys = System::new();
        let mut collector = ProcCollector::new();
        let procs = collector.collect(&mut sys, 1.0, true);
        let me = row(&procs, std::process::id());
        assert!(!me.name.is_empty());
        assert!(me.ident > 0);
        assert!(!me.cmd.is_empty(), "the command line");
        assert!(me.threads.unwrap_or(0) >= 1, "threads: {:?}", me.threads);
        assert!(me.memory > 0);
        assert_eq!(
            (me.cpu, me.read_rate, me.write_rate),
            (0.0, 0, 0),
            "first sample"
        );
        let users = sysinfo::Users::new_with_refreshed_list();
        // SAFETY: `getuid` takes no arguments and cannot fail.
        let uid = unsafe { libc::getuid() };
        let expected = users
            .list()
            .iter()
            .find(|user| **user.id() == uid)
            .map(|user| user.name());
        assert_eq!(me.user.as_deref(), expected, "own user name");
        assert!(
            procs.iter().all(|p| p.pid != 0),
            "kernel_task is not listed"
        );
        // Burn some CPU, then check that it shows.
        let started = Instant::now();
        let mut x = 0u64;
        while started.elapsed().as_millis() < 200 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
        }
        let secs = started.elapsed().as_secs_f64();
        let procs = collector.collect(&mut sys, secs, false);
        let me = row(&procs, std::process::id());
        assert!(me.cpu > 20.0, "busy test thread: {}%", me.cpu);
    }

    /// The test reads the native list (A), the `sysinfo` list (S) and the native list again
    /// (B), each with a fresh collector. A PID in both A and B with the same identity was
    /// alive the whole time, so S must hold it. A PID in S but in neither A nor B must have
    /// started after A began (it lived only between the reads). Each field of S is
    /// compared only where A and B agree on it, so a process that runs `exec`, changes
    /// its user or loses its parent between the reads does not fail the test.
    #[test]
    fn parity_with_the_sysinfo_collector() {
        use super::super::sysinfo_procs::ProcCollector as Reference;

        let native = || {
            let mut sys = System::new();
            ProcCollector::new().collect(&mut sys, 1.0, true)
        };
        let began = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let a = native();
        let mut sys = System::new();
        let s = Reference::new().collect(&mut sys, 1.0, true);
        let b = native();

        let by_pid = |list: &[ProcInfo]| -> HashMap<u32, ProcInfo> {
            list.iter().map(|p| (p.pid, p.clone())).collect()
        };
        let (a, s, b) = (by_pid(&a), by_pid(&s), by_pid(&b));
        assert!(a.len() > 10 && s.len() > 10, "{} {}", a.len(), s.len());

        let mut errors = Vec::new();
        let mut compared = 0;
        let mut env_leaks = 0;
        for (pid, pa) in &a {
            let Some(pb) = b.get(pid).filter(|pb| pb.ident == pa.ident) else {
                continue;
            };
            let Some(ps) = s.get(pid) else {
                errors.push(format!(
                    "{pid} {}: alive throughout, not in sysinfo",
                    pa.name
                ));
                continue;
            };
            if pa.name == pb.name && pa.name != ps.name {
                errors.push(format!(
                    "{pid}: name {:?} vs sysinfo {:?}",
                    pa.name, ps.name
                ));
            }
            // Only readable processes have a thread count here, and only they show a
            // user and parent in `sysinfo`.
            if pa.threads.is_none() {
                continue;
            }
            compared += 1;
            if pa.threads.unwrap_or(0) < 1 {
                errors.push(format!("{pid}: threads {:?}", pa.threads));
            }
            if pa.user == pb.user && pa.user != ps.user {
                errors.push(format!(
                    "{pid}: user {:?} vs sysinfo {:?}",
                    pa.user, ps.user
                ));
            }
            if pa.parent == pb.parent && pa.parent != ps.parent {
                errors.push(format!(
                    "{pid}: parent {:?} vs sysinfo {:?}",
                    pa.parent, ps.parent
                ));
            }
            // A process that rewrote its title leaves empty arguments; `sysinfo` then
            // reads past `argc` into the environment and appends it. That is the one
            // command line difference allowed: the extra text must start with `NAME=`, so
            // a dropped `--key=value` argument still fails. Command lines and the
            // environment can hold secrets, so the message names the process only.
            let env_leak = ps
                .cmd
                .strip_prefix(&*pa.cmd)
                .and_then(|rest| rest.strip_prefix(' '))
                .and_then(|rest| rest.split_once('='))
                .is_some_and(|(key, _)| {
                    key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                });
            if env_leak {
                env_leaks += 1;
            } else if pa.cmd == pb.cmd && pa.cmd != ps.cmd {
                errors.push(format!(
                    "{pid} {}: command lines differ ({} and {} chars)",
                    pa.name,
                    pa.cmd.chars().count(),
                    ps.cmd.chars().count()
                ));
            }
        }
        println!(
            "parity: {compared} readable processes; sysinfo leaked the environment of {env_leaks}"
        );
        for (pid, ps) in &s {
            // `ident` is the start time in seconds for the `sysinfo` collector.
            if !a.contains_key(pid) && !b.contains_key(pid) && ps.ident < began {
                errors.push(format!("{pid} {}: only in sysinfo", ps.name));
            }
        }
        assert!(compared > 10, "readable processes compared: {compared}");
        assert!(
            errors.is_empty(),
            "{} differences:\n{}",
            errors.len(),
            errors.join("\n")
        );
    }

    /// A blank `argv[0]` must not pull the environment into the command line. The
    /// environment holds only the probe, so any leak shows it. `sleep` refuses an empty
    /// argument, so `sh` waiting on its stdin carries the empty extra argument.
    #[test]
    fn empty_argv0_does_not_leak_the_environment() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let spawn = |cmd: &mut Command| {
            cmd.arg0("")
                .env_clear()
                .env("RTOP_LEAK_PROBE", "1")
                .stdin(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let mut sleep = spawn(Command::new("/bin/sleep").arg("30"));
        let mut sh = spawn(Command::new("/bin/sh").args(["-c", "read _", ""]));
        let mut sys = System::new();
        let procs = ProcCollector::new().collect(&mut sys, 1.0, true);
        let cmds = [sleep.id(), sh.id()].map(|pid| row(&procs, pid).cmd.to_string());
        for child in [&mut sleep, &mut sh] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        assert!(
            cmds.iter().all(|cmd| !cmd.contains("RTOP_LEAK_PROBE")),
            "{cmds:?}"
        );
        assert_eq!(cmds, ["30", "-c read _"]);
    }

    #[test]
    fn signal_kills_a_child_and_checks_its_identity() {
        use std::process::Command;

        let mut first = Command::new("sleep").arg("30").spawn().unwrap();
        let mut second = Command::new("sleep").arg("30").spawn().unwrap();
        let mut sys = System::new();
        let mut collector = ProcCollector::new();
        let procs = collector.collect(&mut sys, 1.0, true);
        let ident = |pid: u32| row(&procs, pid).ident;
        let target = |pid: u32, ident: u64| KillTarget {
            pid,
            ident,
            name: "sleep".into(),
        };
        let self_pid = std::process::id();

        let term = KillRequest {
            target: target(first.id(), ident(first.id())),
            signal: KillSignal::Term,
        };
        let cache = collector.state.known.clone();
        assert_eq!(collector.signal(&mut sys, &term, self_pid).result, Ok(()));
        assert_eq!(collector.state.known, cache, "no baseline changes");
        first.wait().unwrap();
        assert_eq!(
            collector.signal(&mut sys, &term, self_pid).result,
            Err(KillError::Gone),
            "reaped child"
        );

        let wrong = KillRequest {
            target: target(second.id(), ident(second.id()) + 1),
            signal: KillSignal::Kill,
        };
        assert_eq!(
            collector.signal(&mut sys, &wrong, self_pid).result,
            Err(KillError::Gone),
            "identity differs"
        );
        assert!(second.try_wait().unwrap().is_none(), "second child alive");
        second.kill().unwrap();
        second.wait().unwrap();
    }
}
