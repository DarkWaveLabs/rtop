//! ZFS ARC panel drawing.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::symbols::line::THICK_HORIZONTAL;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{BLUE, DIM, FAINT, PINK, VIOLET, panel};
use crate::format::bytes;
use crate::zfs::ZfsArc;

pub const MIN_HEIGHT: u16 = 4;
pub const MIN_WIDTH: u16 = 36;

/// Space between two parts of one content line.
const GAP: &str = "  ";
/// The narrowest gauge bar that is worth a place next to `min … max`.
const MIN_GAUGE_BAR: usize = 8;

/// Borders plus one line for each row of content that `arc` has.
pub fn want_height(arc: &ZfsArc) -> u16 {
    2 + 4 + u16::from(arc.l2.is_some())
}

pub fn draw_zfs(frame: &mut Frame, area: Rect, arc: &ZfsArc) {
    let block = panel(format!(" zfs arc {} ", bytes(arc.size)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = usize::from(inner.width);
    let mut lines = vec![
        size_line(arc, width),
        hit_line(arc, width),
        mfu_mru_line(arc, width),
        data_line(arc, width),
    ];
    if let Some(l2) = &arc.l2 {
        let mut hit = boot_prefix(l2.since_boot);
        hit.extend(label_value("hit ", pct(l2.hit_pct)));
        let parts = vec![
            vec![
                Span::styled("l2 ", DIM),
                Span::raw(format!("{} ({})", bytes(l2.size), bytes(l2.asize))),
            ],
            hit,
        ];
        lines.push(fit(parts, &[1], width));
    }
    lines.truncate(usize::from(inner.height));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `size … / target …`, a gauge of size / max, and `min … max …` when the width permits.
fn size_line(arc: &ZfsArc, width: usize) -> Line<'static> {
    let label = vec![
        Span::styled("size ", DIM),
        Span::raw(bytes(arc.size)),
        Span::styled(" / target ", DIM),
        Span::raw(bytes(arc.target)),
    ];
    let limits = Span::styled(
        format!("min {} max {}", bytes(arc.min), bytes(arc.max)),
        DIM,
    );
    let label_w = spans_width(&label);
    let ratio = if arc.max == 0 {
        0.0
    } else {
        arc.size as f64 / arc.max as f64
    };

    if width >= label_w + 1 + MIN_GAUGE_BAR + GAP.len() + limits.width() {
        let bar_w = width - label_w - 1 - GAP.len() - limits.width();
        let mut spans = label;
        spans.push(Span::raw(" "));
        spans.extend(gauge(ratio, bar_w));
        spans.extend([Span::raw(GAP), limits]);
        return Line::from(spans);
    }
    let label = if width >= label_w {
        label
    } else {
        vec![Span::styled("size ", DIM), Span::raw(bytes(arc.size))]
    };
    let label_w = spans_width(&label);
    if width < label_w {
        return Line::default();
    }
    let mut spans = label;
    if width >= label_w + 2 {
        spans.push(Span::raw(" "));
        spans.extend(gauge(ratio, width - label_w - 1));
    }
    Line::from(spans)
}

/// `hit … io … miss … dd … dm …`. Drops `dm`, `dd`, `miss`, `io` in that order to fit.
fn hit_line(arc: &ZfsArc, width: usize) -> Line<'static> {
    let mut hit = boot_prefix(arc.hit_since_boot);
    hit.extend(label_value("hit ", pct(arc.hit_pct)));
    let miss = arc
        .hit_pct
        .map(|h| (100.0 - h - arc.iohit_pct.unwrap_or_default()).max(0.0));
    let io = arc.iohit_pct.map(|io| label_value("io ", pct(Some(io))));

    // Indices: 0 hit, 1 io (empty when absent), 2 miss, 3 dd, 4 dm.
    let parts = vec![
        hit,
        io.unwrap_or_default(),
        label_value("miss ", pct(miss)),
        label_value("dd ", pct(arc.demand_data_pct)),
        label_value("dm ", pct(arc.demand_meta_pct)),
    ];
    fit(parts, &[4, 3, 2, 1], width)
}

/// `mfu …  mru …` and a split bar: MFU in violet, MRU in blue.
fn mfu_mru_line(arc: &ZfsArc, width: usize) -> Line<'static> {
    let parts = vec![
        label_value("mfu ", bytes(arc.mfu)),
        label_value("mru ", bytes(arc.mru)),
    ];
    let mut line = fit(parts, &[1], width);
    let used = line.width();
    if used > 0 && width >= used + 1 + 3 {
        let bar_w = width - used - 1;
        let total = arc.mfu.saturating_add(arc.mru);
        let mfu_w = if total == 0 {
            0
        } else {
            ((arc.mfu as f64 / total as f64) * bar_w as f64).round() as usize
        };
        let (mfu_colour, mru_colour) = if total == 0 {
            (FAINT, FAINT)
        } else {
            (VIOLET, BLUE)
        };
        line.spans.push(Span::raw(" "));
        line.spans.push(bar(mfu_w, mfu_colour));
        line.spans.push(bar(bar_w - mfu_w.min(bar_w), mru_colour));
    }
    line
}

