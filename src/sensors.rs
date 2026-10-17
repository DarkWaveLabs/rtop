//! Temperatures from `sysinfo::Components` and battery state from each OS.

use sysinfo::Components;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Temp {
    pub label: String,
    pub celsius: f32,
    pub critical: Option<f32>,
}

#[cfg_attr(
    target_os = "freebsd",
    allow(dead_code, reason = "no FreeBSD battery reader")
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatteryState {
    Charging,
    Discharging,
    Full,
    NotCharging,
    #[default]
    Unknown,
}

#[cfg_attr(
    target_os = "freebsd",
    allow(dead_code, reason = "no FreeBSD battery reader")
)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Battery {
    /// 0.0 to 100.0.
    pub percent: f32,
    pub state: BatteryState,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sensors {
    /// Hottest first; ties by label. Never holds NaN.
    pub temps: Vec<Temp>,
    pub battery: Option<Battery>,
}

impl Sensors {
    pub fn is_empty(&self) -> bool {
        self.temps.is_empty() && self.battery.is_none()
    }
}

/// Owned by the sampler.
pub struct SensorCollector {
    components: Components,
    last: Sensors,
}

impl SensorCollector {
    pub fn new() -> Self {
        Self {
            components: Components::new_with_refreshed_list(),
            last: Sensors::default(),
        }
    }

    /// Refreshes only when `slow` is true. Otherwise returns a clone of the last value.
    pub fn collect(&mut self, slow: bool) -> Sensors {
        if slow {
            self.components.refresh(true);
            let raw = self
                .components
                .list()
                .iter()
                .filter_map(|c| {
                    Some(Temp {
                        label: c.label().to_owned(),
                        celsius: c.temperature()?,
                        critical: c.critical(),
                    })
                })
                .collect();
            self.last = Sensors {
                temps: tidy_temps(raw),
                battery: platform::read_battery(),
            };
        }
        self.last.clone()
    }
}

/// Pure. Drops NaN and non-finite values, rounds to 0.1 °C, sorts hottest first, ties by label.
pub fn tidy_temps(raw: Vec<Temp>) -> Vec<Temp> {
    let round = |c: f32| (c * 10.0).round() / 10.0;
    let mut temps: Vec<Temp> = raw
        .into_iter()
        .filter(|t| t.celsius.is_finite())
        .map(|t| Temp {
            celsius: round(t.celsius),
            // A critical value of zero or below would make every reading look hot.
            critical: t.critical.filter(|c| c.is_finite() && *c > 0.0),
            label: t.label,
        })
        .collect();
    crate::order::sort_by(&mut temps, |a, b| {
        b.celsius
            .total_cmp(&a.celsius)
            .then_with(|| a.label.cmp(&b.label))
    });
    temps
}

/// Pure. One Linux `/sys/class/power_supply/<name>/` entry.
/// `kind` is the `type` file, `scope` the `scope` file, `present` the `present` file.
/// Returns `None` unless kind is "Battery", scope is not "Device", and present is not "0".
#[cfg(any(test, target_os = "linux"))]
pub fn parse_linux_supply(
    kind: &str,
    status: &str,
    capacity: &str,
    present: Option<&str>,
    scope: Option<&str>,
) -> Option<Battery> {
    if kind.trim() != "Battery"
        || scope.is_some_and(|s| s.trim() == "Device")
        || present.is_some_and(|p| p.trim() == "0")
    {
        return None;
    }
    let percent: f32 = capacity.trim().parse().ok()?;
    if !percent.is_finite() {
        return None;
    }
    let state = match status.trim() {
        "Charging" => BatteryState::Charging,
        "Discharging" => BatteryState::Discharging,
        "Full" => BatteryState::Full,
        "Not charging" => BatteryState::NotCharging,
        _ => BatteryState::Unknown,
    };
    Some(Battery {
        percent: percent.clamp(0.0, 100.0),
        state,
    })
}

/// Reads one Linux power supply entry through `read` (file name → contents).
/// The identity files come first, and an entry that cannot count is dropped before
/// `status` or `capacity` is read: most entries are mains adapters or USB ports, and for a
/// HID battery (scope `Device`: a mouse or keyboard) the kernel answers those two with a
/// blocking query to the device that can take seconds.
#[cfg(any(test, target_os = "linux"))]
fn read_supply_with(mut read: impl FnMut(&str) -> Option<String>) -> Option<Battery> {
    let kind = read("type")?;
    if kind.trim() != "Battery" {
        return None;
    }
    let scope = read("scope");
    let present = read("present");
    if scope.as_deref().is_some_and(|s| s.trim() == "Device")
        || present.as_deref().is_some_and(|p| p.trim() == "0")
    {
        return None;
    }
    let status = read("status").unwrap_or_default();
    let capacity = read("capacity")?;
    parse_linux_supply(
        &kind,
        &status,
        &capacity,
        present.as_deref(),
        scope.as_deref(),
    )
}

/// Pure. Joins several system batteries: mean percent; Charging if any charges,
/// else Discharging if any discharges, else Full if all are full, else NotCharging if any is
/// NotCharging, else Unknown.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub fn combine(batteries: &[Battery]) -> Option<Battery> {
    if batteries.is_empty() {
        return None;
    }
    let percent = batteries.iter().map(|b| b.percent).sum::<f32>() / batteries.len() as f32;
    let any = |state| batteries.iter().any(|b| b.state == state);
    let state = if any(BatteryState::Charging) {
        BatteryState::Charging
    } else if any(BatteryState::Discharging) {
        BatteryState::Discharging
    } else if batteries.iter().all(|b| b.state == BatteryState::Full) {
        BatteryState::Full
    } else if any(BatteryState::NotCharging) {
        BatteryState::NotCharging
    } else {
        BatteryState::Unknown
    };
    Some(Battery { percent, state })
}

