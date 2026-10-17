//! Storage panel drawing.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{BLUE, DIM, FAINT, PINK, heat, panel};
use crate::format;
use crate::storage::{Storage, Volume};

pub const MIN_HEIGHT: u16 = 4;
pub const MIN_WIDTH: u16 = 30;
/// The most volume rows the panel asks for.
pub const MAX_ROWS: u16 = 6;

/// Inner width from which each row also shows its read and write rates.
const RATES_MIN_WIDTH: usize = 70;
/// One rate cell: ` ↓` and the widest `format::rate` text, `1023.9 PiB/s`.
const RATE_CELL: usize = 2 + 12;
/// The mount column is at most this wide. Longer mounts are cut from the left.
const MOUNT_MAX: usize = 20;
/// The used/total column shows only when the mount column can keep this many cells.
const MOUNT_MIN: usize = 10;
/// The meter shows only when it can be this many cells.
const METER_MIN: usize = 4;
/// ` 100%`.
const PCT_W: usize = 5;

/// 2 borders + 1 I/O line + min(volumes, MAX_ROWS).
pub fn want_height(storage: &Storage) -> u16 {
    let rows = storage.volumes.len().min(usize::from(MAX_ROWS));
    // `rows` is at most MAX_ROWS, so the cast cannot truncate.
    2 + 1 + rows as u16
}

pub fn draw_storage(frame: &mut Frame, area: Rect, storage: &Storage) {
    let inner = panel(String::new()).inner(area);
    let rows = usize::from(inner.height.saturating_sub(1)).min(storage.volumes.len());
    let block = panel(format!(" storage {rows}/{} ", storage.volumes.len()));
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let shown = &storage.volumes[..rows];
    let spaces: Vec<String> = shown.iter().map(space_text).collect();
    let columns = Columns::new(
        usize::from(inner.width),
        shown
            .iter()
            .map(|v| v.mount.chars().count())
            .max()
            .unwrap_or(0),
        spaces.iter().map(|s| s.chars().count()).max().unwrap_or(0),
    );

    let mut lines = vec![io_line(storage, usize::from(inner.width))];
    lines.extend(
        shown
            .iter()
            .zip(&spaces)
            .map(|(volume, space)| volume_line(volume, space, &columns)),
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `io ↓ {read} ↑ {write}`, or why there is no total. The `zfs n/a` note shows only when it fits `width`.
fn io_line(storage: &Storage, width: usize) -> Line<'static> {
    let has_zfs = storage.volumes.iter().any(|v| v.file_system == "zfs");
    if !storage.io_known {
        let text = if has_zfs { "io n/a (zfs)" } else { "io n/a" };
        return Line::from(Span::styled(text, DIM));
    }
    let mut spans = vec![
        Span::styled("io ", DIM),
        Span::styled("↓ ", BLUE),
        Span::raw(format::rate(storage.read_rate)),
        Span::styled("  ↑ ", PINK),
        Span::raw(format::rate(storage.write_rate)),
    ];
    let note = Span::styled(" · zfs n/a", DIM);
    let fits = spans.iter().map(Span::width).sum::<usize>() + note.width() <= width;
    if fits
        && storage
            .volumes
            .iter()
            .any(|v| v.file_system == "zfs" && !v.io_known)
    {
        spans.push(note);
    }
    Line::from(spans)
}

/// `{used}/{total}` of one volume.
fn space_text(volume: &Volume) -> String {
    let used = volume.total.saturating_sub(volume.available);
    format!("{}/{}", format::bytes(used), format::bytes(volume.total))
}

/// Cell widths of the volume rows. Every row uses the same widths, so the columns line up.
#[derive(Debug, PartialEq)]
struct Columns {
    mount: usize,
    /// 0 hides the meter and its separator.
    meter: usize,
    /// Width of the used/total text with its separator. 0 hides it.
    space: usize,
    rates: bool,
}

impl Columns {
    /// Drops parts in this order as the width shrinks: rates, then used/total, then the meter.
    fn new(width: usize, mount_len: usize, space_len: usize) -> Self {
        let rates = width >= RATES_MIN_WIDTH;
        let rest = width.saturating_sub(if rates { 2 * RATE_CELL } else { 0 });
        let want_mount = mount_len.clamp(1, MOUNT_MAX);
        let fixed = 1 + METER_MIN + PCT_W;
        let space = space_len + 1;

        let room = rest.saturating_sub(fixed + space);
        let (mount, space) = if room >= want_mount.min(MOUNT_MIN) {
            (want_mount.min(room), space)
        } else {
            (want_mount.min(rest.saturating_sub(fixed)), 0)
        };
        let meter = rest.saturating_sub(mount + 1 + PCT_W + space);
        if meter >= METER_MIN {
            return Self {
                mount,
                meter,
                space,
                rates,
            };
        }
        Self {
            mount: want_mount.min(rest.saturating_sub(PCT_W)),
            meter: 0,
            space: 0,
            rates,
        }
    }
}

fn volume_line(volume: &Volume, space: &str, columns: &Columns) -> Line<'static> {
    let used = volume.total.saturating_sub(volume.available);
    let pct = format::percent(used, volume.total);
    let mut spans = vec![Span::raw(truncate_left(&volume.mount, columns.mount))];
    if columns.meter > 0 {
        let bar = format::meter(pct / 100.0, columns.meter);
        let rest = columns.meter.saturating_sub(bar.chars().count());
        spans.extend([
            Span::raw(" "),
            Span::styled(bar, heat(pct)),
            Span::styled("·".repeat(rest), FAINT),
        ]);
    }
    spans.push(Span::raw(format!(" {pct:>3.0}%")));
    if columns.space > 0 {
        let width = columns.space;
        spans.push(Span::styled(format!("{space:>width$}"), DIM));
    }
    if columns.rates {
        if volume.io_known {
            let cell = RATE_CELL - 2;
            spans.extend([
                Span::styled(" ↓", BLUE),
                Span::raw(format!("{:>cell$}", format::rate(volume.read_rate))),
                Span::styled(" ↑", PINK),
                Span::raw(format!("{:>cell$}", format::rate(volume.write_rate))),
            ]);
        } else {
            let width = 2 * RATE_CELL;
            spans.push(Span::styled(format!("{:>width$}", "n/a"), DIM));
        }
    }
    Line::from(spans)
}

