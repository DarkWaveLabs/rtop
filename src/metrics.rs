//! The sampler thread. It owns the `sysinfo` handles, and it sends one
//! [`Snapshot`] to the UI thread on each tick.
//!
//! No state is shared and there are no locks. The only shared values are the
//! interval and the set of panels the UI drew last, which are atomics. The
//! compiler proves the rest is safe: a `Snapshot` is moved into the channel, so
//! the two threads can never touch the same data at the same time.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use sysinfo::Networks;
use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, System};

use crate::app::Event;
use crate::net::{self, NetIface};
use crate::panel::{PanelId, PanelSet};
use crate::process::{KillReport, KillRequest, ProcCollector};
use crate::sensors::{SensorCollector, Sensors, TempWork};
use crate::storage::{Storage, StorageCollector};
use crate::zfs::{ZfsArc, ZfsCollector};

/// Facts about the machine that do not change while rtop runs.
#[derive(Clone, Debug, Default)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpu_brand: String,
    pub physical_cores: Option<usize>,
}

impl HostInfo {
    pub fn detect() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        Self {
            hostname: System::host_name().unwrap_or_else(|| "unknown".into()),
            os: System::long_os_version().unwrap_or_else(|| "unknown OS".into()),
            arch: System::cpu_arch(),
            cpu_brand: sys
                .cpus()
                .first()
                .map(|cpu| cpu.brand().trim().to_owned())
                .unwrap_or_default(),
            physical_cores: System::physical_core_count(),
        }
    }
}

pub use crate::process::ProcInfo;

/// One reading of the whole system.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cpus: Vec<f32>,
    pub cpu_total: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub rx_rate: u64,
    pub tx_rate: u64,
    pub load: [f64; 3],
    pub uptime: u64,
    pub processes: Vec<ProcInfo>,
    pub interfaces: Vec<NetIface>,
    pub storage: Storage,
    pub zfs: Option<ZfsArc>,
    pub sensors: Sensors,
}

/// How often the slow work runs: the disk list, the battery, the user list, command lines
/// and macOS thread counts. Also how often the drawn sensors panel reads its hottest sensors.
const SLOW_PERIOD: Duration = Duration::from_secs(5);

/// How often the full sensor scan and the space of all disks run, while their panel is drawn.
const SCAN_PERIOD: Duration = Duration::from_secs(30);

/// A message from the UI thread to the sampler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    Kill(KillRequest),
    /// The interval changed: read it again now.
    Wake,
    /// Stop or start sampling. Kills still work while paused.
    Pause(bool),
}

/// Starts the sampler thread. It stops by itself when the UI drops the receiver.
/// It also sends the kill requests that come in on `control`, and reports each one as
/// [`Event::KillDone`].
///
/// `drawn` is the `PanelSet` bit mask of the panels the UI drew last. The sampler reads it
/// at the start of each tick, and skips some slow work for panels that are not drawn.
pub fn spawn_sampler(
    tx: Sender<Event>,
    interval_ms: Arc<AtomicU64>,
    drawn: Arc<AtomicU16>,
    control: Receiver<Control>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::new(drawn);
            // CPU usage is a delta between two refreshes, so the first reading needs a short gap.
            thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
            loop {
                let started = Instant::now();
                if tx.send(Event::Metrics(sampler.sample())).is_err() {
                    break;
                }
                if wait_until_due(&mut sampler, &interval_ms, started, &control, &tx).is_err() {
                    break;
                }
            }
        })?;
    Ok(())
}

/// The UI dropped its receiver, so the sampler stops.
struct Disconnected;

struct Sampler {
    sys: System,
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    networks: Networks,
    /// Not sysinfo's `Networks` on Linux: see `net::SysfsNet`.
    #[cfg(target_os = "linux")]
    networks: net::SysfsNet,
    /// Not sysinfo's `Networks` on macOS: see `net::RouteNet`.
    #[cfg(target_os = "macos")]
    networks: net::RouteNet,
    last: Instant,
    clocks: Clocks,
    /// The panels the UI drew last, as `PanelSet` bits.
    drawn: Arc<AtomicU16>,
    procs: ProcCollector,
    storage: StorageCollector,
    zfs: ZfsCollector,
    sensors: SensorCollector,
}

