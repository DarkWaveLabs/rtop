//! Process data, sorting, filtering, the tree view and kill. The sampler owns the collector.

use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, System, UpdateKind};

/// The longest command line that rtop keeps, in chars.
const CMD_MAX_CHARS: usize = 4096;
/// Tree levels that the prefix draws. A deeper row shows `…` in place of the levels past
/// this, so a long parent chain costs a fixed prefix size for each row, not one that grows
/// with depth (a chain of N nested processes would otherwise cost about N² bytes).
const TREE_MAX_DEPTH: usize = 32;
/// The shortest time between two reloads of the user list.
const USERS_RELOAD: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcState {
    Run,
    Sleep,
    DiskSleep,
    Stop,
    Zombie,
    Idle,
    Dead,
    #[default]
    Unknown,
}

impl ProcState {
    /// Maps the `sysinfo` status. Exact table below. Called on every OS.
    pub fn from_sysinfo(status: sysinfo::ProcessStatus) -> Self {
        match status {
            ProcessStatus::Run => ProcState::Run,
            ProcessStatus::Sleep => ProcState::Sleep,
            ProcessStatus::UninterruptibleDiskSleep => ProcState::DiskSleep,
            ProcessStatus::Stop | ProcessStatus::Tracing | ProcessStatus::Suspended => {
                ProcState::Stop
            }
            ProcessStatus::Zombie => ProcState::Zombie,
            ProcessStatus::Idle => ProcState::Idle,
            ProcessStatus::Dead => ProcState::Dead,
            ProcessStatus::Wakekill
            | ProcessStatus::Waking
            | ProcessStatus::Parked
            | ProcessStatus::LockBlocked
            | ProcessStatus::Unknown(_) => ProcState::Unknown,
        }
    }

    /// One letter, like `ps`: R S D T Z I X, and '?' for Unknown.
    pub fn code(self) -> char {
        match self {
            ProcState::Run => 'R',
            ProcState::Sleep => 'S',
            ProcState::DiskSleep => 'D',
            ProcState::Stop => 'T',
            ProcState::Zombie => 'Z',
            ProcState::Idle => 'I',
            ProcState::Dead => 'X',
            ProcState::Unknown => '?',
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    /// Seconds since the epoch. Kill uses (pid, start_time) to find the same process.
    pub start_time: u64,
    pub name: String,
    /// The joined command line, at most 4096 chars. Empty when unknown.
    pub cmd: Arc<str>,
    pub user: Option<Arc<str>>,
    pub state: ProcState,
    pub threads: Option<u32>,
    /// Percent of one core.
    pub cpu: f32,
    pub memory: u64,
    /// Bytes per second over the last interval. 0 on the first sample of a process.
    pub read_rate: u64,
    pub write_rate: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Pid,
    Name,
    Cpu,
    Memory,
    Io,
}

impl SortKey {
    const ALL: [SortKey; 5] = [
        SortKey::Pid,
        SortKey::Name,
        SortKey::Cpu,
        SortKey::Memory,
        SortKey::Io,
    ];

    /// The `s` key order: Cpu → Memory → Io → Pid → Name → Cpu.
    pub fn next(self) -> Self {
        match self {
            SortKey::Cpu => SortKey::Memory,
            SortKey::Memory => SortKey::Io,
            SortKey::Io => SortKey::Pid,
            SortKey::Pid => SortKey::Name,
            SortKey::Name => SortKey::Cpu,
        }
    }

    /// Header text: "PID", "NAME", "CPU%", "MEM", "IO".
    pub fn title(self) -> &'static str {
        match self {
            SortKey::Pid => "PID",
            SortKey::Name => "NAME",
            SortKey::Cpu => "CPU%",
            SortKey::Memory => "MEM",
            SortKey::Io => "IO",
        }
    }

    /// True for Cpu, Memory and Io.
    pub fn descending(self) -> bool {
        matches!(self, SortKey::Cpu | SortKey::Memory | SortKey::Io)
    }

    /// Config name: "pid", "name", "cpu", "mem", "io".
    pub fn name(self) -> &'static str {
        match self {
            SortKey::Pid => "pid",
            SortKey::Name => "name",
            SortKey::Cpu => "cpu",
            SortKey::Memory => "mem",
            SortKey::Io => "io",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.name() == name)
    }
}

/// How the UI wants the rows.
#[derive(Clone, Copy, Debug)]
pub struct ProcView<'a> {
    /// Case-insensitive substring of the name, or an exact PID. Empty matches all.
    pub filter: &'a str,
    pub sort: SortKey,
    /// Flips the default direction of `sort`.
    pub reverse: bool,
    pub tree: bool,
    /// When true, the filter also matches the command line.
    pub match_cmd: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcRow<'a> {
    pub proc: &'a ProcInfo,
    /// Tree glyphs such as "│ ├─". Empty in the flat view. At most `TREE_MAX_DEPTH`
    /// levels, then `…`.
    pub prefix: String,
}