/// Cuts `text` from the left to `width` characters, with `…` in front when cut. Pads short text.
fn truncate_left(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return format!("{text:<width$}");
    }
    if width == 0 {
        return String::new();
    }
    let tail: String = text.chars().skip(len - (width - 1)).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Volume;

    const GIB: u64 = 1 << 30;

    fn ext4(mount: &str, device: &str) -> Volume {
        Volume {
            mount: mount.into(),
            device: device.into(),
            file_system: "ext4".into(),
            total: 100 * GIB,
            available: 40 * GIB,
            io_group: device.into(),
            io_known: true,
            read_rate: 2 << 20,
            write_rate: 512 << 10,
        }
    }

    fn zfs(mount: &str, dataset: &str) -> Volume {
        Volume {
            mount: mount.into(),
            device: dataset.into(),
            file_system: "zfs".into(),
            total: 900 * GIB,
            available: 300 * GIB,
            io_group: dataset.into(),
            io_known: false,
            read_rate: 0,
            write_rate: 0,
        }
    }

    fn storage(volumes: Vec<Volume>) -> Storage {
        let (read_rate, write_rate) = crate::storage::dedupe_rates(&volumes);
        Storage {
            io_known: volumes.iter().any(|v| v.io_known),
            volumes,
            read_rate,
            write_rate,
        }
    }

    fn render(width: u16, height: u16, storage: &Storage) -> String {
        super::super::render_to_string(width, height, |frame| {
            draw_storage(frame, frame.area(), storage)
        })
    }

    fn line_with<'a>(screen: &'a str, text: &str) -> &'a str {
        screen
            .lines()
            .find(|line| line.contains(text))
            .unwrap_or_else(|| panic!("no line with {text:?} in:\n{screen}"))
    }

    #[test]
    fn want_height_counts_borders_io_line_and_rows() {
        assert_eq!(want_height(&Storage::default()), 3);
        assert_eq!(want_height(&storage(vec![ext4("/", "sda1")])), 4);
        let eight = (0..8).map(|i| ext4(&format!("/m{i}"), "sda")).collect();
        assert_eq!(want_height(&storage(eight)), 2 + 1 + MAX_ROWS);
    }

    #[test]
    fn shows_each_volume_at_want_height() {
        let s = storage(vec![
            ext4("/", "sda1"),
            ext4("/home", "sda2"),
            ext4("/boot/efi", "sda3"),
        ]);
        let screen = render(60, want_height(&s), &s);
        for text in [
            "storage 3/3",
            "io",
            "↓ 6.0 MiB/s",
            "↑ 1.5 MiB/s",
            "/home",
            "/boot/efi",
            "%",
        ] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
        assert!(line_with(&screen, "/home").contains("60%"), "{screen}");
        assert!(
            line_with(&screen, "/home").contains("60.0 GiB/100.0 GiB"),
            "{screen}"
        );
        let root = screen.lines().nth(2).unwrap_or_default();
        assert!(root.contains("│/ "), "root row comes first:\n{screen}");
    }

    #[test]
    fn wide_panel_shows_rates_and_zfs_na() {
        let s = storage(vec![ext4("/", "sda1"), zfs("/tank", "tank")]);
        let screen = render(80, want_height(&s), &s);
        assert!(line_with(&screen, "/tank").contains("n/a"), "{screen}");
        assert!(line_with(&screen, "/tank").contains("67%"), "{screen}");
        assert!(line_with(&screen, "│/ ").contains("2.0 MiB/s"), "{screen}");
        assert!(line_with(&screen, "io ").contains("zfs n/a"), "{screen}");
    }

    #[test]
    fn zfs_note_shows_whole_or_not_at_all() {
        let s = storage(vec![ext4("/", "sda1"), zfs("/tank", "tank")]);
        let wide = render(60, want_height(&s), &s);
        assert!(line_with(&wide, "io ").contains("· zfs n/a"), "{wide}");
        let narrow = render(40, want_height(&s), &s);
        let io = line_with(&narrow, "io ");
        assert!(!io.contains("zfs"), "{narrow}");
        assert!(io.contains("↑ 512.0 KiB/s"), "{narrow}");
    }

    #[test]
    fn narrow_panel_has_no_rate_columns() {
        let s = storage(vec![ext4("/", "sda1"), zfs("/tank", "tank")]);
        let screen = render(60, want_height(&s), &s);
        assert!(!line_with(&screen, "/tank").contains("n/a"), "{screen}");
        assert!(!line_with(&screen, "│/ ").contains("MiB/s"), "{screen}");
    }

    #[test]
    fn only_zfs_volumes_show_io_na_zfs() {
        let s = storage(vec![zfs("/tank", "tank"), zfs("/backup", "backup")]);
        assert!(!s.io_known);
        let screen = render(60, want_height(&s), &s);
        assert!(screen.contains("io n/a (zfs)"), "{screen}");
        assert!(!screen.contains("↓"), "{screen}");
    }

    #[test]
    fn no_counter_without_zfs_shows_io_na() {
        let mut volume = ext4("/", "sda1");
        volume.io_known = false;
        let s = storage(vec![volume]);
        let screen = render(60, want_height(&s), &s);
        let io = line_with(&screen, "io ");
        assert!(io.contains("io n/a"), "{screen}");
        assert!(!io.contains("zfs"), "{screen}");
    }

    #[test]
    fn short_panel_counts_only_the_rows_that_fit() {
        let eight = (0..8).map(|i| ext4(&format!("/m{i}"), "sda")).collect();
        let s = storage(eight);
        let screen = render(40, 5, &s);
        assert!(screen.contains("storage 2/8"), "{screen}");
        assert!(screen.contains("/m0") && screen.contains("/m1"), "{screen}");
        assert!(!screen.contains("/m2"), "{screen}");
    }

    #[test]
    fn panel_at_80x24_width_keeps_the_long_mac_mount() {
        let s = storage(vec![
            ext4("/", "disk3"),
            ext4("/System/Volumes/Data", "disk3"),
        ]);
        let screen = render(40, want_height(&s), &s);
        assert!(screen.contains("/System/Volumes/Data"), "{screen}");
        assert!(line_with(&screen, "Data").contains("60%"), "{screen}");
    }

    #[test]
    fn long_mounts_are_cut_from_the_left() {
        let s = storage(vec![ext4("/very/long/path/to/a/mounted/volume", "sdb1")]);
        let screen = render(40, want_height(&s), &s);
        assert!(screen.contains("…"), "{screen}");
        assert!(screen.contains("volume"), "{screen}");
    }

    #[test]
    fn truncate_left_keeps_the_end() {
        assert_eq!(truncate_left("/a/b/c", 4), "…b/c");
        assert_eq!(truncate_left("/a", 4), "/a  ");
        assert_eq!(truncate_left("/abc", 4), "/abc");
        assert_eq!(truncate_left("/abc", 1), "…");
        assert_eq!(truncate_left("/abc", 0), "");
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let eight = (0..8).map(|i| ext4(&format!("/m{i}"), "sda")).collect();
        let s = storage(eight);
        for (w, h) in [
            (1, 1),
            (200, 2),
            (10, 3),
            (3, 10),
            (30, 4),
            (69, 9),
            (70, 9),
        ] {
            render(w, h, &s);
            render(w, h, &Storage::default());
        }
        super::super::render_to_string(5, 5, |frame| {
            draw_storage(frame, Rect::new(2, 2, 0, 0), &s);
            draw_storage(frame, Rect::new(0, 0, 5, 0), &s);
        });
    }
}
