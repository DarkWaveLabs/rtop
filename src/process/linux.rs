//! The native Linux process collector. With `with_tasks()`, `sysinfo` lists every thread
//! as a process and reads `status`, `stat`, `statm` and `io` for each one, only for rtop to
//! drop the threads again. This collector reads `/proc/<pid>/stat` once for each process,
//! which already holds the thread count, and `io` only where it can be read. Here
//! `ProcInfo::ident` is `starttime` (field 22 of `stat`): clock ticks since boot, so a
//! changed value at the same PID is a new process.
//!
//! Reads on each tick, after one listing of `/proc`:
//!
//! - A process seen before: `stat`, and `io` unless it failed with `EACCES`, `EPERM` or
//!   `ENOENT` for this process before. On a slow tick, `cmdline` while it is still empty.
//! - A kernel thread (`PF_KTHREAD`) seen before: nothing on a fast tick, `stat` on a slow
//!   tick (to find PID reuse). Kernel threads are never listed, as before.
//! - A new PID: `stat`, then `status` (the real UID), `cmdline` and `io`.
//!
//! The decisions live in `State`, which reaches `/proc` only through the `Procfs` trait,
//! so the tests drive it with fixture files. The parsers are pure and run on every host.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use super::{
    KillError, KillRequest, KillSignal, ProcInfo, ProcState, UserNames, check_target, cpu_percent,
    io_rate, join_cmd,
};
#[cfg(target_os = "linux")]
use super::{KillReport, online_cpus, system_user_names};

/// `PF_KTHREAD` from `include/linux/sched.h`: the task is a kernel thread.
const PF_KTHREAD: u64 = 0x0020_0000;

/// A file under `/proc/<pid>/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ProcFile {
    Stat,
    Io,
    Status,
    Cmdline,
}

impl ProcFile {
    fn name(self) -> &'static str {
        match self {
            ProcFile::Stat => "stat",
            ProcFile::Io => "io",
            ProcFile::Status => "status",
            ProcFile::Cmdline => "cmdline",
        }
    }
}

/// Why a read failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fail {
    /// `EACCES` or `EPERM`: `io` of another user's process.
    Denied,
    /// `ENOENT`: the process is gone, or the kernel has no such file (`io` without
    /// `CONFIG_TASK_IO_ACCOUNTING`).
    NotFound,
    /// Any other error, `ESRCH` included.
    Other,
}

impl Fail {
    /// Pure. `std` maps both `EACCES` and `EPERM` to `PermissionDenied`.
    fn from_io(err: &std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::PermissionDenied => Fail::Denied,
            std::io::ErrorKind::NotFound => Fail::NotFound,
            _ => Fail::Other,
        }
    }
}

/// The reads and calls the collector makes. `Live` makes them; the tests fake them.
trait Procfs {
    /// The whole of `/proc/<pid>/<file>`. The slice lives until the next call.
    fn read(&mut self, pid: u32, file: ProcFile) -> Result<&[u8], Fail>;
    /// The user list of the machine.
    fn user_names(&mut self) -> HashMap<u32, Arc<str>>;
    /// `kill`. True when it worked.
    fn kill(&mut self, pid: u32, signal: KillSignal) -> bool;
}

/// The fields rtop reads from `/proc/<pid>/stat`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Stat {
    /// The state letter: `R`, `S`, `D` and so on.
    state: u8,
    ppid: u32,
    flags: u64,
    /// User and system CPU time, in clock ticks.
    utime: u64,
    stime: u64,
    threads: i64,
    /// `starttime`: clock ticks since boot. The process identity.
    start: u64,
    /// Resident pages.
    rss: u64,
}

impl Stat {
    fn cpu_ticks(&self) -> u64 {
        self.utime.saturating_add(self.stime)
    }

    fn is_kernel_thread(&self) -> bool {
        self.flags & PF_KTHREAD != 0
    }
}

/// Pure. Parses a `stat` file into the name and the fields. The name sits between the
/// first `(` and the last `)`, because it may itself hold spaces and `)`. `None` when a
/// field that rtop reads is missing or not a number.
fn parse_stat(buf: &[u8]) -> Option<(&[u8], Stat)> {
    let open = buf.iter().position(|&b| b == b'(')?;
    let close = buf.iter().rposition(|&b| b == b')')?;
    let name = buf.get(open + 1..close)?;
    // Fields from 3 (`state`) on, split on white space.
    let mut fields = buf[close + 1..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    let state = match fields.next()? {
        [letter] => *letter,
        _ => return None,
    };
    let ppid = number(fields.next()?)?;
    // Fields 5 to 8: pgrp, session, tty_nr, tpgid.
    let flags = number(fields.nth(4)?)?;
    // Fields 10 to 13: minflt, cminflt, majflt, cmajflt.
    let utime = number(fields.nth(4)?)?;
    let stime = number(fields.next()?)?;
    // Fields 16 to 19: cutime, cstime, priority, nice.
    let threads = number(fields.nth(4)?)?;
    // Field 21: itrealvalue.
    let start = number(fields.nth(1)?)?;
    // Field 23: vsize. `rss` is a signed `long`.
    let rss: i64 = number(fields.nth(1)?)?;
    Some((
        name,
        Stat {
            state,
            ppid,
            flags,
            utime,
            stime,
            threads,
            start,
            rss: u64::try_from(rss).unwrap_or(0),
        },
    ))
}

/// Pure. A decimal field.
fn number<T: std::str::FromStr>(field: &[u8]) -> Option<T> {
    std::str::from_utf8(field).ok()?.parse().ok()
}

/// Pure. The state letter of `stat`, mapped as `sysinfo` and `ProcState::from_sysinfo`
/// map it.
fn state_from_code(code: u8) -> ProcState {
    match code {
        b'R' => ProcState::Run,
        b'S' => ProcState::Sleep,
        b'D' => ProcState::DiskSleep,
        b'T' | b't' => ProcState::Stop,
        b'Z' => ProcState::Zombie,
        b'X' | b'x' => ProcState::Dead,
        b'I' => ProcState::Idle,
        _ => ProcState::Unknown,
    }
}

/// Pure. `read_bytes` and `write_bytes` from an `io` file. `None` unless both are there.
fn parse_io(buf: &[u8]) -> Option<(u64, u64)> {
    let mut read = None;
    let mut write = None;
    for line in buf.split(|&b| b == b'\n') {
        if let Some(value) = line.strip_prefix(b"read_bytes: ") {
            read = number(value.trim_ascii());
        } else if let Some(value) = line.strip_prefix(b"write_bytes: ") {
            write = number(value.trim_ascii());
        }
    }
    Some((read?, write?))
}

/// Pure. The real UID: the first number on the `Uid:` line of a `status` file.
fn parse_uid(buf: &[u8]) -> Option<u32> {
    let line = buf
        .split(|&b| b == b'\n')
        .find_map(|line| line.strip_prefix(b"Uid:"))?;
    let real = line
        .split(u8::is_ascii_whitespace)
        .find(|field| !field.is_empty())?;
    number(real)
}

/// Pure. Splits a `cmdline` file on NUL, trims ASCII white space from each argument and
/// drops the empty ones, as `sysinfo` does, then joins them.
fn parse_cmdline(buf: &[u8]) -> String {
    let args = buf
        .split(|&b| b == 0)
        .map(<[u8]>::trim_ascii)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned());
    join_cmd(args)
}

