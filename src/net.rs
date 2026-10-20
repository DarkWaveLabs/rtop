//! Network interfaces: per-interface rates and totals.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetIface {
    pub name: String,
    /// Bytes per second over the last interval.
    pub rx_rate: u64,
    pub tx_rate: u64,
    /// Bytes since boot.
    pub total_rx: u64,
    pub total_tx: u64,
}

/// Pure. Drops names that start with "lo" and interfaces with no traffic since boot.
pub fn keep_iface(name: &str, total_rx: u64, total_tx: u64) -> bool {
    !name.starts_with("lo") && (total_rx > 0 || total_tx > 0)
}

/// Rates from cumulative byte counters, for the native readers (`SysfsNet`, `RouteNet`).
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
#[derive(Debug, Default)]
struct Rates {
    /// (rx, tx) totals at the last sample, by interface name.
    last: std::collections::HashMap<String, (u64, u64)>,
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
impl Rates {
    /// Pure. Rates from the change since the last call. An interface moves no bytes in
    /// its first sample, and a counter that went down moves none, as with sysinfo.
    fn update(&mut self, now: Vec<(String, u64, u64)>, secs: f64) -> Vec<NetIface> {
        let ifaces = build(
            now.iter().map(|(name, rx, tx)| {
                let (last_rx, last_tx) = self.last.get(name).copied().unwrap_or((*rx, *tx));
                (
                    name.as_str(),
                    rx.saturating_sub(last_rx),
                    tx.saturating_sub(last_tx),
                    *rx,
                    *tx,
                )
            }),
            secs,
        );
        self.last = now
            .into_iter()
            .map(|(name, rx, tx)| (name, (rx, tx)))
            .collect();
        ifaces
    }
}

/// Linux reader for the byte counters in `/sys/class/net/<name>/statistics/`, the files
/// that sysinfo reads too. rtop does not use sysinfo's `Networks` on Linux: its `refresh`
/// also reads each interface address through `getifaddrs`, and sysinfo 0.39.6
/// (`network_helper.rs`, `sockaddr_to_network_addr`) copies a 128-byte `sockaddr_storage`
/// from each address and netmask. musl allocates a much smaller entry, so the copy can run
/// past it into an unmapped page and kill rtop with SIGSEGV.
#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub struct SysfsNet {
    rates: Rates,
}

#[cfg(target_os = "linux")]
impl SysfsNet {
    /// Reads the counters and returns the list, sorted by name.
    pub fn collect(&mut self, secs: f64) -> Vec<NetIface> {
        self.rates
            .update(read_sysfs(std::path::Path::new("/sys/class/net")), secs)
    }
}

/// macOS reader: one `sysctl(NET_RT_IFLIST2)` lists the interfaces, and one
/// `IFMIB_IFDATA` read for each gives its 64-bit byte counters, the counters sysinfo
/// reads too. rtop does not use sysinfo's `Networks` on macOS: for each interface its
/// `refresh` calls `if_indextoname`, which reads the whole `getifaddrs` list, and then it
/// reads `getifaddrs` once more for the addresses, which rtop does not show. Here the
/// names come from the link-level address that follows each `RTM_IFINFO2` message.
///
/// The counters in the `RTM_IFINFO2` message itself are not used: macOS gives them to a
/// process that is not an Apple platform binary rounded down to 1 KiB and cut to 32 bits
/// (sysinfo issue #1378). On the reference Mac, en0 read 1,025,184,768 received bytes
/// there and 5,320,152,651 through `IFMIB_IFDATA`, and a tunnel that had sent 100 bytes
/// read 0, so it would leave the list.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub struct RouteNet {
    rates: Rates,
    /// The `NET_RT_IFLIST2` dump, kept between calls so that a tick needs one `sysctl`.
    buf: Vec<u8>,
}

#[cfg(target_os = "macos")]
impl RouteNet {
    /// Reads the counters and returns the list, sorted by name.
    pub fn collect(&mut self, secs: f64) -> Vec<NetIface> {
        let now = self.read();
        self.rates.update(now, secs)
    }