/// Pure. Windows `SYSTEM_POWER_STATUS` fields.
/// flag & 128 → None. percent 255 → None. flag & 8 → Charging.
/// ac 1 → Full if percent is 100, else NotCharging. ac 0 → Discharging. Else (ac 255) Unknown.
#[cfg(any(test, windows))]
pub fn from_power_status(ac_line: u8, battery_flag: u8, life_percent: u8) -> Option<Battery> {
    const NO_SYSTEM_BATTERY: u8 = 128;
    const CHARGING: u8 = 8;
    const UNKNOWN_PERCENT: u8 = 255;
    if battery_flag & NO_SYSTEM_BATTERY != 0 || life_percent == UNKNOWN_PERCENT {
        return None;
    }
    let state = if battery_flag & CHARGING != 0 {
        BatteryState::Charging
    } else {
        match ac_line {
            1 if life_percent >= 100 => BatteryState::Full,
            1 => BatteryState::NotCharging,
            0 => BatteryState::Discharging,
            _ => BatteryState::Unknown,
        }
    };
    Some(Battery {
        percent: f32::from(life_percent.min(100)),
        state,
    })
}

/// Pure. macOS power source values.
/// `max <= 0` → None. percent = 100 × current / max, clamped to 0..=100.
/// `is_charging` → Charging. Else `state` "Battery Power" → Discharging.
/// Else `state` "AC Power": Full if current >= max, else NotCharging. Else Unknown.
#[cfg(any(test, target_os = "macos"))]
pub fn from_macos(current: i64, max: i64, is_charging: bool, state: &str) -> Option<Battery> {
    if max <= 0 {
        return None;
    }
    let percent = (current as f64 * 100.0 / max as f64).clamp(0.0, 100.0) as f32;
    let state = if is_charging {
        BatteryState::Charging
    } else {
        match state {
            "Battery Power" => BatteryState::Discharging,
            "AC Power" if current >= max => BatteryState::Full,
            "AC Power" => BatteryState::NotCharging,
            _ => BatteryState::Unknown,
        }
    };
    Some(Battery { percent, state })
}

/// Battery readers, one for each OS.
#[cfg(target_os = "linux")]
mod platform {
    use super::{Battery, combine, read_supply_with};
    use std::fs;
    use std::path::Path;

    const SUPPLY_DIR: &str = "/sys/class/power_supply";

    pub(super) fn read_battery() -> Option<Battery> {
        let batteries: Vec<Battery> = fs::read_dir(SUPPLY_DIR)
            .ok()?
            .flatten()
            .filter_map(|entry| read_supply(&entry.path()))
            .collect();
        combine(&batteries)
    }

