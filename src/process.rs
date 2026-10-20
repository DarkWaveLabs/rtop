//! Process data, sorting, filtering, the tree view and kill. The sampler owns the collector.
//!
//! Each OS has one `ProcCollector` with the same interface: `new`, `collect` (the process
//! list for one tick) and `signal` (the kill check and the signal). The kill check reads
//! `ProcInfo::ident` with the same code as `collect`, so the two always agree.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

#[cfg(any(test, not(any(target_os = "macos", target_os = "linux"))))]
use sysinfo::ProcessStatus;

// macOS and Linux read processes natively; Windows and FreeBSD go through `sysinfo`. The
// tests still build the `sysinfo` collector on every OS, as the reference for the parity
// tests, and build the Linux collector's pure parts on every OS.
#[cfg(any(test, target_os = "linux"))]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::ProcCollector;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::ProcCollector;
#[cfg(any(test, not(any(target_os = "macos", target_os = "linux"))))]
mod sysinfo_procs;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub use sysinfo_procs::ProcCollector;

/// The longest command line that rtop keeps, in chars.
const CMD_MAX_CHARS: usize = 4096;
/// The shortest time between two reloads of the user list.
const USERS_RELOAD: Duration = Duration::from_secs(60);
/// Tree levels that the prefix draws. A deeper row shows `…` in place of the levels past
/// this, so a long parent chain costs a fixed prefix size for each row, not one that grows
/// with depth (a chain of N nested processes would otherwise cost about N² bytes).
const TREE_MAX_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcState {
    Run,
    Sleep,
    #[cfg_attr(
        target_os = "macos",
        allow(dead_code, reason = "the macOS collector never reports it")
    )]
    DiskSleep,
    Stop,
    Zombie,
    Idle,
    #[cfg_attr(
        target_os = "macos",
        allow(dead_code, reason = "the macOS collector never reports it")
    )]
    Dead,
    #[default]
    Unknown,
}

impl ProcState {
    /// Maps the `sysinfo` status. Exact table below. Called on every OS whose collector
    /// is `sysinfo`.
    #[cfg(any(test, not(any(target_os = "macos", target_os = "linux"))))]
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
    /// The identity of the process for the kill check: the same for its whole life, and
    /// different after its PID is reused. Each collector says what it uses.
    pub ident: u64,
    pub name: Arc<str>,
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
pub fn join_cmd<S: AsRef<OsStr>>(parts: impl IntoIterator<Item = S>) -> String {
    let mut out = String::new();
    for (i, part) in parts.into_iter().enumerate() {
        // A char is at most 4 bytes, so this many bytes already hold enough chars.
        if out.len() >= CMD_MAX_CHARS * 4 {
            break;
        }
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&part.as_ref().to_string_lossy());
    }
    if let Some((cut, _)) = out.char_indices().nth(CMD_MAX_CHARS) {
        out.truncate(cut);
    }
    out
}

/// Pure. Bytes per second for a delta counter. The first sample of a process gives 0,
/// because its counter holds the total since the process started.
pub(crate) fn io_rate(delta: u64, secs: f64, first: bool) -> u64 {
    if first {
        0
    } else {
        // `as u64` saturates, and `max` maps NaN to the floor.
        (delta as f64 / secs.max(0.001)) as u64
    }
}

/// Pure. CPU time used over an interval as a percent of one core, capped at `cpus × 100`
/// (a sample that spans a late tick cannot show more than every core). 0 for a `secs`
/// that is not a positive number.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub(crate) fn cpu_percent(delta_ns: u64, secs: f64, cpus: usize) -> f32 {
    if !(secs.is_finite() && secs > 0.0) {
        return 0.0;
    }
    let cap = cpus.max(1) as f64 * 100.0;
    (delta_ns as f64 / (secs * 1e9) * 100.0).min(cap) as f32
}

/// The CPUs the host has online, for the cap of `cpu_percent`. Not
/// `available_parallelism`, which follows rtop's own affinity mask and, on Linux, its
/// cgroup quota: under `taskset -c 0 rtop`, another process's `make -j16` would show at
/// most 100%. 1 when `sysconf` fails. On Linux with musl, `sysconf` counts the affinity
/// mask too, so the Linux collector reads sysfs first.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn online_cpus() -> usize {
    // SAFETY: a plain call with no pointers; it returns -1 on error.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    usize::try_from(n).ok().filter(|&n| n > 0).unwrap_or(1)
}

