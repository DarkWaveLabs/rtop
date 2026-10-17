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
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(not(target_os = "linux"))]
use sysinfo::Networks;
use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, ProcessesToUpdate, System};

use crate::app::Event;
use crate::net::{self, NetIface};
use crate::process::{self, KillRequest, ProcCollector};
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

/// Starts the sampler thread. It stops by itself when the UI drops the receiver.
/// It also sends the kill requests that come in on `kills`, and reports each one as
/// [`Event::KillDone`].
pub fn spawn_sampler(
    tx: Sender<Event>,
    interval_ms: Arc<AtomicU64>,
    kills: Receiver<KillRequest>,
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
                if sampler
                    .wait_until_due(&interval_ms, started, &kills, &tx)
                    .is_err()
                {
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
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process::refresh_kind(true));
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
            procs: ProcCollector::new(),
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
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            process::refresh_kind(slow),
        );
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
        let processes = self.procs.collect(&self.sys, secs, slow);
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

    /// Sleeps in short steps until the next tick is due. It reads the interval again each
    /// time, so a key press that changes the interval takes effect at once, and it sends
    /// each kill request as it comes, so a kill starts within one step.
    fn wait_until_due(
        &mut self,
        interval_ms: &AtomicU64,
        started: Instant,
        kills: &Receiver<KillRequest>,
        tx: &Sender<Event>,
    ) -> Result<(), Disconnected> {
        const STEP: Duration = Duration::from_millis(50);
        loop {
            // A closed kill channel only means that no more kills come; sampling goes on.
            while let Ok(req) = kills.try_recv() {
                let report = process::send_signal(&mut self.sys, &req, std::process::id());
                tx.send(Event::KillDone(report)).map_err(|_| Disconnected)?;
            }
            let due = Duration::from_millis(interval_ms.load(Ordering::Relaxed));
            let elapsed = started.elapsed();
            if elapsed >= due {
                return Ok(());
            }
            thread::sleep((due - elapsed).min(STEP));
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
