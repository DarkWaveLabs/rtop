//! Filesystem usage and disk I/O rates. The disk list comes from `sysinfo::Disks`; on
//! macOS the I/O counters and the space are read here (see `apple`).

use std::collections::{HashMap, HashSet};

use sysinfo::{DiskRefreshKind, DiskUsage, Disks};

/// Every disk of one I/O group reads the same counter: on Linux each mount of one device,
/// on macOS each APFS volume of one whole disk. Linux re-reads and parses all of
/// /proc/diskstats for each `Disk::refresh_specifics`, and macOS reads one IOKit property.
/// So a tick reads the counter of one disk for each group and gives its usage to the other
/// disks of the group. Elsewhere each disk refreshes itself.
const ONE_REFRESH_PER_GROUP: bool = cfg!(any(target_os = "linux", target_os = "macos"));

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
    /// (device, mount) of each disk in the last list. A disk that is not here is new, and
    /// has no space values until it is read.
    listed: HashSet<(String, String)>,
    /// (total, available) bytes by (device, mount). Read only for disks that `keep_mount`
    /// keeps: on the space read and for disks new in a list. A disk without an entry has
    /// total 0, so `keep_volume` drops it.
    space: HashMap<(String, String), (u64, u64)>,
    /// mount → io_group. Filled on re-lists on macOS only; empty elsewhere.
    groups: HashMap<String, String>,
    /// Mount points that are regular files, such as the /etc/hosts that Docker bind-mounts.
    /// Filled on re-lists.
    file_mounts: HashSet<String>,
    /// The block storage driver of each whole disk, found once and read on each call.
    #[cfg(target_os = "macos")]
    drivers: apple::Drivers,
    /// (device, mount) of the disks whose I/O counters the last call read.
    #[cfg(not(target_os = "macos"))]
    io_read: HashSet<(String, String)>,
}

impl StorageCollector {
    /// Lists nothing yet: the first call does that.
    pub fn new() -> Self {
        Self {
            disks: Disks::new(),
            seen: HashSet::new(),
            listed: HashSet::new(),
            space: HashMap::new(),
            groups: HashMap::new(),
            file_mounts: HashSet::new(),
            #[cfg(target_os = "macos")]
            drivers: apple::Drivers::default(),
            #[cfg(not(target_os = "macos"))]
            io_read: HashSet::new(),
        }
    }