impl Sampler {
    fn new(drawn: Arc<AtomicU16>) -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        // A baseline, so the first tick has CPU and I/O rates. The list itself is not used.
        let mut procs = ProcCollector::new();
        procs.collect(&mut sys, 1.0, true);
        #[cfg(target_os = "linux")]
        let mut networks = net::SysfsNet::default();
        #[cfg(target_os = "macos")]
        let mut networks = net::RouteNet::default();
        // The first read only sets the baseline for the first rates.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        networks.collect(0.0);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let networks = Networks::new_with_refreshed_list();
        Self {
            sys,
            networks,
            last: Instant::now(),
            clocks: Clocks::default(),
            drawn,
            procs,
            storage: StorageCollector::new(),
            zfs: ZfsCollector::new(),
            sensors: SensorCollector::new(),
        }
    }

    fn sample(&mut self) -> Snapshot {
        // A stale value costs at most one extra or one skipped refresh.
        let drawn = PanelSet::from_bits(self.drawn.load(Ordering::Relaxed));
        let work = self
            .clocks
            .plan(Instant::now(), drawn, self.sensors.has_temps());
        let slow = work.slow;

        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        let secs = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();

        let interfaces = self.read_networks(secs);
        let (rx_rate, tx_rate) = net::totals(&interfaces);
        let processes = self.procs.collect(&mut self.sys, secs, slow);
        let storage = self.storage.collect(secs, slow, work.space);
        let zfs = self.zfs.collect(slow);
        let sensors = self.sensors.collect(work.temps, slow);

        let mem_used = self.sys.used_memory();
        #[cfg(target_os = "linux")]
        let mem_used = linux_mem_used(mem_used, zfs.as_ref());

        let load = System::load_average();
        Snapshot {
            cpus: self.sys.cpus().iter().map(|cpu| cpu.cpu_usage()).collect(),
            cpu_total: self.sys.global_cpu_usage(),
            mem_used,
            mem_total: self.sys.total_memory(),
            swap_used: self.sys.used_swap(),
            swap_total: self.sys.total_swap(),
            rx_rate,
            tx_rate,
            load: [load.one, load.five, load.fifteen],
            uptime: System::uptime(),
            processes,
            interfaces,
            storage,
            zfs,
            sensors,
        }
    }

    /// The interfaces and their rates over the last `secs` seconds. The list leaves out
    /// loopback, which never leaves the machine.
    fn read_networks(&mut self, secs: f64) -> Vec<NetIface> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            self.networks.refresh(true);
            net::collect(&self.networks, secs)
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.networks.collect(secs)
    }
}

/// What the wait between ticks needs from the sampler. A trait, so the tests can count
/// the calls without reading the machine.
trait Tick {
    /// Checks the target again and sends the signal.
    fn kill(&mut self, req: &KillRequest) -> KillReport;
    /// Takes one sample and drops it, so the next one has fresh baselines for its rates.
    fn baseline(&mut self);
}

impl Tick for Sampler {
    fn kill(&mut self, req: &KillRequest) -> KillReport {
        self.procs.signal(&mut self.sys, req, std::process::id())
    }

    fn baseline(&mut self) {
        self.sample();
    }
}