    /// (name, rx bytes, tx bytes) of each interface, loopback left out. An interface whose
    /// counters cannot be read keeps its last totals (it moves no bytes), or is left out
    /// when it has none, as it would show no traffic in sysinfo either.
    fn read(&mut self) -> Vec<(String, u64, u64)> {
        let Some(len) = route::iflist2(&mut self.buf) else {
            return Vec::new();
        };
        let dump = self.buf.get(..len).unwrap_or_default();
        parse_iflist2(dump)
            .into_iter()
            // `keep_iface` drops loopback, so its counters are not worth a call.
            .filter(|(_, name)| !name.starts_with("lo"))
            .filter_map(|(index, name)| {
                let (rx, tx) =
                    route::ifmib_counters(index).or_else(|| self.rates.last.get(&name).copied())?;
                Some((name, rx, tx))
            })
            .collect()
    }
}

/// The `sysctl` calls of `RouteNet`. What they return is parsed by the pure functions
/// below.
#[cfg(target_os = "macos")]
mod route {
    use std::ptr;

    /// From `net/if_mib.h`; libc 0.2.190 does not have them.
    const NETLINK_GENERIC: libc::c_int = 0;
    const IFMIB_IFDATA: libc::c_int = 2;
    const IFDATA_GENERAL: libc::c_int = 1;

    /// Fills `buf` with the `NET_RT_IFLIST2` dump and returns its length. The buffer is
    /// kept, so a tick makes one call; it grows when an interface appears.
    pub(super) fn iflist2(buf: &mut Vec<u8>) -> Option<usize> {
        let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
        for _ in 0..4 {
            if !buf.is_empty() {
                let mut len = buf.len();
                // SAFETY: `mib` is a valid array of 6 ints and the name length says 6.
                // `buf` is valid for writes of `len` bytes, and `len` is a valid `size_t`
                // that the kernel sets to the number of bytes it wrote, at most the
                // length it was given. No new value is set (null, 0). The dump is parsed
                // as bytes, so the buffer needs no alignment.
                let status = unsafe {
                    libc::sysctl(
                        mib.as_mut_ptr(),
                        mib.len() as libc::c_uint,
                        buf.as_mut_ptr().cast(),
                        &mut len,
                        ptr::null_mut(),
                        0,
                    )
                };
                if status == 0 {
                    return Some(len.min(buf.len()));
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
                    return None;
                }
            }
            let mut need = 0;
            // SAFETY: as above, but with a null old pointer: the kernel only writes the
            // size it needs into `need`, a valid `size_t`.
            let status = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    mib.len() as libc::c_uint,
                    ptr::null_mut(),
                    &mut need,
                    ptr::null_mut(),
                    0,
                )
            };
            if status != 0 {
                return None;
            }
            // Room for an interface or address that appears before the next call.
            buf.resize(need + need / 4 + 1024, 0);
        }
        None
    }

    /// The 64-bit (rx, tx) byte counters of the interface with `index`.
    pub(super) fn ifmib_counters(index: u16) -> Option<(u64, u64)> {
        let mut mib = [
            libc::CTL_NET,
            libc::PF_LINK,
            NETLINK_GENERIC,
            IFMIB_IFDATA,
            libc::c_int::from(index),
            IFDATA_GENERAL,
        ];
        // `struct ifmibdata` is 180 bytes; the extra room keeps a larger one readable.
        let mut record = [0u8; 256];
        let mut len = record.len();
        // SAFETY: `mib` is a valid array of 6 ints and the name length says 6. `record`
        // is valid for writes of `len` bytes, and the kernel sets `len` to the bytes it
        // wrote, at most the length it was given. No new value is set (null, 0). The
        // record is parsed as bytes, so the buffer needs no alignment.
        let status = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                record.as_mut_ptr().cast(),
                &mut len,
                ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return None;
        }
        super::parse_ifmibdata(record.get(..len)?)
    }
}