/// Pure. Filters, sorts (ties by PID) and, in tree mode, orders the rows depth-first.
/// In tree mode, the sort orders siblings. A row whose parent is not visible is a root.
/// A parent loop (from PID reuse) must not hang or repeat rows.
pub fn visible_rows<'a>(procs: &'a [ProcInfo], view: &ProcView<'_>) -> Vec<ProcRow<'a>> {
    let needle = view.filter.to_lowercase();
    // "042" or "+42" is text, not PID 42.
    let pid_filter = view
        .filter
        .parse::<u32>()
        .ok()
        .filter(|pid| pid.to_string() == view.filter);
    let mut shown: Vec<&ProcInfo> = procs
        .iter()
        .filter(|p| {
            needle.is_empty()
                || pid_filter == Some(p.pid)
                || p.name.to_lowercase().contains(&needle)
                || (view.match_cmd && p.cmd.to_lowercase().contains(&needle))
        })
        .collect();
    let descending = view.sort.descending() != view.reverse;
    crate::order::sort_by(&mut shown, |a, b| compare(a, b, view.sort, descending));

    if view.tree {
        tree_rows(&shown)
    } else {
        shown
            .into_iter()
            .map(|proc| ProcRow {
                proc,
                prefix: String::new(),
            })
            .collect()
    }
}

/// Orders two rows by `key`. Equal rows fall back to PID order, which keeps them still
/// between refreshes.
fn compare(a: &ProcInfo, b: &ProcInfo, key: SortKey, descending: bool) -> Ordering {
    let io = |p: &ProcInfo| p.read_rate.saturating_add(p.write_rate);
    let primary = match key {
        SortKey::Pid => a.pid.cmp(&b.pid),
        // Case-insensitive, without an allocation for each comparison.
        SortKey::Name => a
            .name
            .chars()
            .flat_map(char::to_lowercase)
            .cmp(b.name.chars().flat_map(char::to_lowercase)),
        SortKey::Cpu => a.cpu.total_cmp(&b.cpu),
        SortKey::Memory => a.memory.cmp(&b.memory),
        SortKey::Io => io(a).cmp(&io(b)),
    };
    let primary = if descending {
        primary.reverse()
    } else {
        primary
    };
    primary.then(a.pid.cmp(&b.pid))
}

/// Depth-first order of `sorted`. Siblings keep the order of `sorted`.
/// Each row shows once, also when the parent links make a loop.
fn tree_rows<'a>(sorted: &[&'a ProcInfo]) -> Vec<ProcRow<'a>> {
    let n = sorted.len();
    let index: HashMap<u32, usize> = sorted.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    let mut children = vec![Vec::new(); n];
    let mut is_root = vec![true; n];
    for (i, p) in sorted.iter().enumerate() {
        if let Some(&parent) = p.parent.and_then(|pid| index.get(&pid))
            && parent != i
        {
            children[parent].push(i);
            is_root[i] = false;
        }
    }

    let mut rows = Vec::with_capacity(n);
    let mut visited = vec![false; n];
    // Roots first. Then rows that only a parent loop reaches, each as a new root.
    let starts = (0..n).filter(|&i| is_root[i]).chain(0..n);
    for start in starts {
        if visited[start] {
            continue;
        }
        // (row, its prefix, the lead for its children, its depth)
        let mut stack = vec![(start, String::new(), String::new(), 0usize)];
        while let Some((i, prefix, lead, depth)) = stack.pop() {
            if visited[i] {
                continue;
            }
            visited[i] = true;
            rows.push(ProcRow {
                proc: sorted[i],
                prefix,
            });
            let kids: Vec<usize> = children[i]
                .iter()
                .copied()
                .filter(|&c| !visited[c])
                .collect();
            let last = kids.len().saturating_sub(1);
            let kid_depth = depth + 1;
            // Past the cap, the lead stops growing and `…` stands for the hidden levels.
            let (more, grow) = if kid_depth > TREE_MAX_DEPTH {
                ("…", false)
            } else {
                ("", kid_depth < TREE_MAX_DEPTH)
            };
            // Push in reverse, so the first sibling comes off the stack first.
            for (k, &kid) in kids.iter().enumerate().rev() {
                let (branch, rest) = if k == last {
                    ("└─", "  ")
                } else {
                    ("├─", "│ ")
                };
                let kid_lead = if grow {
                    format!("{lead}{rest}")
                } else {
                    lead.clone()
                };
                stack.push((kid, format!("{lead}{more}{branch}"), kid_lead, kid_depth));
            }
        }
    }
    rows
}

/// Pure. Joins argv with spaces (lossy UTF-8) and cuts it to 4096 chars.
pub fn join_cmd(parts: &[std::ffi::OsString]) -> String {
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        // A char is at most 4 bytes, so this many bytes already hold enough chars.
        if out.len() >= CMD_MAX_CHARS * 4 {
            break;
        }
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&part.to_string_lossy());
    }
    if let Some((cut, _)) = out.char_indices().nth(CMD_MAX_CHARS) {
        out.truncate(cut);
    }
    out
}

/// The refresh kind the sampler must use for `refresh_processes_specifics`.
/// Every tick: memory, cpu, disk_usage, tasks, user(OnlyIfNotSet).
/// Slow ticks add cmd(OnlyIfNotSet). `exe` is never refreshed: rtop does not read it.
pub fn refresh_kind(slow: bool) -> sysinfo::ProcessRefreshKind {
    let kind = ProcessRefreshKind::nothing()
        .with_memory()
        .with_cpu()
        .with_disk_usage()
        .with_tasks()
        .with_user(UpdateKind::OnlyIfNotSet);
    if slow {
        kind.with_cmd(UpdateKind::OnlyIfNotSet)
    } else {
        kind
    }
}