    /// The I/O counters of one disk for each I/O group refresh on every call. With `slow`,
    /// the disk list refreshes too, and only disks that are new since the last list read
    /// their space. With `space`, the list refreshes and every disk that `keep_mount`
    /// keeps reads its space.
    pub fn collect(&mut self, secs: f64, slow: bool, space: bool) -> Storage {
        let relisted = (slow || space).then(|| self.relist(space));
        let mut raws: Vec<RawVolume> = self
            .disks
            .list()
            .iter()
            .map(|disk| {
                let mount = disk.mount_point().to_string_lossy().into_owned();
                let device = disk.name().to_string_lossy().into_owned();
                let key = (device, mount);
                let (total, available) = self.space.get(&key).copied().unwrap_or_default();
                let (device, mount) = key;
                RawVolume {
                    io_group: self
                        .groups
                        .get(&mount)
                        .cloned()
                        .unwrap_or_else(|| device.clone()),
                    mount,
                    device,
                    file_system: disk.file_system().to_string_lossy().into_owned(),
                    total,
                    available,
                    usage: DiskUsage::default(),
                }
            })
            .collect();
        let items: Vec<(&str, &str, &str, u64)> = raws
            .iter()
            .map(|r| {
                (
                    r.io_group.as_str(),
                    r.file_system.as_str(),
                    r.mount.as_str(),
                    r.total,
                )
            })
            .collect();
        let io_disks = io_refresh_set(&items, ONE_REFRESH_PER_GROUP);
        let own = self.read_io(&raws, &io_disks, relisted);
        // The other disks of a group never read their own counters.
        let usages = if ONE_REFRESH_PER_GROUP {
            let groups: Vec<&str> = raws.iter().map(|r| r.io_group.as_str()).collect();
            shared_usage(&groups, &own, &io_disks)
        } else {
            own
        };
        for (raw, usage) in raws.iter_mut().zip(usages) {
            raw.usage = usage;
        }
        raws.retain(|r| {
            keep_volume(&r.mount, &r.file_system, r.total) && !self.file_mounts.contains(&r.mount)
        });
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
                    io_group: r.io_group.clone(),
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

    /// Lists the disks again, without their I/O counters or space, then reads the space
    /// of the disks that `space_reads` picks. Returns true when a disk came or went.
    fn relist(&mut self, all_space: bool) -> bool {
        self.disks
            .refresh_specifics(true, DiskRefreshKind::nothing());
        let keys: Vec<(String, String)> = self
            .disks
            .list()
            .iter()
            .map(|disk| {
                (
                    disk.name().to_string_lossy().into_owned(),
                    disk.mount_point().to_string_lossy().into_owned(),
                )
            })
            .collect();
        let file_systems: Vec<String> = self
            .disks
            .list()
            .iter()
            .map(|disk| disk.file_system().to_string_lossy().into_owned())
            .collect();
        let new = new_disks(&self.listed, &keys);
        let mounts: Vec<(&str, &str)> = keys
            .iter()
            .zip(&file_systems)
            .map(|((_, mount), fs)| (mount.as_str(), fs.as_str()))
            .collect();
        for i in space_reads(&mounts, &new, all_space) {
            if let (Some(disk), Some(key)) = (self.disks.list_mut().get_mut(i), keys.get(i)) {
                let read = read_space(disk);
                if let Some(space) = merge_space(self.space.get(key).copied(), read) {
                    self.space.insert(key.clone(), space);
                }
            }
        }
        let now: HashSet<(String, String)> = keys.into_iter().collect();
        let changed = !new.is_empty() || self.listed.iter().any(|key| !now.contains(key));
        self.space.retain(|key, _| now.contains(key));
        self.listed = now;
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
        changed
    }

    /// The usage of each disk in `raws` (the disk list, in order): read for the disks in
    /// `leaders`, zero for the others. `relisted` is set on a re-list tick: true when a
    /// disk came or went. macOS reads the block storage driver of each whole disk; it
    /// finds the drivers again on a re-list, and keeps them in between.
    #[cfg(target_os = "macos")]
    fn read_io(
        &mut self,
        raws: &[RawVolume],
        leaders: &[usize],
        relisted: Option<bool>,
    ) -> Vec<DiskUsage> {
        let groups: Vec<&str> = leaders
            .iter()
            .filter_map(|&i| raws.get(i))
            .map(|raw| raw.io_group.as_str())
            .collect();
        if let Some(changed) = relisted {
            // Only a whole-disk name can be looked up; a group named after its volume
            // has no counter, as with sysinfo.
            let whole: HashSet<&str> = self.groups.values().map(String::as_str).collect();
            let disks: Vec<&str> = groups
                .iter()
                .copied()
                .filter(|group| whole.contains(group))
                .collect();
            self.drivers.sync(&disks, changed);
        }
        let mut own = vec![DiskUsage::default(); raws.len()];
        for (&i, group) in leaders.iter().zip(groups) {
            if let Some(usage) = own.get_mut(i) {
                *usage = self.drivers.usage(group);
            }
        }
        own
    }

    /// The usage of each disk in `raws` (the disk list, in order), after the disks in
    /// `leaders` read their counters. A leader whose counter the last call did not read
    /// moves no bytes: its delta does not span this interval.
    #[cfg(not(target_os = "macos"))]
    fn read_io(
        &mut self,
        raws: &[RawVolume],
        leaders: &[usize],
        _relisted: Option<bool>,
    ) -> Vec<DiskUsage> {
        for &i in leaders {
            if let Some(disk) = self.disks.list_mut().get_mut(i) {
                disk.refresh_specifics(DiskRefreshKind::nothing().with_io_usage());
            }
        }
        let mut own: Vec<DiskUsage> = self.disks.list().iter().map(|disk| disk.usage()).collect();
        let keys: Vec<(String, String)> = raws
            .iter()
            .map(|raw| (raw.device.clone(), raw.mount.clone()))
            .collect();
        self.io_read = fresh_deltas(&mut own, &keys, leaders, &self.io_read);
        own
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
                if !keep_mount(&mount_text, &fs) {
                    return None;
                }
                let from = mount_source(mount)?;
                let whole = apple_whole_disk(&from)?;
                Some((mount_text.into_owned(), whole.to_owned()))
            })
            .collect();
    }
}

/// The (total, available) space of a disk; `None` for a value that could not be read.
/// macOS reads it itself: sysinfo's `Disk::refresh_specifics` asks the volume URL it made
/// when it listed the disk, and CoreFoundation caches resource values in a URL, so a
/// second read gives the first value again.
#[cfg(target_os = "macos")]
fn read_space(disk: &mut sysinfo::Disk) -> (Option<u64>, Option<u64>) {
    apple::volume_space(disk.mount_point())
}

/// The (total, available) space of a disk. sysinfo keeps the last value when a read fails.
#[cfg(not(target_os = "macos"))]
fn read_space(disk: &mut sysinfo::Disk) -> (Option<u64>, Option<u64>) {
    disk.refresh_specifics(DiskRefreshKind::nothing().with_storage());
    (Some(disk.total_space()), Some(disk.available_space()))
}

/// Pure. The space of a disk after a read: each value that could not be read keeps its
/// last value (0 when there is none), as in sysinfo. `None` when nothing is known.
fn merge_space(
    last: Option<(u64, u64)>,
    (total, available): (Option<u64>, Option<u64>),
) -> Option<(u64, u64)> {
    if total.is_none() && available.is_none() {
        return last;
    }
    let (last_total, last_available) = last.unwrap_or_default();
    Some((
        total.unwrap_or(last_total),
        available.unwrap_or(last_available),
    ))
}

/// Pure. Indices of the disks whose space a re-list reads: those that `keep_mount` keeps,
/// all of them with `all`, else only the `new` ones. Items are (mount, file_system).
fn space_reads(items: &[(&str, &str)], new: &[usize], all: bool) -> Vec<usize> {
    let wanted = |i: &usize| {
        items
            .get(*i)
            .is_some_and(|(mount, fs)| keep_mount(mount, fs))
    };
    if all {
        (0..items.len()).filter(wanted).collect()
    } else {
        new.iter().copied().filter(wanted).collect()
    }
}

