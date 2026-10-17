//! ZFS ARC statistics: a pure parser, delta logic and a reader for each OS.

use std::collections::VecDeque;
use std::time::Instant;

/// Raw ARC counters as the kernel gives them. A field that a release does not have is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArcRaw {
    pub size: u64,
    pub c: u64,
    pub c_min: u64,
    pub c_max: u64,
    pub hits: u64,
    /// OpenZFS 2.2 and later.
    pub iohits: Option<u64>,
    pub misses: u64,
    pub demand_data_hits: u64,
    pub demand_data_iohits: Option<u64>,
    pub demand_data_misses: u64,
    pub demand_metadata_hits: u64,
    pub demand_metadata_iohits: Option<u64>,
    pub demand_metadata_misses: u64,
    pub mru_size: u64,
    pub mfu_size: u64,
    /// Present since compressed ARC (ZoL 0.7, from memory). `None` on older releases.
    pub data_size: Option<u64>,
    pub metadata_size: Option<u64>,
    /// Removed in OpenZFS 2.2 (from memory). Fallback for `metadata` only.
    pub arc_meta_used: Option<u64>,
    pub compressed_size: Option<u64>,
    pub uncompressed_size: Option<u64>,
    pub l2_size: u64,
    pub l2_asize: u64,
    pub l2_hits: u64,
    pub l2_misses: u64,
}

/// The ARC values that the panel shows. Percentages are 0.0 to 100.0.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ZfsArc {
    pub size: u64,
    pub target: u64,
    pub min: u64,
    pub max: u64,
    pub mru: u64,
    pub mfu: u64,
    pub data: u64,
    pub metadata: u64,
    /// uncompressed_size / compressed_size. `None` if a value is missing or compressed_size is 0.
    pub compress_ratio: Option<f64>,
    /// Over the window (see `derive`), or since boot when `hit_since_boot` is true.
    /// `None` only when there were no reads at all.
    pub hit_pct: Option<f64>,
    pub iohit_pct: Option<f64>,
    pub demand_data_pct: Option<f64>,
    pub demand_meta_pct: Option<f64>,
    /// True when the four values above are since-boot ratios, not window ratios.
    pub hit_since_boot: bool,
    /// `Some` only when l2_size > 0.
    pub l2: Option<L2Arc>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct L2Arc {
    pub size: u64,
    pub asize: u64,
    /// Δl2_hits / (Δl2_hits + Δl2_misses) over the window, or since boot when `since_boot` is true.
    pub hit_pct: Option<f64>,
    pub since_boot: bool,
}

/// The hit-ratio window. The collector derives against the newest kept sample that is at least this old.
pub const HIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// The most earlier samples that the collector keeps.
const RING_CAP: usize = 64;

/// The kstat field names that `from_pairs` reads. FreeBSD and macOS read these names by sysctl.
/// `from_pairs` matches names itself and does not use this constant (see the cfg table in §5.5).
#[cfg(any(test, target_os = "macos", target_os = "freebsd"))]
pub const FIELDS: [&str; 24] = [
    "size",
    "c",
    "c_min",
    "c_max",
    "hits",
    "iohits",
    "misses",
    "demand_data_hits",
    "demand_data_iohits",
    "demand_data_misses",
    "demand_metadata_hits",
    "demand_metadata_iohits",
    "demand_metadata_misses",
    "mru_size",
    "mfu_size",
    "data_size",
    "metadata_size",
    "arc_meta_used",
    "compressed_size",
    "uncompressed_size",
    "l2_size",
    "l2_asize",
    "l2_hits",
    "l2_misses",
];