/// What the collector remembers about one process between ticks.
struct Known {
    start_time: u64,
    cmd: Arc<str>,
    threads: Option<u32>,
}

impl Known {
    fn new(start_time: u64) -> Self {
        Self {
            start_time,
            cmd: Arc::from(""),
            threads: None,
        }
    }
}

/// Owned by the sampler. Caches command lines, user names and thread counts.
#[derive(Default)]
pub struct ProcCollector {
    known: HashMap<u32, Known>,
    users: HashMap<sysinfo::Uid, Arc<str>>,
    users_loaded: Option<Instant>,
}

impl ProcCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the process list. Skips Linux threads (`thread_kind().is_some()`).
    pub fn collect(&mut self, sys: &sysinfo::System, secs: f64, slow: bool) -> Vec<ProcInfo> {
        let processes: Vec<&sysinfo::Process> = sys
            .processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .collect();
        if slow {
            self.reload_users_on_miss(&processes);
        }

        let mut seen = HashSet::with_capacity(processes.len());
        let mut out = Vec::with_capacity(processes.len());
        for p in processes {
            let pid = p.pid().as_u32();
            let start_time = p.start_time();
            seen.insert(pid);

            let mut first = false;
            let known = match self.known.entry(pid) {
                Entry::Occupied(entry) => {
                    let known = entry.into_mut();
                    if known.start_time != start_time {
                        // A new process has this PID now.
                        *known = Known::new(start_time);
                        first = true;
                    }
                    known
                }
                Entry::Vacant(entry) => {
                    first = true;
                    entry.insert(Known::new(start_time))
                }
            };
            if known.cmd.is_empty() {
                let cmd = join_cmd(p.cmd());
                if !cmd.is_empty() {
                    known.cmd = Arc::from(cmd);
                }
            }
            #[cfg(target_os = "linux")]
            {
                known.threads = linux_threads(p.tasks().map(|tasks| tasks.len()));
            }
            #[cfg(target_os = "macos")]
            if slow {
                known.threads = macos_threads(pid);
            }

            let disk = p.disk_usage();
            out.push(ProcInfo {
                pid,
                parent: p.parent().map(Pid::as_u32),
                start_time,
                name: p.name().to_string_lossy().into_owned(),
                cmd: Arc::clone(&known.cmd),
                user: p.user_id().and_then(|uid| self.users.get(uid)).cloned(),
                state: ProcState::from_sysinfo(p.status()),
                threads: known.threads,
                cpu: p.cpu_usage(),
                memory: p.memory(),
                read_rate: io_rate(disk.read_bytes, secs, first),
                write_rate: io_rate(disk.written_bytes, secs, first),
            });
        }
        self.known.retain(|pid, _| seen.contains(pid));
        out
    }

    /// Reloads the user list when a process has a user ID that is not in it,
    /// at most once each `USERS_RELOAD`.
    fn reload_users_on_miss(&mut self, processes: &[&sysinfo::Process]) {
        let due = self
            .users_loaded
            .is_none_or(|loaded| loaded.elapsed() >= USERS_RELOAD);
        let miss = || {
            processes
                .iter()
                .any(|p| p.user_id().is_some_and(|uid| !self.users.contains_key(uid)))
        };
        if !due || !miss() {
            return;
        }
        let users = sysinfo::Users::new_with_refreshed_list();
        self.users = users
            .list()
            .iter()
            .map(|user| (user.id().clone(), Arc::from(user.name())))
            .collect();
        self.users_loaded = Some(Instant::now());
    }
}

/// Pure. Bytes per second for a delta counter. The first sample of a process gives 0,
/// because its counter holds the total since the process started.
fn io_rate(delta: u64, secs: f64, first: bool) -> u64 {
    if first {
        0
    } else {
        // `as u64` saturates, and `max` maps NaN to the floor.
        (delta as f64 / secs.max(0.001)) as u64
    }
}

/// Pure. `sysinfo` leaves the main thread out of the Linux task list, so add it back.
#[cfg(any(test, target_os = "linux"))]
fn linux_threads(tasks: Option<usize>) -> Option<u32> {
    tasks.map(|n| u32::try_from(n).unwrap_or(u32::MAX).saturating_add(1))
}