/// Blocks until the next tick is due: one interval after `started`. It handles each
/// control message as it comes. A kill is sent at once. `Wake` makes it read the interval
/// again, so a key press that changes the interval takes effect at once. While paused it
/// samples nothing; on resume it takes one baseline sample and the next tick is one
/// interval after it. A closed control channel only means that no more messages come:
/// it then sleeps until the tick is due, and sampling goes on.
fn wait_until_due(
    tick: &mut impl Tick,
    interval_ms: &AtomicU64,
    started: Instant,
    control: &Receiver<Control>,
    tx: &Sender<Event>,
) -> Result<(), Disconnected> {
    let mut started = started;
    let mut paused = false;
    loop {
        let due = Duration::from_millis(interval_ms.load(Ordering::Relaxed));
        let message = if paused {
            control.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            let elapsed = started.elapsed();
            if elapsed >= due {
                return Ok(());
            }
            control.recv_timeout(due - elapsed)
        };
        match message {
            Ok(Control::Kill(req)) => {
                let report = tick.kill(&req);
                tx.send(Event::KillDone(report)).map_err(|_| Disconnected)?;
            }
            // The loop reads the interval again.
            Ok(Control::Wake) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Control::Pause(pause)) => {
                if paused && !pause {
                    tick.baseline();
                    started = Instant::now();
                }
                paused = pause;
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(rest) = due.checked_sub(started.elapsed()) {
                    thread::sleep(rest);
                }
                return Ok(());
            }
        }
    }
}

/// Pure. True on the first tick (`last` is `None`), then once each `period`.
fn due(last: Option<Instant>, now: Instant, period: Duration) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= period)
}

/// When each kind of periodic work last ran. `None` means never, so the first tick runs it.
#[derive(Clone, Copy, Debug, Default)]
struct Clocks {
    slow: Option<Instant>,
    sensors_scan: Option<Instant>,
    /// Any temperature read: the hottest sensors or a full scan.
    sensors_read: Option<Instant>,
    space: Option<Instant>,
}

/// The periodic work of one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work {
    /// The 5 s work: the disk list, the battery, the user list, command lines.
    slow: bool,
    temps: TempWork,
    /// The space of all disks.
    space: bool,
}

impl Clocks {
    /// Pure. The work of a tick at `now`, and the clocks of that work move to `now`.
    /// `drawn` is the set of panels the UI drew last. `has_temps` is false when the last
    /// sensor reading had no temperatures.
    fn plan(&mut self, now: Instant, drawn: PanelSet, has_temps: bool) -> Work {
        let sensors = drawn.contains(PanelId::Sensors);
        // The first scan finds out if the host has sensors at all. An empty reading hides
        // the panel, so it is never drawn: scan it again whatever is drawn.
        let scan = due(self.sensors_scan, now, SCAN_PERIOD)
            && (self.sensors_scan.is_none() || sensors || !has_temps);
        let temps = if scan {
            TempWork::Scan
        } else if sensors && due(self.sensors_read, now, SLOW_PERIOD) {
            TempWork::Hottest
        } else {
            TempWork::Keep
        };
        let space = due(self.space, now, SCAN_PERIOD)
            && (self.space.is_none() || drawn.contains(PanelId::Storage));
        let work = Work {
            slow: due(self.slow, now, SLOW_PERIOD),
            temps,
            space,
        };
        let moves = [
            (work.slow, &mut self.slow),
            (scan, &mut self.sensors_scan),
            (temps != TempWork::Keep, &mut self.sensors_read),
            (space, &mut self.space),
        ];
        for (ran, clock) in moves {
            if ran {
                *clock = Some(now);
            }
        }
        work
    }
}