/// Byte offsets in the macOS routing messages (`net/if.h`, `net/if_dl.h`,
/// `net/if_mib.h`). The messages are packed one after the other, so a message is not
/// aligned for its struct: the parsers read bytes, not structs. A macOS test checks the
/// offsets against libc's structs.
#[cfg(any(test, target_os = "macos"))]
mod layout {
    /// `if_msghdr2`: `ifm_msglen` (u16), `ifm_type` (u8), `ifm_addrs` (i32), `ifm_index` (u16).
    pub const MSGLEN: usize = 0;
    pub const TYPE: usize = 3;
    pub const ADDRS: usize = 4;
    pub const INDEX: usize = 12;
    /// `sizeof(struct if_msghdr2)`: the `sockaddr_dl` follows it.
    pub const IF_MSGHDR2: usize = 160;
    pub const RTM_IFINFO2: u8 = 0x12;
    /// The `ifm_addrs` bit for the link-level address of the interface.
    pub const RTA_IFP: i32 = 0x10;
    /// `sockaddr_dl`: `sdl_len` (u8), `sdl_family` (u8), `sdl_nlen` (u8), `sdl_data`.
    pub const SDL_LEN: usize = 0;
    pub const SDL_FAMILY: usize = 1;
    pub const SDL_NLEN: usize = 5;
    pub const SDL_DATA: usize = 8;
    pub const AF_LINK: u8 = 18;
    /// `ifmibdata`: `ifmd_data` (an `if_data64`) starts at 52; in it `ifi_ibytes` is at
    /// 64 and `ifi_obytes` at 72.
    pub const IFMD_DATA: usize = 52;
    pub const IFI_IBYTES: usize = 64;
    pub const IFI_OBYTES: usize = 72;
}

/// Pure. (interface index, name) for each `RTM_IFINFO2` message in a `NET_RT_IFLIST2`
/// dump, in order. A message with no link-level address or name is skipped. The parse
/// stops at a length that is too short or runs past the dump.
#[cfg(any(test, target_os = "macos"))]
fn parse_iflist2(dump: &[u8]) -> Vec<(u16, String)> {
    let mut ifaces = Vec::new();
    let mut rest = dump;
    while let Some(len) = read_u16(rest, layout::MSGLEN).map(usize::from) {
        if len <= layout::TYPE || len > rest.len() {
            break;
        }
        let (message, tail) = rest.split_at(len);
        rest = tail;
        if message.get(layout::TYPE) == Some(&layout::RTM_IFINFO2)
            && let Some(iface) = parse_ifinfo2(message)
        {
            ifaces.push(iface);
        }
    }
    ifaces
}

/// Pure. The index and the name of one `RTM_IFINFO2` message.
#[cfg(any(test, target_os = "macos"))]
fn parse_ifinfo2(message: &[u8]) -> Option<(u16, String)> {
    let addrs = i32::from_ne_bytes(read_array(message, layout::ADDRS)?);
    if addrs & layout::RTA_IFP == 0 {
        return None;
    }
    let index = read_u16(message, layout::INDEX)?;
    let sdl = message.get(layout::IF_MSGHDR2..)?;
    if sdl.get(layout::SDL_FAMILY) != Some(&layout::AF_LINK) {
        return None;
    }
    let sdl = sdl.get(..usize::from(*sdl.get(layout::SDL_LEN)?))?;
    let name_len = usize::from(*sdl.get(layout::SDL_NLEN)?);
    let name = sdl.get(layout::SDL_DATA..layout::SDL_DATA + name_len)?;
    if name.is_empty() {
        return None;
    }
    Some((index, String::from_utf8_lossy(name).into_owned()))
}

/// Pure. The (rx, tx) byte counters of an `ifmibdata` record.
#[cfg(any(test, target_os = "macos"))]
fn parse_ifmibdata(record: &[u8]) -> Option<(u64, u64)> {
    let at = |field: usize| read_array(record, layout::IFMD_DATA + field).map(u64::from_ne_bytes);
    Some((at(layout::IFI_IBYTES)?, at(layout::IFI_OBYTES)?))
}

#[cfg(any(test, target_os = "macos"))]
fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    read_array(bytes, at).map(u16::from_ne_bytes)
}