/// The thread count of one process, from `proc_pidinfo`. `None` when the call fails,
/// for example for another user's process without root.
#[cfg(target_os = "macos")]
fn macos_threads(pid: u32) -> Option<u32> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: `proc_taskinfo` is a plain C struct of integers, so all zero bytes is a valid
    // value. `proc_pidinfo` writes at most `size` bytes into `info`, which is exactly
    // `size` bytes long and lives until the call returns.
    let (written, info) = unsafe {
        let mut info: libc::proc_taskinfo = std::mem::zeroed();
        let written =
            libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size);
        (written, info)
    };
    if written != size {
        return None;
    }
    u32::try_from(info.pti_threadnum).ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillTarget {
    pub pid: u32,
    pub start_time: u64,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillSignal {
    /// SIGTERM. Not supported on Windows.
    Term,
    /// SIGKILL. On Windows `TerminateProcess` on a handle whose creation time was checked.
    Kill,
}

impl KillSignal {
    /// "TERM" or "KILL".
    pub fn label(self) -> &'static str {
        match self {
            KillSignal::Term => "TERM",
            KillSignal::Kill => "KILL",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillRequest {
    pub target: KillTarget,
    pub signal: KillSignal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillError {
    /// PID 0, PID 1, rtop itself, or PID 4 on Windows.
    Protected,
    /// The PID is gone, or a new process has the PID now.
    Gone,
    /// `kill_with` returned `None`.
    Unsupported,
    /// `kill_with` returned `Some(false)`.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillReport {
    pub request: KillRequest,
    pub result: Result<(), KillError>,
}

impl KillReport {
    /// The footer text. Exact strings are in section 6.7.2.
    pub fn message(&self) -> String {
        let KillTarget { pid, name, .. } = &self.request.target;
        let sig = self.request.signal.label();
        match self.result {
            Ok(()) => format!("sent {sig} to {pid} {name}"),
            Err(KillError::Protected) => format!("refused: pid {pid} is protected"),
            Err(KillError::Gone) => format!("{pid} {name} is not running now"),
            Err(KillError::Unsupported) => {
                format!("{sig} is not supported here; press x then f to force")
            }
            Err(KillError::Failed) => {
                format!("could not signal {pid} {name}: permission denied or process gone")
            }
        }
    }
}

/// Pure. Refuses protected PIDs.
pub fn check_target(pid: u32, self_pid: u32) -> Result<(), KillError> {
    let windows_system = cfg!(windows) && pid == 4;
    if pid == 0 || pid == 1 || pid == self_pid || windows_system {
        Err(KillError::Protected)
    } else {
        Ok(())
    }
}

/// Runs on the sampler thread. Checks the target, refreshes that one PID,
/// compares start_time, then calls `kill_with`. On Windows it opens the process instead,
/// compares the start time of that handle and terminates through the same handle.
pub fn send_signal(sys: &mut sysinfo::System, req: &KillRequest, self_pid: u32) -> KillReport {
    KillReport {
        request: req.clone(),
        result: signal(sys, req, self_pid),
    }
}

/// Not `kill_with`: on Windows `sysinfo` runs `taskkill.exe` by bare name, which Windows
/// looks up in the directory of rtop.exe before System32 (a planted binary would run), and
/// `taskkill` then acts on a bare PID that may belong to a new process by then.
#[cfg(windows)]
fn signal(_sys: &mut System, req: &KillRequest, self_pid: u32) -> Result<(), KillError> {
    check_target(req.target.pid, self_pid)?;
    windows_kill::terminate(&req.target, req.signal)
}

#[cfg(windows)]
mod windows_kill {
    use super::{KillError, KillSignal, KillTarget};
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, FILETIME, HANDLE};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        TerminateProcess,
    };

    /// Seconds from 1601-01-01 (the FILETIME epoch) to 1970-01-01.
    const EPOCH_GAP_SECS: u64 = 11_644_473_600;
    /// FILETIME counts 100 ns ticks.
    const TICKS_PER_SEC: u64 = 10_000_000;

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from a successful `OpenProcess` and is closed only here.
            unsafe { CloseHandle(self.0) }.ok();
        }
    }

    pub(super) fn terminate(target: &KillTarget, signal: KillSignal) -> Result<(), KillError> {
        let access = match signal {
            KillSignal::Term => PROCESS_QUERY_LIMITED_INFORMATION,
            KillSignal::Kill => PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
        };
        // SAFETY: plain FFI call with no pointers; `Handle` closes the handle it returns.
        let handle = match unsafe { OpenProcess(access, false, target.pid) } {
            Ok(handle) => Handle(handle),
            Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => {
                return Err(KillError::Failed);
            }
            Err(_) => return Err(KillError::Gone),
        };
        // The same seconds that `sysinfo` computes for `start_time`.
        if start_secs(&handle) != Some(target.start_time) {
            return Err(KillError::Gone);
        }
        if signal == KillSignal::Term {
            return Err(KillError::Unsupported);
        }
        // SAFETY: the handle is open with PROCESS_TERMINATE and is the process checked above,
        // so a PID reused since the check cannot be hit.
        unsafe { TerminateProcess(handle.0, 1) }.map_err(|_| KillError::Failed)
    }

    fn start_secs(handle: &Handle) -> Option<u64> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: four pointers to live local FILETIMEs; the handle has query rights.
        unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) }
            .ok()?;
        let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        (ticks / TICKS_PER_SEC).checked_sub(EPOCH_GAP_SECS)
    }
}