/// Pure. Linux used memory with the shrinkable ARC counted as free (design §7.6, option A).
/// `sysinfo` gives `total − MemAvailable`, and `MemAvailable` leaves the ARC out.
#[cfg(any(test, target_os = "linux"))]
fn linux_mem_used(used: u64, zfs: Option<&ZfsArc>) -> u64 {
    zfs.map_or(used, |arc| crate::zfs::arc_adjusted_used(used, arc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{KillSignal, KillTarget};
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    #[test]
    fn due_on_the_first_tick_then_once_each_period() {
        let base = Instant::now();
        assert!(due(None, base, SLOW_PERIOD));
        assert!(!due(Some(base), base + Duration::from_secs(4), SLOW_PERIOD));
        assert!(due(Some(base), base + SLOW_PERIOD, SLOW_PERIOD));
        assert!(!due(Some(base), base + SLOW_PERIOD, SCAN_PERIOD));
        assert!(due(Some(base), base + SCAN_PERIOD, SCAN_PERIOD));
        assert!(
            !due(Some(base + Duration::from_secs(1)), base, SLOW_PERIOD),
            "a clock step back is not due"
        );
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn without(id: PanelId) -> PanelSet {
        let mut set = PanelSet::ALL;
        set.remove(id);
        set
    }

    #[test]
    fn the_first_tick_does_everything_whatever_is_drawn() {
        let mut clocks = Clocks::default();
        let work = clocks.plan(Instant::now(), PanelSet::NONE, false);
        assert_eq!(
            work,
            Work {
                slow: true,
                temps: TempWork::Scan,
                space: true
            }
        );
    }

    #[test]
    fn drawn_sensors_read_the_hottest_each_5_s_and_scan_each_30_s() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        clocks.plan(t0, PanelSet::ALL, true);
        let temps = |clocks: &mut Clocks, at| clocks.plan(t0 + secs(at), PanelSet::ALL, true).temps;
        assert_eq!(temps(&mut clocks, 1), TempWork::Keep);
        assert_eq!(temps(&mut clocks, 5), TempWork::Hottest);
        assert_eq!(temps(&mut clocks, 6), TempWork::Keep);
        assert_eq!(temps(&mut clocks, 10), TempWork::Hottest);
        assert_eq!(temps(&mut clocks, 30), TempWork::Scan);
        assert_eq!(temps(&mut clocks, 31), TempWork::Keep);
        assert_eq!(
            temps(&mut clocks, 35),
            TempWork::Hottest,
            "5 s after the scan"
        );
    }

    #[test]
    fn hidden_sensors_are_not_read() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        clocks.plan(t0, PanelSet::ALL, true);
        for at in [5, 10, 30, 60, 95] {
            let work = clocks.plan(t0 + secs(at), without(PanelId::Sensors), true);
            assert_eq!(work.temps, TempWork::Keep, "at {at} s");
        }
    }

    #[test]
    fn an_empty_reading_is_scanned_again_each_30_s_whatever_is_drawn() {
        // An empty reading hides the panel, so it is never drawn. Without this rule it
        // would never be read again.
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        clocks.plan(t0, PanelSet::ALL, false);
        let hidden = without(PanelId::Sensors);
        assert_eq!(
            clocks.plan(t0 + secs(5), hidden, false).temps,
            TempWork::Keep
        );
        assert_eq!(
            clocks.plan(t0 + secs(30), hidden, false).temps,
            TempWork::Scan
        );
        assert_eq!(
            clocks.plan(t0 + secs(35), hidden, false).temps,
            TempWork::Keep
        );
        assert_eq!(
            clocks.plan(t0 + secs(60), hidden, false).temps,
            TempWork::Scan
        );
    }

    #[test]
    fn sensors_drawn_again_with_an_old_reading_refresh_on_the_next_tick() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        clocks.plan(t0, PanelSet::ALL, true);
        let hidden = without(PanelId::Sensors);
        for at in [5, 10, 15] {
            clocks.plan(t0 + secs(at), hidden, true);
        }
        let work = clocks.plan(t0 + secs(16), PanelSet::ALL, true);
        assert_eq!(work.temps, TempWork::Hottest);
        assert!(!work.slow, "not on the 5 s grid, but read at once");

        // Hidden for longer than 30 s: the next tick is the full scan.
        clocks.plan(t0 + secs(17), hidden, true);
        let work = clocks.plan(t0 + secs(50), PanelSet::ALL, true);
        assert_eq!(work.temps, TempWork::Scan);
    }

    #[test]
    fn disk_space_is_read_each_30_s_while_storage_is_drawn() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        clocks.plan(t0, PanelSet::ALL, true);
        let space = |clocks: &mut Clocks, at, drawn| clocks.plan(t0 + secs(at), drawn, true).space;
        assert!(!space(&mut clocks, 5, PanelSet::ALL));
        assert!(space(&mut clocks, 30, PanelSet::ALL));
        assert!(!space(&mut clocks, 35, PanelSet::ALL));
        assert!(!space(&mut clocks, 60, without(PanelId::Storage)));
        assert!(
            space(&mut clocks, 61, PanelSet::ALL),
            "drawn again: read at once"
        );
    }

    #[test]
    fn the_slow_work_keeps_its_5_s_period() {
        let t0 = Instant::now();
        let mut clocks = Clocks::default();
        clocks.plan(t0, PanelSet::NONE, true);
        let slow = |clocks: &mut Clocks, at| clocks.plan(t0 + secs(at), PanelSet::NONE, true).slow;
        assert!(!slow(&mut clocks, 4));
        assert!(slow(&mut clocks, 5));
        assert!(!slow(&mut clocks, 9));
        assert!(slow(&mut clocks, 10));
    }

    /// The cost of each kind of sampler tick on this host, for performance work. Run it with
    /// `cargo test --release sample_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the real machine and prints timings"]
    fn sample_cost() {
        const ROUNDS: usize = 5;
        const FAST_PER_ROUND: usize = 3;
        let drawn = Arc::new(AtomicU16::new(PanelSet::ALL.bits()));
        let mut sampler = Sampler::new(Arc::clone(&drawn));
        thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
        // The first tick also discovers everything, so it is not counted.
        let first = sampler.sample();

        let ago = |n: u64| {
            Some(
                Instant::now()
                    .checked_sub(secs(n))
                    .expect("uptime over 31 s"),
            )
        };
        // The clocks and the drawn panels for each kind of tick.
        let fast = |_: &mut Clocks| PanelSet::ALL;
        let five = |c: &mut Clocks| {
            (c.slow, c.sensors_read) = (ago(6), ago(6));
            PanelSet::ALL
        };
        let five_hidden = |c: &mut Clocks| {
            (c.slow, c.sensors_read) = (ago(6), ago(6));
            without(PanelId::Sensors)
        };
        let thirty = |c: &mut Clocks| {
            *c = Clocks {
                slow: ago(31),
                sensors_scan: ago(31),
                sensors_read: ago(31),
                space: ago(31),
            };
            PanelSet::ALL
        };
        type Setup<'a> = &'a dyn Fn(&mut Clocks) -> PanelSet;
        let kinds: [(&str, Setup); 4] = [
            ("fast tick", &fast),
            ("5 s tick", &five),
            ("5 s tick, sensors hidden", &five_hidden),
            ("30 s tick", &thirty),
        ];
        let mut times = vec![Vec::new(); kinds.len()];
        for _ in 0..ROUNDS {
            let order = std::iter::repeat_n(0, FAST_PER_ROUND).chain(1..kinds.len());
            for kind in order {
                thread::sleep(Duration::from_millis(100));
                let now = Some(Instant::now());
                sampler.clocks = Clocks {
                    slow: now,
                    sensors_scan: now,
                    sensors_read: now,
                    space: now,
                };
                let panels = kinds[kind].1(&mut sampler.clocks);
                drawn.store(panels.bits(), Ordering::Relaxed);
                let started = Instant::now();
                sampler.sample();
                times[kind].push(started.elapsed().as_secs_f64() * 1_000.0);
            }
        }
        let stats = |v: &[f64]| {
            let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
            let max = v.iter().copied().fold(0.0, f64::max);
            format!("mean {mean:.2} ms, max {max:.2} ms over {}", v.len())
        };
        println!("sample_cost: {} processes", first.processes.len());
        for ((name, _), v) in kinds.iter().zip(&times) {
            println!("  {name}: {}", stats(v));
        }
    }

    /// The thread CPU time of one call of each network and storage read on this host, for
    /// performance work. Thread CPU time leaves out waits on other processes. Run it with
    /// `cargo test --release collector_cost -- --ignored --nocapture`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "reads the real machine and prints timings"]
    fn collector_cost() {
        const CALLS: usize = 20;
        fn thread_cpu() -> Duration {
            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: `now` is a valid, writable `timespec` that lives for the call, and
            // `CLOCK_THREAD_CPUTIME_ID` is a clock that macOS supports.
            let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut now) };
            assert_eq!(status, 0, "clock_gettime");
            Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
        }
        fn cost(name: &str, calls: usize, mut work: impl FnMut()) {
            let mut wall = 0.0;
            let mut times: Vec<f64> = (0..calls)
                .map(|_| {
                    thread::sleep(Duration::from_millis(20));
                    let started = thread_cpu();
                    let clock = Instant::now();
                    work();
                    wall += clock.elapsed().as_secs_f64() * 1_000.0;
                    (thread_cpu() - started).as_secs_f64() * 1_000.0
                })
                .collect();
            crate::order::sort_by(&mut times, |a, b| a.total_cmp(b));
            let count = times.len().max(1) as f64;
            let mean = times.iter().sum::<f64>() / count;
            let median = times.get(times.len() / 2).copied().unwrap_or(0.0);
            let wall = wall / count;
            println!(
                "  {name}: mean {mean:.3} ms, median {median:.3} ms over {calls} (wall mean {wall:.3} ms)"
            );
        }

        let mut sampler = Sampler::new(Arc::new(AtomicU16::new(PanelSet::ALL.bits())));
        sampler.storage.collect(1.0, true, true);
        let storage = &mut sampler.storage;
        println!("collector_cost (thread CPU time per call):");
        cost("storage fast tick", CALLS, || {
            storage.collect(0.25, false, false);
        });
        cost("storage re-list", CALLS, || {
            storage.collect(0.25, true, false);
        });
        cost("storage space read", 5, || {
            storage.collect(0.25, false, true);
        });
        cost("network", CALLS, || {
            sampler.read_networks(0.25);
        });
    }

    /// A sampler that reads nothing. It counts its kills and baseline samples.
    #[derive(Clone, Default)]
    struct FakeTick {
        kills: Arc<AtomicUsize>,
        baselines: Arc<AtomicUsize>,
    }

    impl Tick for FakeTick {
        fn kill(&mut self, req: &KillRequest) -> KillReport {
            self.kills.fetch_add(1, Ordering::SeqCst);
            KillReport {
                request: req.clone(),
                result: Ok(()),
            }
        }

        fn baseline(&mut self) {
            self.baselines.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn kill_request() -> KillRequest {
        KillRequest {
            target: KillTarget {
                pid: 500,
                ident: 1,
                name: "sleep".into(),
            },
            signal: KillSignal::Term,
        }
    }

    /// Runs `wait_until_due` on its own thread, as the sampler does.
    fn spawn_wait(
        tick: FakeTick,
        interval_ms: u64,
        control: Receiver<Control>,
    ) -> (
        Arc<AtomicU64>,
        Receiver<Event>,
        thread::JoinHandle<Duration>,
    ) {
        let interval = Arc::new(AtomicU64::new(interval_ms));
        let (tx, rx) = mpsc::channel();
        let shared = Arc::clone(&interval);
        let handle = thread::spawn(move || {
            let mut tick = tick;
            let started = Instant::now();
            assert!(
                wait_until_due(&mut tick, &shared, started, &control, &tx).is_ok(),
                "the UI is still there"
            );
            started.elapsed()
        });
        (interval, rx, handle)
    }

    fn kill_done(rx: &Receiver<Event>) -> bool {
        matches!(
            rx.recv_timeout(Duration::from_secs(2)),
            Ok(Event::KillDone(_))
        )
    }

    #[test]
    fn a_kill_is_handled_at_once_and_wake_shortens_the_wait() {
        let tick = FakeTick::default();
        let (control, control_rx) = mpsc::channel();
        let (interval, events, handle) = spawn_wait(tick.clone(), 10_000, control_rx);

        let asked = Instant::now();
        control.send(Control::Kill(kill_request())).unwrap();
        assert!(kill_done(&events));
        assert!(asked.elapsed() < Duration::from_secs(1), "not at the tick");
        assert_eq!(tick.kills.load(Ordering::SeqCst), 1);

        // As the UI does: store the interval first, then wake the sampler.
        interval.store(10, Ordering::Relaxed);
        control.send(Control::Wake).unwrap();
        let waited = handle.join().unwrap();
        assert!(waited < Duration::from_secs(2), "waited {waited:?}");
        assert_eq!(tick.baselines.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_wait_lasts_one_interval_without_messages() {
        let (_control, control_rx) = mpsc::channel();
        let (_, _, handle) = spawn_wait(FakeTick::default(), 60, control_rx);
        let waited = handle.join().unwrap();
        assert!(waited >= Duration::from_millis(60), "waited {waited:?}");
    }

    #[test]
    fn pause_stops_sampling_but_kills_still_work() {
        let tick = FakeTick::default();
        let (control, control_rx) = mpsc::channel();
        control.send(Control::Pause(true)).unwrap();
        let (_, events, handle) = spawn_wait(tick.clone(), 20, control_rx);

        thread::sleep(Duration::from_millis(150));
        assert!(!handle.is_finished(), "paused: no tick is due");

        control.send(Control::Kill(kill_request())).unwrap();
        assert!(kill_done(&events), "KillDone arrives while paused");
        assert_eq!(tick.kills.load(Ordering::SeqCst), 1);
        assert!(!handle.is_finished());
        assert_eq!(tick.baselines.load(Ordering::SeqCst), 0);

        let resumed = Instant::now();
        control.send(Control::Pause(false)).unwrap();
        handle.join().unwrap();
        assert_eq!(
            tick.baselines.load(Ordering::SeqCst),
            1,
            "one sample on resume, not sent"
        );
        assert!(
            resumed.elapsed() >= Duration::from_millis(20),
            "the next sample comes one interval after the baseline"
        );
    }

    #[test]
    fn a_closed_channel_falls_back_to_a_plain_sleep() {
        let (control, control_rx) = mpsc::channel::<Control>();
        drop(control);
        let (_, _, handle) = spawn_wait(FakeTick::default(), 60, control_rx);
        let waited = handle.join().unwrap();
        assert!(waited >= Duration::from_millis(60), "waited {waited:?}");
    }

    #[test]
    fn a_channel_that_closes_while_paused_lets_sampling_go_on() {
        let (control, control_rx) = mpsc::channel();
        control.send(Control::Pause(true)).unwrap();
        let (_, _, handle) = spawn_wait(FakeTick::default(), 20, control_rx);
        thread::sleep(Duration::from_millis(50));
        drop(control);
        let waited = handle.join().unwrap();
        assert!(waited < Duration::from_secs(2), "waited {waited:?}");
    }

    #[test]
    fn a_gone_ui_stops_the_wait() {
        let (control, control_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        drop(rx);
        control.send(Control::Kill(kill_request())).unwrap();
        let interval = AtomicU64::new(10_000);
        let result = wait_until_due(
            &mut FakeTick::default(),
            &interval,
            Instant::now(),
            &control_rx,
            &tx,
        );
        assert!(result.is_err());
    }

    #[test]
    fn shrinkable_arc_counts_as_free_memory() {
        let arc = ZfsArc {
            size: 6_000,
            min: 1_000,
            ..ZfsArc::default()
        };
        assert_eq!(linux_mem_used(10_000, Some(&arc)), 5_000);
        assert_eq!(linux_mem_used(10_000, None), 10_000, "no ZFS: unchanged");
        assert_eq!(linux_mem_used(3_000, Some(&arc)), 0, "never below zero");
    }
}
