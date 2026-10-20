//! The process collector and kill on top of `sysinfo`, for every OS without a native
//! collector. Here `ProcInfo::ident` is the start time in seconds since the epoch.
//! macOS builds it only for its tests, as the reference for the native collector.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[cfg(not(windows))]
use super::KillSignal;
use super::{
    KillError, KillReport, KillRequest, ProcInfo, ProcState, USERS_RELOAD, check_target, io_rate,
    join_cmd,
};

/// The refresh kind for `refresh_processes_specifics`.
/// Every tick: memory, cpu, disk_usage, tasks, user(OnlyIfNotSet).
/// Slow ticks add cmd(OnlyIfNotSet). `exe` is never refreshed: rtop does not read it.
fn refresh_kind(slow: bool) -> ProcessRefreshKind {
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
    name: Arc<str>,
    cmd: Arc<str>,
    threads: Option<u32>,
}

impl Known {
    fn new(start_time: u64, name: &str) -> Self {
        Self {
            start_time,
            name: Arc::from(name),
            cmd: Arc::from(""),
            threads: None,
        }
    }
}

/// Owned by the sampler. Caches names, command lines, user names and thread counts.
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

    /// Refreshes the processes in `sys` and builds the list.
    /// Skips Linux threads (`thread_kind().is_some()`).
    pub fn collect(&mut self, sys: &mut System, secs: f64, slow: bool) -> Vec<ProcInfo> {
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind(slow));
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
                        *known = Known::new(start_time, &p.name().to_string_lossy());
                        first = true;
                    }
                    known
                }
                Entry::Vacant(entry) => {
                    first = true;
                    entry.insert(Known::new(start_time, &p.name().to_string_lossy()))
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

            let disk = p.disk_usage();
            out.push(ProcInfo {
                pid,
                parent: p.parent().map(Pid::as_u32),
                ident: start_time,
                name: Arc::clone(&known.name),
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

    /// Checks the target again with the same start time that `collect` gives, then
    /// sends the signal. On Windows it opens the process instead, compares the start time
    /// of that handle and terminates through the same handle.
    pub fn signal(&mut self, sys: &mut System, req: &KillRequest, self_pid: u32) -> KillReport {
        KillReport {
            request: req.clone(),
            result: signal(sys, req, self_pid),
        }
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

/// Pure. `sysinfo` leaves the main thread out of the Linux task list, so add it back.
#[cfg(any(test, target_os = "linux"))]
fn linux_threads(tasks: Option<usize>) -> Option<u32> {
    tasks.map(|n| u32::try_from(n).unwrap_or(u32::MAX).saturating_add(1))
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
    use super::super::{KillError, KillSignal, KillTarget};
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
        // The same seconds that `sysinfo` computes for `start_time`, which is `ident` here.
        if start_secs(&handle) != Some(target.ident) {
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

/// Refreshes that one PID, compares its start time with `ident`, then calls `kill_with`.
#[cfg(not(windows))]
fn signal(sys: &mut System, req: &KillRequest, self_pid: u32) -> Result<(), KillError> {
    let target = &req.target;
    check_target(target.pid, self_pid)?;
    let pid = Pid::from_u32(target.pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let process = sys
        .process(pid)
        .filter(|p| p.start_time() == target.ident)
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
    use super::super::{KillError, KillSignal, KillTarget};
    use super::*;

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
    fn signal_refuses_a_protected_pid_without_a_refresh() {
        let mut sys = System::new();
        let req = KillRequest {
            target: KillTarget {
                pid: 1,
                ident: 0,
                name: "init".into(),
            },
            signal: KillSignal::Kill,
        };
        let report = ProcCollector::new().signal(&mut sys, &req, 99);
        assert_eq!(report.result, Err(KillError::Protected));
        assert_eq!(report.request, req);
        assert!(sys.processes().is_empty(), "no refresh");
    }

    /// The `ident` of `pid` as `collect` gives it.
    fn ident_of(collector: &mut ProcCollector, sys: &mut System, pid: u32) -> u64 {
        collector
            .collect(sys, 1.0, true)
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.ident)
            .expect("child listed")
    }

    #[cfg(unix)]
    #[test]
    fn signal_checks_pid_and_ident() {
        use std::process::Command;

        let mut first = Command::new("sleep").arg("30").spawn().unwrap();
        let mut second = Command::new("sleep").arg("30").spawn().unwrap();
        let mut sys = System::new();
        let mut collector = ProcCollector::new();
        let target = |pid: u32, ident: u64| KillTarget {
            pid,
            ident,
            name: "sleep".into(),
        };
        let self_pid = std::process::id();

        let req = KillRequest {
            target: target(first.id(), ident_of(&mut collector, &mut sys, first.id())),
            signal: KillSignal::Term,
        };
        assert_eq!(collector.signal(&mut sys, &req, self_pid).result, Ok(()));
        first.wait().unwrap();
        assert_eq!(
            collector.signal(&mut sys, &req, self_pid).result,
            Err(KillError::Gone),
            "reaped child"
        );

        let ident = ident_of(&mut collector, &mut sys, second.id());
        let wrong = KillRequest {
            target: target(second.id(), ident + 1_000),
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

    #[cfg(windows)]
    #[test]
    fn signal_checks_pid_and_ident() {
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
        let mut collector = ProcCollector::new();
        let target = |pid: u32, ident: u64| KillTarget {
            pid,
            ident,
            name: "ping".into(),
        };
        let self_pid = std::process::id();

        let term = KillRequest {
            target: target(first.id(), ident_of(&mut collector, &mut sys, first.id())),
            signal: KillSignal::Term,
        };
        assert_eq!(
            collector.signal(&mut sys, &term, self_pid).result,
            Err(KillError::Unsupported)
        );
        let kill = KillRequest {
            signal: KillSignal::Kill,
            ..term
        };
        assert_eq!(collector.signal(&mut sys, &kill, self_pid).result, Ok(()));
        first.wait().unwrap();
        assert_eq!(
            collector.signal(&mut sys, &kill, self_pid).result,
            Err(KillError::Gone),
            "reaped child"
        );

        let ident = ident_of(&mut collector, &mut sys, second.id());
        let wrong = KillRequest {
            target: target(second.id(), ident + 1_000),
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

    #[test]
    fn collector_reads_this_process() {
        let mut sys = System::new();
        let mut collector = ProcCollector::new();
        let procs = collector.collect(&mut sys, 1.0, true);
        let me = procs
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("own process listed");
        assert!(!me.name.is_empty());
        assert!(me.ident > 0);
        assert_eq!((me.read_rate, me.write_rate), (0, 0), "first sample");
        assert!(!me.cmd.is_empty(), "slow tick reads the command line");
        #[cfg(target_os = "linux")]
        assert!(me.threads.unwrap_or(0) >= 1, "threads: {:?}", me.threads);
        let users = sysinfo::Users::new_with_refreshed_list();
        let expected = sys
            .process(Pid::from_u32(me.pid))
            .and_then(|p| p.user_id())
            .and_then(|uid| users.get_user_by_id(uid))
            .map(|user| user.name());
        assert_eq!(me.user.as_deref(), expected, "own user name");
        // The cache keeps only listed PIDs, and drops PIDs that are gone.
        collector.known.insert(u32::MAX, Known::new(1, "gone"));
        let procs = collector.collect(&mut sys, 1.0, false);
        assert!(!collector.known.contains_key(&u32::MAX));
        assert_eq!(collector.known.len(), procs.len());
    }
}