/// Builds `ArcRaw` from name and value pairs. Unknown names are ignored.
/// Returns `None` if `size`, `c` or `c_max` is missing.
#[cfg(any(
    test,
    debug_assertions,
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd"
))]
pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, u64)>) -> Option<ArcRaw> {
    let mut raw = ArcRaw::default();
    let (mut size, mut c, mut c_max) = (None, None, None);
    for (name, v) in pairs {
        match name {
            "size" => size = Some(v),
            "c" => c = Some(v),
            "c_min" => raw.c_min = v,
            "c_max" => c_max = Some(v),
            "hits" => raw.hits = v,
            "iohits" => raw.iohits = Some(v),
            "misses" => raw.misses = v,
            "demand_data_hits" => raw.demand_data_hits = v,
            "demand_data_iohits" => raw.demand_data_iohits = Some(v),
            "demand_data_misses" => raw.demand_data_misses = v,
            "demand_metadata_hits" => raw.demand_metadata_hits = v,
            "demand_metadata_iohits" => raw.demand_metadata_iohits = Some(v),
            "demand_metadata_misses" => raw.demand_metadata_misses = v,
            "mru_size" => raw.mru_size = v,
            "mfu_size" => raw.mfu_size = v,
            "data_size" => raw.data_size = Some(v),
            "metadata_size" => raw.metadata_size = Some(v),
            "arc_meta_used" => raw.arc_meta_used = Some(v),
            "compressed_size" => raw.compressed_size = Some(v),
            "uncompressed_size" => raw.uncompressed_size = Some(v),
            "l2_size" => raw.l2_size = v,
            "l2_asize" => raw.l2_asize = v,
            "l2_hits" => raw.l2_hits = v,
            "l2_misses" => raw.l2_misses = v,
            _ => {}
        }
    }
    raw.size = size?;
    raw.c = c?;
    raw.c_max = c_max?;
    Some(raw)
}

/// Parses the text of `/proc/spl/kstat/zfs/arcstats`. Skips the first two lines.
/// Each row is `name type value`; the value is the third token. Rows that do not parse are skipped.
#[cfg(any(test, debug_assertions, target_os = "linux"))]
pub fn parse_kstat(text: &str) -> Option<ArcRaw> {
    from_pairs(text.lines().skip(2).filter_map(|line| {
        let mut tokens = line.split_whitespace();
        let name = tokens.next()?;
        let _kind = tokens.next()?;
        let value = tokens.next()?.parse().ok()?;
        Some((name, value))
    }))
}

/// Pure: makes the panel values from the current raw counters and the window's base counters.
pub fn derive(prev: Option<&ArcRaw>, cur: &ArcRaw) -> ZfsArc {
    let metadata = cur.metadata_size.or(cur.arc_meta_used).unwrap_or_default();
    let data = cur
        .data_size
        .unwrap_or_else(|| cur.size.saturating_sub(metadata));
    let compress_ratio = match (cur.uncompressed_size, cur.compressed_size) {
        (Some(u), Some(c)) if c > 0 => Some(u as f64 / c as f64),
        _ => None,
    };

    let window = prev.and_then(|p| hit_ratios(&delta(p, cur)?));
    let (ratios, hit_since_boot) = match window {
        Some(r) => (r, false),
        None => (hit_ratios(&Counters::of(cur)).unwrap_or_default(), true),
    };

    let l2 = (cur.l2_size > 0).then(|| {
        let window = prev.and_then(|p| {
            let hits = cur.l2_hits.checked_sub(p.l2_hits)?;
            let misses = cur.l2_misses.checked_sub(p.l2_misses)?;
            ratio(hits, hits.saturating_add(misses))
        });
        let (hit_pct, since_boot) = match window {
            Some(pct) => (Some(pct), false),
            None => (
                ratio(cur.l2_hits, cur.l2_hits.saturating_add(cur.l2_misses)),
                true,
            ),
        };
        L2Arc {
            size: cur.l2_size,
            asize: cur.l2_asize,
            hit_pct,
            since_boot,
        }
    });

    ZfsArc {
        size: cur.size,
        target: cur.c,
        min: cur.c_min,
        max: cur.c_max,
        mru: cur.mru_size,
        mfu: cur.mfu_size,
        data,
        metadata,
        compress_ratio,
        hit_pct: ratios.hit,
        iohit_pct: ratios.iohit,
        demand_data_pct: ratios.demand_data,
        demand_meta_pct: ratios.demand_meta,
        hit_since_boot,
        l2,
    }
}

/// Hit and miss counters, either since boot or as the change over a window.
/// `None` in a demand field means a `checked_sub` failed for it.
struct Counters {
    hits: u64,
    /// `None` when the release has no `iohits`.
    iohits: Option<u64>,
    misses: u64,
    demand_data: Option<[u64; 3]>,
    demand_meta: Option<[u64; 3]>,
}