/// Pure. Clock ticks to nanoseconds. Saturates at `u64::MAX`.
fn ticks_to_ns(ticks: u64, clk_tck: u64) -> u64 {
    let ns = u128::from(ticks) * 1_000_000_000 / u128::from(clk_tck.max(1));
    u64::try_from(ns).unwrap_or(u64::MAX)
}

/// Pure. A PID from a `/proc` entry name: digits only, not 0.
fn pid_from_name(name: &str) -> Option<u32> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    name.parse().ok().filter(|&pid| pid > 0)
}

/// The kill check: protected PIDs, then the identity from `stat`, then `kill`.
fn signal(fs: &mut impl Procfs, req: &KillRequest, self_pid: u32) -> Result<(), KillError> {
    let target = &req.target;
    check_target(target.pid, self_pid)?;
    let start = fs
        .read(target.pid, ProcFile::Stat)
        .ok()
        .and_then(parse_stat)
        .map(|(_, stat)| stat.start);
    if start != Some(target.ident) {
        return Err(KillError::Gone);
    }
    if fs.kill(target.pid, req.signal) {
        Ok(())
    } else {
        Err(KillError::Failed)
    }
}

/// What the collector remembers about one process between ticks.
#[derive(Clone, Debug, PartialEq)]
struct Known {
    /// `starttime`.
    ident: u64,
    /// The `State::tick` that last saw the PID. The others are gone.
    tick: u64,
    kind: Kind,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// A kernel thread: not listed, and read only on slow ticks.
    Kernel,
    User(UserProc),
}

#[derive(Clone, Debug, PartialEq)]
struct UserProc {
    name: Arc<str>,
    cmd: Arc<str>,
    ruid: u32,
    /// CPU time at the last sample, in clock ticks.
    cpu: u64,
    io: Io,
}

/// The `io` baseline of a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Io {
    /// No sample yet: the next one gives rate 0.
    Unread,
    /// Bytes read and written at the last sample.
    At(u64, u64),
    /// `io` cannot be read for this process: never read it again.
    Off,
}

impl Io {
    /// Reads `io` (unless it is off) and returns the rates since the last sample.
    fn sample(&mut self, fs: &mut impl Procfs, pid: u32, secs: f64) -> (u64, u64) {
        if *self == Io::Off {
            return (0, 0);
        }
        let now = match fs.read(pid, ProcFile::Io) {
            Ok(buf) => parse_io(buf),
            Err(Fail::Denied | Fail::NotFound) => {
                *self = Io::Off;
                return (0, 0);
            }
            Err(Fail::Other) => None,
        };
        let Some(now) = now else {
            // A short or failed read: start again from the next sample, so no rate spans
            // the gap.
            *self = Io::Unread;
            return (0, 0);
        };
        let (first, last) = match *self {
            Io::At(read, write) => (false, (read, write)),
            _ => (true, now),
        };
        *self = Io::At(now.0, now.1);
        (
            io_rate(now.0.saturating_sub(last.0), secs, first),
            io_rate(now.1.saturating_sub(last.1), secs, first),
        )
    }
}

/// The collector without the reads.
struct State {
    known: HashMap<u32, Known>,
    users: UserNames,
    tick: u64,
    cpus: usize,
    /// `sysconf(_SC_CLK_TCK)`.
    clk_tck: u64,
    /// `sysconf(_SC_PAGESIZE)`.
    page_size: u64,
}

impl State {
    fn new(cpus: usize, clk_tck: u64, page_size: u64) -> Self {
        Self {
            known: HashMap::new(),
            users: UserNames::default(),
            tick: 0,
            cpus,
            clk_tck,
            page_size,
        }
    }

