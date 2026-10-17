//! Small, pure helpers that turn raw numbers into display text.

/// Formats a byte count with binary units, for example `1.5 GiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = n as f64;
    let mut unit = 0;
    // Move up a unit at 1023.95, the point where one decimal place would print "1024.0".
    while value >= 1023.95 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Formats a transfer rate in bytes per second.
pub fn rate(bytes_per_sec: u64) -> String {
    format!("{}/s", bytes(bytes_per_sec))
}

/// Formats an uptime in seconds as `3d 04h 12m`, `4h 12m` or `12m 05s`.
pub fn duration(secs: u64) -> String {
    let (days, hours, mins, secs) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    match (days, hours) {
        (0, 0) => format!("{mins}m {secs:02}s"),
        (0, _) => format!("{hours}h {mins:02}m"),
        _ => format!("{days}d {hours:02}h {mins:02}m"),
    }
}

/// `part` as a percentage of `whole`; zero when `whole` is zero.
pub fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

/// Draws `ratio` (0.0 to 1.0) as a bar up to `width` cells wide.
///
/// Eighth-block glyphs give each cell eight steps, so a narrow bar still moves smoothly.
pub fn meter(ratio: f64, width: usize) -> String {
    const PARTIAL: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    // `as usize` saturates and maps NaN to 0, so bad input cannot overflow the bar.
    let eighths = (ratio.clamp(0.0, 1.0) * (width * 8) as f64).round() as usize;
    let mut bar = "█".repeat(eighths / 8);
    if !eighths.is_multiple_of(8) {
        bar.push(PARTIAL[eighths % 8]);
    }
    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_picks_the_largest_whole_unit() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536 * 1024 * 1024), "1.5 GiB");
        assert_eq!(
            bytes((1 << 30) - 4096),
            "1.0 GiB",
            "never prints 1024.0 MiB"
        );
        assert_eq!(rate(2048), "2.0 KiB/s");
    }

    #[test]
    fn duration_drops_leading_zero_units() {
        assert_eq!(duration(65), "1m 05s");
        assert_eq!(duration(3 * 3_600 + 7 * 60), "3h 07m");
        assert_eq!(duration(2 * 86_400 + 5 * 3_600 + 60), "2d 05h 01m");
    }

    #[test]
    fn percent_handles_zero_total() {
        assert_eq!(percent(5, 0), 0.0);
        assert_eq!(percent(1, 4), 25.0);
    }

    #[test]
    fn meter_uses_partial_blocks() {
        assert_eq!(meter(0.0, 5), "");
        assert_eq!(meter(0.5, 4), "██");
        assert_eq!(meter(1.0, 3), "███");
        assert_eq!(meter(5.0 / 16.0, 2), "▋", "5 of 16 eighths is a 5/8 cell");
        assert_eq!(meter(9.0 / 16.0, 2), "█▏");
        assert_eq!(meter(2.0, 2), "██", "ratio above 1 is clamped");
        assert_eq!(meter(f64::NAN, 4), "", "NaN draws nothing");
    }
}