impl Counters {
    fn of(raw: &ArcRaw) -> Self {
        Counters {
            hits: raw.hits,
            iohits: raw.iohits,
            misses: raw.misses,
            demand_data: Some([
                raw.demand_data_hits,
                raw.demand_data_iohits.unwrap_or_default(),
                raw.demand_data_misses,
            ]),
            demand_meta: Some([
                raw.demand_metadata_hits,
                raw.demand_metadata_iohits.unwrap_or_default(),
                raw.demand_metadata_misses,
            ]),
        }
    }
}

/// `cur − prev` for an optional counter. Both absent is no change; one absent is unknown.
fn delta_opt(cur: Option<u64>, prev: Option<u64>) -> Option<Option<u64>> {
    match (cur, prev) {
        (Some(c), Some(p)) => c.checked_sub(p).map(Some),
        (None, None) => Some(None),
        _ => None,
    }
}

/// The change from `prev` to `cur`. `None` when a main counter went down (a reset).
fn delta(prev: &ArcRaw, cur: &ArcRaw) -> Option<Counters> {
    let demand = |hits: (u64, u64), iohits: (Option<u64>, Option<u64>), misses: (u64, u64)| {
        Some([
            hits.0.checked_sub(hits.1)?,
            delta_opt(iohits.0, iohits.1)?.unwrap_or_default(),
            misses.0.checked_sub(misses.1)?,
        ])
    };
    Some(Counters {
        hits: cur.hits.checked_sub(prev.hits)?,
        iohits: delta_opt(cur.iohits, prev.iohits)?,
        misses: cur.misses.checked_sub(prev.misses)?,
        demand_data: demand(
            (cur.demand_data_hits, prev.demand_data_hits),
            (cur.demand_data_iohits, prev.demand_data_iohits),
            (cur.demand_data_misses, prev.demand_data_misses),
        ),
        demand_meta: demand(
            (cur.demand_metadata_hits, prev.demand_metadata_hits),
            (cur.demand_metadata_iohits, prev.demand_metadata_iohits),
            (cur.demand_metadata_misses, prev.demand_metadata_misses),
        ),
    })
}

#[derive(Default)]
struct Ratios {
    hit: Option<f64>,
    iohit: Option<f64>,
    demand_data: Option<f64>,
    demand_meta: Option<f64>,
}

/// The four ratios. `None` when there were no reads at all.
fn hit_ratios(n: &Counters) -> Option<Ratios> {
    let reads = n
        .hits
        .saturating_add(n.iohits.unwrap_or_default())
        .saturating_add(n.misses);
    if reads == 0 {
        return None;
    }
    let demand = |d: Option<[u64; 3]>| {
        let [hits, iohits, misses] = d?;
        ratio(hits, hits.saturating_add(iohits).saturating_add(misses))
    };
    Some(Ratios {
        hit: ratio(n.hits, reads),
        iohit: n.iohits.and_then(|io| ratio(io, reads)),
        demand_data: demand(n.demand_data),
        demand_meta: demand(n.demand_meta),
    })
}

/// `100 × part / whole`, or `None` when `whole` is 0.
fn ratio(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| part as f64 * 100.0 / whole as f64)
}

/// Pure. Linux used memory with the shrinkable ARC (`size − min`) counted as free. See §7.6.
#[cfg(any(test, target_os = "linux"))]
pub fn arc_adjusted_used(used: u64, arc: &ZfsArc) -> u64 {
    used.saturating_sub(arc.size.saturating_sub(arc.min))
}

/// Owned by the sampler. Keeps a short ring of earlier counters for the hit-ratio window.
pub struct ZfsCollector {
    ring: VecDeque<(Instant, ArcRaw)>,
    present: bool,
}

impl ZfsCollector {
    pub fn new() -> Self {
        ZfsCollector {
            ring: VecDeque::new(),
            present: false,
        }
    }

    /// Reads the counters. Returns `None` when ZFS is absent.
    /// When ZFS is absent, it tries again only on a slow tick.
    pub fn collect(&mut self, slow: bool) -> Option<ZfsArc> {
        if !self.present && !slow {
            return None;
        }
        self.update(Instant::now(), read_raw())
    }