/// User names by real UID, for the native collectors. A miss reloads the list, at most
/// once each `USERS_RELOAD`.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
#[derive(Default)]
pub(crate) struct UserNames {
    names: HashMap<u32, Arc<str>>,
    loaded: Option<std::time::Instant>,
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
impl UserNames {
    pub(crate) fn get(&self, uid: u32) -> Option<Arc<str>> {
        self.names.get(&uid).cloned()
    }

    /// Call after `get` missed. Replaces the names with `load()` unless the last load was
    /// less than `USERS_RELOAD` before `now`. True when it reloaded.
    pub(crate) fn reload_after_miss(
        &mut self,
        now: std::time::Instant,
        load: impl FnOnce() -> HashMap<u32, Arc<str>>,
    ) -> bool {
        let due = self
            .loaded
            .is_none_or(|loaded| now.saturating_duration_since(loaded) >= USERS_RELOAD);
        if due {
            self.names = load();
            self.loaded = Some(now);
        }
        due
    }
}

/// The user list of this machine, by UID, for `UserNames::reload_after_miss`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn system_user_names() -> HashMap<u32, Arc<str>> {
    sysinfo::Users::new_with_refreshed_list()
        .list()
        .iter()
        .map(|user| (**user.id(), Arc::from(user.name())))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillTarget {
    pub pid: u32,
    /// `ProcInfo::ident` of the row that the prompt was opened on.
    pub ident: u64,
    pub name: Arc<str>,
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
    #[cfg_attr(
        any(target_os = "macos", target_os = "linux"),
        allow(dead_code, reason = "the native collectors send every signal")
    )]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

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
            name: format!("p{pid}").into(),
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
        assert_eq!(join_cmd(Vec::<OsString>::new()), "");
        let borrowed = ["a", "b"].map(std::ffi::OsStr::new);
        assert_eq!(join_cmd(borrowed), "a b", "borrowed parts");
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
    fn io_rate_is_zero_on_the_first_sample() {
        assert_eq!(io_rate(1_000_000, 1.0, true), 0);
        assert_eq!(io_rate(1_000, 0.5, false), 2_000);
        assert_eq!(io_rate(1_000, 0.0, false), 1_000_000, "secs floor 0.001");
        assert_eq!(io_rate(1_000, f64::NAN, false), 1_000_000, "NaN secs");
    }

    #[test]
    fn cpu_percent_of_one_core_with_a_cap() {
        assert_eq!(cpu_percent(500_000_000, 1.0, 4), 50.0);
        assert_eq!(cpu_percent(500_000_000, 0.5, 4), 100.0);
        assert_eq!(
            cpu_percent(3_000_000_000, 1.0, 4),
            300.0,
            "three busy cores"
        );
        assert_eq!(
            cpu_percent(9_000_000_000, 1.0, 4),
            400.0,
            "capped at cpus × 100"
        );
        assert_eq!(
            cpu_percent(9_000_000_000, 1.0, 0),
            100.0,
            "no CPU count: one"
        );
        assert_eq!(cpu_percent(0, 1.0, 4), 0.0);
        assert_eq!(cpu_percent(u64::MAX, 1.0, 2), 200.0);
        for secs in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(cpu_percent(1_000, secs, 4), 0.0, "secs {secs}");
        }
    }

    fn names(pairs: &[(u32, &str)]) -> HashMap<u32, Arc<str>> {
        pairs
            .iter()
            .map(|&(uid, name)| (uid, Arc::from(name)))
            .collect()
    }

    #[test]
    fn user_names_reload_on_a_miss_at_most_once_a_minute() {
        let start = std::time::Instant::now();
        let mut users = UserNames::default();
        assert_eq!(users.get(501), None, "nothing loaded");
        assert!(users.reload_after_miss(start, || names(&[(501, "chad")])));
        assert_eq!(users.get(501).as_deref(), Some("chad"));

        let later = start + Duration::from_secs(59);
        let reloaded = users.reload_after_miss(later, || panic!("too soon to reload"));
        assert!(!reloaded);
        assert_eq!(users.get(502), None, "the miss stays until the next reload");

        let due = start + USERS_RELOAD;
        assert!(users.reload_after_miss(due, || names(&[(502, "ann")])));
        assert_eq!(users.get(502).as_deref(), Some("ann"));
        assert_eq!(users.get(501), None, "a reload replaces the list");
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
                    ident: 1,
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
}