#[cfg(not(windows))]
fn signal(sys: &mut System, req: &KillRequest, self_pid: u32) -> Result<(), KillError> {
    let target = &req.target;
    check_target(target.pid, self_pid)?;
    let pid = Pid::from_u32(target.pid);
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let process = sys
        .process(pid)
        .filter(|p| p.start_time() == target.start_time)
        .ok_or(KillError::Gone)?;
    let signal = match req.signal {
        KillSignal::Term => sysinfo::Signal::Term,
        KillSignal::Kill => sysinfo::Signal::Kill,
    };
    match process.kill_with(signal) {
        None => Err(KillError::Unsupported),
        Some(false) => Err(KillError::Failed),
        Some(true) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use sysinfo::ProcessesToUpdate;

    fn proc(pid: u32, name: &str, cpu: f32, memory: u64) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cpu,
            memory,
            ..ProcInfo::default()
        }
    }

    fn child(pid: u32, parent: u32) -> ProcInfo {
        ProcInfo {
            pid,
            parent: Some(parent),
            name: format!("p{pid}"),
            ..ProcInfo::default()
        }
    }

    fn view(sort: SortKey) -> ProcView<'static> {
        ProcView {
            filter: "",
            sort,
            reverse: false,
            tree: false,
            match_cmd: false,
        }
    }

    fn pids(procs: &[ProcInfo], view: &ProcView<'_>) -> Vec<u32> {
        visible_rows(procs, view)
            .iter()
            .map(|row| row.proc.pid)
            .collect()
    }

    fn sample() -> Vec<ProcInfo> {
        vec![
            proc(30, "zsh", 5.0, 10),
            proc(10, "Brave", 50.0, 5),
            proc(20, "cargo", 5.0, 99),
        ]
    }

    #[test]
    fn sorts_by_cpu_with_pid_ties() {
        assert_eq!(pids(&sample(), &view(SortKey::Cpu)), [10, 20, 30]);
    }

    #[test]
    fn sorts_by_memory_descending() {
        assert_eq!(pids(&sample(), &view(SortKey::Memory)), [20, 30, 10]);
    }

    #[test]
    fn sorts_by_pid_ascending() {
        assert_eq!(pids(&sample(), &view(SortKey::Pid)), [10, 20, 30]);
    }

    #[test]
    fn sorts_by_name_case_insensitive() {
        assert_eq!(pids(&sample(), &view(SortKey::Name)), [10, 20, 30]);
    }

    #[test]
    fn sorts_by_io_descending_on_read_plus_write() {
        let procs = vec![
            ProcInfo {
                pid: 1,
                read_rate: 10,
                write_rate: 0,
                ..ProcInfo::default()
            },
            ProcInfo {
                pid: 2,
                read_rate: 5,
                write_rate: 50,
                ..ProcInfo::default()
            },
            ProcInfo {
                pid: 3,
                read_rate: u64::MAX,
                write_rate: u64::MAX,
                ..ProcInfo::default()
            },
        ];
        assert_eq!(pids(&procs, &view(SortKey::Io)), [3, 2, 1]);
    }

    #[test]
    fn filter_matches_name_substring_case_insensitive() {
        let procs = vec![
            proc(1, "launchd", 0.0, 1),
            proc(42, "Cargo", 0.0, 1),
            proc(7, "rust-analyzer", 0.0, 1),
        ];
        let v = ProcView {
            filter: "CAR",
            ..view(SortKey::Pid)
        };
        assert_eq!(pids(&procs, &v), [42]);
        let all = ProcView {
            filter: "",
            ..view(SortKey::Pid)
        };
        assert_eq!(pids(&procs, &all).len(), 3, "empty filter matches all");
    }

    #[test]
    fn filter_matches_an_exact_pid() {
        let procs = vec![proc(42, "a", 0.0, 1), proc(420, "b", 0.0, 1)];
        let v = ProcView {
            filter: "42",
            ..view(SortKey::Pid)
        };
        assert_eq!(pids(&procs, &v), [42]);
    }

    #[test]
    fn match_cmd_extends_the_filter_to_the_command_line() {
        let procs = vec![
            ProcInfo {
                pid: 5,
                name: "node".into(),
                cmd: "node server.js --port 8080".into(),
                ..ProcInfo::default()
            },
            proc(6, "other", 0.0, 1),
        ];
        let off = ProcView {
            filter: "--port",
            ..view(SortKey::Pid)
        };
        assert!(pids(&procs, &off).is_empty());
        let on = ProcView {
            match_cmd: true,
            ..off
        };
        assert_eq!(pids(&procs, &on), [5]);
    }

    #[test]
    fn sort_key_cycle() {
        let mut key = SortKey::Cpu;
        let mut seen = vec![key];
        for _ in 0..5 {
            key = key.next();
            seen.push(key);
        }
        assert_eq!(
            seen,
            [
                SortKey::Cpu,
                SortKey::Memory,
                SortKey::Io,
                SortKey::Pid,
                SortKey::Name,
                SortKey::Cpu
            ]
        );
    }

    #[test]
    fn sort_key_names_round_trip() {
        for key in [
            SortKey::Pid,
            SortKey::Name,
            SortKey::Cpu,
            SortKey::Memory,
            SortKey::Io,
        ] {
            assert_eq!(SortKey::from_name(key.name()), Some(key));
        }
        assert_eq!(SortKey::Memory.name(), "mem");
        assert_eq!(SortKey::from_name("fast"), None);
        assert_eq!(SortKey::Cpu.title(), "CPU%");
        assert_eq!(SortKey::Io.title(), "IO");
        assert!(SortKey::Io.descending());
        assert!(!SortKey::Name.descending());
    }

    #[test]
    fn reverse_flips_each_key() {
        let procs = vec![
            ProcInfo {
                pid: 1,
                name: "a".into(),
                cpu: 1.0,
                memory: 1,
                read_rate: 1,
                ..ProcInfo::default()
            },
            ProcInfo {
                pid: 2,
                name: "b".into(),
                cpu: 2.0,
                memory: 2,
                read_rate: 2,
                ..ProcInfo::default()
            },
        ];
        for key in [
            SortKey::Pid,
            SortKey::Name,
            SortKey::Cpu,
            SortKey::Memory,
            SortKey::Io,
        ] {
            let forward = pids(&procs, &view(key));
            let backward = pids(
                &procs,
                &ProcView {
                    reverse: true,
                    ..view(key)
                },
            );
            let mut flipped = forward.clone();
            flipped.reverse();
            assert_eq!(backward, flipped, "{key:?}");
        }
    }

    #[test]
    fn tree_orders_depth_first_with_prefixes() {
        let procs = vec![
            child(20, 10),
            child(11, 1),
            ProcInfo {
                pid: 1,
                name: "init".into(),
                ..ProcInfo::default()
            },
            child(30, 999),
            child(10, 1),
        ];
        let v = ProcView {
            tree: true,
            ..view(SortKey::Pid)
        };
        let rows = visible_rows(&procs, &v);
        let got: Vec<(u32, &str)> = rows
            .iter()
            .map(|r| (r.proc.pid, r.prefix.as_str()))
            .collect();
        assert_eq!(
            got,
            [(1, ""), (10, "├─"), (20, "│ └─"), (11, "└─"), (30, "")]
        );
    }

    #[test]
    fn tree_prefix_under_a_last_child_uses_spaces() {
        let procs = vec![
            ProcInfo {
                pid: 1,
                ..ProcInfo::default()
            },
            child(2, 1),
            child(3, 2),
            child(4, 3),
            child(5, 3),
        ];
        let v = ProcView {
            tree: true,
            ..view(SortKey::Pid)
        };
        let prefixes: Vec<String> = visible_rows(&procs, &v)
            .into_iter()
            .map(|r| r.prefix)
            .collect();
        assert_eq!(prefixes, ["", "└─", "  └─", "    ├─", "    └─"]);
    }

    #[test]
    fn deep_tree_prefixes_stop_growing() {
        // A chain of nested processes: each one the only child of the one before.
        let depth = 5_000u32;
        let mut procs = vec![ProcInfo {
            pid: 1,
            ..ProcInfo::default()
        }];
        procs.extend((2..=depth).map(|pid| child(pid, pid - 1)));
        let v = ProcView {
            tree: true,
            ..view(SortKey::Pid)
        };
        let rows = visible_rows(&procs, &v);
        let got: Vec<u32> = rows.iter().map(|r| r.proc.pid).collect();
        assert_eq!(got, (1..=depth).collect::<Vec<_>>(), "every row, in order");
        let longest = rows.iter().map(|r| r.prefix.chars().count()).max();
        assert_eq!(
            longest,
            Some(2 * TREE_MAX_DEPTH + 1),
            "levels, then one `…`"
        );
        let deep = &rows[depth as usize - 1].prefix;
        assert!(deep.ends_with("…└─"), "{deep:?}");
        // Up to the cap the prefix is exact.
        assert_eq!(
            rows[TREE_MAX_DEPTH].prefix,
            format!("{}└─", "  ".repeat(TREE_MAX_DEPTH - 1))
        );
    }

    #[test]
    fn tree_sort_orders_siblings() {
        let mut a = child(10, 1);
        a.cpu = 1.0;
        let mut b = child(11, 1);
        b.cpu = 9.0;
        let procs = vec![
            ProcInfo {
                pid: 1,
                ..ProcInfo::default()
            },
            a,
            b,
        ];
        let v = ProcView {
            tree: true,
            ..view(SortKey::Cpu)
        };
        assert_eq!(pids(&procs, &v), [1, 11, 10]);
    }

    #[test]
    fn tree_with_a_parent_loop_shows_each_row_once() {
        let procs = vec![child(5, 6), child(6, 5)];
        let v = ProcView {
            tree: true,
            ..view(SortKey::Pid)
        };
        let rows = visible_rows(&procs, &v);
        let got: Vec<(u32, &str)> = rows
            .iter()
            .map(|r| (r.proc.pid, r.prefix.as_str()))
            .collect();
        assert_eq!(got, [(5, ""), (6, "└─")]);
    }

    #[test]
    fn tree_with_a_self_parent_shows_once() {
        let procs = vec![child(7, 7)];
        let v = ProcView {
            tree: true,
            ..view(SortKey::Pid)
        };
        assert_eq!(pids(&procs, &v), [7]);
    }

    #[test]
    fn tree_filter_makes_an_orphaned_match_a_root() {
        let procs = vec![
            ProcInfo {
                pid: 1,
                name: "init".into(),
                ..ProcInfo::default()
            },
            ProcInfo {
                pid: 10,
                parent: Some(1),
                name: "sshd".into(),
                ..ProcInfo::default()
            },
        ];
        let v = ProcView {
            filter: "ssh",
            tree: true,
            ..view(SortKey::Pid)
        };
        let rows = visible_rows(&procs, &v);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].proc.pid, rows[0].prefix.as_str()), (10, ""));
    }

    #[test]
    fn flat_view_has_no_prefix() {
        let procs = vec![child(2, 1), proc(1, "init", 0.0, 0)];
        let rows = visible_rows(&procs, &view(SortKey::Pid));
        assert!(rows.iter().all(|r| r.prefix.is_empty()));
    }

    #[test]
    fn join_cmd_joins_with_spaces() {
        let parts: Vec<OsString> = ["/bin/sh", "-c", "echo hi"].map(OsString::from).into();
        assert_eq!(join_cmd(&parts), "/bin/sh -c echo hi");
        assert_eq!(join_cmd(&[]), "");
    }

    #[test]
    fn join_cmd_cuts_at_4096_chars() {
        let long = "é".repeat(10_000);
        let joined = join_cmd(&[OsString::from(long)]);
        assert_eq!(joined.chars().count(), 4096);
        assert!(joined.chars().all(|c| c == 'é'));
    }

    #[test]
    fn proc_state_codes() {
        let table = [
            (ProcState::Run, 'R'),
            (ProcState::Sleep, 'S'),
            (ProcState::DiskSleep, 'D'),
            (ProcState::Stop, 'T'),
            (ProcState::Zombie, 'Z'),
            (ProcState::Idle, 'I'),
            (ProcState::Dead, 'X'),
            (ProcState::Unknown, '?'),
        ];
        for (state, code) in table {
            assert_eq!(state.code(), code, "{state:?}");
        }
    }

    #[test]
    fn proc_state_from_sysinfo() {
        let table = [
            (ProcessStatus::Run, ProcState::Run),
            (ProcessStatus::Sleep, ProcState::Sleep),
            (
                ProcessStatus::UninterruptibleDiskSleep,
                ProcState::DiskSleep,
            ),
            (ProcessStatus::Stop, ProcState::Stop),
            (ProcessStatus::Tracing, ProcState::Stop),
            (ProcessStatus::Suspended, ProcState::Stop),
            (ProcessStatus::Zombie, ProcState::Zombie),
            (ProcessStatus::Idle, ProcState::Idle),
            (ProcessStatus::Dead, ProcState::Dead),
            (ProcessStatus::Wakekill, ProcState::Unknown),
            (ProcessStatus::Waking, ProcState::Unknown),
            (ProcessStatus::Parked, ProcState::Unknown),
            (ProcessStatus::LockBlocked, ProcState::Unknown),
            (ProcessStatus::Unknown(7), ProcState::Unknown),
        ];
        for (status, state) in table {
            assert_eq!(ProcState::from_sysinfo(status), state, "{status:?}");
        }
        assert_eq!(
            ProcState::from_sysinfo(ProcessStatus::UninterruptibleDiskSleep).code(),
            'D'
        );
        assert_eq!(
            ProcState::from_sysinfo(ProcessStatus::Unknown(7)).code(),
            '?'
        );
    }

    #[test]
    fn linux_threads_adds_the_main_thread() {
        assert_eq!(linux_threads(Some(0)), Some(1));
        assert_eq!(linux_threads(Some(3)), Some(4));
        assert_eq!(linux_threads(None), None);
    }

    #[test]
    fn refresh_kind_fast_tick() {
        let kind = refresh_kind(false);
        assert_eq!(kind.cmd(), UpdateKind::Never);
        assert_eq!(kind.exe(), UpdateKind::Never);
        assert_eq!(kind.user(), UpdateKind::OnlyIfNotSet);
        assert!(kind.memory() && kind.cpu() && kind.disk_usage() && kind.tasks());
        assert_eq!(kind.cwd(), UpdateKind::Never);
        assert_eq!(kind.environ(), UpdateKind::Never);
        assert_eq!(kind.root(), UpdateKind::Never);
    }

    #[test]
    fn refresh_kind_slow_tick_adds_cmd() {
        let kind = refresh_kind(true);
        assert_eq!(kind.cmd(), UpdateKind::OnlyIfNotSet);
        assert_eq!(kind.exe(), UpdateKind::Never);
        assert_eq!(kind.user(), UpdateKind::OnlyIfNotSet);
        assert!(kind.memory() && kind.cpu() && kind.disk_usage() && kind.tasks());
    }

    #[test]
    fn io_rate_is_zero_on_the_first_sample() {
        assert_eq!(io_rate(1_000_000, 1.0, true), 0);
        assert_eq!(io_rate(1_000, 0.5, false), 2_000);
        assert_eq!(io_rate(1_000, 0.0, false), 1_000_000, "secs floor 0.001");
        assert_eq!(io_rate(1_000, f64::NAN, false), 1_000_000, "NaN secs");
    }

    #[test]
    fn check_target_refuses_protected_pids() {
        assert_eq!(check_target(0, 99), Err(KillError::Protected));
        assert_eq!(check_target(1, 99), Err(KillError::Protected));
        assert_eq!(check_target(99, 99), Err(KillError::Protected));
        assert_eq!(check_target(500, 99), Ok(()));
    }

    #[cfg(windows)]
    #[test]
    fn check_target_refuses_the_windows_system_pid() {
        assert_eq!(check_target(4, 99), Err(KillError::Protected));
    }

    fn report(signal: KillSignal, result: Result<(), KillError>) -> KillReport {
        KillReport {
            request: KillRequest {
                target: KillTarget {
                    pid: 4242,
                    start_time: 1,
                    name: "cargo".into(),
                },
                signal,
            },
            result,
        }
    }

    #[test]
    fn kill_report_messages() {
        assert_eq!(KillSignal::Term.label(), "TERM");
        assert_eq!(KillSignal::Kill.label(), "KILL");
        assert_eq!(
            report(KillSignal::Term, Ok(())).message(),
            "sent TERM to 4242 cargo"
        );
        assert_eq!(
            report(KillSignal::Kill, Ok(())).message(),
            "sent KILL to 4242 cargo"
        );
        assert_eq!(
            report(KillSignal::Term, Err(KillError::Protected)).message(),
            "refused: pid 4242 is protected"
        );
        assert_eq!(
            report(KillSignal::Term, Err(KillError::Gone)).message(),
            "4242 cargo is not running now"
        );
        assert_eq!(
            report(KillSignal::Term, Err(KillError::Unsupported)).message(),
            "TERM is not supported here; press x then f to force"
        );
        assert_eq!(
            report(KillSignal::Kill, Err(KillError::Failed)).message(),
            "could not signal 4242 cargo: permission denied or process gone"
        );
    }

    #[test]
    fn send_signal_refuses_a_protected_pid_without_a_refresh() {
        let mut sys = System::new();
        let req = KillRequest {
            target: KillTarget {
                pid: 1,
                start_time: 0,
                name: "init".into(),
            },
            signal: KillSignal::Kill,
        };
        let report = send_signal(&mut sys, &req, 99);
        assert_eq!(report.result, Err(KillError::Protected));
        assert_eq!(report.request, req);
    }

    #[cfg(unix)]
    #[test]
    fn send_signal_checks_pid_and_start_time() {
        use std::process::Command;

        let mut first = Command::new("sleep").arg("30").spawn().unwrap();
        let mut second = Command::new("sleep").arg("30").spawn().unwrap();
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind(true));
        let start_of = |sys: &System, pid: u32| {
            sys.process(Pid::from_u32(pid))
                .map(|p| p.start_time())
                .unwrap()
        };
        let target = |pid: u32, start_time: u64| KillTarget {
            pid,
            start_time,
            name: "sleep".into(),
        };
        let self_pid = std::process::id();

        let req = KillRequest {
            target: target(first.id(), start_of(&sys, first.id())),
            signal: KillSignal::Term,
        };
        assert_eq!(send_signal(&mut sys, &req, self_pid).result, Ok(()));
        first.wait().unwrap();
        assert_eq!(
            send_signal(&mut sys, &req, self_pid).result,
            Err(KillError::Gone),
            "reaped child"
        );

        let wrong = KillRequest {
            target: target(second.id(), start_of(&sys, second.id()) + 1_000),
            signal: KillSignal::Kill,
        };
        assert_eq!(
            send_signal(&mut sys, &wrong, self_pid).result,
            Err(KillError::Gone),
            "start time differs"
        );
        assert!(second.try_wait().unwrap().is_none(), "second child alive");
        second.kill().unwrap();
        second.wait().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn send_signal_checks_pid_and_start_time() {
        use std::process::{Command, Stdio};

        let ping = || {
            Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut first = ping();
        let mut second = ping();
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind(true));
        let start_of = |sys: &System, pid: u32| {
            sys.process(Pid::from_u32(pid))
                .map(|p| p.start_time())
                .unwrap()
        };
        let target = |pid: u32, start_time: u64| KillTarget {
            pid,
            start_time,
            name: "ping".into(),
        };
        let self_pid = std::process::id();

        let term = KillRequest {
            target: target(first.id(), start_of(&sys, first.id())),
            signal: KillSignal::Term,
        };
        assert_eq!(
            send_signal(&mut sys, &term, self_pid).result,
            Err(KillError::Unsupported)
        );
        let kill = KillRequest {
            signal: KillSignal::Kill,
            ..term
        };
        assert_eq!(send_signal(&mut sys, &kill, self_pid).result, Ok(()));
        first.wait().unwrap();
        assert_eq!(
            send_signal(&mut sys, &kill, self_pid).result,
            Err(KillError::Gone),
            "reaped child"
        );

        let wrong = KillRequest {
            target: target(second.id(), start_of(&sys, second.id()) + 1_000),
            signal: KillSignal::Kill,
        };
        assert_eq!(
            send_signal(&mut sys, &wrong, self_pid).result,
            Err(KillError::Gone),
            "start time differs"
        );
        assert!(second.try_wait().unwrap().is_none(), "second child alive");
        second.kill().unwrap();
        second.wait().unwrap();
    }

    #[test]
    fn collector_reads_this_process() {
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind(true));
        let mut collector = ProcCollector::new();
        let procs = collector.collect(&sys, 1.0, true);
        let me = procs
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("own process listed");
        assert!(!me.name.is_empty());
        assert!(me.start_time > 0);
        assert_eq!((me.read_rate, me.write_rate), (0, 0), "first sample");
        assert!(!me.cmd.is_empty(), "slow tick reads the command line");
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(me.threads.unwrap_or(0) >= 1, "threads: {:?}", me.threads);
        let users = sysinfo::Users::new_with_refreshed_list();
        let expected = sys
            .process(Pid::from_u32(me.pid))
            .and_then(|p| p.user_id())
            .and_then(|uid| users.get_user_by_id(uid))
            .map(|user| user.name());
        assert_eq!(me.user.as_deref(), expected, "own user name");
        // The cache drops PIDs that are gone.
        let empty = System::new();
        assert!(collector.collect(&empty, 1.0, false).is_empty());
        assert!(collector.known.is_empty());
    }
}