    /// Steps 2 to 5 of `collect`, with the reading and the time given.
    fn update(&mut self, now: Instant, raw: Option<ArcRaw>) -> Option<ZfsArc> {
        let Some(cur) = raw else {
            self.ring.clear();
            self.present = false;
            return None;
        };
        self.present = true;

        let base = window_base(&self.ring, now);
        if let Some(i) = base {
            self.ring.drain(..i);
        }
        let base = base.and_then(|_| self.ring.front()).map(|(_, raw)| raw);
        let arc = derive(base, &cur);
        if base.is_some_and(|b| cur.hits < b.hits) {
            self.ring.clear();
        }

        self.ring.push_back((now, cur));
        while self.ring.len() > RING_CAP {
            self.ring.pop_front();
        }
        Some(arc)
    }
}

/// Reads the counters for this OS. A debug build reads `RTOP_ZFS_ARCSTATS` first when it is set.
fn read_raw() -> Option<ArcRaw> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("RTOP_ZFS_ARCSTATS") {
        return read_kstat_file(std::path::Path::new(&path));
    }
    read_os()
}

#[cfg(target_os = "linux")]
fn read_os() -> Option<ArcRaw> {
    read_kstat_file(std::path::Path::new("/proc/spl/kstat/zfs/arcstats"))
}

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
fn read_os() -> Option<ArcRaw> {
    const PREFIX: &str = "kstat.zfs.misc.arcstats.";
    // One call decides presence, so a host without ZFS costs one sysctl on a slow tick.
    let size = sysctl_u64(&format!("{PREFIX}size"))?;
    let rest = FIELDS[1..]
        .iter()
        .filter_map(|&name| sysctl_u64(&format!("{PREFIX}{name}")).map(|v| (name, v)));
    from_pairs(std::iter::once(("size", size)).chain(rest))
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")))]
fn read_os() -> Option<ArcRaw> {
    None
}

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
fn sysctl_u64(name: &str) -> Option<u64> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut v: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `c` is NUL-terminated. `v` and `len` live until the call returns.
    // `newp` is null and `newlen` is 0, so nothing is written to the kernel.
    let rc = unsafe {
        libc::sysctlbyname(
            c.as_ptr(),
            (&raw mut v).cast::<libc::c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<u64>()).then_some(v)
}

/// Reads and parses a kstat file. `None` when the file does not read or parse.
#[cfg(any(test, debug_assertions, target_os = "linux"))]
fn read_kstat_file(path: &std::path::Path) -> Option<ArcRaw> {
    parse_kstat(&std::fs::read_to_string(path).ok()?)
}

/// The index of the window base in `ring`: the newest entry that is at least `HIT_WINDOW`
/// old, else the oldest entry. `None` when the ring is empty.
fn window_base(ring: &VecDeque<(Instant, ArcRaw)>, now: Instant) -> Option<usize> {
    if ring.is_empty() {
        return None;
    }
    ring.iter()
        .rposition(|(at, _)| now.saturating_duration_since(*at) >= HIT_WINDOW)
        .or(Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const A1: &str = include_str!("../testdata/arcstats-2.2.txt");
    const A2: &str = include_str!("../testdata/arcstats-2.2-next.txt");

    /// OpenZFS 2.1 style: no `iohits` rows, `arc_meta_used` present.
    const ARC_2_1: &str = "\
13 1 0x01 23 6624 4625134567890123 1049387654321098
name                            type data
hits                            4    1000000
misses                          4    250000
demand_data_hits                4    600000
demand_data_misses              4    100000
demand_metadata_hits            4    300000
demand_metadata_misses          4    50000
p                               4    2147483648
c                               4    4294967296
c_min                           4    536870912
c_max                           4    8589934592
size                            4    4294967296
compressed_size                 4    2147483648
uncompressed_size               4    3221225472
data_size                       4    3006477107
metadata_size                   4    1073741824
mru_size                        4    1610612736
mfu_size                        4    2147483648
l2_hits                         4    0
l2_misses                       4    0
l2_size                         4    0
l2_asize                        4    0
arc_meta_used                   4    1342177280
arc_meta_limit                  4    3221225472
";

    fn a1() -> ArcRaw {
        parse_kstat(A1).expect("fixture A.1 parses")
    }

    fn a2() -> ArcRaw {
        parse_kstat(A2).expect("fixture A.2 parses")
    }

    fn close(got: Option<f64>, want: f64) -> bool {
        got.is_some_and(|x| (x - want).abs() < 1e-9)
    }

    fn assert_close(got: Option<f64>, want: f64, what: &str) {
        assert!(close(got, want), "{what}: got {got:?}, want {want}");
    }

    #[test]
    fn parse_kstat_reads_the_2_2_fixture() {
        let raw = a1();
        assert_eq!(raw.size, 9_663_676_416);
        assert_eq!(raw.c, 10_737_418_240);
        assert_eq!(raw.c_min, 1_073_741_824);
        assert_eq!(raw.c_max, 17_179_869_184);
        assert_eq!(raw.iohits, Some(37_116));
        assert_eq!(raw.data_size, Some(6_552_476_672));
        assert_eq!(raw.metadata_size, Some(2_147_483_648));
        assert_eq!(raw.arc_meta_used, None);
        assert_eq!(raw.l2_size, 214_748_364_800);
    }

    #[test]
    fn derive_since_boot_from_a1() {
        let arc = derive(None, &a1());
        assert!(arc.hit_since_boot);
        assert_close(arc.hit_pct, 85.0, "hit_pct");
        assert_close(arc.iohit_pct, 0.37116, "iohit_pct");
        assert_close(arc.demand_data_pct, 80.0, "demand_data_pct");
        assert_close(arc.demand_meta_pct, 95.0, "demand_meta_pct");
        let l2 = arc.l2.clone().expect("l2 present");
        assert_eq!(l2.size, 214_748_364_800);
        assert_eq!(l2.asize, 171_798_691_840);
        assert_close(l2.hit_pct, 25.0, "l2.hit_pct");
        assert!(l2.since_boot);
        let ratio = arc.compress_ratio.expect("compress_ratio");
        assert!((ratio - 2.42).abs() < 1e-6, "compress_ratio {ratio}");
        assert_eq!(arc.data, 6_552_476_672);
        assert_eq!(arc.metadata, 2_147_483_648);
        assert_eq!(arc.size, 9_663_676_416);
        assert_eq!(arc.target, 10_737_418_240);
        assert_eq!(arc.min, 1_073_741_824);
        assert_eq!(arc.max, 17_179_869_184);
        assert_eq!(arc.mru, 5_368_709_120);
        assert_eq!(arc.mfu, 3_221_225_472);
    }

    #[test]
    fn derive_over_the_window_from_a1_to_a2() {
        let arc = derive(Some(&a1()), &a2());
        assert!(!arc.hit_since_boot);
        assert_close(arc.hit_pct, 90.0, "hit_pct");
        assert_eq!(arc.iohit_pct, Some(0.0));
        assert_close(arc.demand_data_pct, 90.0, "demand_data_pct");
        assert_close(arc.demand_meta_pct, 90.0, "demand_meta_pct");
        let l2 = arc.l2.expect("l2 present");
        assert_close(l2.hit_pct, 10.0, "l2.hit_pct");
        assert!(!l2.since_boot);
    }

    #[test]
    fn derive_after_a_counter_reset_is_since_boot() {
        assert_eq!(derive(Some(&a2()), &a1()), derive(None, &a1()));
    }

    #[test]
    fn derive_with_no_reads_in_the_window_is_since_boot() {
        assert_eq!(derive(Some(&a1()), &a1()), derive(None, &a1()));
    }

    #[test]
    fn parse_kstat_reads_a_2_1_file() {
        let raw = parse_kstat(ARC_2_1).expect("2.1 fixture parses");
        assert_eq!(raw.iohits, None);
        assert_eq!(raw.data_size, Some(3_006_477_107));

        let arc = derive(None, &raw);
        assert_eq!(arc.data, 3_006_477_107);
        assert_eq!(arc.metadata, 1_073_741_824, "metadata_size wins");
        let ratio = arc.compress_ratio.expect("compress_ratio");
        assert!((ratio - 1.5).abs() < 1e-6, "compress_ratio {ratio}");
        assert_eq!(arc.l2, None);
        assert_eq!(arc.iohit_pct, None);
        assert_close(arc.hit_pct, 80.0, "hit_pct");
        assert!(arc.hit_since_boot);
        assert_close(
            arc.demand_data_pct,
            600_000.0 / 700_000.0 * 100.0,
            "demand_data_pct",
        );
    }

    #[test]
    fn metadata_falls_back_to_arc_meta_used_then_zero() {
        let raw = from_pairs([
            ("size", 1000),
            ("c", 1000),
            ("c_max", 2000),
            ("arc_meta_used", 300),
        ])
        .unwrap();
        let arc = derive(None, &raw);
        assert_eq!((arc.metadata, arc.data), (300, 700));

        let raw = from_pairs([("size", 1000), ("c", 1000), ("c_max", 2000)]).unwrap();
        let arc = derive(None, &raw);
        assert_eq!((arc.metadata, arc.data), (0, 1000));
    }

    #[test]
    fn window_with_no_demand_reads() {
        let base = a1();
        let cur = ArcRaw {
            hits: base.hits + 10,
            ..base.clone()
        };
        let arc = derive(Some(&base), &cur);
        assert_eq!(arc.hit_pct, Some(100.0));
        assert!(!arc.hit_since_boot);
        assert_eq!(arc.demand_data_pct, None);
        assert_eq!(arc.demand_meta_pct, None);
    }

    #[test]
    fn parse_kstat_rejects_bad_input_and_skips_bad_rows() {
        assert_eq!(parse_kstat(""), None);
        assert_eq!(
            parse_kstat("13 1 0x01 37 10656 1 2\nname type data\n"),
            None
        );
        let no_size = "h\nname type data\nc 4 10\nc_max 4 20\n";
        assert_eq!(parse_kstat(no_size), None);

        let bad_row = "h\nname type data\nsize 4 10\nc 4 10\nc_max 4 20\nhits 4 notanumber\n";
        let raw = parse_kstat(bad_row).expect("bad row is skipped");
        assert_eq!(raw.hits, 0);
        assert_eq!(raw.size, 10);
    }

    #[test]
    fn from_pairs_needs_only_size_c_and_c_max() {
        assert!(from_pairs([("size", 1), ("c", 2), ("c_max", 3)]).is_some());
        assert!(from_pairs([("size", 1), ("c", 2)]).is_none());
        assert!(from_pairs([("c", 2), ("c_max", 3)]).is_none());
        assert!(from_pairs([("size", 1), ("c_max", 3)]).is_none());
    }

    #[test]
    fn fields_agree_with_from_pairs() {
        let pairs = FIELDS
            .iter()
            .enumerate()
            .map(|(i, &name)| (name, i as u64 + 1));
        let raw = from_pairs(pairs).expect("all fields given");
        // A full destructure: a new field in `ArcRaw` breaks this test until it is listed.
        let ArcRaw {
            size,
            c,
            c_min,
            c_max,
            hits,
            iohits,
            misses,
            demand_data_hits,
            demand_data_iohits,
            demand_data_misses,
            demand_metadata_hits,
            demand_metadata_iohits,
            demand_metadata_misses,
            mru_size,
            mfu_size,
            data_size,
            metadata_size,
            arc_meta_used,
            compressed_size,
            uncompressed_size,
            l2_size,
            l2_asize,
            l2_hits,
            l2_misses,
        } = raw;
        let plain = [
            size,
            c,
            c_min,
            c_max,
            hits,
            misses,
            demand_data_hits,
            demand_data_misses,
            demand_metadata_hits,
            demand_metadata_misses,
            mru_size,
            mfu_size,
            l2_size,
            l2_asize,
            l2_hits,
            l2_misses,
        ];
        let optional = [
            iohits,
            demand_data_iohits,
            demand_metadata_iohits,
            data_size,
            metadata_size,
            arc_meta_used,
            compressed_size,
            uncompressed_size,
        ];
        assert_eq!(plain.len() + optional.len(), FIELDS.len());
        assert!(plain.iter().all(|&v| v != 0), "{plain:?}");
        assert!(
            optional.iter().all(|v| v.is_some_and(|v| v != 0)),
            "{optional:?}"
        );
        // Distinct values: each name sets its own field.
        let mut all: Vec<u64> = plain
            .into_iter()
            .chain(optional.into_iter().flatten())
            .collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), FIELDS.len());
    }

    #[test]
    fn arc_adjusted_used_counts_shrinkable_arc_as_free() {
        let arc = ZfsArc {
            size: 6_000,
            min: 1_000,
            ..Default::default()
        };
        assert_eq!(arc_adjusted_used(10_000, &arc), 5_000);

        let small = ZfsArc {
            size: 500,
            min: 1_000,
            ..Default::default()
        };
        assert_eq!(arc_adjusted_used(10_000, &small), 10_000, "size < min");

        assert_eq!(arc_adjusted_used(3_000, &arc), 0, "used < shrinkable");
    }

    fn ring_at(base: Instant, secs: &[u64]) -> VecDeque<(Instant, ArcRaw)> {
        secs.iter()
            .map(|&s| (base + Duration::from_secs(s), ArcRaw::default()))
            .collect()
    }

    #[test]
    fn window_base_picks_the_newest_entry_at_least_a_window_old() {
        let base = Instant::now();
        let now = base + Duration::from_secs(10);
        let ring = ring_at(base, &[0, 2, 4, 6]);
        assert_eq!(window_base(&ring, now), Some(2), "the entry at +4 s");

        let young = ring_at(base, &[8, 9]);
        assert_eq!(window_base(&young, now), Some(0), "the oldest, at +8 s");

        assert_eq!(window_base(&VecDeque::new(), now), None);
    }

    #[test]
    fn collector_uses_the_window_and_drops_old_entries() {
        let mut collector = ZfsCollector::new();
        let t0 = Instant::now();
        let first = collector.update(t0, Some(a1())).expect("present");
        assert!(first.hit_since_boot, "no base on the first sample");

        let second = collector
            .update(t0 + Duration::from_secs(6), Some(a2()))
            .expect("present");
        assert!(!second.hit_since_boot);
        assert_close(second.hit_pct, 90.0, "hit_pct");

        // At +12 s the entry at +6 s is the base, so the entry at +0 s goes.
        collector.update(t0 + Duration::from_secs(12), Some(a2()));
        assert_eq!(collector.ring.len(), 2);
        assert_eq!(collector.ring[0].0, t0 + Duration::from_secs(6));
    }

    #[test]
    fn collector_ring_is_capped() {
        let mut collector = ZfsCollector::new();
        let t0 = Instant::now();
        for i in 0..(RING_CAP as u64 + 10) {
            collector.update(t0 + Duration::from_millis(i), Some(a1()));
        }
        assert_eq!(collector.ring.len(), RING_CAP);
    }

    #[test]
    fn collector_counter_reset_clears_the_ring() {
        let mut collector = ZfsCollector::new();
        let t0 = Instant::now();
        collector.update(t0, Some(a2()));
        collector.update(t0 + Duration::from_secs(1), Some(a2()));
        let arc = collector
            .update(t0 + Duration::from_secs(2), Some(a1()))
            .expect("present");
        assert!(arc.hit_since_boot);
        assert_eq!(collector.ring.len(), 1, "only the new sample is kept");
        assert_eq!(collector.ring[0].1, a1());
    }

    #[test]
    fn collector_absent_clears_state_and_waits_for_a_slow_tick() {
        let mut collector = ZfsCollector::new();
        let t0 = Instant::now();
        collector.update(t0, Some(a1()));
        assert!(collector.present);

        assert_eq!(collector.update(t0 + Duration::from_secs(1), None), None);
        assert!(!collector.present);
        assert!(collector.ring.is_empty(), "no stale base");

        // Absent and not slow: no read at all.
        assert_eq!(collector.collect(false), None);
    }

    #[test]
    fn collector_on_this_host_does_not_panic() {
        let mut collector = ZfsCollector::new();
        assert_eq!(
            collector.collect(false),
            None,
            "a new collector waits for a slow tick"
        );
        let _ = collector.collect(true);
    }

    #[test]
    fn file_reader_takes_a_path() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
        assert_eq!(read_kstat_file(&dir.join("arcstats-2.2.txt")), Some(a1()));
        assert_eq!(read_kstat_file(&dir.join("no-such-file.txt")), None);
    }
}
