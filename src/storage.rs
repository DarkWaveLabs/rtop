//! Filesystem usage and disk I/O rates from `sysinfo::Disks`.

use std::collections::{HashMap, HashSet};

use sysinfo::{DiskRefreshKind, DiskUsage, Disks};

/// Linux: each `Disk::refresh_specifics` re-reads and parses all of /proc/diskstats, and
/// every mount of one device reads the same counter. So a tick refreshes one disk for each
/// device name and gives its usage to the other mounts of that device. Elsewhere a name is
/// not a counter key (macOS names are volume labels), so each disk refreshes itself.
const ONE_REFRESH_PER_DEVICE: bool = cfg!(target_os = "linux");

/// File systems with no real storage behind them.
const PSEUDO_FILE_SYSTEMS: [&str; 6] = [
    "autofs", "devfs", "devtmpfs", "overlay", "squashfs", "nullfs",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Volume {
    pub mount: String,
    /// The device or dataset name from `Disk::name` (lossy UTF-8). On macOS this is the volume name.
    pub device: String,
    /// From `Disk::file_system` (lossy UTF-8).
    pub file_system: String,
    pub total: u64,
    pub available: u64,
    /// Volumes with the same group share one I/O counter. The total counts each group once.
    /// macOS: the whole-disk BSD name (`apple_whole_disk`), else `device`. Other targets: `device`.
    pub io_group: String,
    /// False when the OS gives no I/O counter for the volume: file system `zfs`, or both
    /// since-boot totals are 0. The UI shows `n/a`.
    pub io_known: bool,
    /// Bytes per second over the last interval. 0 on the first sample of a (device, mount), and when `io_known` is false.
    pub read_rate: u64,
    pub write_rate: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Storage {
    /// Sorted: "/" first, then by mount point.
    pub volumes: Vec<Volume>,
    /// Sum over distinct `io_group` values of volumes with `io_known`.
    pub read_rate: u64,
    pub write_rate: u64,
    /// True when at least one volume has `io_known`.
    pub io_known: bool,
}

/// Owned by the sampler.
pub struct StorageCollector {
    disks: Disks,
    /// (device, mount) of each volume in the last call. A key that is not here is a first sample.
    seen: HashSet<(String, String)>,
    /// mount → io_group. Filled on slow calls on macOS only; empty elsewhere.
    groups: HashMap<String, String>,
    /// Mount points that are regular files, such as the /etc/hosts that Docker bind-mounts.
    /// Filled on slow calls.
    file_mounts: HashSet<String>,
}

impl StorageCollector {
    pub fn new() -> Self {
        Self {
            disks: Disks::new_with_refreshed_list_specifics(
                DiskRefreshKind::nothing().with_storage().with_io_usage(),
            ),
            seen: HashSet::new(),
            groups: HashMap::new(),
            file_mounts: HashSet::new(),
        }
    }

    /// I/O counters of kept disks refresh on every call. Space and the disk list refresh only when `slow` is true.
    pub fn collect(&mut self, secs: f64, slow: bool) -> Storage {
        if slow {
            // A re-list also reads the I/O counters, so the fast loop below is not needed.
            self.disks.refresh_specifics(
                true,
                DiskRefreshKind::nothing().with_storage().with_io_usage(),
            );
            #[cfg(target_os = "macos")]
            self.refresh_groups();
            self.file_mounts = self
                .disks
                .list()
                .iter()
                .map(|disk| disk.mount_point())
                .filter(|mount| is_file_mount(mount))
                .map(|mount| mount.to_string_lossy().into_owned())
                .collect();
        }
        let io_disks = {
            let texts: Vec<_> = self
                .disks
                .list()
                .iter()
                .map(|disk| {
                    (
                        disk.name().to_string_lossy(),
                        disk.file_system().to_string_lossy(),
                        disk.mount_point().to_string_lossy(),
                        disk.total_space(),
                    )
                })
                .collect();
            let items: Vec<(&str, &str, &str, u64)> = texts
                .iter()
                .map(|(name, fs, mount, total)| {
                    (name.as_ref(), fs.as_ref(), mount.as_ref(), *total)
                })
                .collect();
            io_refresh_set(&items, ONE_REFRESH_PER_DEVICE)
        };
        if !slow {
            for &i in &io_disks {
                if let Some(disk) = self.disks.list_mut().get_mut(i) {
                    disk.refresh_specifics(DiskRefreshKind::nothing().with_io_usage());
                }
            }
        }
        // Device name → the usage of the one disk refreshed for it. Also on slow ticks: the
        // other mounts' own deltas there span the time since the last re-list, not `secs`.
        let shared: HashMap<String, DiskUsage> = if ONE_REFRESH_PER_DEVICE {
            io_disks
                .iter()
                .filter_map(|&i| self.disks.list().get(i))
                .map(|disk| (disk.name().to_string_lossy().into_owned(), disk.usage()))
                .collect()
        } else {
            HashMap::new()
        };

        let raws: Vec<RawVolume> = self
            .disks
            .list()
            .iter()
            .map(|disk| RawVolume {
                mount: disk.mount_point().to_string_lossy().into_owned(),
                device: disk.name().to_string_lossy().into_owned(),
                file_system: disk.file_system().to_string_lossy().into_owned(),
                total: disk.total_space(),
                available: disk.available_space(),
                usage: shared
                    .get(disk.name().to_string_lossy().as_ref())
                    .copied()
                    .unwrap_or_else(|| disk.usage()),
            })
            .filter(|r| {
                keep_volume(&r.mount, &r.file_system, r.total)
                    && !self.file_mounts.contains(&r.mount)
            })
            .collect();
        let items: Vec<(&str, &str, &str)> = raws
            .iter()
            .map(|r| (r.device.as_str(), r.file_system.as_str(), r.mount.as_str()))
            .collect();
        let kept: Vec<&RawVolume> = drop_zfs_children(&items)
            .into_iter()
            .filter_map(|i| raws.get(i))
            .collect();
        let keys: Vec<(String, String)> = kept
            .iter()
            .map(|r| (r.device.clone(), r.mount.clone()))
            .collect();
        let firsts = first_samples(&mut self.seen, &keys);

        let mut volumes: Vec<Volume> = kept
            .into_iter()
            .zip(firsts)
            .map(|(r, first)| {
                let known = io_known(
                    &r.file_system,
                    r.usage.total_read_bytes,
                    r.usage.total_written_bytes,
                );
                let (read_rate, write_rate) = if known {
                    (
                        rate(r.usage.read_bytes, secs, first),
                        rate(r.usage.written_bytes, secs, first),
                    )
                } else {
                    (0, 0)
                };
                Volume {
                    mount: r.mount.clone(),
                    device: r.device.clone(),
                    file_system: r.file_system.clone(),
                    total: r.total,
                    available: r.available,
                    io_group: self
                        .groups
                        .get(&r.mount)
                        .cloned()
                        .unwrap_or_else(|| r.device.clone()),
                    io_known: known,
                    read_rate,
                    write_rate,
                }
            })
            .collect();
        sort_volumes(&mut volumes);

        let (read_rate, write_rate) = dedupe_rates(&volumes);
        Storage {
            io_known: volumes.iter().any(|v| v.io_known),
            volumes,
            read_rate,
            write_rate,
        }
    }

    /// Maps each kept mount to its whole disk, so the APFS volumes of one container share one counter.
    #[cfg(target_os = "macos")]
    fn refresh_groups(&mut self) {
        self.groups = self
            .disks
            .list()
            .iter()
            .filter_map(|disk| {
                let mount = disk.mount_point();
                let mount_text = mount.to_string_lossy();
                let fs = disk.file_system().to_string_lossy();
                if !keep_volume(&mount_text, &fs, disk.total_space()) {
                    return None;
                }
                let from = mount_source(mount)?;
                let whole = apple_whole_disk(&from)?;
                Some((mount_text.into_owned(), whole.to_owned()))
            })
            .collect();
    }
}

/// One disk from `sysinfo`, read once per call.
struct RawVolume {
    mount: String,
    device: String,
    file_system: String,
    total: u64,
    available: u64,
    usage: DiskUsage,
}

/// `f_mntfromname` of the file system mounted at `mount`, for example `/dev/disk3s1s1`.
#[cfg(target_os = "macos")]
fn mount_source(mount: &std::path::Path) -> Option<String> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(mount.as_os_str().as_bytes()).ok()?;
    let mut buf = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is a valid NUL-terminated C string that lives for the whole call, and
    // `buf` points to writable memory of the size and alignment of `libc::statfs`. `statfs`
    // fills the whole struct when it returns 0, so `assume_init` reads only initialised memory.
    let stat = unsafe {
        if libc::statfs(path.as_ptr(), buf.as_mut_ptr()) != 0 {
            return None;
        }
        buf.assume_init()
    };
    // Stop at the first NUL inside the array, so a missing terminator cannot overrun it.
    let bytes: Vec<u8> = stat
        .f_mntfromname
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// True when `mount` is a regular file, not a directory. Docker and Podman bind-mount
/// files such as `/etc/hosts`; the mount table lists them, but they are not filesystems.
fn is_file_mount(mount: &std::path::Path) -> bool {
    std::fs::metadata(mount).is_ok_and(|meta| meta.is_file())
}

/// Pure filter. Drops pseudo and system volumes.
pub fn keep_volume(mount: &str, file_system: &str, total: u64) -> bool {
    if total == 0 || PSEUDO_FILE_SYSTEMS.contains(&file_system) {
        return false;
    }
    if mount.starts_with("/System/Volumes/") && mount != "/System/Volumes/Data" {
        return false;
    }
    !mount.starts_with("/snap/")
}

/// Pure. Indices of the disks whose I/O counters a fast tick refreshes: kept by
/// `keep_volume` and not ZFS. With `one_per_device`, only the first such disk of each
/// device name. Items are (device, file_system, mount, total).
fn io_refresh_set(items: &[(&str, &str, &str, u64)], one_per_device: bool) -> Vec<usize> {
    let mut devices = HashSet::new();
    items
        .iter()
        .enumerate()
        .filter(|(_, (device, fs, mount, total))| {
            *fs != "zfs"
                && keep_volume(mount, fs, *total)
                && (!one_per_device || devices.insert(*device))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Pure. Drops a ZFS dataset `pool/a/b` when its pool root `pool` is also in the list,
/// except the volume mounted at "/", which is always kept.
/// Input items are (device, file_system, mount). Returns the indices to keep, in input order.
pub fn drop_zfs_children(items: &[(&str, &str, &str)]) -> Vec<usize> {
    let pools: HashSet<&str> = items
        .iter()
        .filter(|(device, fs, _)| *fs == "zfs" && !device.contains('/'))
        .map(|(device, _, _)| *device)
        .collect();
    items
        .iter()
        .enumerate()
        .filter(|(_, (device, fs, mount))| {
            let child_of_listed_pool = *fs == "zfs"
                && device
                    .split_once('/')
                    .is_some_and(|(pool, _)| pools.contains(pool));
            *mount == "/" || !child_of_listed_pool
        })
        .map(|(i, _)| i)
        .collect()
}

/// Pure. Sums `read_rate` and `write_rate` one time for each distinct `io_group`,
/// over volumes with `io_known` only. For a group, it takes the first volume's rates.
pub fn dedupe_rates(volumes: &[Volume]) -> (u64, u64) {
    let mut groups = HashSet::new();
    volumes
        .iter()
        .filter(|v| v.io_known && groups.insert(v.io_group.as_str()))
        .fold((0u64, 0u64), |(read, write), v| {
            (
                read.saturating_add(v.read_rate),
                write.saturating_add(v.write_rate),
            )
        })
}

/// Pure. The whole-disk BSD name from a macOS `statfs` `f_mntfromname`:
/// "/dev/disk3s1s1" → Some("disk3"); "/dev/disk10s5" → Some("disk10"); "map auto_home" → None.
#[cfg(any(test, target_os = "macos"))]
pub fn apple_whole_disk(mntfrom: &str) -> Option<&str> {
    let name = mntfrom.strip_prefix("/dev/")?;
    let unit = name.strip_prefix("disk")?;
    let digits = unit
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(unit.len());
    let rest = unit.get(digits..)?;
    if digits == 0 || !(rest.is_empty() || rest.starts_with('s')) {
        return None;
    }
    name.get(.."disk".len() + digits)
}

/// True when the OS gives an I/O counter for the volume.
fn io_known(file_system: &str, total_read: u64, total_written: u64) -> bool {
    file_system != "zfs" && (total_read > 0 || total_written > 0)
}

/// Bytes per second. 0 on the first sample, because the first delta is the total since boot.
fn rate(delta: u64, secs: f64, first: bool) -> u64 {
    if first {
        return 0;
    }
    // `max` maps NaN to the floor too, and `as u64` saturates.
    (delta as f64 / secs.max(0.001)) as u64
}

/// For each (device, mount) in `now`: true when it was not in `seen`. Then `seen` becomes `now`.
fn first_samples(seen: &mut HashSet<(String, String)>, now: &[(String, String)]) -> Vec<bool> {
    let firsts = now.iter().map(|key| !seen.contains(key)).collect();
    *seen = now.iter().cloned().collect();
    firsts
}

/// "/" first, then by mount point.
fn sort_volumes(volumes: &mut [Volume]) {
    crate::order::sort_by(volumes, |a, b| {
        (a.mount != "/", &a.mount).cmp(&(b.mount != "/", &b.mount))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(mount: &str, io_group: &str, io_known: bool, read: u64, write: u64) -> Volume {
        Volume {
            mount: mount.into(),
            device: io_group.into(),
            file_system: "ext4".into(),
            total: 100,
            available: 50,
            io_group: io_group.into(),
            io_known,
            read_rate: read,
            write_rate: write,
        }
    }

    fn key(device: &str, mount: &str) -> (String, String) {
        (device.into(), mount.into())
    }

    #[test]
    fn io_refresh_set_takes_one_disk_per_device_on_linux() {
        // A Kubernetes node: many bind mounts of the root device.
        let mut items = vec![("/dev/sda1", "ext4", "/", 100)];
        let subpaths: Vec<String> = (0..40).map(|i| format!("/var/lib/kubelet/{i}")).collect();
        items.extend(
            subpaths
                .iter()
                .map(|m| ("/dev/sda1", "ext4", m.as_str(), 100)),
        );
        items.extend([
            ("/dev/nvme0n1p2", "xfs", "/data", 100),
            ("tank/home", "zfs", "/home", 100),
            ("tmpfs", "devtmpfs", "/dev", 100),
            ("/dev/sdb1", "ext4", "/empty", 0),
            ("/dev/nvme0n1p2", "xfs", "/data2", 100),
        ]);
        assert_eq!(io_refresh_set(&items, true), [0, 41]);
        let all: Vec<usize> = (0..=41).chain([45]).collect();
        assert_eq!(
            io_refresh_set(&items, false),
            all,
            "every kept, non-ZFS disk"
        );
        assert!(io_refresh_set(&[], true).is_empty());
    }

    #[test]
    fn keep_volume_keeps_real_filesystems() {
        for (mount, fs, total) in [
            ("/", "apfs", 1),
            ("/System/Volumes/Data", "apfs", 1),
            ("/home", "ext4", 1),
            ("/boot/efi", "vfat", 1),
        ] {
            assert!(keep_volume(mount, fs, total), "should keep {mount} {fs}");
        }
    }

    #[test]
    fn keep_volume_drops_pseudo_and_system_volumes() {
        for (mount, fs, total) in [
            ("/System/Volumes/VM", "apfs", 1),
            ("/System/Volumes/Preboot", "apfs", 1),
            ("/snap/core/1", "squashfs", 1),
            ("/dev", "devfs", 1),
            ("/x", "ext4", 0),
        ] {
            assert!(!keep_volume(mount, fs, total), "should drop {mount} {fs}");
        }
    }

    #[test]
    fn keep_volume_drops_each_pseudo_file_system() {
        for fs in [
            "autofs", "devfs", "devtmpfs", "overlay", "squashfs", "nullfs",
        ] {
            assert!(!keep_volume("/mnt/x", fs, 1), "should drop {fs}");
        }
        assert!(!keep_volume("/snap/firefox/42", "ext4", 1));
    }

    #[test]
    fn drop_zfs_children_keeps_pool_roots_and_other_pools() {
        let items = [
            ("tank", "zfs", "/tank"),
            ("tank/home", "zfs", "/tank/home"),
            ("tank/home/alice", "zfs", "/tank/home/alice"),
            ("rpool/ROOT/ubuntu", "zfs", "/"),
            ("/dev/sda1", "ext4", "/boot"),
        ];
        assert_eq!(drop_zfs_children(&items), [0, 3, 4]);
    }

    #[test]
    fn drop_zfs_children_never_drops_root_on_freebsd_layout() {
        let items = [
            ("zroot", "zfs", "/zroot"),
            ("zroot/ROOT/default", "zfs", "/"),
            ("zroot/usr/home", "zfs", "/usr/home"),
        ];
        assert_eq!(drop_zfs_children(&items), [0, 1]);
    }

    #[test]
    fn drop_zfs_children_ignores_slash_devices_of_other_file_systems() {
        let items = [("dev", "zfs", "/dev"), ("dev/sda1", "ext4", "/boot")];
        assert_eq!(drop_zfs_children(&items), [0, 1]);
        assert_eq!(drop_zfs_children(&[]), Vec::<usize>::new());
    }

    #[test]
    fn dedupe_rates_counts_each_group_once() {
        let volumes = [
            volume("/a", "sda1", true, 100, 10),
            volume("/b", "sda1", true, 100, 10),
            volume("/c", "sdb1", true, 50, 5),
        ];
        assert_eq!(dedupe_rates(&volumes), (150, 15));
    }

    #[test]
    fn dedupe_rates_counts_one_apfs_container_once() {
        let volumes = [
            volume("/", "disk3", true, 100, 40),
            volume("/System/Volumes/Data", "disk3", true, 100, 40),
        ];
        assert_eq!(dedupe_rates(&volumes), (100, 40));
    }

    #[test]
    fn dedupe_rates_skips_volumes_without_a_counter() {
        let volumes = [
            volume("/tank", "tank", false, 999, 999),
            volume("/a", "sda1", true, 100, 10),
        ];
        assert_eq!(dedupe_rates(&volumes), (100, 10));
        assert_eq!(dedupe_rates(&[]), (0, 0));
    }

    #[test]
    fn dedupe_rates_takes_the_first_rates_of_a_group() {
        let volumes = [
            volume("/", "disk3", true, 100, 40),
            volume("/System/Volumes/Data", "disk3", true, 120, 50),
        ];
        assert_eq!(dedupe_rates(&volumes), (100, 40));
    }

    #[test]
    fn dedupe_rates_saturates() {
        let volumes = [
            volume("/a", "sda", true, u64::MAX, 1),
            volume("/b", "sdb", true, 5, 1),
        ];
        assert_eq!(dedupe_rates(&volumes), (u64::MAX, 2));
    }

    #[test]
    fn apple_whole_disk_strips_slices() {
        assert_eq!(apple_whole_disk("/dev/disk3s1s1"), Some("disk3"));
        assert_eq!(apple_whole_disk("/dev/disk3s5"), Some("disk3"));
        assert_eq!(apple_whole_disk("/dev/disk10s2"), Some("disk10"));
        assert_eq!(apple_whole_disk("/dev/disk4"), Some("disk4"));
    }

    #[test]
    fn apple_whole_disk_rejects_other_sources() {
        assert_eq!(apple_whole_disk("map auto_home"), None);
        assert_eq!(apple_whole_disk("//user@host/share"), None);
        assert_eq!(apple_whole_disk(""), None);
        assert_eq!(apple_whole_disk("/dev/disk"), None);
        assert_eq!(apple_whole_disk("/dev/diskXs1"), None);
        assert_eq!(apple_whole_disk("/dev/disk3x"), None);
    }

    #[test]
    fn io_known_needs_a_counter_and_not_zfs() {
        assert!(!io_known("zfs", 5, 5));
        assert!(!io_known("ext4", 0, 0));
        assert!(io_known("ext4", 5, 0));
        assert!(io_known("apfs", 0, 7));
    }

    #[test]
    fn rate_is_zero_on_the_first_sample() {
        assert_eq!(rate(1_000_000, 1.0, true), 0);
        assert_eq!(rate(1_000, 0.5, false), 2_000);
    }

    #[test]
    fn rate_never_divides_by_zero() {
        assert_eq!(rate(1, 0.0, false), 1_000);
        assert_eq!(rate(1, f64::NAN, false), 1_000);
        assert_eq!(rate(u64::MAX, 0.001, false), u64::MAX);
    }

    #[test]
    fn first_samples_is_keyed_by_device_and_mount() {
        let mut seen = HashSet::new();
        assert_eq!(first_samples(&mut seen, &[key("sda1", "/a")]), [true]);
        assert_eq!(first_samples(&mut seen, &[key("sda1", "/a")]), [false]);

        let now = [key("sda1", "/a"), key("sda1", "/b")];
        assert_eq!(first_samples(&mut seen, &now), [false, true]);
        assert_eq!(seen, HashSet::from(now.clone()));
    }

    #[test]
    fn first_samples_forgets_a_mount_that_went_away() {
        let mut seen = HashSet::from([key("sda1", "/a"), key("sda1", "/b")]);
        assert_eq!(first_samples(&mut seen, &[key("sda1", "/b")]), [false]);
        assert_eq!(seen, HashSet::from([key("sda1", "/b")]));

        let now = [key("sda1", "/a"), key("sda1", "/b")];
        assert_eq!(first_samples(&mut seen, &now), [true, false]);
    }

    #[test]
    fn sort_puts_root_first_then_mount_order() {
        let mut volumes = [
            volume("/home", "a", true, 0, 0),
            volume("/boot", "b", true, 0, 0),
            volume("/", "c", true, 0, 0),
            volume("/System/Volumes/Data", "d", true, 0, 0),
        ];
        sort_volumes(&mut volumes);
        let mounts: Vec<&str> = volumes.iter().map(|v| v.mount.as_str()).collect();
        assert_eq!(mounts, ["/", "/System/Volumes/Data", "/boot", "/home"]);
    }

    #[test]
    fn is_file_mount_is_true_only_for_a_regular_file() {
        // Docker bind-mounts files such as /etc/hosts; they are not filesystems.
        let root = std::env::temp_dir().join(format!(
            "rtop-storage-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("hosts");
        std::fs::write(&file, "127.0.0.1 localhost\n").unwrap();

        let got = [
            is_file_mount(&file),
            is_file_mount(&root),
            is_file_mount(&root.join("missing")),
        ];
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(got, [true, false, false], "file, directory, missing");
    }

    #[test]
    fn collector_reads_this_host_without_panic() {
        let mut collector = StorageCollector::new();
        let first = collector.collect(1.0, true);
        let second = collector.collect(0.25, false);
        for storage in [&first, &second] {
            assert!(storage.volumes.windows(2).all(|w| w[1].mount != "/"));
            assert!(storage.volumes.iter().all(|v| v.total > 0));
        }
        assert!(
            first
                .volumes
                .iter()
                .all(|v| v.read_rate == 0 && v.write_rate == 0)
        );
        assert_eq!(first.read_rate, 0);
    }
}