/// Pure. Zeroes the deltas in `own` of the `leaders` whose key is not in `last` (the keys
/// read on the last call), and returns the keys of the leaders, read now. `own` and
/// `keys` have one entry for each disk.
#[cfg(any(test, not(target_os = "macos")))]
fn fresh_deltas(
    own: &mut [DiskUsage],
    keys: &[(String, String)],
    leaders: &[usize],
    last: &HashSet<(String, String)>,
) -> HashSet<(String, String)> {
    let mut read = HashSet::new();
    for &i in leaders {
        let (Some(key), Some(usage)) = (keys.get(i), own.get_mut(i)) else {
            continue;
        };
        if !last.contains(key) {
            usage.read_bytes = 0;
            usage.written_bytes = 0;
        }
        read.insert(key.clone());
    }
    read
}

/// One disk from `sysinfo`, read once per call.
struct RawVolume {
    mount: String,
    device: String,
    /// See `Volume::io_group`.
    io_group: String,
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

/// Pure filter. Drops pseudo and system volumes, and volumes of unknown size.
pub fn keep_volume(mount: &str, file_system: &str, total: u64) -> bool {
    total > 0 && keep_mount(mount, file_system)
}

/// Pure. `keep_volume` before the size is known: true when the mount and the file
/// system are worth a space read.
fn keep_mount(mount: &str, file_system: &str) -> bool {
    if PSEUDO_FILE_SYSTEMS.contains(&file_system) {
        return false;
    }
    if mount.starts_with("/System/Volumes/") && mount != "/System/Volumes/Data" {
        return false;
    }
    !mount.starts_with("/snap/")
}

/// Pure. Indices of the disks whose I/O counters a fast tick refreshes: kept by
/// `keep_volume` and not ZFS. With `one_per_group`, only the first such disk of each
/// I/O group. Items are (io_group, file_system, mount, total).
fn io_refresh_set(items: &[(&str, &str, &str, u64)], one_per_group: bool) -> Vec<usize> {
    let mut groups = HashSet::new();
    items
        .iter()
        .enumerate()
        .filter(|(_, (group, fs, mount, total))| {
            *fs != "zfs"
                && keep_volume(mount, fs, *total)
                && (!one_per_group || groups.insert(*group))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Pure. The usage that each disk reports: the usage of the disk in `refreshed` that has
/// the same group, or its own when no disk of its group is there. `groups` and `own` have
/// one entry for each disk.
fn shared_usage(groups: &[&str], own: &[DiskUsage], refreshed: &[usize]) -> Vec<DiskUsage> {
    let mut by_group: HashMap<&str, DiskUsage> = HashMap::new();
    for &i in refreshed {
        if let (Some(&group), Some(&usage)) = (groups.get(i), own.get(i)) {
            by_group.entry(group).or_insert(usage);
        }
    }
    groups
        .iter()
        .zip(own)
        .map(|(group, usage)| by_group.get(group).copied().unwrap_or(*usage))
        .collect()
}

/// Pure. Indices in `now` of the (device, mount) keys that are not in `listed`.
fn new_disks(listed: &HashSet<(String, String)>, now: &[(String, String)]) -> Vec<usize> {
    now.iter()
        .enumerate()
        .filter(|(_, key)| !listed.contains(*key))
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

/// Pure. The usage of a counter from its (read, written) totals at the last read and now.
/// The first read moves no bytes, as a first sample has rate 0 anyway. A failed read
/// (`now` is `None`) keeps the last totals and moves no bytes.
#[cfg(any(test, target_os = "macos"))]
fn counter_usage(last: Option<(u64, u64)>, now: Option<(u64, u64)>) -> DiskUsage {
    let Some((read, written)) = now.or(last) else {
        return DiskUsage::default();
    };
    let (last_read, last_written) = match (last, now) {
        (Some(last), Some(_)) => last,
        _ => (read, written),
    };
    DiskUsage {
        read_bytes: read.saturating_sub(last_read),
        written_bytes: written.saturating_sub(last_written),
        total_read_bytes: read,
        total_written_bytes: written,
    }
}

/// Pure. The usage of a read that gave `now`, from the baseline in `last`, which then
/// becomes `now`. A failed read drops the baseline, so the next good read is a first read
/// and moves no bytes: a rate never spans a gap.
#[cfg(any(test, target_os = "macos"))]
fn advance_counter(last: &mut Option<(u64, u64)>, now: Option<(u64, u64)>) -> DiskUsage {
    let usage = counter_usage(*last, now);
    *last = now;
    usage
}

/// Pure. True when a group's driver must be looked up: it has none (`failed` is `None`),
/// its last read failed, or the disk list changed (`recheck`).
#[cfg(any(test, target_os = "macos"))]
fn needs_lookup(failed: Option<bool>, recheck: bool) -> bool {
    recheck || failed.unwrap_or(true)
}

/// macOS disk I/O counters and disk space, read through IOKit and CoreFoundation.
///
/// sysinfo finds the I/O counters of a disk on each refresh: it matches the BSD name in
/// the IOKit registry and walks up to the `IOBlockStorageDriver`, which cost about 2.2 ms
/// of CPU per tick for one whole disk on the reference Mac. Here the driver of each whole
/// disk is found once, when the disk list changes, kept, and each tick reads only its
/// `Statistics` property, with the keys sysinfo reads.
#[cfg(target_os = "macos")]
mod apple {
    use std::collections::HashMap;
    use std::ffi::{CString, c_char, c_void};
    use std::path::Path;
    use std::ptr;

    use objc2_core_foundation::{
        CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL,
        kCFURLVolumeAvailableCapacityForImportantUsageKey, kCFURLVolumeAvailableCapacityKey,
        kCFURLVolumeTotalCapacityKey,
    };
    use objc2_io_kit::{
        IOBSDNameMatching, IOObjectConformsTo, IOObjectRelease, IORegistryEntryCreateCFProperty,
        IORegistryEntryGetParentEntry, IORegistryEntryGetRegistryEntryID,
        IOServiceGetMatchingService, io_registry_entry_t, kIOServicePlane,
    };
    use sysinfo::DiskUsage;

    use super::{advance_counter, needs_lookup};

    /// `MACH_PORT_NULL`: IOKit then uses its default main port.
    const DEFAULT_PORT: libc::mach_port_t = 0;
    const DRIVER_CLASS: &str = "IOBlockStorageDriver";
    /// The driver property with the counters, and the counters in it.
    const STATISTICS: &str = "Statistics";
    const BYTES_READ: &str = "Bytes (Read)";
    const BYTES_WRITTEN: &str = "Bytes (Write)";

    /// An `io_name_t`: a NUL-terminated name in a 128-byte array, the size the IOKit
    /// calls take.
    fn io_name(name: &str) -> [c_char; 128] {
        let mut out = [0; 128];
        for (slot, byte) in out.iter_mut().zip(name.bytes().take(127)) {
            *slot = byte as c_char;
        }
        out
    }

    /// One IOKit object that this code holds one reference on. Drop releases it, once.
    struct Object(io_registry_entry_t);

    impl Object {
        /// Takes over a +1 reference from IOKit. `None` for the null object.
        fn new(raw: io_registry_entry_t) -> Option<Self> {
            (raw != 0).then_some(Self(raw))
        }

        /// The registry entry ID, which is unique for the life of the system.
        fn id(&self) -> Option<u64> {
            let mut id = 0;
            // SAFETY: `self.0` is a live object that this value holds a reference on, and
            // `id` is a valid, writable `u64` for the call.
            let status = unsafe { IORegistryEntryGetRegistryEntryID(self.0, &mut id) };
            (status == libc::KERN_SUCCESS).then_some(id)
        }

        fn is_driver(&self) -> bool {
            let mut class = io_name(DRIVER_CLASS);
            // SAFETY: `self.0` is a live object that this value holds a reference on, and
            // `class` is a NUL-terminated name in the 128-byte array the call reads.
            unsafe { IOObjectConformsTo(self.0, &mut class) }
        }

        /// The (read, written) byte counters of a block storage driver.
        fn statistics(&self) -> Option<(u64, u64)> {
            let key = CFString::from_static_str(STATISTICS);
            // SAFETY: `self.0` is a live object that this value holds a reference on, and
            // `key` is a CFString. A null allocator is the default one. The result is a
            // copy (+1) that `CFRetained` owns and releases once.
            let value = unsafe { IORegistryEntryCreateCFProperty(self.0, Some(&key), None, 0) }?;
            let stats = value.downcast::<CFDictionary>().ok()?;
            statistics_bytes(&stats)
        }
    }

    impl Drop for Object {
        fn drop(&mut self) {
            IOObjectRelease(self.0);
        }
    }

    /// The block storage driver of the whole disk `bsd_name` (such as `disk3`): the first
    /// parent in the service plane that is an `IOBlockStorageDriver` and has the byte
    /// counters, as sysinfo looks for it.
    fn find_driver(bsd_name: &str) -> Option<Object> {
        let name = CString::new(bsd_name).ok()?;
        // SAFETY: `name` is a NUL-terminated C string that lives for the call. The result
        // is a new dictionary (+1) that `CFRetained` owns.
        let matching = unsafe { IOBSDNameMatching(DEFAULT_PORT, 0, name.as_ptr()) }?;
        // The call consumes one reference: this one is a new +1 retain of the dictionary,
        // and `matching` releases the first when it drops.
        let matching = CFRetained::<CFDictionary>::from(&*matching);
        // SAFETY: `matching` is a matching dictionary, given by value as the call
        // consumes it. The returned service is +1, or 0 for none; `Object` owns it.
        let mut entry =
            Object::new(unsafe { IOServiceGetMatchingService(DEFAULT_PORT, Some(matching)) })?;
        let mut plane = io_name(&kIOServicePlane.to_string_lossy());
        loop {
            let mut parent = 0;
            // SAFETY: `entry.0` is a live object that `entry` holds a reference on,
            // `plane` is a NUL-terminated name in the 128-byte array the call reads, and
            // `parent` is a valid, writable `io_registry_entry_t`. On success the parent is
            // +1 and `Object` owns it.
            let status = unsafe { IORegistryEntryGetParentEntry(entry.0, &mut plane, &mut parent) };
            if status != libc::KERN_SUCCESS {
                return None;
            }
            // The child is released here, when `entry` takes the parent.
            entry = Object::new(parent)?;
            if entry.is_driver() && entry.statistics().is_some() {
                return Some(entry);
            }
        }
    }

    /// Pure over its input. The (read, written) byte counters in a driver's `Statistics`
    /// dictionary, or `None` when one is missing or not a number.
    pub(super) fn statistics_bytes(stats: &CFDictionary) -> Option<(u64, u64)> {
        let read = CFString::from_static_str(BYTES_READ);
        let written = CFString::from_static_str(BYTES_WRITTEN);
        Some((number(stats, &read)?, number(stats, &written)?))
    }

    /// The value of `key` in a dictionary with string keys, as sysinfo reads it: a CFNumber
    /// as `i64`, cast to `u64`.
    fn number(dict: &CFDictionary, key: &CFString) -> Option<u64> {
        // SAFETY: IOKit and CFURL property dictionaries have CFString keys and CF object
        // values, so viewing it with those types reads it as it is. The value is checked
        // with `downcast` before it is used.
        let dict = unsafe { dict.cast_unchecked::<CFString, CFType>() };
        let value = dict.get(key)?;
        Some(value.downcast::<CFNumber>().ok()?.as_i64()? as u64)
    }

    /// A kept driver and the counters it gave last.
    struct Driver {
        object: Object,
        id: Option<u64>,
        last: Option<(u64, u64)>,
        failed: bool,
    }

    /// The driver of each whole disk that leads an I/O group, by group (BSD name).
    /// Dropping it releases every driver.
    #[derive(Default)]
    pub(super) struct Drivers {
        by_group: HashMap<String, Driver>,
    }

    impl Drivers {
        /// Keeps a driver for each of `groups` and releases the others. A group without a
        /// driver, or whose last read failed, looks it up. With `recheck` (the disk list
        /// changed), every group looks it up again, and keeps its counters only when it
        /// finds the same driver: a new disk can take the BSD name of one that went.
        pub(super) fn sync(&mut self, groups: &[&str], recheck: bool) {
            self.by_group
                .retain(|group, _| groups.contains(&group.as_str()));
            for &group in groups {
                let failed = self.by_group.get(group).map(|driver| driver.failed);
                if !needs_lookup(failed, recheck) {
                    continue;
                }
                let Some(object) = find_driver(group) else {
                    self.by_group.remove(group);
                    continue;
                };
                let id = object.id();
                match self.by_group.get_mut(group) {
                    Some(driver) if id.is_some() && driver.id == id => {
                        driver.object = object;
                        driver.failed = false;
                    }
                    _ => {
                        let driver = Driver {
                            object,
                            id,
                            last: None,
                            failed: false,
                        };
                        self.by_group.insert(group.to_owned(), driver);
                    }
                }
            }
        }

        /// Reads the counters of `group` and returns the usage since the last read. A group
        /// without a driver has no counter (all zero).
        pub(super) fn usage(&mut self, group: &str) -> DiskUsage {
            let Some(driver) = self.by_group.get_mut(group) else {
                return DiskUsage::default();
            };
            let now = driver.object.statistics();
            driver.failed = now.is_none();
            advance_counter(&mut driver.last, now)
        }
    }

    #[link(name = "objc", kind = "dylib")]
    unsafe extern "C" {
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    /// An autorelease pool for the length of a scope, as sysinfo uses around its disk list:
    /// the volume URL calls run Foundation code that can autorelease objects, and the
    /// sampler thread has no pool of its own.
    struct Pool(*mut c_void);

    impl Pool {
        fn push() -> Self {
            // SAFETY: pushing a pool has no preconditions; `Drop` pops this pool, on this
            // thread, and no object from it is used after that.
            Self(unsafe { objc_autoreleasePoolPush() })
        }
    }

    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from the push above on this thread, and the pools are
            // popped in reverse order because each `Pool` lives in one scope.
            unsafe { objc_autoreleasePoolPop(self.0) }
        }
    }

    /// The (total, available) bytes of the volume mounted at `mount`, as sysinfo reads
    /// them: available for important usage, or plain available when that is 0. A fresh
    /// URL each time, so no cached value is given.
    pub(super) fn volume_space(mount: &Path) -> (Option<u64>, Option<u64>) {
        let _pool = Pool::push();
        // SAFETY: these are constant CFStrings that CoreFoundation defines and never frees.
        let keys = unsafe {
            [
                kCFURLVolumeTotalCapacityKey,
                kCFURLVolumeAvailableCapacityForImportantUsageKey,
                kCFURLVolumeAvailableCapacityKey,
            ]
        };
        let [Some(total), Some(important), Some(available)] = keys else {
            return (None, None);
        };
        let Some(url) = CFURL::from_directory_path(mount) else {
            return (None, None);
        };
        let keys = CFArray::from_objects(&[total, important, available]);
        // SAFETY: `keys` is an array of CFStrings, the type the call wants, and a null
        // error pointer is allowed. The result is a new dictionary (+1) that `CFRetained`
        // owns and releases once.
        let props =
            unsafe { url.resource_properties_for_keys(Some(keys.as_opaque()), ptr::null_mut()) };
        let Some(props) = props else {
            return (None, None);
        };
        let available = number(&props, important)
            .filter(|&bytes| bytes != 0)
            .or_else(|| number(&props, available));
        (number(&props, total), available)
    }
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
    fn new_disks_are_the_keys_not_listed_before() {
        let listed = HashSet::from([key("disk3", "/"), key("disk3", "/System/Volumes/Data")]);
        let now = [
            key("disk3", "/"),
            key("USB", "/Volumes/USB"),
            key("disk3", "/System/Volumes/Data"),
            key("Other", "/"),
        ];
        assert_eq!(
            new_disks(&listed, &now),
            [1, 3],
            "a new device on a known mount too"
        );
        assert_eq!(new_disks(&HashSet::new(), &now), [0, 1, 2, 3], "first list");
        assert!(new_disks(&listed, &[]).is_empty());
    }

    fn usage(read: u64, written: u64) -> DiskUsage {
        DiskUsage {
            read_bytes: read,
            written_bytes: written,
            total_read_bytes: 1_000_000,
            total_written_bytes: 1_000_000,
        }
    }

    #[test]
    fn a_group_shares_its_refreshed_usage_on_a_relist_tick() {
        // On a re-list tick every disk read its counter, but only disk 0 was also read on
        // the fast ticks before. Disk 1's own delta spans 5 s, so it takes disk 0's.
        let groups = ["disk3", "disk3", "disk4", "tank"];
        let own = [usage(100, 10), usage(500, 50), usage(7, 1), usage(9, 9)];
        let leaders = [0, 2];
        assert_eq!(
            shared_usage(&groups, &own, &leaders),
            [usage(100, 10), usage(100, 10), usage(7, 1), usage(9, 9)],
            "a group without a refreshed disk keeps its own usage"
        );
        assert!(shared_usage(&[], &[], &[]).is_empty());
    }

    #[test]
    fn io_refresh_set_takes_one_disk_per_apfs_container() {
        let items = [
            ("disk3", "apfs", "/", 100),
            ("disk3", "apfs", "/System/Volumes/Data", 100),
            ("disk3", "apfs", "/System/Volumes/VM", 100),
            ("disk5", "apfs", "/Volumes/USB", 100),
        ];
        assert_eq!(io_refresh_set(&items, true), [0, 3]);
    }

    #[test]
    fn space_reads_skip_volumes_that_keep_volume_drops_by_mount_or_file_system() {
        let items = [
            ("/", "apfs"),
            ("/System/Volumes/VM", "apfs"),
            ("/System/Volumes/Data", "apfs"),
            ("/dev", "devfs"),
            ("/Volumes/USB", "msdos"),
            ("/snap/core/1", "ext4"),
        ];
        assert_eq!(space_reads(&items, &[], true), [0, 2, 4], "the space read");
        assert_eq!(
            space_reads(&items, &[1, 3, 4], false),
            [4],
            "a re-list reads only new disks"
        );
        assert!(space_reads(&items, &[], false).is_empty());
        assert!(space_reads(&items, &[99], false).is_empty(), "a bad index");
        assert!(space_reads(&[], &[], true).is_empty());
    }

    #[test]
    fn keep_mount_is_keep_volume_without_the_size() {
        for (mount, fs) in [
            ("/", "apfs"),
            ("/System/Volumes/Data", "apfs"),
            ("/System/Volumes/VM", "apfs"),
            ("/dev", "devfs"),
            ("/snap/x", "ext4"),
        ] {
            assert_eq!(
                keep_mount(mount, fs),
                keep_volume(mount, fs, 1),
                "{mount} {fs}"
            );
            assert!(!keep_volume(mount, fs, 0), "{mount} {fs} of unknown size");
        }
    }

    #[test]
    fn merge_space_keeps_the_last_value_of_a_failed_read() {
        assert_eq!(merge_space(None, (Some(100), Some(40))), Some((100, 40)));
        assert_eq!(
            merge_space(Some((100, 40)), (Some(100), Some(30))),
            Some((100, 30))
        );
        assert_eq!(merge_space(Some((100, 40)), (None, None)), Some((100, 40)));
        assert_eq!(
            merge_space(Some((100, 40)), (Some(200), None)),
            Some((200, 40))
        );
        assert_eq!(
            merge_space(Some((100, 40)), (None, Some(10))),
            Some((100, 10))
        );
        assert_eq!(merge_space(None, (None, Some(10))), Some((0, 10)));
        assert_eq!(merge_space(None, (None, None)), None);
    }

    #[test]
    fn a_leader_not_read_on_the_last_call_moves_no_bytes() {
        // Disk 2 just became the leader of its group: its counter last moved when it was
        // listed, so its delta does not span this interval.
        let keys = [key("sda1", "/"), key("sda1", "/home"), key("sdb1", "/data")];
        let mut own = [usage(100, 10), usage(0, 0), usage(9_000, 900)];
        let last = HashSet::from([key("sda1", "/"), key("sdc1", "/gone")]);
        let read = fresh_deltas(&mut own, &keys, &[0, 2], &last);
        assert_eq!(own, [usage(100, 10), usage(0, 0), usage(0, 0)]);
        assert_eq!(own[2].total_read_bytes, 1_000_000, "the totals stay");
        assert_eq!(
            read,
            HashSet::from([key("sda1", "/"), key("sdb1", "/data")])
        );

        let mut next = [usage(50, 5), usage(0, 0), usage(70, 7)];
        fresh_deltas(&mut next, &keys, &[0, 2], &read);
        assert_eq!(
            next,
            [usage(50, 5), usage(0, 0), usage(70, 7)],
            "read last time"
        );
    }

    #[test]
    fn counter_usage_moves_bytes_only_between_two_reads() {
        let totals = |read, written| DiskUsage {
            read_bytes: 0,
            written_bytes: 0,
            total_read_bytes: read,
            total_written_bytes: written,
        };
        assert_eq!(counter_usage(None, None), DiskUsage::default());
        assert_eq!(
            counter_usage(None, Some((500, 70))),
            totals(500, 70),
            "first read"
        );
        assert_eq!(
            counter_usage(Some((500, 70)), Some((800, 90))),
            DiskUsage {
                read_bytes: 300,
                written_bytes: 20,
                total_read_bytes: 800,
                total_written_bytes: 90,
            }
        );
        assert_eq!(
            counter_usage(Some((500, 70)), None),
            totals(500, 70),
            "a failed read"
        );
        assert_eq!(
            counter_usage(Some((500, 70)), Some((100, 80))),
            DiskUsage {
                read_bytes: 0,
                written_bytes: 10,
                total_read_bytes: 100,
                total_written_bytes: 80,
            },
            "a counter that went down moves nothing"
        );
    }

    #[test]
    fn a_rate_never_spans_a_failed_read() {
        let mut last = None;
        advance_counter(&mut last, Some((500, 70)));
        assert_eq!(
            advance_counter(&mut last, Some((800, 90))).read_bytes,
            300,
            "one tick"
        );
        let failed = advance_counter(&mut last, None);
        assert_eq!((failed.read_bytes, failed.written_bytes), (0, 0));
        assert_eq!(failed.total_read_bytes, 800, "the failed tick keeps the totals");
        let next = advance_counter(&mut last, Some((2_000, 190)));
        assert_eq!(
            (next.read_bytes, next.written_bytes),
            (0, 0),
            "the first read after a gap is a first sample"
        );
        assert_eq!(next.total_read_bytes, 2_000);
        assert_eq!(
            advance_counter(&mut last, Some((2_100, 195))).read_bytes,
            100,
            "then rates go on"
        );
    }

    #[test]
    fn a_driver_is_looked_up_when_missing_failed_or_the_list_changed() {
        assert!(needs_lookup(None, false), "no driver yet");
        assert!(needs_lookup(Some(true), false), "the last read failed");
        assert!(!needs_lookup(Some(false), false));
        assert!(needs_lookup(Some(false), true), "the disk list changed");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn statistics_bytes_reads_the_two_byte_counters() {
        use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType};
        let dict = |entries: &[(&str, &CFType)]| {
            let keys: Vec<_> = entries.iter().map(|(k, _)| CFString::from_str(k)).collect();
            let key_refs: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
            let values: Vec<&CFType> = entries.iter().map(|(_, v)| *v).collect();
            CFDictionary::<CFString, CFType>::from_slices(&key_refs, &values)
        };
        let read = CFNumber::new_i64(3_855_113_187_328);
        let written = CFNumber::new_i64(2_942_327_005_184);
        let ops = CFNumber::new_i64(70_456_726);
        let text = CFString::from_str("many");
        let full = dict(&[
            ("Operations (Write)", ops.as_ref()),
            ("Bytes (Read)", read.as_ref()),
            ("Bytes (Write)", written.as_ref()),
        ]);
        assert_eq!(
            apple::statistics_bytes(full.as_opaque()),
            Some((3_855_113_187_328, 2_942_327_005_184))
        );
        let missing = dict(&[("Bytes (Read)", read.as_ref())]);
        assert_eq!(apple::statistics_bytes(missing.as_opaque()), None);
        let not_a_number = dict(&[
            ("Bytes (Read)", read.as_ref()),
            ("Bytes (Write)", text.as_ref()),
        ]);
        assert_eq!(apple::statistics_bytes(not_a_number.as_opaque()), None);
        assert_eq!(apple::statistics_bytes(dict(&[]).as_opaque()), None);
    }

    /// The whole disk and the sysinfo I/O totals of each kept volume of this host.
    #[cfg(target_os = "macos")]
    fn sysinfo_disk_totals() -> Vec<(String, (u64, u64))> {
        let disks = Disks::new_with_refreshed_list_specifics(
            DiskRefreshKind::nothing().with_io_usage().with_storage(),
        );
        disks
            .list()
            .iter()
            .filter(|disk| {
                keep_volume(
                    &disk.mount_point().to_string_lossy(),
                    &disk.file_system().to_string_lossy(),
                    disk.total_space(),
                )
            })
            .filter_map(|disk| {
                let from = mount_source(disk.mount_point())?;
                let whole = apple_whole_disk(&from)?.to_owned();
                let usage = disk.usage();
                Some((whole, (usage.total_read_bytes, usage.total_written_bytes)))
            })
            .collect()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kept_drivers_give_the_disk_counters_of_sysinfo() {
        let theirs = sysinfo_disk_totals();
        assert!(!theirs.is_empty(), "this Mac has a kept volume on a disk");
        let mut groups: Vec<&str> = theirs.iter().map(|(group, _)| group.as_str()).collect();
        groups.sort_unstable();
        groups.dedup();
        let mut drivers = apple::Drivers::default();
        drivers.sync(&groups, true);
        let totals = |drivers: &mut apple::Drivers, group| {
            let usage = drivers.usage(group);
            (usage.total_read_bytes, usage.total_written_bytes)
        };
        let before: Vec<(u64, u64)> = groups.iter().map(|g| totals(&mut drivers, g)).collect();
        let theirs = sysinfo_disk_totals();
        let after: Vec<(u64, u64)> = groups.iter().map(|g| totals(&mut drivers, g)).collect();
        for (group, (read, written)) in &theirs {
            let at = groups.iter().position(|g| g == group).expect("same groups");
            let (low, high) = (before[at], after[at]);
            // The counters only grow, so sysinfo's read lies between the two of ours.
            assert!(low.0 > 0 || low.1 > 0, "{group} has counters");
            assert!(
                (low.0..=high.0).contains(read),
                "{group} read {low:?} {read} {high:?}"
            );
            assert!(
                (low.1..=high.1).contains(written),
                "{group} written {low:?} {written} {high:?}"
            );
        }

        // A recheck finds the same drivers and keeps their counters: no first read.
        drivers.sync(&groups, true);
        let usage = drivers.usage(groups[0]);
        assert!(usage.total_read_bytes >= after[0].0);
        // A group that goes is released; one that is not a disk has no counter.
        drivers.sync(&[], false);
        assert_eq!(drivers.usage(groups[0]), DiskUsage::default());
        drivers.sync(&["disk999"], false);
        assert_eq!(drivers.usage("disk999"), DiskUsage::default());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn volume_space_gives_the_space_of_sysinfo() {
        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());
        let root = disks
            .list()
            .iter()
            .find(|disk| disk.mount_point() == std::path::Path::new("/"))
            .expect("a root volume");
        let (total, available) = apple::volume_space(root.mount_point());
        assert_eq!(total, Some(root.total_space()));
        let available = available.expect("available space");
        // Free space moves between the reads; 1 GiB is far more than a test run writes.
        assert!(
            available.abs_diff(root.available_space()) < 1 << 30,
            "{available} vs {}",
            root.available_space()
        );
        let missing = apple::volume_space(std::path::Path::new("/nonexistent/rtop"));
        assert_eq!(missing, (None, None));
    }

    #[test]
    fn collector_keeps_the_volumes_and_sizes_of_a_full_sysinfo_list() {
        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());
        let raws: Vec<(String, String, String, u64)> = disks
            .list()
            .iter()
            .map(|disk| {
                (
                    disk.name().to_string_lossy().into_owned(),
                    disk.file_system().to_string_lossy().into_owned(),
                    disk.mount_point().to_string_lossy().into_owned(),
                    disk.total_space(),
                )
            })
            .filter(|(_, fs, mount, total)| {
                keep_volume(mount, fs, *total) && !is_file_mount(std::path::Path::new(mount))
            })
            .collect();
        let items: Vec<(&str, &str, &str)> = raws
            .iter()
            .map(|(device, fs, mount, _)| (device.as_str(), fs.as_str(), mount.as_str()))
            .collect();
        let mut theirs: Vec<(String, u64)> = drop_zfs_children(&items)
            .into_iter()
            .map(|i| (raws[i].2.clone(), raws[i].3))
            .collect();
        theirs.sort();

        let storage = StorageCollector::new().collect(1.0, true, true);
        let mut ours: Vec<(String, u64)> = storage
            .volumes
            .iter()
            .map(|v| (v.mount.clone(), v.total))
            .collect();
        ours.sort();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn collector_reads_this_host_without_panic() {
        let mut collector = StorageCollector::new();
        let first = collector.collect(1.0, true, true);
        let second = collector.collect(0.25, false, false);
        let relist = collector.collect(0.25, true, false);
        for storage in [&first, &second, &relist] {
            assert!(storage.volumes.windows(2).all(|w| w[1].mount != "/"));
            assert!(storage.volumes.iter().all(|v| v.total > 0));
        }
        let mounts = |s: &Storage| {
            s.volumes
                .iter()
                .map(|v| v.mount.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            mounts(&relist),
            mounts(&first),
            "a re-list without space keeps the space of known disks"
        );
        assert!(
            first
                .volumes
                .iter()
                .all(|v| v.read_rate == 0 && v.write_rate == 0)
        );
        assert_eq!(first.read_rate, 0);
    }
}
