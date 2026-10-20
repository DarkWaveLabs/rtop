//! The sampler thread. It owns the `sysinfo` handles, and it sends one
//! [`Snapshot`] to the UI thread on each tick.
//!
//! No state is shared and there are no locks. The only shared value is the
//! interval, which is an atomic. The compiler proves the rest is safe: a
//! `Snapshot` is moved into the channel, so the two threads can never touch
//! the same data at the same time.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(not(target_os = "linux"))]
use sysinfo::Networks;
use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, System};

use crate::app::Event;
use crate::net::{self, NetIface};
use crate::process::{KillReport, KillRequest, ProcCollector};
use crate::sensors::{SensorCollector, Sensors};
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

/// How often the slow work runs: the disk list and space, sensors, battery, the user list,
/// command lines and macOS thread counts.
const SLOW_PERIOD: Duration = Duration::from_secs(5);

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
pub fn spawn_sampler(
    tx: Sender<Event>,
    interval_ms: Arc<AtomicU64>,
    control: Receiver<Control>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::new();
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
    #[cfg(not(target_os = "linux"))]
    networks: Networks,
    /// Not sysinfo's `Networks` on Linux: see `net::SysfsNet`.
    #[cfg(target_os = "linux")]
    networks: net::SysfsNet,
    last: Instant,
    last_slow: Option<Instant>,
    procs: ProcCollector,
    storage: StorageCollector,
    zfs: ZfsCollector,
    sensors: SensorCollector,
}

impl Sampler {
    fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        // A baseline, so the first tick has CPU and I/O rates. The list itself is not used.
        let mut procs = ProcCollector::new();
        procs.collect(&mut sys, 1.0, true);
        #[cfg(target_os = "linux")]
        let networks = {
            let mut networks = net::SysfsNet::default();
            // The first read only sets the baseline for the first rates.
            networks.collect(0.0);
            networks
        };
        Self {
            sys,
            #[cfg(not(target_os = "linux"))]
            networks: Networks::new_with_refreshed_list(),
            #[cfg(target_os = "linux")]
            networks,
            last: Instant::now(),
            last_slow: None,
            procs,
            storage: StorageCollector::new(),
            zfs: ZfsCollector::new(),
            sensors: SensorCollector::new(),
        }
    }

    fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let slow = is_slow(self.last_slow, now);
        if slow {
            self.last_slow = Some(now);
        }

        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        #[cfg(not(target_os = "linux"))]
        self.networks.refresh(true);

        let secs = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();

        // `net::collect` leaves out loopback, which never leaves the machine.
        #[cfg(not(target_os = "linux"))]
        let interfaces = net::collect(&self.networks, secs);
        #[cfg(target_os = "linux")]
        let interfaces = self.networks.collect(secs);
        let (rx_rate, tx_rate) = net::totals(&interfaces);
        let processes = self.procs.collect(&mut self.sys, secs, slow);
        let storage = self.storage.collect(secs, slow);
        let zfs = self.zfs.collect(slow);
        let sensors = self.sensors.collect(slow);

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

/// Pure. True on the first tick, then once each `SLOW_PERIOD`.
fn is_slow(last_slow: Option<Instant>, now: Instant) -> bool {
    last_slow.is_none_or(|last| now.saturating_duration_since(last) >= SLOW_PERIOD)
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
    fn first_tick_is_slow_then_once_each_period() {
        let base = Instant::now();
        assert!(is_slow(None, base));
        assert!(!is_slow(Some(base), base + Duration::from_secs(4)));
        assert!(is_slow(Some(base), base + SLOW_PERIOD));
        assert!(
            !is_slow(Some(base + Duration::from_secs(1)), base),
            "a clock step back is not slow"
        );
    }

    /// The cost of one sampler tick on this host, for performance work. Run it with
    /// `cargo test --release sample_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the real machine and prints timings"]
    fn sample_cost() {
        const FAST: usize = 15;
        const SLOW: usize = 5;
        let mut sampler = Sampler::new();
        thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
        // The first tick also discovers everything, so it is not counted.
        let first = sampler.sample();
        let mut fast = Vec::with_capacity(FAST);
        let mut slow = Vec::with_capacity(SLOW);
        for i in 0..FAST + SLOW {
            thread::sleep(Duration::from_millis(100));
            let is_slow = i % 4 == 0 && slow.len() < SLOW;
            sampler.last_slow = if is_slow { None } else { Some(Instant::now()) };
            let started = Instant::now();
            sampler.sample();
            let took = started.elapsed().as_secs_f64() * 1_000.0;
            if is_slow { &mut slow } else { &mut fast }.push(took);
        }
        let stats = |v: &[f64]| {
            let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
            let max = v.iter().copied().fold(0.0, f64::max);
            format!("mean {mean:.2} ms, max {max:.2} ms over {}", v.len())
        };
        println!(
            "sample_cost: {} processes; fast tick {}; slow tick {}",
            first.processes.len(),
            stats(&fast),
            stats(&slow)
        );
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