/// `data …  meta …  comp ….××`. Drops `comp` when the ratio is unknown.
fn data_line(arc: &ZfsArc, width: usize) -> Line<'static> {
    let mut parts = vec![
        label_value("data ", bytes(arc.data)),
        label_value("meta ", bytes(arc.metadata)),
    ];
    if let Some(ratio) = arc.compress_ratio {
        parts.push(label_value("comp ", format!("{ratio:.2}×")));
    }
    fit(parts, &[2, 1], width)
}

/// Joins the non-empty parts with `GAP`. While the line is wider than `width`, it removes the
/// part at the next index of `drop_order`, and at the end any part. A part is never cut.
fn fit(mut parts: Vec<Vec<Span<'static>>>, drop_order: &[usize], width: usize) -> Line<'static> {
    let joined_width = |parts: &[Vec<Span>]| {
        let shown: Vec<usize> = parts
            .iter()
            .map(|p| spans_width(p))
            .filter(|&w| w > 0)
            .collect();
        shown.iter().sum::<usize>() + GAP.len() * shown.len().saturating_sub(1)
    };
    let mut order = drop_order.iter().copied().chain((0..parts.len()).rev());
    while joined_width(&parts) > width {
        let Some(i) = order.next() else { break };
        if let Some(part) = parts.get_mut(i) {
            part.clear();
        }
    }
    let mut spans = Vec::new();
    for part in parts.into_iter().filter(|p| !p.is_empty()) {
        if !spans.is_empty() {
            spans.push(Span::raw(GAP));
        }
        spans.extend(part);
    }
    Line::from(spans)
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn label_value(label: &'static str, value: String) -> Vec<Span<'static>> {
    vec![Span::styled(label, DIM), Span::raw(value)]
}

/// `boot ` before a since-boot value, so the user knows it is not live.
fn boot_prefix(since_boot: bool) -> Vec<Span<'static>> {
    if since_boot {
        vec![Span::styled("boot ", PINK)]
    } else {
        Vec::new()
    }
}

/// A percentage with one decimal, or `—` when unknown.
fn pct(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |v| format!("{v:.1}%"))
}

fn bar(cells: usize, colour: Color) -> Span<'static> {
    Span::styled(THICK_HORIZONTAL.repeat(cells), colour)
}

