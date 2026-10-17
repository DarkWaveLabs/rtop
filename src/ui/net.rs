//! Network panel drawing.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Cell, Row, Table};

use super::{BLUE, DIM, PINK, panel};
use crate::format;
use crate::net::NetIface;

pub const MIN_HEIGHT: u16 = 4;
pub const MAX_ROWS: u16 = 6;

/// The total column shows only from this inner width.
const TOTAL_MIN_WIDTH: u16 = 48;
/// Widest rate text: `1023.9 KiB/s`.
const RATE_WIDTH: u16 = 12;
/// Widest total text: `999M/9.9G`, as wide as the `TOTAL ↓/↑` header.
const TOTAL_WIDTH: u16 = 9;

/// 2 borders + 1 header + min(ifaces, MAX_ROWS).
pub fn want_height(ifaces: &[NetIface]) -> u16 {
    let rows = u16::try_from(ifaces.len())
        .unwrap_or(u16::MAX)
        .min(MAX_ROWS);
    2 + 1 + rows
}

pub fn draw_interfaces(frame: &mut Frame, area: Rect, ifaces: &[NetIface]) {
    let noun = if ifaces.len() == 1 {
        "interface"
    } else {
        "interfaces"
    };
    let block = panel(format!(" network · {} {noun} ", ifaces.len()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let with_total = inner.width >= TOTAL_MIN_WIDTH;
    let header_style = Style::new().fg(DIM).add_modifier(Modifier::BOLD);
    let mut header = vec![
        Cell::from("IFACE"),
        Cell::from(Line::from("↓ RX/s").right_aligned()),
        Cell::from(Line::from("↑ TX/s").right_aligned()),
    ];
    let mut widths = vec![
        Constraint::Fill(1),
        Constraint::Length(RATE_WIDTH),
        Constraint::Length(RATE_WIDTH),
    ];
    if with_total {
        header.push(Cell::from(Line::from("TOTAL ↓/↑").right_aligned()));
        widths.push(Constraint::Length(TOTAL_WIDTH));
    }

    let busiest = busiest(ifaces);
    let rows: Vec<Row<'static>> = ifaces
        .iter()
        .enumerate()
        .map(|(i, iface)| {
            let mut cells = vec![
                Cell::from(iface.name.clone()),
                Cell::from(Line::from(format::rate(iface.rx_rate)).right_aligned()).style(BLUE),
                Cell::from(Line::from(format::rate(iface.tx_rate)).right_aligned()).style(PINK),
            ];
            if with_total {
                let total = format!(
                    "{}/{}",
                    short_bytes(iface.total_rx),
                    short_bytes(iface.total_tx)
                );
                cells.push(Cell::from(Line::from(total).right_aligned()).style(DIM));
            }
            let row = Row::new(cells);
            if busiest == Some(i) {
                row.style(Style::new().add_modifier(Modifier::BOLD))
            } else {
                row
            }
        })
        .collect();

    let table = Table::new(rows, widths)
        .header(Row::new(header).style(header_style))
        .column_spacing(2);
    frame.render_widget(table, inner);
}

/// Index of the interface with the largest rx + tx rate. `None` when nothing moves,
/// so an idle panel has no bold row. The first one wins a tie.
fn busiest(ifaces: &[NetIface]) -> Option<usize> {
    let load = |i: &NetIface| i.rx_rate.saturating_add(i.tx_rate);
    ifaces
        .iter()
        .enumerate()
        .filter(|(_, i)| load(i) > 0)
        .fold(None, |best: Option<(usize, u64)>, (idx, i)| match best {
            Some((_, top)) if top >= load(i) => best,
            _ => Some((idx, load(i))),
        })
        .map(|(idx, _)| idx)
}

/// A byte count in at most four cells, for example `512B`, `1.5K`, `340M`.
/// One decimal below 10, none above, so `rx/tx` fits the total column.
fn short_bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "K", "M", "G", "T", "P", "E"];
    let mut value = n as f64;
    let mut unit = 0;
    // Move up at 999.5, the point where a whole number would print four digits.
    while value >= 999.5 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{n}B"),
        _ if value < 9.95 => format!("{value:.1}{}", UNITS[unit]),
        _ => format!("{value:.0}{}", UNITS[unit]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::render_to_string;

    fn iface(name: &str, rx_rate: u64, tx_rate: u64) -> NetIface {
        NetIface {
            name: name.into(),
            rx_rate,
            tx_rate,
            total_rx: rx_rate * 1_000,
            total_tx: tx_rate * 1_000,
        }
    }

    fn three() -> Vec<NetIface> {
        vec![
            iface("en0", 2_048, 512),
            iface("utun3", 10, 20),
            iface("wg0", 0, 300 << 10),
        ]
    }

    fn render(width: u16, height: u16, ifaces: &[NetIface]) -> String {
        render_to_string(width, height, |frame| {
            draw_interfaces(frame, frame.area(), ifaces)
        })
    }

    #[test]
    fn want_height_is_borders_header_and_capped_rows() {
        assert_eq!(want_height(&[]), 3);
        assert_eq!(want_height(&three()), 6);
        let many: Vec<NetIface> = (0..10).map(|i| iface(&format!("en{i}"), 1, 1)).collect();
        assert_eq!(want_height(&many), 2 + 1 + MAX_ROWS);
    }

    #[test]
    fn wide_panel_shows_title_names_rates_and_totals() {
        let ifaces = three();
        let screen = render(60, want_height(&ifaces), &ifaces);
        for text in [
            "network · 3 interfaces",
            "IFACE",
            "en0",
            "utun3",
            "wg0",
            "/s",
            "2.0 KiB/s",
            "300.0 KiB/s",
            "TOTAL",
            "2.0M/500K",
        ] {
            assert!(screen.contains(text), "missing {text:?} in:\n{screen}");
        }
    }

    #[test]
    fn one_interface_is_singular_in_the_title() {
        let ifaces = [iface("en0", 1, 1)];
        let screen = render(60, want_height(&ifaces), &ifaces);
        assert!(screen.contains("network · 1 interface "), "{screen}");
    }

    #[test]
    fn narrow_panel_drops_the_total_column() {
        let ifaces = three();
        let screen = render(40, 5, &ifaces);
        assert!(!screen.contains("TOTAL"), "{screen}");
        assert!(screen.contains("IFACE"), "{screen}");
        assert!(screen.contains("en0"), "{screen}");
    }

    #[test]
    fn total_column_appears_at_48_inner_columns() {
        let ifaces = three();
        assert!(render(50, 6, &ifaces).contains("TOTAL"));
        assert!(!render(49, 6, &ifaces).contains("TOTAL"));
    }

    #[test]
    fn rows_beyond_the_panel_height_are_cut() {
        let many: Vec<NetIface> = (0..10).map(|i| iface(&format!("en{i}"), 1, 1)).collect();
        let screen = render(60, want_height(&many), &many);
        assert!(screen.contains("network · 10 interfaces"), "{screen}");
        assert!(screen.contains("en5"), "{screen}");
        assert!(!screen.contains("en6"), "{screen}");
    }

    #[test]
    fn busiest_is_the_largest_sum_of_rates() {
        assert_eq!(busiest(&three()), Some(2));
        assert_eq!(busiest(&[iface("a", 5, 5), iface("b", 10, 0)]), Some(0));
        assert_eq!(busiest(&[iface("a", 0, 0), iface("b", 0, 0)]), None);
        assert_eq!(busiest(&[]), None);
    }

    #[test]
    fn busiest_row_is_bold() {
        use ratatui::style::Modifier;
        use ratatui::{Terminal, backend::TestBackend};

        let ifaces = three();
        let mut terminal = Terminal::new(TestBackend::new(60, 6)).unwrap();
        terminal
            .draw(|frame| draw_interfaces(frame, frame.area(), &ifaces))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let bold_at = |name: &str| {
            let y = (0..6)
                .find(|&y| {
                    (0..60)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .contains(name)
                })
                .unwrap();
            let x = (0..60)
                .find(|&x| buffer[(x, y)].symbol() == &name[..1])
                .unwrap();
            buffer[(x, y)].modifier.contains(Modifier::BOLD)
        };
        assert!(bold_at("wg0"));
        assert!(!bold_at("en0"));
    }

    #[test]
    fn short_bytes_fits_four_cells() {
        assert_eq!(short_bytes(0), "0B");
        assert_eq!(short_bytes(999), "999B");
        assert_eq!(short_bytes(1_000), "1.0K");
        assert_eq!(short_bytes(1_536), "1.5K");
        assert_eq!(short_bytes(500 << 10), "500K");
        assert_eq!(short_bytes(1 << 30), "1.0G");
        assert_eq!(short_bytes(u64::MAX), "16E");
        for n in (0..64).map(|s| (1u64 << s).saturating_mul(1_000)) {
            assert!(short_bytes(n).len() <= 4, "{n} gives {}", short_bytes(n));
        }
    }

    #[test]
    fn tiny_and_empty_areas_do_not_panic() {
        let ifaces = three();
        for (w, h) in [(1, 1), (2, 2), (10, 3), (200, 2), (5, 20)] {
            render(w, h, &ifaces);
        }
        render(60, 3, &[]);
        render_to_string(10, 10, |frame| {
            draw_interfaces(frame, Rect::new(0, 0, 0, 0), &ifaces)
        });
    }
}