    fn read_supply(dir: &Path) -> Option<Battery> {
        read_supply_with(|name| fs::read_to_string(dir.join(name)).ok())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Battery, combine, from_macos};
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFString, CFType};
    use objc2_io_kit::{
        IOPSCopyPowerSourcesInfo, IOPSCopyPowerSourcesList, IOPSGetPowerSourceDescription,
        kIOPSCurrentCapacityKey, kIOPSInternalBatteryType, kIOPSIsChargingKey, kIOPSMaxCapacityKey,
        kIOPSPowerSourceStateKey, kIOPSTypeKey,
    };
    use std::ffi::CStr;

    pub(super) fn read_battery() -> Option<Battery> {
        let info = IOPSCopyPowerSourcesInfo()?;
        // SAFETY: `info` is the blob that `IOPSCopyPowerSourcesInfo` returned, which is the
        // type this function takes. It stays alive (retained) for the whole block.
        let list = unsafe { IOPSCopyPowerSourcesList(Some(&info)) }?;
        // SAFETY: Apple documents the list as a CFArray of CFTypeRefs; every element is a CF
        // object, so viewing it as `CFArray<CFType>` is valid. The array is not mutated.
        let sources = unsafe { list.cast_unchecked::<CFType>() };
        let batteries: Vec<Battery> = sources
            .iter()
            .filter_map(|source| {
                // SAFETY: `info` is the blob from `IOPSCopyPowerSourcesInfo` and `source` is an
                // element of the list made from it, as the function requires. The binding
                // retains the returned dictionary, so it stays valid while it is read.
                let desc = unsafe { IOPSGetPowerSourceDescription(Some(&info), Some(&source)) }?;
                // SAFETY: the description dictionary has CFString keys (IOPSKeys.h) and CF
                // object values. Each value is checked with `downcast_ref` before use.
                let desc = unsafe { desc.cast_unchecked::<CFString, CFType>() };
                read_source(desc)
            })
            .collect();
        combine(&batteries)
    }

    fn read_source(desc: &CFDictionary<CFString, CFType>) -> Option<Battery> {
        // The `kIOPS*Key` constants are C strings, but the dictionary keys are CFStrings.
        let get = |key: &'static CStr| desc.get(&CFString::from_static_str(key.to_str().ok()?));
        let text = |key: &'static CStr| {
            get(key).and_then(|v| v.downcast_ref::<CFString>().map(ToString::to_string))
        };
        let number =
            |key: &'static CStr| get(key).and_then(|v| v.downcast_ref::<CFNumber>()?.as_i64());

        let kind = text(kIOPSTypeKey)?;
        if kind != kIOPSInternalBatteryType.to_str().ok()? {
            return None;
        }
        let current = number(kIOPSCurrentCapacityKey)?;
        let max = number(kIOPSMaxCapacityKey)?;
        let charging = get(kIOPSIsChargingKey)
            .and_then(|v| v.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool))
            .unwrap_or(false);
        let state = text(kIOPSPowerSourceStateKey).unwrap_or_default();
        from_macos(current, max, charging, &state)
    }
}

#[cfg(windows)]
mod platform {
    use super::{Battery, from_power_status};
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    pub(super) fn read_battery() -> Option<Battery> {
        let mut status = SYSTEM_POWER_STATUS::default();
        // SAFETY: `status` is a valid, writable `SYSTEM_POWER_STATUS` that lives for the call.
        unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
        from_power_status(
            status.ACLineStatus,
            status.BatteryFlag,
            status.BatteryLifePercent,
        )
    }
}