/// A line gauge `width` cells wide: the filled part in violet, the rest faint.
fn gauge(ratio: f64, width: usize) -> [Span<'static>; 2] {
    // `as usize` saturates and maps NaN to 0.
    let filled = ((ratio.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    [bar(filled, VIOLET), bar(width - filled, FAINT)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::render_to_string;
    use crate::zfs::{derive, parse_kstat};

    fn a1() -> crate::zfs::ArcRaw {
        parse_kstat(include_str!("../../testdata/arcstats-2.2.txt")).unwrap()
    }

    fn a2() -> crate::zfs::ArcRaw {
        parse_kstat(include_str!("../../testdata/arcstats-2.2-next.txt")).unwrap()
    }

    fn render(width: u16, height: u16, arc: &ZfsArc) -> String {
        render_to_string(width, height, |frame| draw_zfs(frame, frame.area(), arc))
    }

    /// The screen line that holds `needle`.
    fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
        screen
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{screen}"))
    }

    #[test]
    fn want_height_adds_a_line_for_l2() {
        let with_l2 = derive(None, &a1());
        assert_eq!(want_height(&with_l2), 7);
        let without = ZfsArc {
            l2: None,
            ..with_l2
        };
        assert_eq!(want_height(&without), 6);
    }

    #[test]
    fn window_values_show_without_boot() {
        let arc = derive(Some(&a1()), &a2());
        let screen = render(60, 7, &arc);
        for text in ["zfs arc", "9.0 GiB", "90.0%", "l2", "2.42"] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
        assert!(!screen.contains("boot"), "unexpected boot in:\n{screen}");
    }

    #[test]
    fn since_boot_values_are_labelled() {
        let screen = render(60, 7, &derive(None, &a1()));
        assert!(screen.contains("boot hit 85.0%"), "{screen}");
    }

    #[test]
    fn hit_line_drops_whole_parts_at_80_columns() {
        let arc = derive(Some(&a1()), &a2());
        let screen = render(40, want_height(&arc), &arc);
        let line = line_with(&screen, "hit 90.0%");
        assert!(line.contains("io 0.0%"), "{line}");
        assert!(line.contains("miss 10.0%"), "{line}");
        assert!(!line.contains("dd"), "{line}");
        assert!(!line.contains("dm"), "{line}");

        let boot = derive(None, &a1());
        let screen = render(40, want_height(&boot), &boot);
        let line = line_with(&screen, "boot hit 85.0%");
        assert!(line.contains("io 0.4%"), "{line}");
        assert!(line.contains("miss 14.6%"), "{line}");
        assert!(!line.contains("dd") && !line.contains("dm"), "{line}");
    }

    #[test]
    fn hit_line_shows_every_part_when_wide() {
        let arc = derive(Some(&a1()), &a2());
        let screen = render(80, want_height(&arc), &arc);
        let line = line_with(&screen, "hit 90.0%");
        assert!(
            line.contains("hit 90.0%  io 0.0%  miss 10.0%  dd 90.0%  dm 90.0%"),
            "{line}"
        );
    }

    #[test]
    fn missing_values_show_a_dash_and_io_is_dropped() {
        let arc = ZfsArc {
            size: 1 << 30,
            max: 2 << 30,
            hit_since_boot: true,
            ..ZfsArc::default()
        };
        let screen = render(80, want_height(&arc), &arc);
        let line = line_with(&screen, "hit");
        assert!(line.contains("boot hit —  miss —  dd —  dm —"), "{line}");
        assert!(!line.contains("io"), "{line}");
        assert!(!screen.contains("comp"), "no ratio, no comp:\n{screen}");
    }

    #[test]
    fn minimum_size_shows_size_and_hit_lines() {
        let arc = derive(Some(&a1()), &a2());
        let screen = render(MIN_WIDTH, MIN_HEIGHT, &arc);
        assert!(screen.contains("size 9.0 GiB"), "{screen}");
        assert!(screen.contains("hit 90.0%"), "{screen}");
    }

    #[test]
    fn want_height_shows_every_line() {
        let arc = derive(Some(&a1()), &a2());
        let screen = render(60, want_height(&arc), &arc);
        for text in [
            "size 9.0 GiB / target 10.0 GiB",
            "hit 90.0%",
            "mfu 3.0 GiB  mru 5.0 GiB",
            "data 6.1 GiB  meta 2.0 GiB  comp 2.42×",
            "l2 200.0 GiB (160.0 GiB)  hit 10.0%",
        ] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
    }

    #[test]
    fn wide_gauge_shows_min_and_max() {
        let arc = derive(Some(&a1()), &a2());
        let screen = render(100, want_height(&arc), &arc);
        assert!(screen.contains("min 1.0 GiB max 16.0 GiB"), "{screen}");
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let arc = derive(None, &a1());
        for (w, h) in [(1, 1), (10, 3), (200, 2), (0, 0), (3, 7)] {
            render_to_string(w.max(1), h.max(1), |frame| {
                draw_zfs(frame, Rect::new(0, 0, w, h), &arc)
            });
        }
    }
}
