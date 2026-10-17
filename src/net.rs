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

/// Linux reader for the byte counters in `/sys/class/net/<name>/statistics/`, the files
/// that sysinfo reads too. rtop does not use sysinfo's `Networks` on Linux: its `refresh`
/// also reads each interface address through `getifaddrs`, and sysinfo 0.39.6
/// (`network_helper.rs`, `sockaddr_to_network_addr`) copies a 128-byte `sockaddr_storage`
/// from each address and netmask. musl allocates a much smaller entry, so the copy can run
/// past it into an unmapped page and kill rtop with SIGSEGV.
#[cfg(any(test, target_os = "linux"))]
#[derive(Debug, Default)]
pub struct SysfsNet {
    /// (rx, tx) totals at the last sample, by interface name.
    last: std::collections::HashMap<String, (u64, u64)>,
}

#[cfg(any(test, target_os = "linux"))]
impl SysfsNet {
    /// Reads the counters and returns the list, sorted by name.
    #[cfg(target_os = "linux")]
    pub fn collect(&mut self, secs: f64) -> Vec<NetIface> {
        self.update(read_sysfs(std::path::Path::new("/sys/class/net")), secs)
    }

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

/// Builds the list from refreshed `Networks`, sorted by name. Not on Linux (see `SysfsNet`).
#[cfg(not(target_os = "linux"))]
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
        let mut net = SysfsNet::default();
        let got = net.update(counters(&[("eth0", 5_000, 700)]), 1.0);
        assert_eq!(got, [iface("eth0", 0, 0, 5_000, 700, 1.0)]);
    }

    #[test]
    fn sysfs_rates_come_from_the_change_since_the_last_sample() {
        let mut net = SysfsNet::default();
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
        let mut net = SysfsNet::default();
        net.update(counters(&[("eth0", 9_000, 9_000)]), 1.0);
        let got = net.update(counters(&[("eth0", 100, 50)]), 1.0);
        assert_eq!((got[0].rx_rate, got[0].tx_rate), (0, 0));
        assert_eq!((got[0].total_rx, got[0].total_tx), (100, 50));
    }

    #[test]
    fn sysfs_forgets_an_interface_that_went_away() {
        let mut net = SysfsNet::default();
        net.update(counters(&[("usb0", 1_000, 1_000)]), 1.0);
        net.update(counters(&[]), 1.0);
        // usb0 comes back: its first sample again moves no bytes.
        let got = net.update(counters(&[("usb0", 3_000, 3_000)]), 1.0);
        assert_eq!((got[0].rx_rate, got[0].tx_rate), (0, 0));
    }

    #[test]
    fn sysfs_update_drops_loopback_and_sorts() {
        let mut net = SysfsNet::default();
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

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn collect_on_this_host_is_sorted_and_has_no_loopback() {
        let networks = sysinfo::Networks::new_with_refreshed_list();
        let ifaces = collect(&networks, 1.0);
        assert!(ifaces.windows(2).all(|w| w[0].name <= w[1].name));
        assert!(ifaces.iter().all(|i| !i.name.starts_with("lo")));
        assert!(ifaces.iter().all(|i| i.total_rx + i.total_tx > 0));
    }
}