/// FreeBSD and other targets: no battery reader in this round.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use super::Battery;

    pub(super) fn read_battery() -> Option<Battery> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BatteryState::*;

    /// Runs `read_supply_with` over `files` and returns its result and the files it read.
    fn read_files(files: &[(&str, &str)]) -> (Option<Battery>, Vec<String>) {
        let mut opened = Vec::new();
        let battery = read_supply_with(|name| {
            opened.push(name.to_owned());
            files
                .iter()
                .find(|(file, _)| *file == name)
                .map(|(_, text)| (*text).to_owned())
        });
        (battery, opened)
    }

    #[test]
    fn device_batteries_are_dropped_before_status_or_capacity_is_read() {
        // A HID battery (mouse, keyboard) answers `status` and `capacity` with a blocking
        // query to the device, which can stall the sampler for seconds.
        for (scope, present) in [("Device\n", "1\n"), ("System\n", "0\n")] {
            let (battery, opened) = read_files(&[
                ("type", "Battery\n"),
                ("scope", scope),
                ("present", present),
                ("status", "Discharging\n"),
                ("capacity", "40\n"),
            ]);
            assert_eq!(battery, None, "{scope:?} {present:?}");
            assert!(
                !opened.iter().any(|f| f == "status" || f == "capacity"),
                "{scope:?} {present:?}: read {opened:?}"
            );
        }
    }

    #[test]
    fn system_battery_reads_through_the_helper() {
        let (battery, _) = read_files(&[
            ("type", "Battery\n"),
            ("scope", "System\n"),
            ("present", "1\n"),
            ("status", "Charging\n"),
            ("capacity", "77\n"),
        ]);
        assert_eq!(battery, Some(bat(77.0, Charging)));
        // Old kernels have no `scope` or `present` file.
        let (battery, _) = read_files(&[
            ("type", "Battery\n"),
            ("status", "Discharging\n"),
            ("capacity", "12\n"),
        ]);
        assert_eq!(battery, Some(bat(12.0, Discharging)));
        let (battery, opened) = read_files(&[("type", "Mains\n"), ("online", "1\n")]);
        assert_eq!(battery, None);
        assert_eq!(opened, ["type"], "a mains adapter costs one read");
    }

    fn temp(label: &str, celsius: f32) -> Temp {
        Temp {
            label: label.into(),
            celsius,
            critical: None,
        }
    }

    fn bat(percent: f32, state: BatteryState) -> Battery {
        Battery { percent, state }
    }

    #[test]
    fn tidy_temps_drops_values_that_are_not_finite() {
        let tidy = tidy_temps(vec![
            temp("nan", f32::NAN),
            temp("inf", f32::INFINITY),
            temp("neg", f32::NEG_INFINITY),
            temp("ok", 40.0),
        ]);
        assert_eq!(tidy, vec![temp("ok", 40.0)]);
    }

    #[test]
    fn tidy_temps_sorts_hottest_first_with_ties_by_label() {
        let tidy = tidy_temps(vec![temp("b", 40.0), temp("a", 70.0), temp("c", 70.0)]);
        let labels: Vec<&str> = tidy.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["a", "c", "b"]);
    }

    #[test]
    fn tidy_temps_rounds_to_one_decimal() {
        let tidy = tidy_temps(vec![temp("x", 41.26)]);
        assert_eq!(tidy[0].celsius, 41.3);
    }

    #[test]
    fn tidy_temps_drops_a_critical_value_that_is_not_usable() {
        let mut nan = temp("nan", 50.0);
        nan.critical = Some(f32::NAN);
        let mut zero = temp("zero", 50.0);
        zero.critical = Some(0.0);
        let mut good = temp("good", 50.0);
        good.critical = Some(100.0);
        let tidy = tidy_temps(vec![nan, zero, good]);
        let criticals: Vec<Option<f32>> = tidy.iter().map(|t| t.critical).collect();
        assert_eq!(criticals, [Some(100.0), None, None]);
    }

    #[test]
    fn linux_supply_reads_a_charging_battery() {
        assert_eq!(
            parse_linux_supply("Battery", "Charging\n", "87\n", None, None),
            Some(bat(87.0, Charging))
        );
    }

    #[test]
    fn linux_supply_reads_a_battery_that_is_not_charging() {
        assert_eq!(
            parse_linux_supply("Battery", "Not charging", "80", Some("1"), Some("System")),
            Some(bat(80.0, NotCharging))
        );
    }

    #[test]
    fn linux_supply_ignores_entries_that_are_not_system_batteries() {
        assert_eq!(
            parse_linux_supply("Mains", "Charging", "87", None, None),
            None
        );
        assert_eq!(
            parse_linux_supply("Battery", "Discharging", "50", None, Some("Device")),
            None,
            "a mouse or keyboard battery"
        );
        assert_eq!(
            parse_linux_supply("Battery", "Discharging", "50", Some("0"), None),
            None,
            "an empty battery bay"
        );
        assert_eq!(
            parse_linux_supply("Battery", "Discharging", "abc", None, None),
            None
        );
    }

    #[test]
    fn linux_supply_maps_each_status_string() {
        for (status, state) in [
            ("Full", Full),
            ("Discharging", Discharging),
            ("Unknown", Unknown),
            ("Not charging\r\n", NotCharging),
            ("Charging", Charging),
            ("something new", Unknown),
        ] {
            let battery = parse_linux_supply("Battery", status, "50", None, None);
            assert_eq!(battery.map(|b| b.state), Some(state), "{status:?}");
        }
    }

    #[test]
    fn linux_supply_clamps_capacity() {
        let battery = parse_linux_supply("Battery", "Full", "104", None, None);
        assert_eq!(battery.map(|b| b.percent), Some(100.0));
    }

    #[test]
    fn combine_joins_batteries() {
        assert_eq!(combine(&[]), None);
        assert_eq!(
            combine(&[bat(80.0, Discharging), bat(60.0, Charging)]),
            Some(bat(70.0, Charging))
        );
        assert_eq!(
            combine(&[bat(100.0, Full), bat(100.0, Full)]),
            Some(bat(100.0, Full))
        );
        assert_eq!(
            combine(&[bat(90.0, NotCharging), bat(100.0, Full)]).map(|b| b.state),
            Some(NotCharging)
        );
        assert_eq!(combine(&[bat(50.0, Unknown)]), Some(bat(50.0, Unknown)));
    }

    #[test]
    fn combine_prefers_discharging_over_full() {
        assert_eq!(
            combine(&[bat(100.0, Full), bat(40.0, Discharging)]).map(|b| b.state),
            Some(Discharging)
        );
    }

    #[test]
    fn windows_power_status_maps_each_case() {
        assert_eq!(from_power_status(0, 1, 55), Some(bat(55.0, Discharging)));
        assert_eq!(from_power_status(1, 8, 40).map(|b| b.state), Some(Charging));
        assert_eq!(from_power_status(1, 1, 100), Some(bat(100.0, Full)));
        assert_eq!(
            from_power_status(1, 1, 90).map(|b| b.state),
            Some(NotCharging)
        );
        assert_eq!(
            from_power_status(255, 1, 50).map(|b| b.state),
            Some(Unknown)
        );
        assert_eq!(from_power_status(1, 128, 255), None, "no system battery");
        assert_eq!(from_power_status(0, 1, 255), None, "unknown percent");
    }

    #[test]
    fn macos_power_source_maps_each_case() {
        assert_eq!(
            from_macos(4000, 5000, false, "Battery Power"),
            Some(bat(80.0, Discharging))
        );
        assert_eq!(
            from_macos(5000, 5000, false, "AC Power").map(|b| b.state),
            Some(Full)
        );
        assert_eq!(
            from_macos(4500, 5000, false, "AC Power"),
            Some(bat(90.0, NotCharging))
        );
        assert_eq!(
            from_macos(4500, 5000, true, "AC Power").map(|b| b.state),
            Some(Charging)
        );
        assert_eq!(
            from_macos(4500, 5000, false, "UPS Power").map(|b| b.state),
            Some(Unknown)
        );
        assert_eq!(from_macos(10, 0, false, "AC Power"), None);
        assert_eq!(
            from_macos(6000, 5000, false, "AC Power"),
            Some(bat(100.0, Full))
        );
    }

    #[test]
    fn default_sensors_are_empty() {
        assert!(Sensors::default().is_empty());
        let with_temp = Sensors {
            temps: vec![temp("cpu", 50.0)],
            battery: None,
        };
        assert!(!with_temp.is_empty());
        let with_battery = Sensors {
            temps: Vec::new(),
            battery: Some(bat(50.0, Full)),
        };
        assert!(!with_battery.is_empty());
    }

    #[test]
    fn collector_returns_the_last_value_on_fast_ticks() {
        let mut collector = SensorCollector::new();
        let first = collector.collect(true);
        assert_eq!(collector.collect(false), first);
        assert!(first.temps.iter().all(|t| t.celsius.is_finite()));
    }
}