    fn collect(
        &mut self,
        fs: &mut impl Procfs,
        pids: &[u32],
        secs: f64,
        slow: bool,
    ) -> Vec<ProcInfo> {
        self.tick = self.tick.wrapping_add(1);
        let mut out = Vec::with_capacity(pids.len());
        // Rows without a user name, with their real UID.
        let mut missed = Vec::new();
        for &pid in pids {
            if pid == 0 {
                continue;
            }
            if let Some((info, ruid)) = self.read(fs, pid, secs, slow) {
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
                .reload_after_miss(Instant::now(), || fs.user_names())
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
        fs: &mut impl Procfs,
        pid: u32,
        secs: f64,
        slow: bool,
    ) -> Option<(ProcInfo, u32)> {
        let tick = self.tick;
        if !slow
            && let Some(known) = self.known.get_mut(&pid)
            && known.kind == Kind::Kernel
        {
            known.tick = tick;
            return None;
        }
        let (stat, new_name) = {
            let (name, stat) = parse_stat(fs.read(pid, ProcFile::Stat).ok()?)?;
            let same = self
                .known
                .get(&pid)
                .is_some_and(|known| known.ident == stat.start);
            // Only a new user process needs its name; take it before the next read
            // reuses the buffer.
            let new_name = (!same && !stat.is_kernel_thread())
                .then(|| Arc::from(String::from_utf8_lossy(name)));
            (stat, new_name)
        };
        if let Some(name) = new_name {
            return self.read_new(fs, pid, &stat, name);
        }
        let Some(known) = self.known.get_mut(&pid).filter(|k| k.ident == stat.start) else {
            // A new kernel thread, or a new one at a known PID.
            self.known.insert(
                pid,
                Known {
                    ident: stat.start,
                    tick,
                    kind: Kind::Kernel,
                },
            );
            return None;
        };
        known.tick = tick;
        let Kind::User(user) = &mut known.kind else {
            return None;
        };
        let delta_ns = ticks_to_ns(stat.cpu_ticks().saturating_sub(user.cpu), self.clk_tck);
        user.cpu = stat.cpu_ticks();
        let (read_rate, write_rate) = user.io.sample(fs, pid, secs);
        if slow
            && user.cmd.is_empty()
            && let Ok(buf) = fs.read(pid, ProcFile::Cmdline)
        {
            let cmd = parse_cmdline(buf);
            if !cmd.is_empty() {
                user.cmd = Arc::from(cmd);
            }
        }
        let mut info = row(pid, known.ident, user, &stat, &self.users, self.page_size);
        info.cpu = cpu_percent(delta_ns, secs, self.cpus);
        info.read_rate = read_rate;
        info.write_rate = write_rate;
        Some((info, user.ruid))
    }

    /// A new user process at `pid`. Its CPU and I/O show 0 on this first sample.
    fn read_new(
        &mut self,
        fs: &mut impl Procfs,
        pid: u32,
        stat: &Stat,
        name: Arc<str>,
    ) -> Option<(ProcInfo, u32)> {
        // Without its user the row is not complete: leave it out, and try again on the
        // next tick. `stat` and `status` have the same access rules, so this happens only
        // when the process exits between the two reads.
        let Some(ruid) = fs.read(pid, ProcFile::Status).ok().and_then(parse_uid) else {
            self.known.remove(&pid);
            return None;
        };
        let cmd = fs
            .read(pid, ProcFile::Cmdline)
            .map(parse_cmdline)
            .unwrap_or_default();
        let mut io = Io::Unread;
        io.sample(fs, pid, 1.0);
        let user = UserProc {
            name,
            cmd: Arc::from(cmd),
            ruid,
            cpu: stat.cpu_ticks(),
            io,
        };
        let info = row(pid, stat.start, &user, stat, &self.users, self.page_size);
        self.known.insert(
            pid,
            Known {
                ident: stat.start,
                tick: self.tick,
                kind: Kind::User(user),
            },
        );
        Some((info, ruid))
    }
}

/// The row of a user process, with CPU and I/O at 0.
fn row(
    pid: u32,
    ident: u64,
    user: &UserProc,
    stat: &Stat,
    users: &UserNames,
    page_size: u64,
) -> ProcInfo {
    ProcInfo {
        pid,
        // Parent PID 0 means none, as in `sysinfo`.
        parent: (stat.ppid != 0).then_some(stat.ppid),
        ident,
        name: Arc::clone(&user.name),
        cmd: Arc::clone(&user.cmd),
        user: users.get(user.ruid),
        state: state_from_code(stat.state),
        threads: u32::try_from(stat.threads).ok(),
        cpu: 0.0,
        memory: stat.rss.saturating_mul(page_size),
        read_rate: 0,
        write_rate: 0,
    }
}

/// Owned by the sampler. Caches the identity, name, command line, user and the CPU and
/// I/O baselines of each process.
#[cfg(target_os = "linux")]
pub struct ProcCollector {
    live: Live,
    state: State,
    pids: Vec<u32>,
}

#[cfg(target_os = "linux")]
impl Default for ProcCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
impl ProcCollector {
    pub fn new() -> Self {
        Self {
            live: Live::default(),
            state: State::new(
                online_cpus(),
                sysconf(libc::_SC_CLK_TCK, 100),
                sysconf(libc::_SC_PAGESIZE, 4096),
            ),
            pids: Vec::new(),
        }
    }

    /// Reads all processes. `_sys` is for the `sysinfo` collector only.
    pub fn collect(&mut self, _sys: &mut sysinfo::System, secs: f64, slow: bool) -> Vec<ProcInfo> {
        if !list_pids(&mut self.pids) {
            // Keep the cache, so the next tick still has its baselines.
            return Vec::new();
        }
        self.state.collect(&mut self.live, &self.pids, secs, slow)
    }

    /// Reads `starttime` of the PID again and sends the signal only when it equals the
    /// `ident` of the target. It changes no baseline in the cache.
    pub fn signal(
        &mut self,
        _sys: &mut sysinfo::System,
        req: &KillRequest,
        self_pid: u32,
    ) -> KillReport {
        KillReport {
            request: req.clone(),
            result: signal(&mut self.live, req, self_pid),
        }
    }
}

/// `sysconf(name)`, or `fallback` when it is not a positive number.
#[cfg(target_os = "linux")]
fn sysconf(name: libc::c_int, fallback: u64) -> u64 {
    // SAFETY: a plain call with no pointers; it returns -1 on error.
    let value = unsafe { libc::sysconf(name) };
    u64::try_from(value)
        .ok()
        .filter(|&v| v > 0)
        .unwrap_or(fallback)
}

/// Fills `pids` with the numeric entries of `/proc`. False when `/proc` cannot be read.
#[cfg(target_os = "linux")]
fn list_pids(pids: &mut Vec<u32>) -> bool {
    pids.clear();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in dir.flatten() {
        if let Some(pid) = entry.file_name().to_str().and_then(pid_from_name) {
            pids.push(pid);
        }
    }
    true
}

/// The largest file `Live` reads. `cmdline` is the only one that can be large, and rtop
/// keeps 4096 chars of it.
#[cfg(target_os = "linux")]
const READ_MAX: u64 = 1 << 20;
/// A read buffer that grew past this (a long `cmdline`) is dropped before the next read.
#[cfg(target_os = "linux")]
const BUF_KEEP: usize = 64 << 10;

/// The real reads, with the path and the file buffer kept between calls.
#[cfg(target_os = "linux")]
struct Live {
    path: String,
    buf: Vec<u8>,
}

#[cfg(target_os = "linux")]
impl Default for Live {
    fn default() -> Self {
        Self {
            path: String::with_capacity(32),
            buf: Vec::with_capacity(4096),
        }
    }
}

#[cfg(target_os = "linux")]
impl Procfs for Live {
    fn read(&mut self, pid: u32, file: ProcFile) -> Result<&[u8], Fail> {
        use std::fmt::Write as _;
        use std::io::Read as _;

        self.path.clear();
        // Writing to a `String` cannot fail.
        let _ = write!(self.path, "/proc/{pid}/{}", file.name());
        if self.buf.capacity() > BUF_KEEP {
            self.buf = Vec::with_capacity(4096);
        }
        self.buf.clear();
        // `Take` reads with the generic loop: `File::read_to_end` would first ask for the
        // file size and position (two more system calls), which procfs reports as 0.
        std::fs::File::open(&self.path)
            .and_then(|f| f.take(READ_MAX).read_to_end(&mut self.buf))
            .map_err(|err| Fail::from_io(&err))?;
        Ok(&self.buf)
    }

    fn user_names(&mut self) -> HashMap<u32, Arc<str>> {
        system_user_names()
    }

    fn kill(&mut self, pid: u32, signal: KillSignal) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        let signal = match signal {
            KillSignal::Term => libc::SIGTERM,
            KillSignal::Kill => libc::SIGKILL,
        };
        // SAFETY: a plain system call with no pointers.
        unsafe { libc::kill(pid, signal) == 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::super::KillTarget;
    use super::*;

    const STAT_NAME_WITH_PAREN: &[u8] = include_bytes!("../../testdata/proc/stat-name-with-paren");
    const STAT_KTHREAD: &[u8] = include_bytes!("../../testdata/proc/stat-kthread");
    const STAT_ZOMBIE: &[u8] = include_bytes!("../../testdata/proc/stat-zombie");
    const IO: &[u8] = include_bytes!("../../testdata/proc/io");
    const STATUS: &[u8] = include_bytes!("../../testdata/proc/status");
    const CMDLINE: &[u8] = include_bytes!("../../testdata/proc/cmdline");

    /// A fake `/proc`: the content of each file by PID; a missing file fails with
    /// `ENOENT`. `calls` logs each call.
    #[derive(Default)]
    struct Fake {
        files: HashMap<(u32, ProcFile), Result<Vec<u8>, Fail>>,
        users: HashMap<u32, Arc<str>>,
        kill_works: bool,
        calls: Vec<(&'static str, u32)>,
    }

    impl Fake {
        fn calls(&mut self) -> Vec<(&'static str, u32)> {
            std::mem::take(&mut self.calls)
        }

        fn set(&mut self, pid: u32, file: ProcFile, content: &[u8]) {
            self.files.insert((pid, file), Ok(content.to_vec()));
        }

        fn fail(&mut self, pid: u32, file: ProcFile, fail: Fail) {
            self.files.insert((pid, file), Err(fail));
        }

        /// A user process of UID 1000 with the fixture `status`, `cmdline` and `io`.
        fn process(&mut self, pid: u32, stat: Stat, name: &str) {
            self.set(pid, ProcFile::Stat, &stat_line(pid, name, &stat));
            self.set(pid, ProcFile::Status, STATUS);
            self.set(pid, ProcFile::Cmdline, CMDLINE);
            self.set(pid, ProcFile::Io, IO);
        }

        fn io(&mut self, pid: u32, read: u64, write: u64) {
            let text = format!("rchar: 1\nread_bytes: {read}\nwrite_bytes: {write}\n");
            self.set(pid, ProcFile::Io, text.as_bytes());
        }
    }

    impl Procfs for Fake {
        fn read(&mut self, pid: u32, file: ProcFile) -> Result<&[u8], Fail> {
            self.calls.push((file.name(), pid));
            match self.files.get(&(pid, file)) {
                Some(Ok(content)) => Ok(content),
                Some(Err(fail)) => Err(*fail),
                None => Err(Fail::NotFound),
            }
        }
        fn user_names(&mut self) -> HashMap<u32, Arc<str>> {
            self.calls.push(("users", 0));
            self.users.clone()
        }
        fn kill(&mut self, pid: u32, _signal: KillSignal) -> bool {
            self.calls.push(("kill", pid));
            self.kill_works
        }
    }

    /// A full 52-field `stat` line with the fields that rtop reads.
    fn stat_line(pid: u32, name: &str, s: &Stat) -> Vec<u8> {
        let state = char::from(s.state);
        format!(
            "{pid} ({name}) {state} {} {pid} {pid} 0 -1 {} 10 0 0 0 {} {} 0 0 20 0 {} 0 {} 9999 {} \
             18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 1 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
            s.ppid, s.flags, s.utime, s.stime, s.threads, s.start, s.rss
        )
        .into_bytes()
    }

    fn sleeping(start: u64) -> Stat {
        Stat {
            state: b'S',
            ppid: 1,
            flags: 0x0040_0100,
            utime: 100,
            stime: 50,
            threads: 3,
            start,
            rss: 10,
        }
    }

    fn kthread(start: u64) -> Stat {
        Stat {
            ppid: 2,
            flags: 0x0020_8040,
            threads: 1,
            rss: 0,
            ..sleeping(start)
        }
    }

    /// 4 CPUs, 100 Hz, 4 KiB pages.
    fn state() -> State {
        State::new(4, 100, 4096)
    }

    fn row_of(rows: &[ProcInfo], pid: u32) -> &ProcInfo {
        rows.iter().find(|p| p.pid == pid).expect("row listed")
    }

    #[test]
    fn parses_a_name_with_spaces_and_parentheses() {
        let (name, stat) = parse_stat(STAT_NAME_WITH_PAREN).unwrap();
        assert_eq!(name, b"tmux: server) a");
        assert_eq!(
            stat,
            Stat {
                state: b'S',
                ppid: 1,
                flags: 4_194_560,
                utime: 250,
                stime: 125,
                threads: 3,
                start: 123_456,
                rss: 1024,
            }
        );
        assert!(!stat.is_kernel_thread());
        assert_eq!(stat.cpu_ticks(), 375);
    }

    #[test]
    fn parses_a_kernel_thread_and_a_zombie() {
        let (name, stat) = parse_stat(STAT_KTHREAD).unwrap();
        assert_eq!(name, b"kthreadd");
        assert!(stat.is_kernel_thread());
        assert_eq!((stat.ppid, stat.start), (0, 2));

        let (name, stat) = parse_stat(STAT_ZOMBIE).unwrap();
        assert_eq!(name, b"defunct");
        assert!(!stat.is_kernel_thread());
        assert_eq!(state_from_code(stat.state), ProcState::Zombie);
        assert_eq!((stat.ppid, stat.threads, stat.rss), (4242, 1, 0));
    }

    #[test]
    fn stat_parse_failures() {
        assert_eq!(parse_stat(b""), None);
        assert_eq!(parse_stat(b"1 (x S 0 0"), None, "no closing parenthesis");
        assert_eq!(parse_stat(b"1 x) S 0"), None, "no opening parenthesis");
        assert_eq!(parse_stat(b"1 (x) S 0 1 1 0 -1 4"), None, "cut short");
        let line = stat_line(5, "x", &sleeping(9));
        // Keeps rss and the first digits of the field after it.
        let cut = &line[..line.len() - 70];
        assert!(parse_stat(cut).is_some(), "fields after rss are not needed");
        let bad = String::from_utf8(line)
            .unwrap()
            .replace(" 9 9999 ", " nine 9999 ");
        assert_eq!(parse_stat(bad.as_bytes()), None, "starttime not a number");
        let two_letters = stat_line(5, "x", &sleeping(9));
        let two_letters = String::from_utf8(two_letters)
            .unwrap()
            .replace(") S ", ") SS ");
        assert_eq!(parse_stat(two_letters.as_bytes()), None);
        let negative_rss = String::from_utf8(stat_line(5, "x", &sleeping(9)))
            .unwrap()
            .replace(" 9999 10 ", " 9999 -1 ");
        assert_eq!(parse_stat(negative_rss.as_bytes()).unwrap().1.rss, 0);
    }

    #[test]
    fn state_map() {
        let table = [
            (b'R', ProcState::Run),
            (b'S', ProcState::Sleep),
            (b'D', ProcState::DiskSleep),
            (b'T', ProcState::Stop),
            (b't', ProcState::Stop),
            (b'Z', ProcState::Zombie),
            (b'X', ProcState::Dead),
            (b'x', ProcState::Dead),
            (b'I', ProcState::Idle),
            (b'K', ProcState::Unknown),
            (b'W', ProcState::Unknown),
            (b'P', ProcState::Unknown),
            (b'?', ProcState::Unknown),
        ];
        for (code, state) in table {
            assert_eq!(state_from_code(code), state, "{}", char::from(code));
        }
    }

    #[test]
    fn parses_io_status_and_cmdline() {
        assert_eq!(parse_io(IO), Some((40_960, 8_192)));
        assert_eq!(
            parse_io(b"rchar: 5\nread_bytes: 7\n"),
            None,
            "no write_bytes"
        );
        assert_eq!(parse_io(b""), None);
        assert_eq!(
            parse_uid(STATUS),
            Some(1000),
            "the real UID, not the effective one"
        );
        assert_eq!(parse_uid(b"Name:\tx\nGid:\t5\t5\t5\t5\n"), None);
        assert_eq!(parse_uid(b"Uid:\n"), None);
        assert_eq!(
            parse_cmdline(CMDLINE),
            "/usr/bin/python3 -m http.server 8000"
        );
        assert_eq!(parse_cmdline(b""), "", "a kernel thread or a zombie");
        assert_eq!(parse_cmdline(b"\0\0 \0"), "");
        assert_eq!(
            parse_cmdline(b"sleep\x0030"),
            "sleep 30",
            "no NUL at the end"
        );
        assert_eq!(parse_cmdline(b"caf\xe9\0"), "caf\u{fffd}");
    }

    #[test]
    fn tick_and_name_helpers() {
        assert_eq!(ticks_to_ns(100, 100), 1_000_000_000);
        assert_eq!(ticks_to_ns(250, 250), 1_000_000_000);
        assert_eq!(ticks_to_ns(1, 100), 10_000_000);
        assert_eq!(ticks_to_ns(7, 0), 7_000_000_000, "CLK_TCK 0 counts as 1");
        assert_eq!(ticks_to_ns(u64::MAX, 1), u64::MAX, "saturates");
        assert_eq!(pid_from_name("4242"), Some(4242));
        for name in ["", "0", "self", "+5", "-5", "12a", "99999999999"] {
            assert_eq!(pid_from_name(name), None, "{name:?}");
        }
        assert_eq!(ProcFile::Status.name(), "status");
    }

    #[test]
    fn errno_map() {
        use std::io::{Error, ErrorKind};
        let kind = |k: ErrorKind| Fail::from_io(&Error::from(k));
        assert_eq!(kind(ErrorKind::PermissionDenied), Fail::Denied);
        assert_eq!(kind(ErrorKind::NotFound), Fail::NotFound);
        assert_eq!(kind(ErrorKind::InvalidData), Fail::Other);
    }

    #[test]
    fn user_process_call_plan_and_rates() {
        let mut k = Fake::default();
        k.process(10, sleeping(500), "python3");
        k.users.insert(1000, "chad".into());
        let mut s = state();

        let first = s.collect(&mut k, &[10], 1.0, true);
        assert_eq!(
            k.calls(),
            [
                ("stat", 10),
                ("status", 10),
                ("cmdline", 10),
                ("io", 10),
                ("users", 0)
            ],
            "new PID; users on the slow-tick miss"
        );
        let p = row_of(&first, 10);
        assert_eq!(p.ident, 500);
        assert_eq!(&*p.name, "python3");
        assert_eq!(&*p.cmd, "/usr/bin/python3 -m http.server 8000");
        assert_eq!(p.user.as_deref(), Some("chad"));
        assert_eq!(p.parent, Some(1));
        assert_eq!(p.threads, Some(3));
        assert_eq!(p.memory, 10 * 4096);
        assert_eq!(p.state, ProcState::Sleep);
        assert_eq!(
            (p.cpu, p.read_rate, p.write_rate),
            (0.0, 0, 0),
            "first sample"
        );

        // 50 ticks (0.5 s at 100 Hz) of CPU and some I/O over half a second.
        let busy = Stat {
            state: b'R',
            utime: 130,
            stime: 70,
            threads: 5,
            rss: 20,
            ..sleeping(500)
        };
        k.set(10, ProcFile::Stat, &stat_line(10, "python3", &busy));
        k.io(10, 40_960 + 1_000, 8_192 + 500);
        let next = s.collect(&mut k, &[10], 0.5, false);
        assert_eq!(k.calls(), [("stat", 10), ("io", 10)], "fast tick");
        let p = row_of(&next, 10);
        assert!((p.cpu - 100.0).abs() < 1e-3, "{}", p.cpu);
        assert_eq!((p.read_rate, p.write_rate), (2_000, 1_000));
        assert_eq!(
            (p.state, p.threads, p.memory),
            (ProcState::Run, Some(5), 20 * 4096)
        );
        assert_eq!(p.ident, 500);

        // No user-list reload on a slow tick without a miss; the command line is set.
        let idle = s.collect(&mut k, &[10], 1.0, true);
        assert_eq!(
            k.calls(),
            [("stat", 10), ("io", 10)],
            "slow tick, nothing missing"
        );
        assert_eq!(row_of(&idle, 10).cpu, 0.0, "idle shows 0");
    }

    #[test]
    fn name_with_a_parenthesis_and_a_zombie_are_listed() {
        let mut k = Fake::default();
        k.set(4242, ProcFile::Stat, STAT_NAME_WITH_PAREN);
        k.set(4242, ProcFile::Status, STATUS);
        k.set(777, ProcFile::Stat, STAT_ZOMBIE);
        k.set(777, ProcFile::Status, STATUS);
        k.fail(777, ProcFile::Io, Fail::Denied);
        let rows = state().collect(&mut k, &[4242, 777], 1.0, false);
        let p = row_of(&rows, 4242);
        assert_eq!((&*p.name, &*p.cmd), ("tmux: server) a", ""));
        assert_eq!(p.memory, 1024 * 4096);
        let z = row_of(&rows, 777);
        assert_eq!(
            (&*z.name, z.state, z.parent),
            ("defunct", ProcState::Zombie, Some(4242))
        );
    }

    #[test]
    fn kernel_threads_are_skipped_and_read_only_on_slow_ticks() {
        let mut k = Fake::default();
        k.set(2, ProcFile::Stat, STAT_KTHREAD);
        k.process(10, sleeping(500), "a");
        let mut s = state();

        let rows = s.collect(&mut k, &[2, 10], 1.0, false);
        assert!(rows.iter().all(|p| p.pid != 2), "not listed");
        assert_eq!(
            k.calls().iter().filter(|c| c.1 == 2).collect::<Vec<_>>(),
            [&("stat", 2)],
            "first sight: stat only"
        );
        assert_eq!(s.known[&2].kind, Kind::Kernel);

        s.collect(&mut k, &[2, 10], 1.0, false);
        assert!(
            !k.calls().iter().any(|c| c.1 == 2),
            "no read on a fast tick"
        );
        assert!(s.known.contains_key(&2), "still remembered");

        s.collect(&mut k, &[2, 10], 1.0, true);
        assert!(k.calls().contains(&("stat", 2)), "read on a slow tick");

        // The kernel thread exited and a user process has the PID now. A fast tick does
        // not look; the next slow tick finds it.
        k.process(2, sleeping(900), "reused");
        let fast = s.collect(&mut k, &[2, 10], 1.0, false);
        assert!(fast.iter().all(|p| p.pid != 2));
        let slow = s.collect(&mut k, &[2, 10], 1.0, true);
        let p = row_of(&slow, 2);
        assert_eq!((&*p.name, p.ident), ("reused", 900));

        // A kernel thread that is no longer listed leaves the cache.
        k.set(3, ProcFile::Stat, &stat_line(3, "kworker/0:1", &kthread(4)));
        s.collect(&mut k, &[3], 1.0, false);
        assert_eq!(s.known.keys().collect::<Vec<_>>(), [&3]);
        s.collect(&mut k, &[], 1.0, false);
        assert!(s.known.is_empty());
    }

    #[test]
    fn io_denied_or_missing_is_not_read_again() {
        let mut k = Fake::default();
        k.process(10, sleeping(500), "denied");
        k.fail(10, ProcFile::Io, Fail::Denied);
        k.process(11, sleeping(501), "missing");
        k.files.remove(&(11, ProcFile::Io));
        k.process(12, sleeping(502), "flaky");
        k.fail(12, ProcFile::Io, Fail::Other);
        let mut s = state();
        s.collect(&mut k, &[10, 11, 12], 1.0, false);
        k.calls();

        for _ in 0..2 {
            let rows = s.collect(&mut k, &[10, 11, 12], 1.0, false);
            assert_eq!(
                k.calls(),
                [("stat", 10), ("stat", 11), ("stat", 12), ("io", 12)],
                "only the other error is read again"
            );
            assert!(rows.iter().all(|p| (p.read_rate, p.write_rate) == (0, 0)));
        }

        // After the other error, the first good read is a first sample again.
        k.io(12, 5_000, 5_000);
        let rows = s.collect(&mut k, &[12], 1.0, false);
        assert_eq!(row_of(&rows, 12).read_rate, 0);
        k.io(12, 6_000, 5_000);
        let rows = s.collect(&mut k, &[12], 1.0, false);
        assert_eq!(row_of(&rows, 12).read_rate, 1_000);
    }

    #[test]
    fn changed_starttime_is_a_new_process() {
        let mut k = Fake::default();
        k.process(10, sleeping(500), "old");
        k.io(10, 100, 100);
        let mut s = state();
        s.collect(&mut k, &[10], 1.0, false);
        k.calls();

        let reused = Stat {
            utime: 9_000,
            ..sleeping(777)
        };
        k.set(10, ProcFile::Stat, &stat_line(10, "new", &reused));
        k.set(10, ProcFile::Status, b"Uid:\t0\t0\t0\t0\n");
        k.set(10, ProcFile::Cmdline, b"/usr/bin/new\0");
        k.io(10, 1_000_000, 1_000_000);
        let rows = s.collect(&mut k, &[10], 1.0, false);
        assert_eq!(
            k.calls(),
            [("stat", 10), ("status", 10), ("cmdline", 10), ("io", 10)]
        );
        let p = row_of(&rows, 10);
        assert_eq!((p.ident, &*p.name, &*p.cmd), (777, "new", "/usr/bin/new"));
        assert_eq!(
            (p.cpu, p.read_rate),
            (0.0, 0),
            "first sample of the new process"
        );
        let Kind::User(user) = &s.known[&10].kind else {
            panic!("a user process");
        };
        assert_eq!(user.ruid, 0);
    }

    #[test]
    fn empty_command_line_is_read_again_on_a_slow_tick() {
        let mut k = Fake::default();
        k.process(10, sleeping(500), "a");
        k.set(10, ProcFile::Cmdline, b"");
        let mut s = state();
        let rows = s.collect(&mut k, &[10], 1.0, false);
        assert_eq!(&*row_of(&rows, 10).cmd, "");
        k.set(10, ProcFile::Cmdline, b"a\0--late\0");
        k.calls();
        s.collect(&mut k, &[10], 1.0, false);
        assert!(!k.calls().contains(&("cmdline", 10)), "not on a fast tick");
        let rows = s.collect(&mut k, &[10], 1.0, true);
        assert!(k.calls().contains(&("cmdline", 10)));
        assert_eq!(&*row_of(&rows, 10).cmd, "a --late");
        s.collect(&mut k, &[10], 1.0, true);
        assert!(!k.calls().contains(&("cmdline", 10)), "read once it is set");
    }

    #[test]
    fn unreadable_processes_are_left_out() {
        let mut k = Fake::default();
        // stat is gone (exited) or cannot be parsed.
        k.process(1, sleeping(1), "x");
        k.fail(1, ProcFile::Stat, Fail::NotFound);
        k.process(2, sleeping(2), "x");
        k.set(2, ProcFile::Stat, b"2 (x");
        // status fails on first sight: left out, and tried again on the next tick.
        k.process(3, sleeping(3), "late");
        k.fail(3, ProcFile::Status, Fail::Other);
        k.process(4, sleeping(4), "kept");
        let mut s = state();
        let rows = s.collect(&mut k, &[0, 1, 2, 3, 4], 1.0, false);
        assert_eq!(rows.iter().map(|p| p.pid).collect::<Vec<_>>(), [4]);
        assert!(
            !k.calls().iter().any(|&(_, pid)| pid == 0),
            "PID 0 is skipped"
        );
        assert!(!s.known.contains_key(&3));

        k.set(3, ProcFile::Status, STATUS);
        let rows = s.collect(&mut k, &[3, 4], 1.0, false);
        assert_eq!(&*row_of(&rows, 3).name, "late");
    }

    #[test]
    fn user_names_load_on_a_slow_tick_miss_only() {
        let mut k = Fake::default();
        k.process(10, sleeping(1), "a");
        let mut s = state();
        let rows = s.collect(&mut k, &[10], 1.0, false);
        assert_eq!(row_of(&rows, 10).user, None);
        assert!(!k.calls().contains(&("users", 0)), "not on a fast tick");
        k.users.insert(1000, "chad".into());
        let rows = s.collect(&mut k, &[10], 1.0, true);
        assert_eq!(row_of(&rows, 10).user.as_deref(), Some("chad"));
        assert!(k.calls().contains(&("users", 0)));
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
        k.process(10, sleeping(77), "x");
        let mut s = state();
        s.collect(&mut k, &[10], 1.0, false);
        let cache = s.known.clone();
        k.calls();

        assert_eq!(
            signal(&mut k, &kill_request(1, 0), 99),
            Err(KillError::Protected)
        );
        assert_eq!(
            signal(&mut k, &kill_request(99, 0), 99),
            Err(KillError::Protected)
        );
        assert_eq!(k.calls(), [], "no read for a protected PID");

        assert_eq!(
            signal(&mut k, &kill_request(10, 78), 99),
            Err(KillError::Gone)
        );
        assert_eq!(k.calls(), [("stat", 10)], "no kill on a wrong identity");
        assert_eq!(
            signal(&mut k, &kill_request(11, 77), 99),
            Err(KillError::Gone),
            "no stat"
        );
        k.set(12, ProcFile::Stat, b"12 (x) S");
        assert_eq!(
            signal(&mut k, &kill_request(12, 77), 99),
            Err(KillError::Gone),
            "bad stat"
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
            [("stat", 10), ("kill", 10), ("stat", 10), ("kill", 10)]
        );
        assert_eq!(s.known, cache, "no baseline changes");
    }

    // Live tests: they read this Linux host, and run in CI.

    #[cfg(target_os = "linux")]
    #[test]
    fn live_reads_this_process() {
        let me = std::process::id();
        let mut live = Live::default();
        let (name, stat) = parse_stat(live.read(me, ProcFile::Stat).unwrap())
            .map(|(name, stat)| (name.to_vec(), stat))
            .unwrap();
        assert!(!name.is_empty());
        assert!(stat.start > 0 && stat.threads >= 1 && stat.rss > 0);
        assert!(parse_uid(live.read(me, ProcFile::Status).unwrap()).is_some());
        assert!(!parse_cmdline(live.read(me, ProcFile::Cmdline).unwrap()).is_empty());
        assert_eq!(live.read(u32::MAX, ProcFile::Stat), Err(Fail::NotFound));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn collector_reads_this_process() {
        let mut sys = sysinfo::System::new();
        let mut collector = ProcCollector::new();
        let procs = collector.collect(&mut sys, 1.0, true);
        let me = row_of(&procs, std::process::id());
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
        // SAFETY: `getuid` takes no arguments and cannot fail.
        let uid = unsafe { libc::getuid() };
        let expected = system_user_names().get(&uid).cloned();
        assert_eq!(me.user, expected, "own user name");
        // PID 2 is `kthreadd` outside a PID namespace.
        let kthreadd = Live::default()
            .read(2, ProcFile::Stat)
            .ok()
            .and_then(parse_stat)
            .is_some_and(|(_, stat)| stat.is_kernel_thread());
        if kthreadd {
            assert!(
                procs.iter().all(|p| p.pid != 2),
                "kernel threads are not listed"
            );
        }
        // Burn some CPU, then check that it shows.
        let started = Instant::now();
        let mut x = 0u64;
        while started.elapsed().as_millis() < 200 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
        }
        let secs = started.elapsed().as_secs_f64();
        let procs = collector.collect(&mut sys, secs, false);
        let me = row_of(&procs, std::process::id());
        assert!(me.cpu > 20.0, "busy test thread: {}%", me.cpu);
    }

    /// The CPU cap counts the host's CPUs, not the ones rtop may run on. A thread pinned to
    /// CPU 0 makes the collector as `taskset -c 0 rtop` would.
    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_cap_ignores_rtop_affinity() {
        let (pinned, parallelism, cpus) = std::thread::spawn(|| {
            // SAFETY: `set` is a `cpu_set_t` (plain integers, so zeroed is valid) that
            // lives for both calls and is passed with its own size. PID 0 is this thread.
            let pinned = unsafe {
                let mut set: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_SET(0, &mut set);
                libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &raw const set) == 0
            };
            let parallelism = std::thread::available_parallelism().map_or(1, usize::from);
            (pinned, parallelism, ProcCollector::new().state.cpus)
        })
        .join()
        .unwrap();
        let online = online_cpus();
        println!("online {online}, pinned {pinned}, available_parallelism {parallelism}");
        if pinned {
            assert_eq!(parallelism, 1, "the pin took");
        }
        assert_eq!(cpus, online);
    }

    /// The test reads the native list (A), the `sysinfo` list (S) and the native list again
    /// (B), each with a fresh collector. A PID in both A and B with the same identity was
    /// alive the whole time, so S must hold it. A PID in S but in neither A nor B must have
    /// started after A began. Each field of S is compared only where A and B agree on it,
    /// so a process that runs `exec`, changes its user or loses its parent between the
    /// reads does not fail the test.
    #[cfg(target_os = "linux")]
    #[test]
    fn parity_with_the_sysinfo_collector() {
        use super::super::sysinfo_procs::ProcCollector as Reference;

        let native = || {
            let mut sys = sysinfo::System::new();
            ProcCollector::new().collect(&mut sys, 1.0, true)
        };
        let began = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let a = native();
        let mut sys = sysinfo::System::new();
        let s = Reference::new().collect(&mut sys, 1.0, true);
        let b = native();

        let by_pid = |list: &[ProcInfo]| -> HashMap<u32, ProcInfo> {
            list.iter().map(|p| (p.pid, p.clone())).collect()
        };
        let (a, s, b) = (by_pid(&a), by_pid(&s), by_pid(&b));
        assert!(a.len() > 1 && s.len() > 1, "{} {}", a.len(), s.len());

        let mut errors = Vec::new();
        let mut compared = 0;
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
            compared += 1;
            if pa.threads.unwrap_or(0) < 1 {
                errors.push(format!("{pid}: threads {:?}", pa.threads));
            }
            if pa.name == pb.name && pa.name != ps.name {
                errors.push(format!(
                    "{pid}: name {:?} vs sysinfo {:?}",
                    pa.name, ps.name
                ));
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
            // Command lines can hold secrets, so the message names the process only.
            if pa.cmd == pb.cmd && pa.cmd != ps.cmd {
                errors.push(format!(
                    "{pid} {}: command lines differ ({} and {} chars)",
                    pa.name,
                    pa.cmd.chars().count(),
                    ps.cmd.chars().count()
                ));
            }
        }
        println!("parity: {compared} processes compared");
        for (pid, ps) in &s {
            // `ident` is the start time in seconds for the `sysinfo` collector. It comes
            // from the boot time in whole seconds, so allow 2 s of rounding.
            if !a.contains_key(pid) && !b.contains_key(pid) && ps.ident.saturating_add(2) < began {
                errors.push(format!("{pid} {}: only in sysinfo", ps.name));
            }
        }
        assert!(compared > 1, "processes compared: {compared}");
        assert!(
            errors.is_empty(),
            "{} differences:\n{}",
            errors.len(),
            errors.join("\n")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn signal_kills_a_child_and_checks_its_identity() {
        use std::process::Command;

        let mut first = Command::new("sleep").arg("30").spawn().unwrap();
        let mut second = Command::new("sleep").arg("30").spawn().unwrap();
        let mut sys = sysinfo::System::new();
        let mut collector = ProcCollector::new();
        let procs = collector.collect(&mut sys, 1.0, true);
        let ident = |pid: u32| row_of(&procs, pid).ident;
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