/// The `N` bytes at `at`, or `None` past the end.
#[cfg(any(test, target_os = "macos"))]
fn read_array<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

/// (name, rx bytes, tx bytes) for each entry in `root`. A counter that is missing or
/// does not parse reads as 0, as in sysinfo. A missing `root` gives an empty list.
#[cfg(any(test, target_os = "linux"))]
fn read_sysfs(root: &std::path::Path) -> Vec<(String, u64, u64)> {
    let counter = |dir: &std::path::Path, file: &str| {
        std::fs::read_to_string(dir.join(file))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let stats = entry.path().join("statistics");
            Some((
                name,
                counter(&stats, "rx_bytes"),
                counter(&stats, "tx_bytes"),
            ))
        })
        .collect()
}

/// Builds the list from refreshed `Networks`, sorted by name. Not on Linux or macOS (see
/// `SysfsNet` and `RouteNet`).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn collect(networks: &sysinfo::Networks, secs: f64) -> Vec<NetIface> {
    build(
        networks.list().iter().map(|(name, data)| {
            (
                name.as_str(),
                data.received(),
                data.transmitted(),
                data.total_received(),
                data.total_transmitted(),
            )
        }),
        secs,
    )
}

/// Pure. Sum of rx_rate and tx_rate.
pub fn totals(ifaces: &[NetIface]) -> (u64, u64) {
    ifaces.iter().fold((0, 0), |(rx, tx), i| {
        (rx.saturating_add(i.rx_rate), tx.saturating_add(i.tx_rate))
    })
}

/// Pure. Keeps, converts and sorts raw rows of
/// (name, received, transmitted, total received, total transmitted).
fn build<'a>(
    rows: impl IntoIterator<Item = (&'a str, u64, u64, u64, u64)>,
    secs: f64,
) -> Vec<NetIface> {
    let mut ifaces: Vec<NetIface> = rows
        .into_iter()
        .filter(|&(name, _, _, total_rx, total_tx)| keep_iface(name, total_rx, total_tx))
        .map(|(name, rx, tx, total_rx, total_tx)| iface(name, rx, tx, total_rx, total_tx, secs))
        .collect();
    crate::order::sort_by(&mut ifaces, |a, b| a.name.cmp(&b.name));
    ifaces
}

/// Pure. One interface from the bytes moved in the last `secs` seconds.
fn iface(
    name: &str,
    received: u64,
    transmitted: u64,
    total_rx: u64,
    total_tx: u64,
    secs: f64,
) -> NetIface {
    // `max` also maps NaN to the floor, so a bad clock cannot give a NaN rate.
    let secs = secs.max(0.001);
    NetIface {
        name: name.to_owned(),
        rx_rate: (received as f64 / secs) as u64,
        tx_rate: (transmitted as f64 / secs) as u64,
        total_rx,
        total_tx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, rx_rate: u64, tx_rate: u64) -> NetIface {
        NetIface {
            name: name.into(),
            rx_rate,
            tx_rate,
            ..NetIface::default()
        }
    }

    #[test]
    fn keep_iface_drops_loopback() {
        assert!(!keep_iface("lo", 500, 500));
        assert!(!keep_iface("lo0", 500, 500));
    }

    #[test]
    fn keep_iface_drops_interfaces_with_no_traffic_since_boot() {
        assert!(!keep_iface("en5", 0, 0));
    }

    #[test]
    fn keep_iface_keeps_an_interface_with_any_traffic() {
        assert!(keep_iface("en0", 1, 0));
        assert!(keep_iface("en0", 0, 1));
    }

    #[test]
    fn totals_sums_the_rates() {
        let ifaces = [
            named("en0", 100, 10),
            named("en1", 20, 2),
            named("wg0", 3, 300),
        ];
        assert_eq!(totals(&ifaces), (123, 312));
    }

    #[test]
    fn totals_of_nothing_is_zero() {
        assert_eq!(totals(&[]), (0, 0));
    }

    #[test]
    fn totals_saturate_instead_of_overflowing() {
        let ifaces = [named("a", u64::MAX, 1), named("b", 1, u64::MAX)];
        assert_eq!(totals(&ifaces), (u64::MAX, u64::MAX));
    }

    #[test]
    fn iface_divides_the_interval_bytes_by_the_seconds() {
        let got = iface("en0", 3_000, 1_500, 9_000, 4_000, 1.5);
        assert_eq!(
            got,
            NetIface {
                name: "en0".into(),
                rx_rate: 2_000,
                tx_rate: 1_000,
                total_rx: 9_000,
                total_tx: 4_000,
            }
        );
    }

    #[test]
    fn iface_floors_the_seconds_at_one_millisecond() {
        let zero = iface("en0", 5, 7, 5, 7, 0.0);
        assert_eq!((zero.rx_rate, zero.tx_rate), (5_000, 7_000));
        let negative = iface("en0", 5, 7, 5, 7, -2.0);
        assert_eq!((negative.rx_rate, negative.tx_rate), (5_000, 7_000));
        let nan = iface("en0", 5, 7, 5, 7, f64::NAN);
        assert_eq!((nan.rx_rate, nan.tx_rate), (5_000, 7_000));
    }

    #[test]
    fn build_sorts_by_name_and_drops_unwanted_interfaces() {
        let rows = [
            ("wlan0", 10, 0, 100, 50),
            ("lo", 999, 999, 999, 999),
            ("eth0", 20, 4, 200, 40),
            ("en5", 0, 0, 0, 0),
            ("docker0", 0, 0, 1, 0),
        ];
        let names: Vec<String> = build(rows, 2.0).into_iter().map(|i| i.name).collect();
        assert_eq!(names, ["docker0", "eth0", "wlan0"]);
    }

    #[test]
    fn build_computes_rates_with_the_given_seconds() {
        let got = build([("eth0", 20, 4, 200, 40)], 2.0);
        assert_eq!(got, [iface("eth0", 20, 4, 200, 40, 2.0)]);
        assert_eq!((got[0].rx_rate, got[0].tx_rate), (10, 2));
    }

    fn counters(rows: &[(&str, u64, u64)]) -> Vec<(String, u64, u64)> {
        rows.iter()
            .map(|&(name, rx, tx)| (name.to_owned(), rx, tx))
            .collect()
    }

    #[test]
    fn sysfs_first_sample_moves_no_bytes() {
        let mut net = Rates::default();
        let got = net.update(counters(&[("eth0", 5_000, 700)]), 1.0);
        assert_eq!(got, [iface("eth0", 0, 0, 5_000, 700, 1.0)]);
    }

    #[test]
    fn sysfs_rates_come_from_the_change_since_the_last_sample() {
        let mut net = Rates::default();
        net.update(counters(&[("eth0", 5_000, 700), ("wlan0", 10, 10)]), 1.0);
        let got = net.update(counters(&[("eth0", 9_000, 1_700), ("wlan0", 10, 10)]), 2.0);
        assert_eq!(
            got,
            [
                iface("eth0", 4_000, 1_000, 9_000, 1_700, 2.0),
                iface("wlan0", 0, 0, 10, 10, 2.0),
            ]
        );
        assert_eq!((got[0].rx_rate, got[0].tx_rate), (2_000, 500));
    }

    #[test]
    fn sysfs_counter_reset_gives_zero_not_a_wrap() {
        let mut net = Rates::default();
        net.update(counters(&[("eth0", 9_000, 9_000)]), 1.0);
        let got = net.update(counters(&[("eth0", 100, 50)]), 1.0);
        assert_eq!((got[0].rx_rate, got[0].tx_rate), (0, 0));
        assert_eq!((got[0].total_rx, got[0].total_tx), (100, 50));
    }

    #[test]
    fn sysfs_forgets_an_interface_that_went_away() {
        let mut net = Rates::default();
        net.update(counters(&[("usb0", 1_000, 1_000)]), 1.0);
        net.update(counters(&[]), 1.0);
        // usb0 comes back: its first sample again moves no bytes.
        let got = net.update(counters(&[("usb0", 3_000, 3_000)]), 1.0);
        assert_eq!((got[0].rx_rate, got[0].tx_rate), (0, 0));
    }

    #[test]
    fn sysfs_update_drops_loopback_and_sorts() {
        let mut net = Rates::default();
        let got = net.update(
            counters(&[("wlan0", 1, 1), ("lo", 9, 9), ("eth0", 1, 1)]),
            1.0,
        );
        let names: Vec<&str> = got.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["eth0", "wlan0"]);
    }

    #[test]
    fn read_sysfs_reads_each_interface_and_treats_a_bad_counter_as_zero() {
        let root = std::env::temp_dir().join(format!(
            "rtop-net-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        for (name, rx, tx) in [("eth0", "1234\n", "567\n"), ("odd0", "x\n", "")] {
            let stats = root.join(name).join("statistics");
            std::fs::create_dir_all(&stats).unwrap();
            std::fs::write(stats.join("rx_bytes"), rx).unwrap();
            std::fs::write(stats.join("tx_bytes"), tx).unwrap();
        }
        std::fs::create_dir_all(root.join("bare0")).unwrap();

        let mut got = read_sysfs(&root);
        got.sort();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            got,
            counters(&[("bare0", 0, 0), ("eth0", 1234, 567), ("odd0", 0, 0)])
        );
    }

    #[test]
    fn read_sysfs_of_a_missing_directory_is_empty() {
        assert!(read_sysfs(std::path::Path::new("/nonexistent/rtop/net")).is_empty());
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn collect_on_this_host_is_sorted_and_has_no_loopback() {
        let networks = sysinfo::Networks::new_with_refreshed_list();
        let ifaces = collect(&networks, 1.0);
        assert!(ifaces.windows(2).all(|w| w[0].name <= w[1].name));
        assert!(ifaces.iter().all(|i| !i.name.starts_with("lo")));
        assert!(ifaces.iter().all(|i| i.total_rx + i.total_tx > 0));
    }

    /// A synthetic `RTM_IFINFO2` message: the `if_msghdr2` header, then a `sockaddr_dl`
    /// with `name` and a 6-byte hardware address, padded to 4 bytes as the kernel does.
    fn ifinfo2(index: u16, name: &str) -> Vec<u8> {
        let sdl_len = (layout::SDL_DATA + name.len() + 6).max(20);
        let len = (layout::IF_MSGHDR2 + sdl_len).next_multiple_of(4);
        let mut message = vec![0u8; len];
        message[..2].copy_from_slice(&(len as u16).to_ne_bytes());
        message[2] = 5; // RTM_VERSION
        message[layout::TYPE] = layout::RTM_IFINFO2;
        message[layout::ADDRS..layout::ADDRS + 4].copy_from_slice(&layout::RTA_IFP.to_ne_bytes());
        message[layout::INDEX..layout::INDEX + 2].copy_from_slice(&index.to_ne_bytes());
        // A truncated counter in `ifm_data` (at 32), which the parser must not use.
        message[32 + layout::IFI_IBYTES..32 + layout::IFI_IBYTES + 8]
            .copy_from_slice(&1_024u64.to_ne_bytes());
        let sdl = &mut message[layout::IF_MSGHDR2..];
        sdl[layout::SDL_LEN] = sdl_len as u8;
        sdl[layout::SDL_FAMILY] = layout::AF_LINK;
        sdl[2..4].copy_from_slice(&index.to_ne_bytes());
        sdl[4] = 6; // IFT_ETHER
        sdl[layout::SDL_NLEN] = name.len() as u8;
        sdl[6] = 6; // sdl_alen
        let name_end = layout::SDL_DATA + name.len();
        sdl[layout::SDL_DATA..name_end].copy_from_slice(name.as_bytes());
        sdl[name_end..name_end + 6].copy_from_slice(&[0xa, 0xb, 0xc, 0xd, 0xe, 0xf]);
        message
    }

    /// A message of another type (an address), `len` bytes long.
    fn other_message(kind: u8, len: u16) -> Vec<u8> {
        let mut message = vec![0x55u8; usize::from(len)];
        message[..2].copy_from_slice(&len.to_ne_bytes());
        message[layout::TYPE] = kind;
        message
    }

    fn named_ifaces(rows: &[(u16, &str)]) -> Vec<(u16, String)> {
        rows.iter()
            .map(|&(index, name)| (index, name.to_owned()))
            .collect()
    }

    #[test]
    fn parse_iflist2_takes_each_interface_in_order_and_skips_other_messages() {
        let dump = [
            ifinfo2(1, "lo0"),
            other_message(0x0c, 60), // RTM_NEWADDR
            other_message(0x13, 68), // RTM_NEWMADDR2
            ifinfo2(13, "en0"),
            ifinfo2(11, "bridge0"),
            other_message(0x13, 56),
        ]
        .concat();
        assert_eq!(
            parse_iflist2(&dump),
            named_ifaces(&[(1, "lo0"), (13, "en0"), (11, "bridge0")])
        );
        assert!(parse_iflist2(&[]).is_empty());
    }

    #[test]
    fn parse_iflist2_reads_a_dump_at_any_alignment() {
        let dump = [
            ifinfo2(4, "en0"),
            other_message(0x0c, 61),
            ifinfo2(5, "utun3"),
        ]
        .concat();
        for shift in 0..8 {
            let mut shifted = vec![0u8; shift];
            shifted.extend_from_slice(&dump);
            assert_eq!(
                parse_iflist2(&shifted[shift..]),
                named_ifaces(&[(4, "en0"), (5, "utun3")]),
                "shift {shift}"
            );
        }
    }

    #[test]
    fn parse_iflist2_stops_at_a_bad_length() {
        let first = ifinfo2(1, "en0");
        let second = ifinfo2(2, "en1");
        let cut = [first.as_slice(), &second[..second.len() - 1]].concat();
        assert_eq!(
            parse_iflist2(&cut),
            named_ifaces(&[(1, "en0")]),
            "past the end"
        );
        for len in [0u16, 1, 3] {
            let mut bad = other_message(0x0c, 8);
            bad[..2].copy_from_slice(&len.to_ne_bytes());
            let dump = [first.as_slice(), &bad, &second].concat();
            assert_eq!(
                parse_iflist2(&dump),
                named_ifaces(&[(1, "en0")]),
                "length {len}"
            );
        }
        assert!(parse_iflist2(&[7]).is_empty(), "a lone byte");
    }

    #[test]
    fn parse_iflist2_skips_a_message_without_a_usable_name() {
        let good = ifinfo2(9, "en9");
        let mut no_ifp = ifinfo2(1, "en1");
        no_ifp[layout::ADDRS..layout::ADDRS + 4].copy_from_slice(&0i32.to_ne_bytes());
        let mut not_link = ifinfo2(2, "en2");
        not_link[layout::IF_MSGHDR2 + layout::SDL_FAMILY] = 2; // AF_INET
        let mut long_name = ifinfo2(3, "en3");
        long_name[layout::IF_MSGHDR2 + layout::SDL_NLEN] = 200;
        let mut no_name = ifinfo2(4, "en4");
        no_name[layout::IF_MSGHDR2 + layout::SDL_NLEN] = 0;
        // Only the header: the `sockaddr_dl` is missing.
        let mut short = ifinfo2(5, "en5");
        short.truncate(layout::IF_MSGHDR2);
        short[..2].copy_from_slice(&(layout::IF_MSGHDR2 as u16).to_ne_bytes());
        let dump = [no_ifp, not_link, long_name, no_name, short, good].concat();
        assert_eq!(parse_iflist2(&dump), named_ifaces(&[(9, "en9")]));
    }

    #[test]
    fn parse_ifmibdata_reads_the_64_bit_byte_counters() {
        let mut record = vec![0u8; 180];
        let rx_at = layout::IFMD_DATA + layout::IFI_IBYTES;
        let tx_at = layout::IFMD_DATA + layout::IFI_OBYTES;
        record[rx_at..rx_at + 8].copy_from_slice(&5_320_152_651u64.to_ne_bytes());
        record[tx_at..tx_at + 8].copy_from_slice(&100u64.to_ne_bytes());
        assert_eq!(parse_ifmibdata(&record), Some((5_320_152_651, 100)));
        assert_eq!(parse_ifmibdata(&record[..tx_at + 7]), None, "short record");
        assert_eq!(parse_ifmibdata(&[]), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn layout_matches_the_macos_structs() {
        use std::mem::{offset_of, size_of};
        assert_eq!(offset_of!(libc::if_msghdr2, ifm_msglen), layout::MSGLEN);
        assert_eq!(offset_of!(libc::if_msghdr2, ifm_type), layout::TYPE);
        assert_eq!(offset_of!(libc::if_msghdr2, ifm_addrs), layout::ADDRS);
        assert_eq!(offset_of!(libc::if_msghdr2, ifm_index), layout::INDEX);
        assert_eq!(size_of::<libc::if_msghdr2>(), layout::IF_MSGHDR2);
        assert_eq!(libc::RTM_IFINFO2, libc::c_int::from(layout::RTM_IFINFO2));
        assert_eq!(offset_of!(libc::sockaddr_dl, sdl_len), layout::SDL_LEN);
        assert_eq!(
            offset_of!(libc::sockaddr_dl, sdl_family),
            layout::SDL_FAMILY
        );
        assert_eq!(offset_of!(libc::sockaddr_dl, sdl_nlen), layout::SDL_NLEN);
        assert_eq!(offset_of!(libc::sockaddr_dl, sdl_data), layout::SDL_DATA);
        assert_eq!(libc::AF_LINK, libc::c_int::from(layout::AF_LINK));
        assert_eq!(offset_of!(libc::if_data64, ifi_ibytes), layout::IFI_IBYTES);
        assert_eq!(offset_of!(libc::if_data64, ifi_obytes), layout::IFI_OBYTES);
        assert_eq!(offset_of!(libc::ifmibdata, ifmd_data), layout::IFMD_DATA);
    }

    /// The (rx, tx) totals of `RouteNet::read`, by name.
    #[cfg(target_os = "macos")]
    fn route_totals(net: &mut RouteNet) -> std::collections::BTreeMap<String, (u64, u64)> {
        net.read()
            .into_iter()
            .map(|(name, rx, tx)| (name, (rx, tx)))
            .collect()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn route_net_gives_the_interfaces_and_totals_of_sysinfo() {
        for _ in 0..5 {
            let mut net = RouteNet::default();
            let before = route_totals(&mut net);
            let networks = sysinfo::Networks::new_with_refreshed_list();
            let after = route_totals(&mut net);
            if !before.keys().eq(after.keys()) {
                continue; // An interface came or went between the reads.
            }
            let theirs: std::collections::BTreeMap<String, (u64, u64)> = networks
                .list()
                .iter()
                .filter(|(name, _)| !name.starts_with("lo"))
                .map(|(name, data)| {
                    (
                        name.clone(),
                        (data.total_received(), data.total_transmitted()),
                    )
                })
                .collect();
            if !before.keys().eq(theirs.keys()) {
                continue;
            }
            for (name, &(rx, tx)) in &theirs {
                let (low, high) = (before[name], after[name]);
                // The counters only grow, so sysinfo's read lies between the two of ours.
                assert!(
                    (low.0..=high.0).contains(&rx),
                    "{name} rx {low:?} {rx} {high:?}"
                );
                assert!(
                    (low.1..=high.1).contains(&tx),
                    "{name} tx {low:?} {tx} {high:?}"
                );
            }
            return;
        }
        panic!("the interface list did not match sysinfo's in 5 tries");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn route_net_on_this_host_is_sorted_and_has_no_loopback() {
        let mut net = RouteNet::default();
        let first = net.collect(1.0);
        assert!(
            first.iter().all(|i| i.rx_rate == 0 && i.tx_rate == 0),
            "first sample"
        );
        let ifaces = net.collect(1.0);
        assert!(ifaces.windows(2).all(|w| w[0].name < w[1].name));
        assert!(ifaces.iter().all(|i| !i.name.starts_with("lo")));
        assert!(ifaces.iter().all(|i| i.total_rx + i.total_tx > 0));
    }
}
